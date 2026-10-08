//! The index-run in-flight marker (audit 2026-08-16 P1-2, D#192(3)).
//!
//! A run commits file hashes before it commits cross-file edges, so a run
//! killed in between leaves files that hash as current with their edges
//! missing, and nothing else records it: the marker is what makes the next
//! incremental run re-index the whole tree.
//!
//! It held `"1"`, with no owner. A run another process still had in flight — a
//! CLI query indexing a new file while the MCP server ran its own incremental
//! run — read as a crash, and the server re-indexed every file (tokio: 2 of 12
//! rounds, 3.9–5.1 s each). And whichever run finished first deleted the key,
//! so the evidence of a crash beside it went too.
//!
//! The marker is now a SET of run tokens, one per run, and a run holds
//! `index-run.lock` (beside `index.db`) SHARED from adding its token until it
//! removes it. A live run's token is therefore always covered by a held lock:
//! a probe that takes the lock EXCLUSIVE knows every token present belongs to
//! a dead run; one that cannot learns nothing and leaves the tokens for a probe
//! with no run in flight. A run removes only its own token, and the recovery
//! run retires exactly the tokens its probe found dead. `"1"`, the marker an
//! older binary writes, is a token only the recovery retires.
//!
//! Where the lock cannot be had — a non-Unix target, an in-memory database, a
//! lock file that cannot be opened — a run holds nothing and a probe reads
//! every token as dead: the behaviour before this module.

use crate::storage::db::Database;
use crate::storage::schema::META_KEY_INDEX_RUN_IN_FLIGHT;
use anyhow::Result;
use std::sync::atomic::{AtomicU64, Ordering};

/// The lock file beside `index.db`. Beside the database rather than under the
/// project root: the index is where the runs meet (a linked worktree reads the
/// main checkout's, and tests keep theirs in a separate directory).
#[cfg(unix)]
const RUN_LOCK: &str = "index-run.lock";

/// A run's token in the marker, and the shared hold on [`RUN_LOCK`] that
/// covers it. Dropped without [`finish`] — an error return mid-run — the token
/// stays and the hold goes, which is exactly a crash as the next probe sees it.
pub(super) struct RunMark {
    token: String,
    _hold: Option<std::fs::File>,
}

/// Start a run: take the shared hold, then add this run's token. In that order,
/// so no probe can find the token without the hold.
pub(super) fn begin(db: &Database) -> Result<RunMark> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let hold = hold_shared(db);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let token = format!(
        "{}-{nanos}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    );
    db.conn()
        .prepare_cached(
            "INSERT INTO meta (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = value || ' ' || excluded.value",
        )?
        .execute([META_KEY_INDEX_RUN_IN_FLIGHT, token.as_str()])?;
    Ok(RunMark { token, _hold: hold })
}

/// End a run that committed its cross-file edges: remove its own token, and
/// only its own, then let go of the hold.
pub(super) fn finish(db: &Database, mark: RunMark) -> Result<()> {
    retire(db, std::slice::from_ref(&mark.token))
}

/// The tokens of the runs that died in flight, or None when there are none —
/// or when a run is in flight now, which says nothing about the tokens beside
/// its own and leaves them for a probe with nobody running.
///
/// The lock is taken before the marker is read, and held while it is. Read
/// first, a run finishing in between — token removed, then lock released —
/// left its token in what was read and was reported dead (pre-tag review
/// round 2: a flock delayed by 1 s re-indexed 4 of 4 files).
pub(super) fn dead_runs(db: &Database) -> Result<Option<Vec<String>>> {
    let held = match probe_exclusive(db) {
        Probe::InFlight => return Ok(None),
        Probe::Held(file) => Some(file),
        Probe::NoLock => None,
    };
    let value = crate::storage::queries::get_meta(db.conn(), META_KEY_INDEX_RUN_IN_FLIGHT)?;
    drop(held);
    let tokens: Vec<String> = value
        .as_deref()
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    Ok((!tokens.is_empty()).then_some(tokens))
}

/// Remove `tokens` from the marker, and the marker once it is empty. Two
/// statements, neither a read-modify-write: a token another run adds between
/// them keeps the marker non-empty, and so keeps it.
pub(super) fn retire(db: &Database, tokens: &[String]) -> Result<()> {
    let conn = db.conn();
    for token in tokens {
        conn.prepare_cached(
            "UPDATE meta SET value = TRIM(REPLACE(' ' || value || ' ', ' ' || ?2 || ' ', ' ')) \
             WHERE key = ?1",
        )?
        .execute([META_KEY_INDEX_RUN_IN_FLIGHT, token.as_str()])?;
    }
    conn.prepare_cached("DELETE FROM meta WHERE key = ?1 AND TRIM(value) = ''")?
        .execute([META_KEY_INDEX_RUN_IN_FLIGHT])?;
    Ok(())
}

#[cfg(unix)]
fn run_lock_path(db: &Database) -> Option<std::path::PathBuf> {
    let path = db.conn().path().filter(|p| !p.is_empty())?;
    Some(std::path::Path::new(path).with_file_name(RUN_LOCK))
}

