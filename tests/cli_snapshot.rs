//! CLI smoke tests for `code-graph-mcp snapshot create|inspect`.

use std::process::Command;
use tempfile::TempDir;

fn cli_bin() -> String {
    env!("CARGO_BIN_EXE_code-graph-mcp").to_string()
}

fn init_git_repo() -> TempDir {
    let dir = TempDir::new().unwrap();
    let p = dir.path();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(p)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "t@t"])
        .current_dir(p)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "t"])
        .current_dir(p)
        .status()
        .unwrap();
    std::fs::create_dir_all(p.join("src")).unwrap();
    std::fs::write(p.join("src/lib.rs"), "pub fn h() {}\n").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(p)
        .status()
        .unwrap();
    Command::new("git")
        .args(["commit", "-q", "-m", "init"])
        .current_dir(p)
        .status()
        .unwrap();
    dir
}

/// The `[snapshot] url` trust gate, exercised through the real binary.
///
/// # Why this is not covered by the unit tests
///
/// `resolve_snapshot_source_impl` takes `url_trusted` as a PARAMETER, and every
/// unit test passes it directly. The env read lives in the thin public wrapper
/// `resolve_snapshot_source`, which no test reaches — so renaming the variable,
/// inverting the comparison, or reading `SNAPSHOT_TRUST_ORIGIN` here by
/// copy-paste would leave the whole suite green while the gate that blocks
/// malicious-repo snapshot injection silently stopped opening (or worse, stopped
/// closing). Subprocesses, not `set_var`: the env is process-global and the rest
/// of the suite runs in parallel.
///
/// Case B is the non-vacuity anchor. Without a case that is ALLOWED through,
/// "everything was refused" is equally consistent with a gate that works and a
/// gate wired to a variable nobody sets. Port 1 is reserved, so the trusted
/// fetch fails on connection-refused immediately and offline.
#[test]
fn cli_snapshot_url_override_honors_only_its_own_env_signal() {
    let repo = init_git_repo();
    std::fs::write(
        repo.path().join(".code-graph.toml"),
        "[snapshot]\nurl = \"https://127.0.0.1:1/attacker.db.zst\"\n",
    )
    .unwrap();

    const REFUSAL: &str = "CODE_GRAPH_SNAPSHOT_TRUST_URL=1 in your environment to trust it";
    const ATTEMPTED: &str = "Snapshot install failed";
    let pin = "a".repeat(64);

    /// label, the one trust signal this case sets (if any), and whether the
    /// committed url override is expected to be honored.
    type Case<'a> = (&'a str, Option<(&'a str, &'a str)>, bool);

    let cases: [Case; 6] = [
        ("no signal", None, false),
        (
            "TRUST_URL=1",
            Some(("CODE_GRAPH_SNAPSHOT_TRUST_URL", "1")),
            true,
        ),
        (
            "TRUST_ORIGIN=1 must not unlock a url override",
            Some(("CODE_GRAPH_SNAPSHOT_TRUST_ORIGIN", "1")),
            false,
        ),
        (
            "PIN must not unlock a url override",
            Some(("CODE_GRAPH_SNAPSHOT_PIN", pin.as_str())),
            false,
        ),
        (
            "TRUST_URL=0",
            Some(("CODE_GRAPH_SNAPSHOT_TRUST_URL", "0")),
            false,
        ),
        (
            "TRUST_URL=true is not the literal 1",
            Some(("CODE_GRAPH_SNAPSHOT_TRUST_URL", "true")),
            false,
        ),
    ];

    for (label, signal, honored) in cases {
        let _ = std::fs::remove_dir_all(repo.path().join(".code-graph"));
        let mut cmd = Command::new(cli_bin());
        cmd.args(["reindex", "--from-snapshot"])
            .current_dir(repo.path())
            .env("CODE_GRAPH_DISABLE_MODEL_DOWNLOAD", "1")
            // Pinned, not left to the ambient env (same reason cli_e2e.rs pins
            // it): the refusal this test matches on is a `tracing::warn!`, and
            // `EnvFilter::try_from_default_env()` lets a shell or CI exporting
            // `RUST_LOG=error` swallow it. The five refuse-cases would then fail
            // claiming the gate had been BYPASSED while their own captured
            // output says "No snapshot source resolved" — sending whoever hits
            // it to debug a security gate that is working.
            .env("RUST_LOG", "warn")
            // Never inherit the developer's own trust signals, and keep the
            // connection-refused outcome independent of an ambient proxy.
            .env_remove("CODE_GRAPH_SNAPSHOT_TRUST_URL")
            .env_remove("CODE_GRAPH_SNAPSHOT_TRUST_ORIGIN")
            .env_remove("CODE_GRAPH_SNAPSHOT_PIN")
            .env_remove("HTTPS_PROXY")
            .env_remove("https_proxy")
            .env_remove("ALL_PROXY")
            .env_remove("all_proxy");
        if let Some((k, v)) = signal {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );

        if honored {
            assert!(
                !text.contains(REFUSAL),
                "[{label}] override was refused despite its own trust signal — \
                 the env read in resolve_snapshot_source is not wired.\n{text}"
            );
            assert!(
                text.contains(ATTEMPTED),
                "[{label}] expected the trusted url to be fetched (and fail on \
                 connection-refused); without this the other cases prove nothing.\n{text}"
            );
        } else {
            assert!(
                text.contains(REFUSAL),
                "[{label}] a committed url override was honored without \
                 CODE_GRAPH_SNAPSHOT_TRUST_URL=1.\n{text}"
            );
            assert!(
                !text.contains(ATTEMPTED),
                "[{label}] the untrusted url was contacted.\n{text}"
            );
        }
    }
}

