//! Consumer-side snapshot fetch + install pipeline.

use anyhow::Result;
use std::path::Path;

use super::config::load_config;

/// Resolve where the snapshot lives. Order:
/// 1. `.code-graph.toml` `[snapshot] url` (must be HTTPS)
/// 2. `[snapshot] disabled = true` → None
/// 3. Auto-detect from `git remote get-url origin` → GitHub release asset
pub fn resolve_snapshot_source(root: &Path) -> Option<String> {
    resolve_snapshot_source_impl(root, url_override_trusted(), origin_trusted())
}

/// Out-of-band trust signal for a `.code-graph.toml [snapshot] url` override.
/// The override lives in the repo, so any clone/PR can set it — a committed url
/// is therefore NOT honored unless the developer opts in via this environment
/// variable, which a PR cannot set. This is what blocks malicious-repo snapshot
/// injection (the audit's top finding): the auto-detected GitHub-release path
/// (origin remote) stays opt-out, but an arbitrary url override requires consent.
fn url_override_trusted() -> bool {
    std::env::var("CODE_GRAPH_SNAPSHOT_TRUST_URL")
        .ok()
        .as_deref()
        == Some("1")
}

/// Out-of-band trust for the auto-detected origin GitHub-release snapshot path.
/// Symmetric with [`url_override_trusted`]: opening an UNTRUSTED repo (e.g. cloned
/// to review before building) would otherwise auto-fetch + install that repo's
/// own published `code-graph-snapshot-*.db.zst` with only same-origin TOFU
/// verification — seeding a misleading graph (no code execution, but callgraph /
/// impact / dead-code on unchanged files would reflect attacker-chosen edges).
/// Trusted when the developer explicitly opts in, OR when a `CODE_GRAPH_SNAPSHOT_PIN`
/// is set (the pin makes a poisoned download fail content verification, so an
/// auto-fetch is safe).
fn origin_trusted() -> bool {
    std::env::var("CODE_GRAPH_SNAPSHOT_TRUST_ORIGIN")
        .ok()
        .as_deref()
        == Some("1")
        || std::env::var("CODE_GRAPH_SNAPSHOT_PIN")
            .ok()
            .filter(|s| !s.is_empty())
            .is_some()
}

/// Testable core of [`resolve_snapshot_source`]: `url_trusted` is the out-of-band
/// consent for a toml url override (env-read in the public wrapper).
pub(crate) fn resolve_snapshot_source_impl(
    root: &Path,
    url_trusted: bool,
    origin_trusted: bool,
) -> Option<String> {
    let cfg = load_config(root).ok()?;
    if cfg.snapshot.disabled {
        return None;
    }
    if let Some(url) = cfg.snapshot.url {
        // Said once, through tracing (audit 2026-08-29 CON-06). These used to
        // print twice: the comment that stood here said tracing was invisible on
        // the CLI/MCP startup paths, which stopped being true when `main` began
        // installing a subscriber for every subcommand. Being LOUD still matters
        // — without it users just see "No snapshot source resolved" and have no
        // idea their TOML override was rejected.
        if !url.starts_with("https://") {
            let msg = format!(
                "warning: .code-graph.toml [snapshot] url must start with https:// (got '{url}'), ignoring"
            );
            tracing::warn!("{msg}");
            return None;
        }
        if !url_trusted {
            let msg = format!(
                "warning: .code-graph.toml [snapshot] url override ('{url}') is NOT honored by \
                 default — a committed url could redirect the code graph to an attacker-chosen \
                 database. Set CODE_GRAPH_SNAPSHOT_TRUST_URL=1 in your environment to trust it."
            );
            tracing::warn!("{msg}");
            return None;
        }
        return Some(url);
    }
    gate_origin_url(|| resolve_from_github(root), origin_trusted)
}

