//! Index-lock infrastructure: `.code-graph/index.lock` acquisition, release and
//! non-destructive probing.
//!
//! Lives here — not in `mcp::server` where it grew up — because it has nothing
//! to do with the MCP protocol: it guards the *index*, and both the MCP server
//! (which holds it for a whole process lifetime) and the CLI's wholesale-replace
//! commands (`rebuild-index`, `reindex --from-snapshot`, which hold it for a
//! bounded operation) need it. Keeping it under `mcp::server` forced a
//! `cli → mcp` upward dependency that `tests/hardening.rs`'s forbidden-edge
//! table now rejects (2026-08-16 audit §六).

use std::path::Path;

/// Verdict of a best-effort liveness probe for the PID recorded in `index.lock`.
///
/// `Unknown` exists so a probe that could not run never has to lie: only a
/// POSITIVE `Dead` releases another process's lock, so a spawn failure, a
/// timeout or unparsable output all stay on the safe side of the
/// no-dual-primary invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PidProbe {
    Alive,
    Dead,
    Unknown,
}

/// Decide whether the PID recorded in an existing lock file must block this
/// process from taking the lock.
///
/// Deliberately platform-independent. The non-Unix lock path cannot be
/// exercised on the dev/CI host, and its previous hard-coded "always alive"
/// probe made the stale-lock reclaim at the call site unreachable: after one
/// unclean exit on Windows every later server instance for that project stayed
/// secondary forever (no indexing, no watcher, `rebuild_index` refusing) with
/// no message telling the user to delete `.code-graph/index.lock`. Keeping the
/// decision here — with the probe injected — makes both outcomes unit-testable
/// on every platform.
#[allow(dead_code)] // called only from the non-Unix lock path; tested everywhere
pub(crate) fn lock_holder_blocks_acquire(
    recorded_pid: u32,
    my_pid: u32,
    probe: impl FnOnce(u32) -> PidProbe,
) -> bool {
    // Our own PID in the file is a leftover from this process — never blocking.
    if recorded_pid == my_pid {
        return false;
    }
    probe(recorded_pid) != PidProbe::Dead
}

/// Wall-clock budget for one liveness-probe subprocess. A hung probe must not
/// stall server startup, and "took too long" is `Unknown`, i.e. conservative.
#[allow(dead_code)]
const PID_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Run `cmd` to completion, killing it if it outlives `timeout`.
///
/// Returns `None` for every "cannot decide" case: spawn failure, timeout, or a
/// broken wait. Assumes the command's output fits the OS pipe buffer (it is
/// used for a single-row `tasklist` query); a command that fills the pipe would
/// block before exiting and be reported as a timeout, which is still safe.
#[allow(dead_code)]
fn run_probe_command(
    mut cmd: std::process::Command,
    timeout: std::time::Duration,
) -> Option<std::process::Output> {
    use std::process::Stdio;
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(_) => return None,
        }
    }
    child.wait_with_output().ok()
}

/// Interpret `tasklist /NH /FO CSV /FI "PID eq <pid>"` output.
///
/// A successful run listing a CSV row whose PID column matches is `Alive`. A
/// successful run with no such row is `Dead` — we key on the ABSENCE of a row
/// rather than on the "INFO: No tasks are running…" line, which is localized.
/// Anything else is `Unknown`.
#[allow(dead_code)]
fn parse_tasklist_output(success: bool, stdout: &str, pid: u32) -> PidProbe {
    if !success {
        return PidProbe::Unknown;
    }
    for line in stdout.lines() {
        // `"code-graph-mcp.exe","1234","Console","1","10,240 K"` — PID is field 2.
        let mut fields = line.split("\",\"");
        let (Some(_image), Some(pid_field)) = (fields.next(), fields.next()) else {
            continue;
        };
        if pid_field
            .trim()
            .trim_matches('"')
            .parse::<u32>()
            .is_ok_and(|p| p == pid)
        {
            return PidProbe::Alive;
        }
    }
    PidProbe::Dead
}