#[test]
fn cli_snapshot_create_then_inspect_round_trip() {
    let repo = init_git_repo();
    let out = repo.path().join("snap.db");

    let status = Command::new(cli_bin())
        .args(["snapshot", "create", "--out"])
        .arg(&out)
        .arg("--quiet")
        .arg("--root")
        .arg(repo.path())
        .status()
        .unwrap();
    assert!(status.success(), "create failed");
    assert!(out.exists());

    // Compress and inspect
    let bytes = std::fs::read(&out).unwrap();
    let zst = repo.path().join("snap.db.zst");
    std::fs::write(&zst, zstd::encode_all(&bytes[..], 9).unwrap()).unwrap();

    let output = Command::new(cli_bin())
        .args(["snapshot", "inspect"])
        .arg(&zst)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "inspect failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(!json["tool_version"].as_str().unwrap().is_empty());
    assert!(json["schema_version"].as_i64().unwrap() > 0);
    assert!(json["created_at"].as_i64().unwrap() > 0);
    assert_eq!(json["includes_vec"].as_bool(), Some(false));
}

#[test]
fn cli_snapshot_inspect_missing_file_exits_nonzero() {
    let output = Command::new(cli_bin())
        .args(["snapshot", "inspect", "/nonexistent/path.db.zst"])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

/// D#638: the bundled SQLite is built with `SQLITE_USE_URI`, so a name that
/// begins with `file:` is parsed as a URI whatever the open flags say, and
/// `inspect file:x.db` read `x.db`. Unix only: `:` cannot be in a Windows name.
#[cfg(unix)]
#[test]
fn cli_snapshot_inspect_reads_a_file_whose_name_begins_with_file_colon() {
    let repo = init_git_repo();
    let snap = repo.path().join("snap.db");
    let status = Command::new(cli_bin())
        .args(["snapshot", "create", "--out"])
        .arg(&snap)
        .arg("--quiet")
        .arg("--root")
        .arg(repo.path())
        .status()
        .unwrap();
    assert!(status.success(), "create failed");

    let dir = TempDir::new().unwrap();
    let named = dir.path().join("file:x.db");
    let other = dir.path().join("x.db");
    std::fs::copy(&snap, &named).unwrap();
    std::fs::copy(&snap, &other).unwrap();
    {
        let c = rusqlite::Connection::open(&other).unwrap();
        let changed = c
            .execute(
                "UPDATE meta SET value = 'not-this-file' WHERE key = 'snapshot_source_commit'",
                [],
            )
            .unwrap();
        assert_eq!(changed, 1, "fixture: x.db has no source commit to change");
    }
    let other_bytes = std::fs::read(&other).unwrap();

    let inspect = |cwd: &std::path::Path, arg: &std::ffi::OsStr| {
        let out = Command::new(cli_bin())
            .current_dir(cwd)
            .args(["snapshot", "inspect"])
            .arg(arg)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "inspect {arg:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()
    };
    // Controls: named absolutely, neither path is a URI, and the two files
    // read differently, so equality below names the file that was read.
    let by_absolute = inspect(repo.path(), named.as_os_str());
    let other_json = inspect(repo.path(), other.as_os_str());
    assert_eq!(other_json["source_commit"], "not-this-file");
    assert_ne!(by_absolute["source_commit"], "not-this-file");

    let by_name = inspect(dir.path(), std::ffi::OsStr::new("file:x.db"));
    assert_eq!(
        by_name, by_absolute,
        "inspect file:x.db did not read the file it was named"
    );
    assert_eq!(std::fs::read(&other).unwrap(), other_bytes, "x.db changed");
    let mut names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["file:x.db", "x.db"], "inspect left side files");
}

