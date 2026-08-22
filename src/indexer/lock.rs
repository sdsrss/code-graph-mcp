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

/// Does this `io::Error` mean "somebody else is holding the lock file"?
///
/// The Windows lock is an OPEN HANDLE, not a file that exists: an acquisition
/// asks for write access, and the holder's share mode refuses it with
/// `ERROR_SHARING_VIOLATION` (32) or, for a byte-range lock underneath us,
/// `ERROR_LOCK_VIOLATION` (33). Those two codes are the only ones that mean
/// "held". Everything else — a read-only directory, a path that does not exist,
/// an exotic filesystem — is a NON-answer and must read as "not held", because
/// [`other_process_holds_index_lock`]'s callers turn a `true` into a refusal
/// (`lock_index_for_replace`) and must never refuse a rebuild that used to work
/// merely because the lock file could not be opened at all.
///
/// Deliberately platform-independent and taking the raw code rather than the
/// error, so the decision this lock hinges on is unit-testable on the dev host —
/// the same reason the PID-liveness decision this replaces was factored out.
/// Rust maps neither code to a named `ErrorKind`, so the raw value is the tell.
#[allow(dead_code)] // called only from the non-Unix lock path; tested everywhere
pub(crate) fn is_lock_conflict(raw_os_error: Option<i32>) -> bool {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const ERROR_LOCK_VIOLATION: i32 = 33;
    matches!(
        raw_os_error,
        Some(ERROR_SHARING_VIOLATION) | Some(ERROR_LOCK_VIOLATION)
    )
}

/// Unix twin of [`is_lock_conflict`], holding the same contract for `flock`:
/// only a genuine conflict counts as "held", everything else is a NON-answer
/// that must read as "not held".
///
/// `flock(LOCK_EX | LOCK_NB)` reports a conflict as `EWOULDBLOCK` (`EAGAIN`
/// under a different name on both Linux and macOS) and nothing else does. Its
/// other failures are all non-answers of one kind or another — `EINTR` (a
/// signal arrived mid-call), `ENOLCK` (the kernel is out of lock records),
/// `EOPNOTSUPP`/`ENOTSUP` (a filesystem that does not implement flock at all,
/// which some network and FUSE mounts do not) — and reading any of them as
/// "held" makes `lock_index_for_replace` refuse a rebuild, telling the user
/// another process holds a lock that nobody holds.
#[cfg(unix)]
pub(crate) fn is_flock_conflict(raw_os_error: Option<i32>) -> bool {
    // EAGAIN is the same value on every target Rust supports here, so matching
    // EWOULDBLOCK alone covers both spellings.
    matches!(raw_os_error, Some(e) if e == libc::EWOULDBLOCK)
}

