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
pub(super) fn dead_runs(db: &Database) -> Result<Option<Vec<String>>> {
    let Some(value) = crate::storage::queries::get_meta(db.conn(), META_KEY_INDEX_RUN_IN_FLIGHT)?
    else {
        return Ok(None);
    };
    let tokens: Vec<String> = value.split_whitespace().map(str::to_string).collect();
    if tokens.is_empty() || a_run_is_in_flight(db) {
        return Ok(None);
    }
    Ok(Some(tokens))
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
    let file = crate::utils::owned::hold_owned(&run_lock_path(db)?).ok()?;
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

/// Whether some run holds [`RUN_LOCK`] now. Only a conflict means yes: an
/// absent lock file, an open that fails or any other error means no, which
/// reads every token as dead — the behaviour without this lock.
#[cfg(unix)]
fn a_run_is_in_flight(db: &Database) -> bool {
    use std::os::unix::io::AsRawFd;
    let Some(path) = run_lock_path(db) else {
        return false;
    };
    let Ok(file) = crate::utils::owned::probe_owned(&path) else {
        return false;
    };
    for _ in 0..5 {
        // SAFETY: as in `hold_shared`; the lock taken here is released at once.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
            return false;
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EINTR) {
            return crate::indexer::lock::is_flock_conflict(err.raw_os_error());
        }
    }
    false
}

#[cfg(not(unix))]
fn a_run_is_in_flight(_db: &Database) -> bool {
    false
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
        finish(&db, live).unwrap();
        assert_eq!(marker(&db), None);

        let crashed = begin(&db).unwrap();
        let token = crashed.token.clone();
        drop(crashed);
        assert_eq!(dead_runs(&db).unwrap(), Some(vec![token]));
    }
}