/// Take [`RUN_LOCK`] shared. Waits out a probe or a finishing run's exclusive
/// moment, which is brief, for at most two seconds; past that, or on any
/// error, the run goes unheld.
#[cfg(unix)]
fn hold_shared(db: &Database) -> Option<std::fs::File> {
    use std::os::unix::io::AsRawFd;
    let file = crate::utils::owned::share_owned(&run_lock_path(db)?).ok()?;
    for _ in 0..200 {
        // SAFETY: `file` is an open File owned by this scope, so its fd is valid
        // for the call; flock has no other precondition.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0 {
            return Some(file);
        }
        let err = std::io::Error::last_os_error();
        if !crate::indexer::lock::is_flock_conflict(err.raw_os_error())
            && err.raw_os_error() != Some(libc::EINTR)
        {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    None
}

#[cfg(not(unix))]
fn hold_shared(_db: &Database) -> Option<std::fs::File> {
    None
}

/// What an exclusive try on [`RUN_LOCK`] found.
enum Probe {
    /// Some run holds it: every token may be live.
    #[cfg_attr(not(unix), allow(dead_code))]
    InFlight,
    /// Taken, by this probe: no run is in flight while the handle lives.
    #[cfg_attr(not(unix), allow(dead_code))]
    Held(std::fs::File),
    /// No answer — no lock file, an open that fails, any other error — which
    /// reads every token as dead: the behaviour without this lock.
    NoLock,
}

#[cfg(unix)]
fn probe_exclusive(db: &Database) -> Probe {
    use std::os::unix::io::AsRawFd;
    let Some(path) = run_lock_path(db) else {
        return Probe::NoLock;
    };
    let Ok(file) = crate::utils::owned::probe_owned(&path) else {
        return Probe::NoLock;
    };
    for _ in 0..5 {
        // SAFETY: as in `hold_shared`. The lock is released when `file` is
        // dropped, which closes the only descriptor of its open file.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Probe::Held(file);
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EINTR) {
            return if crate::indexer::lock::is_flock_conflict(err.raw_os_error()) {
                Probe::InFlight
            } else {
                Probe::NoLock
            };
        }
    }
    Probe::NoLock
}

#[cfg(not(unix))]
fn probe_exclusive(_db: &Database) -> Probe {
    Probe::NoLock
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker(db: &Database) -> Option<String> {
        crate::storage::queries::get_meta(db.conn(), META_KEY_INDEX_RUN_IN_FLIGHT).unwrap()
    }

    #[test]
    fn retire_removes_whole_tokens_and_then_the_key() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Database::open(&dir.path().join("index.db")).unwrap();
        crate::storage::queries::set_meta(db.conn(), META_KEY_INDEX_RUN_IN_FLIGHT, "1 12-3 1-2")
            .unwrap();
        retire(&db, &["1".to_string()]).unwrap();
        assert_eq!(
            marker(&db).as_deref(),
            Some("12-3 1-2"),
            "a token, not a prefix"
        );
        retire(&db, &["1-2".to_string(), "12-3".to_string()]).unwrap();
        assert_eq!(marker(&db), None, "an empty marker is no marker");
        retire(&db, &["gone".to_string()]).unwrap();
        assert_eq!(marker(&db), None);
    }

    /// A hard-linked lock file (what `cp -al` / `rsync --link-dest` leave) is
    /// still held: the hold never writes through it (pre-tag review round 2).
    #[cfg(unix)]
    #[test]
    fn a_hard_linked_run_lock_is_still_held() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Database::open(&dir.path().join("index.db")).unwrap();
        std::fs::write(dir.path().join(RUN_LOCK), b"").unwrap();
        std::fs::hard_link(dir.path().join(RUN_LOCK), dir.path().join("backup.lock")).unwrap();
        let run = begin(&db).unwrap();
        assert!(
            run._hold.is_some(),
            "a hard link must not leave the run unheld"
        );
        assert_eq!(
            dead_runs(&db).unwrap(),
            None,
            "and a probe must see it in flight"
        );
        finish(&db, run).unwrap();
    }

    /// A run's token is covered by its hold for as long as it is in the
    /// marker: a probe meanwhile finds a run in flight, not a dead one; once
    /// the run finishes, its token is gone; dropped unfinished, it is a crash.
    #[cfg(unix)]
    #[test]
    fn a_live_token_is_never_reported_dead() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Database::open(&dir.path().join("index.db")).unwrap();
        let live = begin(&db).unwrap();
        assert!(
            live._hold.is_some(),
            "the hold must be taken beside a file database"
        );
        assert_eq!(dead_runs(&db).unwrap(), None, "a held token is no crash");
        assert!(
            crate::storage::queries::get_meta(db.conn(), META_KEY_INDEX_RUN_IN_FLIGHT)
                .unwrap()
                .is_some(),
            "the probe leaves a live run's token alone"
        );
        finish(&db, live).unwrap();
        assert_eq!(marker(&db), None);

        let crashed = begin(&db).unwrap();
        let token = crashed.token.clone();
        drop(crashed);
        assert_eq!(dead_runs(&db).unwrap(), Some(vec![token]));
    }
}