/// Open `index.lock` the way the non-Unix lock needs it: write access, shared
/// for READ only.
///
/// `FILE_SHARE_READ` and nothing else is what makes this a lock. A second
/// opener asking for write access is refused while this handle lives, and the
/// OS closes the handle when the process dies however it dies — which is why
/// this design has no stale-lock state to reclaim, no PID to probe, and no
/// window in which two processes can both decide the other one is gone.
/// Read sharing stays on so the PID written into the file remains readable for
/// diagnostics; a holder that shared nothing would hide its own identity from
/// the tools meant to report it.
///
/// `create` separates the two callers: acquisition creates (and truncates, so a
/// crashed run's PID does not linger), while the probe must neither create nor
/// modify — an absent lock file means nobody has ever locked here, i.e. free.
#[cfg(not(unix))]
fn open_lock_handle(lock_path: &Path, create: bool) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(create).truncate(create);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        opts.share_mode(FILE_SHARE_READ);
    }
    // A non-Unix, non-Windows target would compile the line above away and get a
    // plain open — mutual exclusion silently gone. Fail loudly instead: there is
    // no such target in the release matrix, and a lock that only pretends to
    // exclude is worse than one that does not build.
    #[cfg(not(windows))]
    compile_error!(
        "the non-Unix index lock relies on Win32 share modes; port it before building this target"
    );
    opts.open(lock_path)
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
        // Only EWOULDBLOCK means somebody actually holds it. Reporting every
        // failure as a conflict accuses a holder that does not exist — on a
        // network or FUSE mount without flock support (`EOPNOTSUPP`) the message
        // sent the reader hunting for a second process for a filesystem
        // limitation. `is_flock_conflict` already drew this line; this call site
        // was not asking it.
        //
        // Behaviour is unchanged: either way we fall back to secondary mode,
        // because a lock we could not take is a lock we do not hold. Only the
        // diagnosis differs.
        let err = std::io::Error::last_os_error();
        if is_flock_conflict(err.raw_os_error()) {
            tracing::info!(
                "Another instance holds the index lock — running in secondary (read-only) mode"
            );
        } else {
            tracing::warn!(
                "Could not lock the index ({err}) — this filesystem may not support flock; \
                 running in secondary (read-only) mode"
            );
        }
        return None;
    }

    // Write our PID for diagnostics (not used for locking logic).
    //
    // Truncate first: the file is opened with `truncate(false)` so an existing
    // lock file keeps its inode, and a shorter PID written over a longer one
    // left the tail of the old number behind — PID 123456 followed by PID 999
    // read back as `999456`, a process that may well exist and is not us.
    use std::io::Write;
    let _ = file.set_len(0);
    let mut f = &file;
    let _ = f.write_all(std::process::id().to_string().as_bytes());

    Some(file)
}

/// Non-unix (Windows) acquisition: the lock is an exclusive open HANDLE, the
/// same shape as the Unix flock above.
///
/// This used to be a PID file — `create_new`, and on `AlreadyExists` read the
/// recorded PID, probe it with `tasklist`, and delete the file if the holder
/// looked dead. Two processes starting together both read the same dead PID,
/// both decided to reclaim, and the second one's `remove_file` deleted the lock
/// the first had just created: both then held "the lock" and indexed one DB as
/// two primaries (2026-08-16 audit §五, "Windows 残锁 TOCTOU"). The delete was
/// unconditional, so no amount of re-checking before it closed the window —
/// there is no atomic "unlink only if this is still the file I inspected".
///
/// Handing mutual exclusion to the OS removes the race and its whole
/// supporting cast: no liveness probe (the kernel drops the handle when the
/// holder dies, however it dies), no stale lock to reclaim, and no
/// permanent-secondary fault after an unclean exit. The PID is still written,
/// now purely as diagnostics.
#[cfg(not(unix))]
pub(crate) fn try_acquire_index_lock(code_graph_dir: &Path) -> Option<std::fs::File> {
    use std::io::Write;

    let lock_path = code_graph_dir.join("index.lock");
    match open_lock_handle(&lock_path, true) {
        Ok(mut f) => {
            let _ = f.write_all(std::process::id().to_string().as_bytes());
            let _ = f.flush();
            Some(f)
        }
        Err(e) if is_lock_conflict(e.raw_os_error()) => {
            tracing::info!(
                "Another instance holds the index lock — running in secondary (read-only) mode"
            );
            None
        }
        Err(e) => {
            tracing::warn!(
                "Could not open index lock: {} — running in secondary mode",
                e
            );
            None
        }
    }
}

/// Release the index lock. **A deliberate no-op on every platform**: the lock is
/// the open handle, so dropping the `File` already released it, and the lock
/// FILE must stay on disk.
///
/// Unlinking it is not merely redundant, it breaks mutual exclusion, in the same
/// way on both platforms for two different reasons. On Unix a concurrent
/// holder's flock is pinned to the *inode*, so once the directory entry is gone
/// the next opener creates a NEW inode, flocks that, and the two processes stop
/// excluding each other — two primaries indexing one DB. A server shutting down
/// mid-rebuild used to delete the CLI's lock file out from under it exactly this
/// way (2026-08-16 audit §四). On Windows the delete used to be the *reclaim*
/// step of the PID-file design, and deleting a file another process may have
/// just created was that design's core race (see [`try_acquire_index_lock`]);
/// with the handle-based lock there is nothing left to reclaim.
///
/// The window matters because the CLI's `rebuild-index` /
/// `reindex --from-snapshot` hold this same lock through an [`IndexLockGuard`]
/// while a server may be starting or stopping. Keeping the no-op here — rather
/// than a `cfg` at each call site — is what stops the delete from growing back.
pub(crate) fn release_index_lock(_code_graph_dir: &Path) {}