/// Is this a git object id, and nothing else?
///
/// SEC-06 (audit 2026-08-29). The value this guards comes out of the snapshot
/// database being verified — attacker-controlled if the snapshot is — and used to
/// go straight into an argv position with no `--` separator, so a value starting
/// with `-` reached `git cat-file` as an OPTION. No exploit was demonstrated (no
/// shell is involved and `cat-file` exposes nothing executable), so this closes a
/// missing guard rather than a proven hole.
///
/// Validated rather than only separator-escaped, which is strictly better here:
/// an object id has exactly one shape (40 hex for SHA-1, 64 for SHA-256), so junk
/// is refused before it becomes a confusing git error. The `--` goes in too — the
/// sibling call site at `cli/health.rs:415` already passes it, and its comment
/// predicted that the inconsistency is how the next site would end up without it.
fn is_commit_id(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Gate the auto-detected origin GitHub-release snapshot behind out-of-band trust
/// ([`origin_trusted`]). Untrusted → skipped (the security fix); trusted (env opt-in
/// or a pin) → install proceeds.
///
/// SEC-07 (audit 2026-08-29). The resolver is a CLOSURE and stays uncalled without
/// trust. It used to be an eagerly-evaluated argument, so `git remote get-url origin`
/// plus a `gh api` round-trip ran on every project open regardless of trust and this
/// gate only suppressed the install afterwards — a check that reads as if it governs
/// the network call while not preventing it. Untrusted is the default, so gating
/// first also drops a pair of subprocesses from the overwhelmingly common path.
///
/// What that costs, stated rather than hidden: resolving first was deliberate — it
/// let the opt-in hint name the actual snapshot URL and stay silent for the many
/// repos that publish none. A gate that declines to look cannot know either, and
/// warning unconditionally would fire on every repo in the world. So the hint drops
/// to `debug` (visible when someone is asking why no snapshot installed) and the
/// opt-in lives where the other trust knobs are documented: README's env-var table.
pub(crate) fn gate_origin_url(
    resolve_origin_url: impl FnOnce() -> Option<String>,
    origin_trusted: bool,
) -> Option<String> {
    if !origin_trusted {
        tracing::debug!(
            "skipping GitHub-release snapshot auto-detect: installing a snapshot from the repo's \
             own release is opt-in, because a snapshot from an untrusted repo could seed a \
             misleading code graph (no code execution, but callgraph / impact / dead-code on \
             unchanged files would reflect attacker-chosen edges). Set \
             CODE_GRAPH_SNAPSHOT_TRUST_ORIGIN=1 (or a CODE_GRAPH_SNAPSHOT_PIN) to enable it."
        );
        return None;
    }
    resolve_origin_url()
}

fn resolve_from_github(root: &Path) -> Option<String> {
    // Silence stderr — non-git roots would otherwise leak `fatal: not a git repository`.
    let remote = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(root)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let url = String::from_utf8_lossy(&remote.stdout).trim().to_string();
    let (owner, repo) = parse_github_remote(&url)?;
    fetch_latest_snapshot_asset_url(&owner, &repo)
}

/// GitHub's own account/repository name alphabet. Anything outside it cannot be a
/// real remote, so refusing it costs nothing.
///
/// SEC-07 (audit 2026-08-29): `repo` is the tail of a `splitn(2, '/')`, so it used
/// to accept embedded `/` and `..` and carry them straight into the `gh api` path.
/// The demonstrated blast radius was small — `endpoint` always starts with the
/// literal `repos/`, so `gh` could not be pointed at another host, and the GET's
/// response never flows back to whoever wrote the remote — which makes this a
/// missing guard rather than a proven hole. It is still repo-supplied text reaching
/// an argv position, and the check is one predicate.
fn is_github_name(s: &str) -> bool {
    !s.is_empty()
        // `.` is legal INSIDE a name (`my_repo.v2`), so the alphabet alone still
        // admits the two names that are pure path traversal. GitHub rejects both.
        && s.chars().any(|c| c != '.')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

pub(crate) fn parse_github_remote(url: &str) -> Option<(String, String)> {
    // Supports https://github.com/o/r(.git) and git@github.com:o/r(.git)
    let stripped = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("git@github.com:"))?;
    let stripped = stripped.strip_suffix(".git").unwrap_or(stripped);
    let mut parts = stripped.splitn(2, '/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.trim_end_matches('/').to_string();
    if !is_github_name(&owner) || !is_github_name(&repo) {
        return None;
    }
    Some((owner, repo))
}

fn fetch_latest_snapshot_asset_url(owner: &str, repo: &str) -> Option<String> {
    // Use `gh api` for uniform auth (public + private). Fail silent on no `gh`.
    // Wrap with a 5s SIGTERM watchdog so a slow proxy / hung gh subprocess
    // can't block MCP server startup indefinitely (Task 10 review Q5).
    let endpoint = format!("repos/{owner}/{repo}/releases/latest");
    let child = std::process::Command::new("gh")
        .args(["api", &endpoint])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let output = wait_with_watchdog(child, std::time::Duration::from_secs(5))?;
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let assets = json.get("assets")?.as_array()?;
    let mut matches: Vec<&str> = assets
        .iter()
        .filter_map(|a| {
            let name = a.get("name")?.as_str()?;
            if name.starts_with("code-graph-snapshot-") && name.ends_with(".db.zst") {
                a.get("browser_download_url")?.as_str()
            } else {
                None
            }
        })
        .collect();
    // Deterministic pick when multiple assets match (lexicographic by URL)
    matches.sort();
    matches.first().map(|s| s.to_string())
}

use anyhow::Context;

const MAX_DECOMPRESSED_BYTES: u64 = 100 * 1024 * 1024; // 100 MB
                                                       // Cap the COMPRESSED payload too — a snapshot zst is single-digit MB in practice,
                                                       // but a missing/lying Content-Length must not let a huge body exhaust memory/disk.
const MAX_COMPRESSED_BYTES: u64 = 100 * 1024 * 1024; // 100 MB

/// Wait up to `cap` for the child to exit; SIGTERM it on Unix if it doesn't,
/// then collect output. Cancellation-aware so the happy path doesn't leak a
/// 5s sleeping thread that could SIGTERM a recycled PID.
fn wait_with_watchdog(
    child: std::process::Child,
    cap: std::time::Duration,
) -> Option<std::process::Output> {
    #[cfg(unix)]
    {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let pid = child.id() as i32;
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_w = Arc::clone(&cancel);
        let watchdog = std::thread::spawn(move || {
            let start = std::time::Instant::now();
            while start.elapsed() < cap {
                if cancel_w.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            // Cap exceeded: send SIGTERM. Pid is still alive because cancel
            // is set immediately after wait_with_output returns.
            // SAFETY: kill(2) is always safe to call — it cannot violate memory
            // safety. `pid` is our own spawned child's id; SIGTERM (not SIGKILL)
            // asks the slow/hung `gh` subprocess to exit and is a no-op if it
            // already died. The cancel flag makes a recycled-PID race impossible.
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
        });
        let result = child.wait_with_output().ok().filter(|o| o.status.success());
        cancel.store(true, Ordering::Relaxed);
        let _ = watchdog.join();
        result
    }
    #[cfg(not(unix))]
    {
        // No watchdog on non-Unix: rely on `gh.exe`'s own timeout behavior.
        let _ = cap;
        child.wait_with_output().ok().filter(|o| o.status.success())
    }
}

pub fn try_install(url: &str, root: &Path) -> Result<String> {
    use crate::storage::db::Database;
    use std::time::{SystemTime, UNIX_EPOCH};

    if !(url.starts_with("https://") || url.starts_with("file://")) {
        anyhow::bail!("snapshot url must be https:// (got {url})");
    }

    let cg_dir = root.join(crate::domain::CODE_GRAPH_DIR);
    // The fourth `.code-graph` creator, and the one the SEC-03 batch missed:
    // `create_dir_all` succeeds silently on a symlinked directory, so a snapshot
    // would land — and its atomic rename would replace files — outside the
    // project root.
    crate::utils::owned::ensure_owned_dir(&cg_dir)?;

    // Use a per-invocation unique suffix so concurrent installers don't clobber
    // each other's in-progress partials.  The final atomic rename serialises who
    // wins; the loser's rename simply replaces what the winner wrote (both files
    // are valid, so the last rename wins cleanly).
    static INSTALL_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = INSTALL_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pid = std::process::id() as u64;
    let id: u64 = pid.wrapping_mul(0x9e37_79b9_7f4a_7c15).wrapping_add(seq);
    let zst_partial = cg_dir.join(format!(".snapshot-{id:016x}.db.zst.partial"));
    let db_partial = cg_dir.join(format!(".snapshot-{id:016x}.db.partial"));
    // WAL sidecars SQLite creates next to db_partial; cleaned on every exit path.
    let wal_partial = cg_dir.join(format!(".snapshot-{id:016x}.db.partial-wal"));
    let shm_partial = cg_dir.join(format!(".snapshot-{id:016x}.db.partial-shm"));

    let install_inner = || -> Result<String> {
        download(url, &zst_partial)?;
        // Verify integrity BEFORE decompressing — never decode untrusted bytes.
        verify_checksum(url, &zst_partial)?;
        decompress_with_cap(&zst_partial, &db_partial, MAX_DECOMPRESSED_BYTES)?;
        validate(&db_partial, root)?;

        // Write consumer-side meta (source_url + fetched_at) into our UNIQUE
        // partial, NOT the shared final index.db. The previous order — rename into
        // place, THEN open index.db and WAL-write meta — raced a concurrent
        // installer's rename: thread B replacing index.db reinitialised the shared
        // `index.db-shm` WAL-index under thread A's open connection, so A's next
        // frame write SIGBUSed in walIndexAppend (the
        // snapshot_install_concurrent_serialized_via_filesystem flake). Writing meta
        // to our own partial means no thread ever WAL-writes the shared file; the
        // atomic rename below is the sole operation on index.db.
        let commit = {
            let db = Database::open(&db_partial)?;
            let conn = db.conn();
            super::meta::write_meta(conn, super::meta::META_SNAPSHOT_SOURCE_URL, url)?;
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            super::meta::write_meta(
                conn,
                super::meta::META_SNAPSHOT_FETCHED_AT,
                &now.to_string(),
            )?;
            let commit = super::meta::read_meta(conn, super::meta::META_SNAPSHOT_SOURCE_COMMIT)?
                .unwrap_or_default();
            // Fold the WAL into the main file so the rename moves a complete DB and
            // leaves no orphaned -wal still carrying the meta we just wrote.
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
            commit
        }; // connection dropped here → -wal/-shm released before the rename

        // POSIX rename(2) atomically replaces the destination — pre-deleting would
        // open a TOCTOU window where a concurrent reader sees no file. The partial
        // is now a complete, closed DB; the last concurrent rename wins cleanly.
        let final_db = cg_dir.join("index.db");

        // The DESTINATION's sidecars, not ours. rename(2) replaces index.db and
        // nothing else, so a `-wal` stranded beside it by a killed writer outlives
        // the swap and SQLite replays those pages into the file we just installed
        // — reverting the snapshot to fragments of the database it replaced, with
        // `integrity_check` still reporting ok (audit 2026-08-16 P1-1). Removing
        // them here rather than in each caller is the point: two of the three call
        // sites had grown their own copy of this and the MCP server path had not.
        //
        // Before the rename, not after: the old index.db is a complete database
        // without its WAL tail and is about to be replaced anyway, whereas a gap
        // on the other side would expose the NEW file to exactly the replay this
        // removes. Best-effort — a sidecar we cannot delete (Windows share
        // violation from a live reader) must not fail an otherwise good install,
        // and the rename below is still strictly better than not installing.
        for suffix in ["-wal", "-shm"] {
            let sidecar = cg_dir.join(format!("index.db{suffix}"));
            if sidecar.exists() {
                if let Err(e) = std::fs::remove_file(&sidecar) {
                    tracing::warn!(
                        "[snapshot] could not remove stale {} before install: {e}",
                        sidecar.display()
                    );
                }
            }
        }

        std::fs::rename(&db_partial, &final_db)?;
        let _ = std::fs::remove_file(&wal_partial);
        let _ = std::fs::remove_file(&shm_partial);
        Ok(commit)
    };

    match install_inner() {
        Ok(commit) => {
            let _ = std::fs::remove_file(&zst_partial);
            Ok(commit)
        }
        Err(e) => {
            // Best-effort: remove our own partials. We do NOT delete final_db
            // here even if this thread renamed it — a concurrent thread could
            // have rename-replaced it with its own valid snapshot since, and
            // deleting would destroy the only good copy. A snapshot DB without
            // source_url/fetched_at meta is still a usable index.
            let _ = std::fs::remove_file(&zst_partial);
            let _ = std::fs::remove_file(&db_partial);
            let _ = std::fs::remove_file(&wal_partial);
            let _ = std::fs::remove_file(&shm_partial);
            Err(e)
        }
    }
}

/// Build a blocking HTTP client that follows redirects ONLY while every hop
/// stays on HTTPS. A redirect to a non-`https` location is rejected instead of
/// followed: otherwise a network attacker could downgrade the snapshot artifact
/// or its `.blake3` integrity sidecar to plaintext and substitute content,
/// silently defeating the checksum verification this module performs (audit
/// 2026-06-03 #1 — redirect-downgrade gap caught in branch review).
fn https_only_client(timeout: std::time::Duration) -> Result<reqwest::blocking::Client> {
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.url().scheme() != "https" {
            let bad = attempt.url().clone();
            attempt.error(format!(
                "refusing redirect to non-HTTPS location '{bad}' (integrity downgrade)"
            ))
        } else if attempt.previous().len() >= 10 {
            attempt.error("too many redirects".to_string())
        } else {
            attempt.follow()
        }
    });
    Ok(reqwest::blocking::Client::builder()
        .timeout(timeout)
        .redirect(policy)
        .build()?)
}

fn download(url: &str, dest: &Path) -> Result<()> {
    if let Some(file_path) = url.strip_prefix("file://") {
        // file:// is test-only and config-controlled; no path sanitisation.
        std::fs::copy(file_path, dest).context("file:// copy")?;
        return Ok(());
    }
    // Stream to disk with a hard cap on the compressed side instead of buffering
    // the whole response. Reject early if Content-Length advertises oversize, and
    // cap bytes actually written so a missing/lying length still can't run away.
    // https_only_client rejects redirect-downgrade to plaintext.
    let mut resp = https_only_client(std::time::Duration::from_secs(30))?
        .get(url)
        .send()?
        .error_for_status()?;
    if let Some(len) = resp.content_length() {
        if len > MAX_COMPRESSED_BYTES {
            anyhow::bail!(
                "snapshot download advertises {len} bytes (> {MAX_COMPRESSED_BYTES} cap)"
            );
        }
    }
    let mut out = std::fs::File::create(dest).context("create download file")?;
    let mut writer = CapWriter::new(&mut out, MAX_COMPRESSED_BYTES);
    std::io::copy(&mut resp, &mut writer).context("stream download to disk")?;
    Ok(())
}

/// Out-of-band integrity pin for the snapshot artifact, read from
/// `CODE_GRAPH_SNAPSHOT_PIN` (a blake3 hex digest). Like the url-trust signal, it
/// lives in the *environment* — deliberately NOT in `.code-graph.toml` — so a
/// committed/PR-injected config file cannot set it. See [`verify_checksum_impl`]
/// for why this matters.
fn snapshot_pin() -> Option<String> {
    std::env::var("CODE_GRAPH_SNAPSHOT_PIN")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// blake3 of the (cap-bounded) artifact as lower-hex.
fn hash_artifact(artifact: &Path) -> Result<String> {
    let mut file = std::fs::File::open(artifact).context("open artifact for checksum")?;
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut file, &mut hasher).context("hash artifact")?;
    Ok(hasher.finalize().to_hex().to_string())
}

/// Verify the downloaded compressed artifact's integrity. Env-reads the
/// out-of-band pin and delegates to [`verify_checksum_impl`].
fn verify_checksum(url: &str, artifact: &Path) -> Result<()> {
    verify_checksum_impl(url, artifact, snapshot_pin())
}

/// Two integrity tiers, strongest first:
///
/// 1. **Out-of-band pin** (`pin`, from `CODE_GRAPH_SNAPSHOT_PIN`): when set it is
///    the SOLE authority — the artifact must blake3-match it, no network sidecar
///    is consulted, and it applies even to `file://` sources. This is what closes
///    the residual `CODE_GRAPH_SNAPSHOT_TRUST_URL` gap: once a url override is
///    trusted, the attacker controls both the artifact AND its `<url>.blake3`
///    sidecar, so a sidecar-derived checksum is circular. A pin lives in the
///    developer's environment (a committed/PR file can't set it), so it holds
///    independent of the url host.
/// 2. **`<url>.blake3` sidecar** (when no pin): hard-fail on mismatch. When no
///    sidecar can be fetched (404 / network error / an unpublished checksum) and
///    no pin is set, integrity cannot be established, so install is REFUSED —
///    this fail-CLOSES (the M11 hardening; it used to warn and continue).
///    Escape hatches: set `CODE_GRAPH_SNAPSHOT_PIN`, or have the publisher serve
///    the `.blake3` sidecar. `file://` sources are the one exception — they are
///    test/config-controlled and local, so they install without a sidecar (TOFU).
fn verify_checksum_impl(url: &str, artifact: &Path, pin: Option<String>) -> Result<()> {
    if let Some(pin) = pin {
        let pin = pin.trim();
        // blake3 hex is 64 chars; reject anything else loudly rather than failing
        // with a confusing "mismatch" when a user pastes the wrong value.
        if pin.len() != 64 || !pin.bytes().all(|b| b.is_ascii_hexdigit()) {
            anyhow::bail!(
                "CODE_GRAPH_SNAPSHOT_PIN must be a 64-char blake3 hex digest \
                 (got {} chars) — refusing to install",
                pin.len()
            );
        }
        let actual = hash_artifact(artifact)?;
        if !actual.eq_ignore_ascii_case(pin) {
            anyhow::bail!(
                "snapshot checksum mismatch (blake3): CODE_GRAPH_SNAPSHOT_PIN expected \
                 {pin}, got {actual} — refusing to install"
            );
        }
        tracing::debug!(
            "snapshot checksum verified against CODE_GRAPH_SNAPSHOT_PIN (blake3 {actual})"
        );
        return Ok(());
    }

    // file:// is test/config-controlled; no network sidecar to fetch.
    if url.starts_with("file://") {
        return Ok(());
    }
    let sidecar_url = format!("{url}.blake3");
    // Same https-only redirect guard as the artifact download: a downgraded
    // sidecar would let an attacker hand us a checksum matching their artifact.
    let fetched = https_only_client(std::time::Duration::from_secs(15))?
        .get(&sidecar_url)
        .send();
    let expected = match fetched {
        Ok(r) if r.status().is_success() => r.text().unwrap_or_default().trim().to_string(),
        _ => String::new(),
    };
    if expected.is_empty() {
        // No out-of-band pin AND no fetchable integrity sidecar → integrity cannot
        // be established. Refuse rather than install unverified content: a network
        // attacker, a 404'd/blocked sidecar, or an HTTPS-downgrade could otherwise
        // hand us an arbitrary artifact over the origin-trusted path (M11 — this
        // used to warn and fail OPEN). Escape hatches: an out-of-band
        // CODE_GRAPH_SNAPSHOT_PIN, or the publisher serving the `.blake3` sidecar.
        anyhow::bail!(
            "snapshot integrity could not be verified: no checksum sidecar at \
             {sidecar_url} and no CODE_GRAPH_SNAPSHOT_PIN set — refusing to install. \
             Set CODE_GRAPH_SNAPSHOT_PIN=<blake3 hex> or ensure the publisher serves \
             the .blake3 sidecar alongside the snapshot."
        );
    }
    // Stream-hash the artifact (it is capped at MAX_COMPRESSED_BYTES).
    let actual = hash_artifact(artifact)?;
    if !actual.eq_ignore_ascii_case(&expected) {
        anyhow::bail!(
            "snapshot checksum mismatch (blake3): expected {expected}, got {actual} — refusing to install"
        );
    }
    tracing::debug!("snapshot checksum verified (blake3 {actual})");
    Ok(())
}

fn decompress_with_cap(src: &Path, dst: &Path, cap: u64) -> Result<()> {
    let f = std::fs::File::open(src).context("open compressed")?;
    let mut decoder = zstd::Decoder::new(f).context("zstd decoder init")?;
    let mut out = std::fs::File::create(dst).context("create decompressed")?;
    let mut writer = CapWriter::new(&mut out, cap);
    std::io::copy(&mut decoder, &mut writer).context("zstd decode")?;
    Ok(())
}

struct CapWriter<'a, W: std::io::Write> {
    inner: &'a mut W,
    written: u64,
    cap: u64,
}