/// D#638's other door: a raw `--out` goes to `VACUUM INTO`, whose file name
/// SQLite parses as a URI the same way, so `create --out file:x.db` wrote `x.db`.
#[cfg(unix)]
#[test]
fn cli_snapshot_create_writes_the_out_path_it_is_given_when_it_begins_with_file_colon() {
    let repo = init_git_repo();
    let dir = TempDir::new().unwrap();
    let status = Command::new(cli_bin())
        .current_dir(dir.path())
        .args([
            "snapshot",
            "create",
            "--out",
            "file:x.db",
            "--quiet",
            "--root",
        ])
        .arg(repo.path())
        .status()
        .unwrap();
    assert!(status.success(), "create failed");
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["file:x.db"], "create wrote somewhere else");
}

/// `VACUUM INTO` takes its file name as SQL text. Making a relative `--out`
/// absolute before that put the current directory through a lossy UTF-8
/// conversion, so under a directory whose name is not UTF-8 SQLite wrote into
/// the U+FFFD twin, or nowhere. Linux only: macOS refuses such a name.
#[cfg(target_os = "linux")]
#[test]
fn cli_snapshot_create_writes_a_relative_out_under_a_non_utf8_directory() {
    use std::os::unix::ffi::OsStrExt;
    let repo = init_git_repo();
    let dir = TempDir::new().unwrap();
    let cwd = dir.path().join(std::ffi::OsStr::from_bytes(b"bad\xffdir"));
    let twin = dir.path().join("bad\u{FFFD}dir");
    std::fs::create_dir(&cwd).unwrap();
    std::fs::create_dir(&twin).unwrap();
    let out = Command::new(cli_bin())
        .current_dir(&cwd)
        .args(["snapshot", "create", "--out", "x.db", "--quiet", "--root"])
        .arg(repo.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        cwd.join("x.db").is_file(),
        "x.db is not in the current directory"
    );
    assert_eq!(
        std::fs::read_dir(&twin).unwrap().count(),
        0,
        "create wrote into the U+FFFD twin"
    );
}