/// A held index lock for callers that take it for a bounded operation rather
/// than for a whole process lifetime — the CLI's wholesale-replace commands
/// (`rebuild-index`, `reindex --from-snapshot`).
///
/// Dropping the handle IS the release, on both platforms: Unix flock lives on
/// the open file description, and the Windows lock is the exclusive open itself.
/// There is deliberately no `Drop` impl beyond that — in particular the lock
/// FILE is left in place. It used to be removed on the non-Unix leg, back when
/// the lock was the file's existence plus a PID; see [`release_index_lock`] for
/// why unlinking it breaks mutual exclusion on both platforms now.
pub(crate) struct IndexLockGuard {
    _file: std::fs::File,
}

/// Take the index lock for a bounded operation. `None` means somebody else holds
/// it, or the lock file could not be opened at all — the caller decides which
/// (see `lock_index_for_replace` in the CLI, which re-probes to tell them apart).
pub(crate) fn acquire_index_lock_guard(code_graph_dir: &Path) -> Option<IndexLockGuard> {
    let file = try_acquire_index_lock(code_graph_dir)?;
    Some(IndexLockGuard { _file: file })
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
    //
    // EINTR is retried rather than answered. It means a signal arrived mid-call,
    // i.e. we never learned anything, and this probe's non-answer reads as "free"
    // — which for `lock_index_for_replace` means proceeding to replace an index a
    // live server may be writing. A retry costs nothing and removes the one
    // transient that could turn a held lock into a destructive go-ahead
    // (pre-tag review, Minor #1). The bound is a formality: a non-blocking flock
    // returns immediately, so it cannot be interrupted repeatedly.
    //
    // SAFETY: `file` is an open File owned by this scope, so `as_raw_fd()` is a valid
    // live fd; every flock call acts on that same fd before `file` is dropped.
    let mut attempts = 0u8;
    let err = loop {
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
            return false;
        }
        let e = std::io::Error::last_os_error();
        attempts += 1;
        if e.raw_os_error() != Some(libc::EINTR) || attempts >= 5 {
            break e;
        }
    };
    // Only EWOULDBLOCK means somebody holds it. Anything else is a non-answer,
    // and answering "held" would make lock_index_for_replace refuse the run —
    // the same asymmetry the non-Unix probe below deliberately avoids.
    if is_flock_conflict(err.raw_os_error()) {
        true
    } else {
        tracing::warn!(
            "could not probe the index lock at {}: {} — treating it as free",
            lock_path.display(),
            err
        );
        false
    }
}