impl<'a, W: std::io::Write> CapWriter<'a, W> {
    fn new(inner: &'a mut W, cap: u64) -> Self {
        Self {
            inner,
            written: 0,
            cap,
        }
    }
}

impl<'a, W: std::io::Write> std::io::Write for CapWriter<'a, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.written + buf.len() as u64 > self.cap {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("snapshot exceeds {} byte cap", self.cap),
            ));
        }
        let n = self.inner.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn validate(db_path: &Path, root: &Path) -> Result<()> {
    use crate::storage::db::Database;
    use crate::storage::schema::SCHEMA_VERSION;

    let db = Database::open(db_path).context("open snapshot for validation")?;
    let conn = db.conn();

    let snap_schema: i32 = super::meta::read_meta(conn, super::meta::META_SNAPSHOT_SCHEMA_VERSION)?
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if snap_schema > SCHEMA_VERSION {
        anyhow::bail!(
            "snapshot schema v{snap_schema} newer than binary v{SCHEMA_VERSION}, \
             upgrade code-graph-mcp"
        );
    }

    // Warn (not fail) if snapshot commit is not in local history
    if let Some(commit) = super::meta::read_meta(conn, super::meta::META_SNAPSHOT_SOURCE_COMMIT)? {
        // SEC-06 (audit 2026-08-29). `commit` comes out of the snapshot database
        // being verified — attacker-controlled if the snapshot is — and went
        // straight into an argv position with no `--` separator, so a value
        // starting with `-` reached git as an OPTION. No exploit was demonstrated
        // (no shell, and `cat-file` exposes nothing executable), so this closes a
        // missing guard rather than a proven hole; the sibling call site at
        // `cli/health.rs:415` already passes `--` and its comment predicted that
        // the inconsistency is how the next site would end up without it.
        //
        // Validated rather than separator-escaped, which the sibling's comment
        // asks for and which is strictly better here: a commit id has exactly one
        // shape, so junk is refused early and loudly instead of becoming a
        // confusing git error.
        if is_commit_id(&commit) {
            // Silence stderr — `cat-file -e` prints "fatal: ..." when commit missing,
            // which is the expected case we want to detect (forks/rebases).
            let exists = std::process::Command::new("git")
                .args(["cat-file", "-e", "--", &commit])
                .current_dir(root)
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !exists {
                tracing::warn!(
                    "snapshot commit {commit} not in local git history (fork/rebase?), continuing"
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// SEC-06: the snapshot-supplied commit reaches `git` only when it is an
    /// object id. Option-shaped and traversal-shaped values are the point — they
    /// are what a hostile snapshot would put there.
    #[test]
    fn only_a_real_object_id_is_handed_to_git() {
        for good in [
            "0123456789abcdef0123456789abcdef01234567",
            "0123456789ABCDEF0123456789abcdef01234567",
            &"a".repeat(64),
        ] {
            assert!(super::is_commit_id(good), "{good} is a valid object id");
        }
        for bad in [
            "",
            "--upload-pack=touch /tmp/pwned",
            "-e",
            "--help",
            "HEAD",
            "0123456789abcdef0123456789abcdef0123456", // 39 — one short
            "0123456789abcdef0123456789abcdef012345678", // 41 — one long
            "0123456789abcdef0123456789abcdef0123456g", // non-hex
            "../../etc/passwd",
        ] {
            assert!(
                !super::is_commit_id(bad),
                "{bad:?} must never reach the git argv"
            );
        }
    }

    use super::*;
    use tempfile::TempDir;

    /// Replaces the prior 110-MB integration test (`snapshot_integration.rs::
    /// snapshot_install_rejects_oversized_uncompressed`) that flaked on
    /// GitHub-hosted Linux runners with SIGBUS — heap-allocating 110 MB and
    /// writing it to disk while three other integration tests run in parallel
    /// hit mmap/disk-pressure boundaries on the small CI tmpfs. The cap
    /// behavior lives entirely inside `decompress_with_cap`/`CapWriter`, so
    /// scoping the test here keeps the same coverage at 5 KiB instead.
    /// Minimal local HTTP server: the first request gets a 302 redirect to a
    /// plaintext `http://` location on the same port; that location serves
    /// attacker-controlled bytes. Reproduces an HTTPS→HTTP downgrade-via-redirect
    /// (the initial request here is http for test simplicity; the redirect *target*
    /// being non-https is what the integrity gate must reject). Detached thread.
    fn spawn_downgrade_redirect_server() -> String {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let evil_target = format!("http://127.0.0.1:{port}/evil.db.zst");
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = match stream {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let mut buf = [0u8; 1024];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req
                    .lines()
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/");
                let resp = if path.contains("evil") {
                    "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nevil"
                        .to_string()
                } else {
                    format!("HTTP/1.1 302 Found\r\nLocation: {evil_target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                };
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
        });
        base
    }

    // Security regression (audit #1 follow-up, branch review H1): the snapshot
    // download MUST NOT follow a redirect that downgrades to a non-HTTPS location.
    // Following it lets a network attacker serve a crafted artifact (and matching
    // .blake3 sidecar) over plaintext, defeating the checksum gate entirely.
    #[test]
    fn download_rejects_http_redirect_downgrade() {
        let base = spawn_downgrade_redirect_server();
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("snapshot.db.zst");
        let res = download(&format!("{base}/snapshot.db.zst"), &dest);
        assert!(
            res.is_err(),
            "download must reject a redirect to a non-HTTPS location (integrity downgrade); got {res:?}"
        );
        // The error must fire before any attacker bytes are persisted.
        if dest.exists() {
            let got = std::fs::read(&dest).unwrap_or_default();
            assert_ne!(
                got, b"evil",
                "must not persist content fetched over a downgraded redirect"
            );
        }
    }

    // An out-of-band CODE_GRAPH_SNAPSHOT_PIN matching the artifact accepts it
    // WITHOUT touching the network — the url here is attacker-shaped, so a non-Ok
    // result (a sidecar fetch to attacker.invalid) would prove the pin did not
    // short-circuit. Ok proves the pin is the sole, url-independent authority.
    #[test]
    fn verify_checksum_pin_match_accepts_without_network() {
        let tmp = TempDir::new().unwrap();
        let artifact = tmp.path().join("snap.db.zst");
        let bytes = b"the genuine snapshot bytes";
        std::fs::write(&artifact, bytes).unwrap();
        let pin = blake3::hash(bytes).to_hex().to_string();
        let r = verify_checksum_impl("https://attacker.invalid/snap.db.zst", &artifact, Some(pin));
        assert!(r.is_ok(), "a correct pin must accept the artifact: {r:?}");
    }

    // Closes the residual CODE_GRAPH_SNAPSHOT_TRUST_URL gap: once a url override is
    // trusted, the attacker serves BOTH a malicious artifact and a `.blake3`
    // sidecar matching it, so the sidecar-only check would pass. With a pin set to
    // the genuine hash, the attacker's different artifact is rejected — the pin
    // never consults the attacker-controlled sidecar.
    #[test]
    fn verify_checksum_pin_defeats_attacker_matched_sidecar() {
        let tmp = TempDir::new().unwrap();
        let genuine_pin = blake3::hash(b"genuine snapshot").to_hex().to_string();
        let evil = tmp.path().join("evil.db.zst");
        std::fs::write(&evil, b"malicious snapshot").unwrap();
        let err = verify_checksum_impl(
            "https://attacker.invalid/evil.db.zst",
            &evil,
            Some(genuine_pin),
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("mismatch"),
            "a pinned hash must reject an attacker artifact even with a matching sidecar: {err:?}"
        );
    }

    #[test]
    fn verify_checksum_pin_mismatch_names_the_env_var() {
        let tmp = TempDir::new().unwrap();
        let artifact = tmp.path().join("snap.db.zst");
        std::fs::write(&artifact, b"genuine").unwrap();
        let err = verify_checksum_impl(
            "https://x.invalid/x.db.zst",
            &artifact,
            Some("0".repeat(64)),
        )
        .unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("mismatch") && chain.contains("CODE_GRAPH_SNAPSHOT_PIN"),
            "expected a pin-mismatch error naming the env var, got: {chain}"
        );
    }

    #[test]
    fn verify_checksum_pin_rejects_malformed() {
        let tmp = TempDir::new().unwrap();
        let artifact = tmp.path().join("snap.db.zst");
        std::fs::write(&artifact, b"x").unwrap();
        // Too short + non-hex → loud rejection, not a confusing "mismatch".
        let err = verify_checksum_impl(
            "https://x.invalid/x.db.zst",
            &artifact,
            Some("nothex".into()),
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("CODE_GRAPH_SNAPSHOT_PIN"),
            "a malformed pin must name the env var: {err:?}"
        );
    }

    // M11: with no out-of-band pin, a NETWORK snapshot whose integrity sidecar
    // cannot be fetched (404 / network error / no .blake3 published) must be
    // REFUSED, not installed unverified — otherwise a network attacker (or a
    // blocked/downgraded sidecar fetch) hands us an arbitrary artifact. `.invalid`
    // never resolves, so the sidecar fetch fails → empty → the fail-closed path.
    #[test]
    fn verify_checksum_no_pin_missing_sidecar_fails_closed() {
        let tmp = TempDir::new().unwrap();
        let artifact = tmp.path().join("snap.db.zst");
        std::fs::write(&artifact, b"unverifiable bytes").unwrap();
        let r = verify_checksum_impl(
            "https://sidecar-absent.invalid/snap.db.zst",
            &artifact,
            None,
        );
        assert!(
            r.is_err(),
            "a network snapshot with no fetchable integrity sidecar and no pin must be refused; got {r:?}",
        );
    }

    // No pin: behavior is unchanged — a file:// source still installs without a
    // network sidecar (the existing TOFU path the production tests rely on).
    #[test]
    fn verify_checksum_no_pin_file_url_is_ok() {
        let tmp = TempDir::new().unwrap();
        let artifact = tmp.path().join("snap.db.zst");
        std::fs::write(&artifact, b"whatever").unwrap();
        assert!(verify_checksum_impl("file:///x.db.zst", &artifact, None).is_ok());
    }

    #[test]
    fn decompress_with_cap_rejects_oversized() {
        let payload = vec![0u8; 5 * 1024]; // 5 KiB decompressed
        let zst_bytes = zstd::encode_all(&payload[..], 1).unwrap();
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("payload.zst");
        let dst = tmp.path().join("out.bin");
        std::fs::write(&src, zst_bytes).unwrap();

        let cap: u64 = 1024;
        let err = decompress_with_cap(&src, &dst, cap).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("cap") || chain.contains(&cap.to_string()),
            "expected 'cap' or '{cap}' in error chain, got: {chain}"
        );
    }
}