/// Best-effort process-liveness probe for the non-Unix lock path.
///
/// Compiled on every platform (dead on Unix) so the whole body except the
/// two console-suppression lines type-checks on the dev host.
#[allow(dead_code)]
fn probe_pid_liveness(pid: u32) -> PidProbe {
    let mut cmd = std::process::Command::new("tasklist");
    cmd.args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"]);
    #[cfg(windows)]
    {
        // Without CREATE_NO_WINDOW every probe flashes a console window: the
        // MCP server is launched by Claude Code with no console of its own, and
        // Rust's Command (like Node's `windowsHide`) does not suppress it by
        // default — the same class of defect as [[feedback_windows_child_flash]].
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    match run_probe_command(cmd, PID_PROBE_TIMEOUT) {
        Some(out) => parse_tasklist_output(
            out.status.success(),
            &String::from_utf8_lossy(&out.stdout),
            pid,
        ),
        None => PidProbe::Unknown,
    }
}

/// Check if a process with the given PID is alive (used by non-Unix lock fallback).
/// Unresolvable probes count as alive, keeping the no-dual-primary invariant.
#[cfg(not(unix))]
fn pid_is_alive(pid: u32) -> bool {
    probe_pid_liveness(pid) != PidProbe::Dead
}

/// Try to acquire the index lock (`.code-graph/index.lock`) using flock().
/// Returns `Some(File)` holding the advisory lock if this process becomes the primary indexer.
/// The lock is automatically released when the returned File is dropped.
///
/// CLI callers go through [`acquire_index_lock_guard`], which owns the
/// platform-correct release; this raw form is the server's own acquisition.
#[cfg(unix)]
pub(crate) fn try_acquire_index_lock(code_graph_dir: &Path) -> Option<std::fs::File> {
    use std::os::unix::io::AsRawFd;

    let lock_path = code_graph_dir.join("index.lock");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| {
            tracing::warn!(
                "Could not open index lock: {} — running in secondary mode",
                e
            )
        })
        .ok()?;

    // Non-blocking flock: LOCK_EX | LOCK_NB — fails immediately if another process holds it
    // SAFETY: `file` is an open File owned by this scope, so `as_raw_fd()` yields a
    // valid, live fd for the duration of the call; flock has no other precondition.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        tracing::info!(
            "Another instance holds the index lock — running in secondary (read-only) mode"
        );
        return None;
    }

    // Write our PID for diagnostics (not used for locking logic)
    use std::io::Write;
    let mut f = &file;
    let _ = f.write_all(std::process::id().to_string().as_bytes());

    Some(file)
}