/// Non-unix (Windows) probe: mirror of the Unix one — try to take the lock, and
/// let go at once if we got it.
///
/// Same shape, same three answers: an exclusive open that SUCCEEDS means nobody
/// held it (the handle is dropped immediately, so the real acquisition path is
/// never disturbed); a sharing violation means somebody does; anything else —
/// including a lock file that does not exist, i.e. nobody has ever locked here —
/// is "free", because a non-answer must never block a legitimate CLI run.
///
/// This replaces reading the recorded PID and probing it with `tasklist`, which
/// answered from a file that a crashed process leaves behind indefinitely. It
/// also closes a smaller hole in passing: the old code excluded our OWN pid, so
/// a lock held by this very process read as free.
#[cfg(not(unix))]
pub fn other_process_holds_index_lock(code_graph_dir: &Path) -> bool {
    // Deliberately do NOT create the file: its absence means no process has ever
    // locked here.
    match open_lock_handle(&code_graph_dir.join("index.lock"), false) {
        Ok(_probe) => false,
        Err(e) => is_lock_conflict(e.raw_os_error()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Unconditional now: the Windows tests below take temp dirs too. It used to be
    // `#[cfg(unix)]` because only Unix tests constructed one, and the unused import
    // became a hard `cargo check` failure on the windows leg once `[lints]` denied
    // warnings crate-wide — a platform this repo's dev box never compiles.
    use tempfile::TempDir;

    // The lock file is opened with `truncate(false)` — deliberately, since the
    // flock lives on the inode — so the PID written for diagnostics landed ON
    // TOP of whatever was there. A shorter PID over a longer one left the old
    // number's tail: `123456` then `999` read back as `999456`, a PID that may
    // well resolve to a live and entirely unrelated process.
    #[cfg(unix)]
    #[test]
    fn test_pid_write_does_not_leave_the_previous_pid_tail() {
        let dir = TempDir::new().unwrap();
        let cg = dir.path();
        // A crashed run's leftover, deliberately longer than any PID we write.
        std::fs::write(cg.join("index.lock"), "123456789012").unwrap();

        let _held = try_acquire_index_lock(cg).expect("leftover file must not block acquisition");

        let written = std::fs::read_to_string(cg.join("index.lock")).unwrap();
        assert_eq!(
            written,
            std::process::id().to_string(),
            "the lock file must hold exactly this PID — a tail from the previous \
             writer makes the diagnostic name a process that is not the holder"
        );
    }

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
    // Non-Unix (Windows) lock. The OS mechanics can only run on Windows, so the
    // behavioural tests below are `#[cfg(windows)]` and get their evidence from
    // the CI windows leg; the DECISION they hinge on is factored out here and
    // tested on every host, the same way the PID-liveness decision this replaces
    // was.
    // ---------------------------------------------------------------------

    /// Unix half of the same decision. Kept beside the Windows one so the two
    /// platforms' answers to "is this error "held"?" stay visibly identical in
    /// shape: exactly one conflict code, everything else a non-answer.
    #[cfg(unix)]
    #[test]
    fn test_is_flock_conflict_only_ewouldblock() {
        assert!(
            is_flock_conflict(Some(libc::EWOULDBLOCK)),
            "EWOULDBLOCK is the only errno flock reports for a real conflict"
        );
        // Non-answers. `lock_index_for_replace` turns `true` into a refusal, so
        // reading any of these as "held" refuses a rebuild and blames a process
        // that does not exist.
        assert!(
            !is_flock_conflict(Some(libc::ENOLCK)),
            "ENOLCK means the kernel ran out of lock records, not that it is held"
        );
        assert!(
            !is_flock_conflict(Some(libc::EINTR)),
            "EINTR means a signal arrived mid-call, not that it is held"
        );
        assert!(
            !is_flock_conflict(Some(libc::EOPNOTSUPP)),
            "a filesystem without flock support decides nothing"
        );
        assert!(
            !is_flock_conflict(Some(libc::EBADF)),
            "EBADF is our own bug, not a holder"
        );
        assert!(
            !is_flock_conflict(None),
            "an error with no OS code decides nothing"
        );
    }

    #[test]
    fn test_is_lock_conflict_only_the_two_sharing_codes() {
        // 32 = ERROR_SHARING_VIOLATION, 33 = ERROR_LOCK_VIOLATION: somebody holds it.
        assert!(is_lock_conflict(Some(32)), "a sharing violation means held");
        assert!(is_lock_conflict(Some(33)), "a lock violation means held");
        // Everything else is a NON-answer and must read as "not held".
        // `lock_index_for_replace` turns `true` into a refusal, so widening this
        // to "any error" would make a read-only directory or a missing file stop
        // a rebuild that used to work.
        assert!(
            !is_lock_conflict(Some(2)),
            "ERROR_FILE_NOT_FOUND means nobody has ever locked here, not 'held'"
        );
        assert!(
            !is_lock_conflict(Some(5)),
            "ERROR_ACCESS_DENIED is a non-answer (read-only dir), not 'held'"
        );
        assert!(
            !is_lock_conflict(None),
            "an error with no OS code decides nothing"
        );
    }

    // The TOCTOU this replaced (2026-08-16 audit §五): the PID-file design let a
    // second process delete the lock file the first had just created, so both
    // became primary. With the handle-based lock a second acquisition is refused
    // by the OS while the first handle lives — no window, nothing to reclaim.
    // Reverting `try_acquire_index_lock` to the create_new + remove_file form
    // turns this red (the old code even reclaimed its OWN pid's lock).
    #[cfg(windows)]
    #[test]
    fn test_second_acquire_is_refused_while_the_handle_lives() {
        let dir = TempDir::new().unwrap();
        let cg = dir.path();
        let first = try_acquire_index_lock(cg).expect("a fresh dir must be lockable");
        assert!(
            try_acquire_index_lock(cg).is_none(),
            "a second acquisition must be refused while the first handle lives — \
             this is the no-dual-primary invariant"
        );
        drop(first);
        assert!(
            try_acquire_index_lock(cg).is_some(),
            "closing the handle must release the lock"
        );
    }

    // The permanent-secondary fault the PID design had: a lock FILE left behind
    // by a crashed process named a PID nobody could resolve, and every later
    // instance stayed read-only. The file is no longer the lock, so a leftover is
    // inert — the OS dropped the handle when that process died.
    #[cfg(windows)]
    #[test]
    fn test_leftover_lock_file_from_a_crashed_run_is_inert() {
        let dir = TempDir::new().unwrap();
        let cg = dir.path();
        std::fs::write(cg.join("index.lock"), "424242").unwrap();
        assert!(
            !other_process_holds_index_lock(cg),
            "a lock file nobody has open must read as free"
        );
        assert!(
            try_acquire_index_lock(cg).is_some(),
            "a crashed run's leftover lock file must not strand this instance in secondary mode"
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_other_process_holds_index_lock_tracks_the_handle() {
        let dir = TempDir::new().unwrap();
        let cg = dir.path();
        // No lock file → free, and the probe must not create one.
        assert!(!other_process_holds_index_lock(cg));
        assert!(
            !cg.join("index.lock").exists(),
            "the probe must not create the lock file"
        );

        let holder = try_acquire_index_lock(cg).expect("a fresh dir must be lockable");
        assert!(
            other_process_holds_index_lock(cg),
            "a held handle must be detected"
        );
        drop(holder);
        assert!(
            !other_process_holds_index_lock(cg),
            "a released lock must read as free"
        );
    }

    // 2026-08-16 audit §四: server shutdown used to delete the lock FILE. On Unix
    // mutual exclusion is inode-scoped, so an unlinked lock lets the next opener
    // create a fresh inode and become a second primary — including while a CLI
    // `rebuild-index` still holds the old one; on Windows the same delete was the
    // reclaim step of the racy PID design. The guard is a behavioural one (does
    // the file survive), not a source-text one: reverting `release_index_lock` to
    // an unconditional `remove_file` turns it red. It runs on both platforms now
    // that both release the same way.
    #[test]
    fn test_release_index_lock_keeps_the_file() {
        let dir = TempDir::new().unwrap();
        let cg = dir.path();
        let guard = acquire_index_lock_guard(cg).expect("lock must be acquirable in a fresh dir");
        let lock_path = cg.join("index.lock");
        assert!(lock_path.exists(), "acquisition must create the lock file");

        release_index_lock(cg);
        assert!(
            lock_path.exists(),
            "the lock file must survive release — deleting it lets a concurrent \
             holder's handle stop excluding new openers"
        );

        // Dropping the guard is the real release; the file still stays.
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
}
