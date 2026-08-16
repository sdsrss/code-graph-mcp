//! Generic accessors for the single-row-per-key `meta` table (added in v7).
//!
//! The table predates these helpers, so several callers still inline the same
//! three statements (`db.rs` for the embedding dim, `embedding_cache.rs` for the
//! model fingerprint, `snapshot::meta` for provenance). New callers should use
//! these; the key itself belongs in `schema.rs` next to the others.

use anyhow::Result;
use rusqlite::Connection;

pub fn get_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare_cached("SELECT value FROM meta WHERE key = ?1")?;
    let mut rows = stmt.query([key])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO meta (key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )?
    .execute([key, value])?;
    Ok(())
}

pub fn delete_meta(conn: &Connection, key: &str) -> Result<()> {
    conn.prepare_cached("DELETE FROM meta WHERE key = ?1")?
        .execute([key])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db::Database;
    use tempfile::TempDir;

    #[test]
    fn set_get_delete_round_trip() {
        let tmp = TempDir::new().unwrap();
        let db = Database::open(&tmp.path().join("m.db")).unwrap();
        let conn = db.conn();
        assert_eq!(get_meta(conn, "k").unwrap(), None);
        set_meta(conn, "k", "1").unwrap();
        assert_eq!(get_meta(conn, "k").unwrap().as_deref(), Some("1"));
        set_meta(conn, "k", "2").unwrap();
        assert_eq!(
            get_meta(conn, "k").unwrap().as_deref(),
            Some("2"),
            "set must upsert, not fail on the existing key"
        );
        delete_meta(conn, "k").unwrap();
        assert_eq!(get_meta(conn, "k").unwrap(), None);
        // Deleting an absent key is a no-op, not an error: the clear path runs
        // unconditionally at the end of a run.
        delete_meta(conn, "k").unwrap();
    }
}