/// Non-unix fallback: PID-based lock with create_new atomicity.
#[cfg(not(unix))]
pub(crate) fn try_acquire_index_lock(code_graph_dir: &Path) -> Option<std::fs::File> {
    use std::io::Write;

    let lock_path = code_graph_dir.join("index.lock");
    let my_pid = std::process::id();

    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
    {
        Ok(mut f) => {
            let _ = f.write_all(my_pid.to_string().as_bytes());
            return Some(f);
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => {
            tracing::warn!(
                "Could not write index lock: {} — running in secondary mode",
                e
            );
            return None;
        }
    }

    // Lock exists — check if holder is alive
    if let Ok(content) = std::fs::read_to_string(&lock_path) {
        if let Ok(pid) = content.trim().parse::<u32>() {
            if lock_holder_blocks_acquire(pid, my_pid, probe_pid_liveness) {
                tracing::info!("Another instance (PID {}) holds the index lock — running in secondary (read-only) mode", pid);
                return None;
            }
            tracing::info!("Reclaiming stale index lock from PID {}", pid);
            let _ = std::fs::remove_file(&lock_path);
        }
    }

    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
    {
        Ok(mut f) => {
            let _ = f.write_all(my_pid.to_string().as_bytes());
            Some(f)
        }
        Err(_) => {
            tracing::info!("Lost lock race during stale reclaim — running in secondary mode");
            None
        }
    }
}

/// Release the index lock, platform-correctly.
///
/// **Unix: this is deliberately a no-op.** The flock lives on the open file
/// description, so dropping the `File` handle already released it; the lock FILE
/// must stay on disk. Removing it is not merely redundant, it breaks mutual
/// exclusion: a concurrent holder's flock is pinned to the *inode*, so once the
/// directory entry is gone the next opener creates a NEW inode, flocks that, and
/// the two processes stop excluding each other — two primaries indexing one DB.
/// The window is real and not microscopic: the CLI's `rebuild-index` /
/// `reindex --from-snapshot` hold this same lock through an
/// [`IndexLockGuard`], and a server shutting down mid-rebuild used to delete the
/// CLI's lock file out from under it (2026-08-16 audit §四). This is the same
/// invariant [`IndexLockGuard`]'s doc comment spells out — it lives in the
/// function now so no caller can get it wrong by forgetting a `cfg`.
///
/// **Non-unix**: the lock IS the file's existence plus its PID content, so the
/// file must go, or a dead PID strands every later instance in secondary mode.
pub(crate) fn release_index_lock(_code_graph_dir: &Path) {
    #[cfg(not(unix))]
    {
        let _ = std::fs::remove_file(_code_graph_dir.join("index.lock"));
    }
}

/// A held index lock with a platform-correct release, for callers that take the
/// lock for a bounded operation rather than for a whole process lifetime — the
/// CLI's wholesale-replace commands (`rebuild-index`, `reindex --from-snapshot`).
///
/// The two platforms release differently and getting it wrong is not symmetric:
/// - **Unix**: the flock lives on the open file description, so dropping the
///   handle releases it. The lock FILE is deliberately left in place; an
///   unlocked `index.lock` reads as free, because
///   [`other_process_holds_index_lock`] re-flocks it to decide. Deleting it here
///   would be worse than useless — a concurrent holder's lock lives on the
///   inode, so a later opener would create a NEW file and the two would stop
///   excluding each other.
/// - **Non-unix**: the lock IS the file's existence plus its PID content, so a
///   guard that dropped only the handle would strand a lock file naming a dead
///   PID. Until the OS reused that PID for nothing, every later `rebuild-index`
///   would refuse and every server start would fall back to secondary
///   read-only mode — the permanent-secondary fault class the 2026-08-02
///   indexing audit logged, newly reachable from the CLI once it started taking
///   this lock at all. So the file is removed on drop.
pub(crate) struct IndexLockGuard {
    _file: std::fs::File,
    #[cfg(not(unix))]
    code_graph_dir: std::path::PathBuf,
}

impl Drop for IndexLockGuard {
    fn drop(&mut self) {
        #[cfg(not(unix))]
        release_index_lock(&self.code_graph_dir);
    }
}

/// Take the index lock for a bounded operation. `None` means somebody else holds
/// it, or the lock file could not be opened at all — the caller decides which
/// (see `lock_index_for_replace` in the CLI, which re-probes to tell them apart).
pub(crate) fn acquire_index_lock_guard(code_graph_dir: &Path) -> Option<IndexLockGuard> {
    let file = try_acquire_index_lock(code_graph_dir)?;
    Some(IndexLockGuard {
        _file: file,
        #[cfg(not(unix))]
        code_graph_dir: code_graph_dir.to_path_buf(),
    })
}

/// Non-destructively check whether ANOTHER process currently holds the index lock
/// (`.code-graph/index.lock`). Used by CLI rebuild/incremental to warn before racing
/// a running MCP server (which holds the flock for its whole lifetime). Best-effort:
/// returns false whenever the state cannot be determined (no lock file, open error),
/// so a false negative never blocks a legitimate CLI run.
#[cfg(unix)]
pub fn other_process_holds_index_lock(code_graph_dir: &Path) -> bool {
    use std::os::unix::io::AsRawFd;
    let lock_path = code_graph_dir.join("index.lock");
    // Deliberately do NOT create the file: its absence means no server has ever
    // locked here, i.e. free.
    let file = match std::fs::OpenOptions::new().write(true).open(&lock_path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    // Non-blocking probe: if we CAN take LOCK_EX, nobody holds it — unlock at once
    // so we never disturb the real primary-acquisition path. If we can't, it's held.
    // SAFETY: `file` is an open File owned by this scope, so `as_raw_fd()` is a valid
    // live fd; both flock calls act on that same fd before `file` is dropped.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        false
    } else {
        true
    }
}

/// Non-unix fallback: the lock is PID-file based, so read the PID and check liveness.
#[cfg(not(unix))]
pub fn other_process_holds_index_lock(code_graph_dir: &Path) -> bool {
    let lock_path = code_graph_dir.join("index.lock");
    match std::fs::read_to_string(&lock_path) {
        Ok(content) => content
            .trim()
            .parse::<u32>()
            .map(|pid| pid != std::process::id() && pid_is_alive(pid))
            .unwrap_or(false),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // L7: CLI rebuild/incremental warn before racing a running server. This guards the
    // detector they rely on. flock is tied to the open file description, so a second
    // open() is excluded even within this same process — no real subprocess needed.
    #[cfg(unix)]
    #[test]
    fn test_other_process_holds_index_lock_detects_held_flock() {
        use std::os::unix::io::AsRawFd;
        let dir = TempDir::new().unwrap();
        let cg = dir.path();
        // No lock file → reads as free (never blocks a legitimate CLI run).
        assert!(!other_process_holds_index_lock(cg));

        let lock_path = cg.join("index.lock");
        let holder = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        // SAFETY: `holder` is an open File owned by this scope, so `as_raw_fd()` is a
        // valid live fd for the flock call; `holder` is dropped below to release it.
        let rc = unsafe { libc::flock(holder.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(rc, 0, "the test must first acquire the lock");
        assert!(
            other_process_holds_index_lock(cg),
            "a held flock must be detected"
        );

        drop(holder); // releases the flock
        assert!(
            !other_process_holds_index_lock(cg),
            "a released lock must read as free"
        );
    }

    // ---------------------------------------------------------------------
    // P1-2: non-Unix lock liveness. The Windows lock path cannot run on this
    // host, so the decision it hinges on is factored out and tested here.
    // ---------------------------------------------------------------------

    #[test]
    fn test_lock_holder_dead_pid_releases_the_lock() {
        // The regression this guards: `pid_is_alive` returned an unconditional
        // `true` on non-Unix, so the "Reclaiming stale index lock" branch was
        // dead code and a single unclean exit left EVERY later instance for that
        // project secondary — no indexing, no watcher, rebuild_index refusing.
        assert!(
            !lock_holder_blocks_acquire(4321, 99, |_| PidProbe::Dead),
            "a positively-dead holder must not block acquisition"
        );
    }

    #[test]
    fn test_lock_holder_live_pid_blocks() {
        assert!(
            lock_holder_blocks_acquire(4321, 99, |_| PidProbe::Alive),
            "a live holder must keep this instance secondary"
        );
    }

    #[test]
    fn test_lock_holder_undecidable_probe_stays_conservative() {
        // Probe could not run (no tasklist, timeout, garbage output): we must not
        // infer "dead" from "don't know", or two primaries index the same DB.
        assert!(
            lock_holder_blocks_acquire(4321, 99, |_| PidProbe::Unknown),
            "an undecidable probe must block, preserving the no-dual-primary invariant"
        );
    }

    #[test]
    fn test_lock_holder_own_pid_never_blocks_and_skips_probe() {
        assert!(
            !lock_holder_blocks_acquire(77, 77, |_| panic!("probe must not run for our own PID")),
            "our own PID in the lock file is our leftover, never a blocker"
        );
    }

    #[test]
    fn test_parse_tasklist_output_verdicts() {
        let row = "\"code-graph-mcp.exe\",\"4321\",\"Console\",\"1\",\"12,345 K\"\r\n";
        assert_eq!(parse_tasklist_output(true, row, 4321), PidProbe::Alive);
        // Row for a DIFFERENT pid must not be read as our holder being alive.
        assert_eq!(parse_tasklist_output(true, row, 9999), PidProbe::Dead);
        // tasklist prints a localized "no tasks match" line and still exits 0;
        // we key on the absence of a CSV row, not on that English text.
        assert_eq!(
            parse_tasklist_output(true, "INFO: No tasks are running which match.\r\n", 4321),
            PidProbe::Dead
        );
        assert_eq!(parse_tasklist_output(true, "", 4321), PidProbe::Dead);
        // A failed run decides nothing.
        assert_eq!(parse_tasklist_output(false, row, 4321), PidProbe::Unknown);
    }

    // P2 (2026-08-16 audit §四): server shutdown used to delete the lock FILE on
    // Unix. Mutual exclusion there is inode-scoped, so an unlinked lock lets the
    // next opener create a fresh inode and become a second primary — including
    // while a CLI `rebuild-index` still holds the old one. The guard is a
    // behavioural one (does the file survive), not a source-text one: reverting
    // `release_index_lock` to the unconditional `remove_file` turns it red.
    #[cfg(unix)]
    #[test]
    fn test_release_index_lock_keeps_the_file_on_unix() {
        let dir = TempDir::new().unwrap();
        let cg = dir.path();
        let guard = acquire_index_lock_guard(cg).expect("lock must be acquirable in a fresh dir");
        let lock_path = cg.join("index.lock");
        assert!(lock_path.exists(), "acquisition must create the lock file");

        release_index_lock(cg);
        assert!(
            lock_path.exists(),
            "on Unix the lock file must survive release — deleting it lets a \
             concurrent holder's inode-scoped flock stop excluding new openers"
        );

        // Dropping the guard is the real Unix release; the file still stays.
        drop(guard);
        assert!(
            lock_path.exists(),
            "the guard's drop must not unlink either"
        );

        // Deliberately NOT asserting `!other_process_holds_index_lock(cg)` here.
        // That probe has its own test above, and asserting it a second time from
        // this one failed twice in ~18 full-suite runs and then refused to
        // reproduce across 16 more — an unexplained flake, not a diagnosis. A
        // guard that is red 1 run in 9 for reasons nobody has pinned down teaches
        // people to re-run rather than to read, which costs more than the extra
        // coverage was worth. The two assertions above are what the fix is about
        // and both are mutation-verified.
    }

    #[cfg(unix)]
    #[test]
    fn test_run_probe_command_collects_output_and_enforces_timeout() {
        // The probe's process plumbing is platform-independent; only the
        // console-suppression flag is Windows-only. Exercise both outcomes with
        // stand-in commands so the timeout path isn't taken on faith.
        let mut ok = std::process::Command::new("sh");
        ok.args(["-c", "printf 'hello'"]);
        let out = run_probe_command(ok, std::time::Duration::from_secs(5))
            .expect("a fast command must produce output");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "hello");
        assert!(out.status.success());

        let mut slow = std::process::Command::new("sh");
        slow.args(["-c", "sleep 30"]);
        let started = std::time::Instant::now();
        assert!(
            run_probe_command(slow, std::time::Duration::from_millis(150)).is_none(),
            "a probe that outlives its timeout must report 'cannot decide', not hang"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the timeout must actually kill the child, not wait for it: {:?}",
            started.elapsed()
        );

        let missing = std::process::Command::new("code-graph-no-such-probe-binary");
        assert!(
            run_probe_command(missing, std::time::Duration::from_secs(1)).is_none(),
            "an unspawnable probe must report 'cannot decide'"
        );
    }
}