/// `snapshot create --out <out>` for `repo`, a raw (uncompressed) snapshot.
#[cfg(unix)]
fn create_raw_snapshot(repo: &std::path::Path, out: &std::path::Path) {
    let status = Command::new(cli_bin())
        .args(["snapshot", "create", "--out"])
        .arg(out)
        .arg("--quiet")
        .arg("--root")
        .arg(repo)
        .status()
        .unwrap();
    assert!(status.success(), "create failed");
}

/// D#666 (M13): a rollback-mode raw snapshot, which is what `create` writes, is
/// read where it is, not through a copy in TMPDIR. With TMPDIR naming a
/// directory that does not exist no copy can be made, so the WAL-mode twin of
/// the same snapshot is refused on `inspect tempdir`: the control that shows
/// TMPDIR reached the binary. Unix only: `std::env::temp_dir` reads TMPDIR there.
#[cfg(unix)]
#[test]
fn cli_snapshot_inspect_reads_a_rollback_mode_raw_file_in_place() {
    let repo = init_git_repo();
    let dir = TempDir::new().unwrap();
    let rollback = dir.path().join("rollback.db");
    create_raw_snapshot(repo.path(), &rollback);
    let header = std::fs::read(&rollback).unwrap();
    assert_eq!(header[18..20], [1, 1], "fixture: create wrote a WAL header");
    let wal = dir.path().join("wal.db");
    std::fs::copy(&rollback, &wal).unwrap();
    {
        let c = rusqlite::Connection::open(&wal).unwrap();
        let mode: String = c
            .query_row("PRAGMA journal_mode = wal", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal", "fixture: wal.db is not in WAL mode");
    }

    let no_tmp = dir.path().join("no-such-tmpdir");
    let inspect = |file: &std::path::Path| {
        Command::new(cli_bin())
            .env("TMPDIR", &no_tmp)
            .args(["snapshot", "inspect"])
            .arg(file)
            .output()
            .unwrap()
    };
    let control = inspect(&wal);
    let control_err = String::from_utf8_lossy(&control.stderr);
    assert!(
        !control.status.success() && control_err.contains("inspect tempdir"),
        "control: a WAL-mode file read without TMPDIR (exit {:?}): {control_err}",
        control.status.code()
    );

    let out = inspect(&rollback);
    assert!(
        out.status.success(),
        "a rollback-mode raw snapshot was read through a copy: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(json["schema_version"].as_i64().unwrap() > 0);
    assert!(!no_tmp.exists(), "inspect created TMPDIR");
}

/// D#666 (M8): SQLite's errors quote the name it was handed, and `inspect`
/// hands it a relative name as typed with `./` in front (`sqlite_file_name`),
/// not made absolute. The 0.182.0 CHANGELOG quotes that text for the input
/// that reaches it: a relative name whose absolute path is longer than SQLite
/// takes (its `-journal` name over 512 bytes, so the file's over 504), which
/// fails to open on every version.
#[cfg(unix)]
#[test]
fn cli_snapshot_inspect_error_quotes_a_relative_name_with_dot_slash() {
    let repo = init_git_repo();
    let dir = TempDir::new().unwrap();
    let snap = dir.path().join("snap.db");
    create_raw_snapshot(repo.path(), &snap);
    // Measured on the canonical path, which is what SQLite builds from getcwd.
    let mut deep = std::fs::canonicalize(dir.path()).unwrap();
    while deep.join("s.db").as_os_str().len() <= 520 {
        deep.push("d".repeat(200));
    }
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::copy(&snap, deep.join("s.db")).unwrap();

    let short = Command::new(cli_bin())
        .args(["snapshot", "inspect"])
        .arg(&snap)
        .output()
        .unwrap();
    assert!(
        short.status.success(),
        "control: the same bytes at a short path did not read: {}",
        String::from_utf8_lossy(&short.stderr)
    );

    let out = Command::new(cli_bin())
        .current_dir(&deep)
        .args(["snapshot", "inspect", "s.db"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "a path over 504 bytes opened");
    assert!(
        stderr.contains("unable to open database file: ./s.db"),
        "the error does not quote ./s.db: {stderr}"
    );
}

// clap-migrated (audit #4 Step 4): `snapshot` is now a nested #[command(subcommand)].
// clap owns --help for the parent and each sub, plus no-subcommand /
// unknown-subcommand rejection (exit 2), replacing the hand-rolled args[2]/args[3]
// dispatch. These lock the new surface and guard against internal-note leak.

const INTERNAL_TOKENS: &[&str] = &["audit #", "clap-migrat", "args[3]", "hand-rolled"];

#[test]
fn cli_snapshot_help_lists_subcommands_no_leak() {
    let out = Command::new(cli_bin())
        .args(["snapshot", "--help"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "snapshot --help should exit 0");
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("create") && s.contains("inspect"),
        "parent help must list both subcommands; got: {s}"
    );
    let low = s.to_lowercase();
    for tok in INTERNAL_TOKENS {
        assert!(
            !low.contains(&tok.to_lowercase()),
            "snapshot --help leaked {tok:?}; got: {s}"
        );
    }
}

#[test]
fn cli_snapshot_create_help_shows_out_no_leak() {
    let out = Command::new(cli_bin())
        .args(["snapshot", "create", "--help"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "snapshot create --help should exit 0"
    );
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("--out"), "create help must show --out; got: {s}");
    let low = s.to_lowercase();
    for tok in INTERNAL_TOKENS {
        assert!(
            !low.contains(&tok.to_lowercase()),
            "snapshot create --help leaked {tok:?}; got: {s}"
        );
    }
}

#[test]
fn cli_snapshot_no_subcommand_exits_2() {
    // clap requires a subcommand (was: hand-rolled Usage + exit 2).
    let out = Command::new(cli_bin()).args(["snapshot"]).output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "snapshot with no subcommand must exit 2"
    );
}

#[test]
fn cli_snapshot_unknown_subcommand_exits_2() {
    let out = Command::new(cli_bin())
        .args(["snapshot", "bogus"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "unknown snapshot subcommand must exit 2"
    );
}

/// E2E 2026-10-16 round 2 (plugin QA, P1-1): the shipped workflow template
/// compressed with an external `zstd` and uploaded the `.db.zst` alone, while
/// install fails closed without its `.blake3` sidecar — so the documented team
/// flow could never install. The template must upload exactly what `snapshot
/// create --out X.db.zst` writes: X.db.zst and X.db.zst.blake3.
#[test]
fn the_snapshot_template_uploads_the_checksum_sidecar_create_writes() {
    let template = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("claude-plugin/templates/code-graph-snapshot.yml"),
    )
    .unwrap();
    let out = template
        .split("snapshot create --out ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .expect("the template runs `snapshot create --out <file>`")
        .trim_matches('"');
    assert!(
        out.ends_with(".db.zst"),
        "create must compress itself, writing the sidecar: {out}"
    );
    let upload = template
        .split("gh release upload")
        .nth(1)
        .expect("the template uploads to the release");
    assert!(
        upload.contains(&format!("\"{out}\"")),
        "uploads {out}: {upload}"
    );
    assert!(
        upload.contains(&format!("\"{out}.blake3\"")),
        "uploads {out}.blake3: {upload}"
    );
    assert!(
        !template.contains("zstd -9"),
        "no external compression: it writes no sidecar"
    );

    // And `create` does write that sidecar.
    let repo = init_git_repo();
    let snap = repo.path().join("s.db.zst");
    let status = Command::new(cli_bin())
        .args([
            "snapshot",
            "create",
            "--out",
            snap.to_str().unwrap(),
            "--quiet",
        ])
        .current_dir(repo.path())
        .env("CODE_GRAPH_INTERNAL", "1")
        .status()
        .unwrap();
    assert!(status.success());
    assert!(repo.path().join("s.db.zst.blake3").is_file());
}
