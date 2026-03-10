use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;
use super::schema;

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;

        conn.execute_batch("
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA cache_size = -64000;
            PRAGMA mmap_size = 268435456;
            PRAGMA temp_store = MEMORY;
            PRAGMA foreign_keys = ON;
        ")?;

        conn.execute_batch(schema::CREATE_TABLES)?;

        conn.pragma_update(None, "user_version", schema::SCHEMA_VERSION)?;

        Ok(Self { conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_init_creates_db_and_tables() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        let db = Database::open(&db_path).unwrap();

        let tables: Vec<String> = db.conn()
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert!(tables.contains(&"files".to_string()));
        assert!(tables.contains(&"nodes".to_string()));
        assert!(tables.contains(&"edges".to_string()));
        assert!(tables.contains(&"context_sandbox".to_string()));
        assert!(tables.contains(&"merkle_state".to_string()));
    }

    #[test]
    fn test_schema_version_is_set() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        let db = Database::open(&db_path).unwrap();

        let version: i32 = db.conn()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);
    }

    #[test]
    fn test_wal_mode_enabled() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        let db = Database::open(&db_path).unwrap();

        let mode: String = db.conn()
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }
}
