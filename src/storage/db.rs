use anyhow::Result;
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::sync::Once;
use super::schema;

// FFI declaration for sqlite-vec init function (compiled via build.rs)
extern "C" {
    fn sqlite3_vec_init(
        db: *mut rusqlite::ffi::sqlite3,
        pz_err_msg: *mut *mut std::os::raw::c_char,
        p_api: *const rusqlite::ffi::sqlite3_api_routines,
    ) -> std::os::raw::c_int;
}

static VEC_INIT: Once = Once::new();

fn register_sqlite_vec() {
    VEC_INIT.call_once(|| {
        // SAFETY: sqlite3_vec_init has the exact C ABI signature expected by
        // sqlite3_auto_extension in rusqlite's FFI bindings. No transmute needed.
        // The Once guard ensures single registration. SQLite is compiled with
        // SQLITE_THREADSAFE=1 (bundled default), making global extension registration safe.
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(sqlite3_vec_init));
        }
    });
}

pub struct Database {
    conn: Connection,
    vec_enabled: bool,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_impl(path, false)
    }

    pub fn open_with_vec(path: &Path) -> Result<Self> {
        Self::open_impl(path, true)
    }

    /// Open an existing database in strict read-only mode. Used by secondary
    /// MCP instances (those that failed to acquire the index flock) so the
    /// SQLite driver hard-refuses any write, eliminating race conditions
    /// against the primary instance's indexing transactions.
    ///
    /// Requires the file to already exist (returns Err if not). No schema
    /// migrations or table creation happens — secondary relies entirely on
    /// the primary's bootstrap.
    pub fn open_readonly(path: &Path) -> Result<Self> {
        register_sqlite_vec();
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )?;
        // PRAGMA query_only is belt-and-suspenders on top of the flag —
        // any accidental write attempt errors out at the SQL layer.
        conn.execute_batch("
            PRAGMA query_only = ON;
            PRAGMA busy_timeout = 5000;
        ")?;
        // Detect vec tables via sqlite_master so consumers know if vector
        // search is available without needing a separate probe.
        let vec_enabled: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='node_vectors'",
                [],
                |_| Ok(true),
            )
            .unwrap_or(false);
        Ok(Self { conn, vec_enabled })
    }

    fn open_impl(path: &Path, enable_vec: bool) -> Result<Self> {
        match Self::open_impl_inner(path, enable_vec) {
            Ok(db) => Ok(db),
            Err(e) if Self::is_corruption_error(&e) && path.exists() => {
                tracing::warn!(
                    "[db] Database corrupt ({}), deleting for rebuild: {}",
                    path.display(), e
                );
                // Remove DB + WAL + SHM files — the index is a pure cache
                std::fs::remove_file(path).ok();
                let wal_path = path.with_extension("db-wal");
                let shm_path = path.with_extension("db-shm");
                if wal_path.exists() { std::fs::remove_file(&wal_path).ok(); }
                if shm_path.exists() { std::fs::remove_file(&shm_path).ok(); }
                // Retry once with a fresh database
                Self::open_impl_inner(path, enable_vec)
            }
            Err(e) => Err(e),
        }
    }

    fn open_impl_inner(path: &Path, enable_vec: bool) -> Result<Self> {
        // Always register sqlite-vec extension (it's process-global anyway via auto_extension)
        register_sqlite_vec();

        let conn = Connection::open(path)?;

        conn.execute_batch("
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA cache_size = -64000;
            PRAGMA mmap_size = 268435456;
            PRAGMA temp_store = MEMORY;
            PRAGMA foreign_keys = ON;
            PRAGMA busy_timeout = 5000;
        ")?;

        // Check existing schema version — migrate if needed, bail only on future versions
        let existing_version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;

        if existing_version > schema::SCHEMA_VERSION {
            anyhow::bail!(
                "Database schema version v{} is newer than supported v{}. Please update code-graph-mcp.",
                existing_version,
                schema::SCHEMA_VERSION
            );
        }

        if existing_version > 0 && existing_version < schema::SCHEMA_VERSION {
            // Run migrations sequentially
            if existing_version < 2 {
                let tx = conn.unchecked_transaction()?;
                schema::migrate_v1_to_v2(&conn)?;
                tx.commit()?;
            }
            if existing_version < 3 {
                let tx = conn.unchecked_transaction()?;
                schema::migrate_v2_to_v3(&conn)?;
                tx.commit()?;
            }
            if existing_version < 4 {
                let tx = conn.unchecked_transaction()?;
                schema::migrate_v3_to_v4(&conn)?;
                tx.commit()?;
            }
            if existing_version < 5 {
                let tx = conn.unchecked_transaction()?;
                schema::migrate_v4_to_v5(&conn)?;
                tx.commit()?;
            }
            if existing_version < 6 {
                let tx = conn.unchecked_transaction()?;
                schema::migrate_v5_to_v6(&conn)?;
                tx.commit()?;
            }
            if existing_version < 7 {
                let tx = conn.unchecked_transaction()?;
                schema::migrate_v6_to_v7(&conn)?;
                tx.commit()?;
            }
        }

        conn.execute_batch(&schema::create_tables_sql())?;

        if enable_vec {
            conn.execute_batch(&schema::create_vec_tables_sql())?;
            // Enforce that the on-disk vec0 table dimension matches the
            // compile-time EMBEDDING_DIM. On mismatch (e.g., user upgrades
            // the embedding model) we drop the vec table and rebuild at the
            // new dim rather than silently producing corrupt similarity scores.
            Self::ensure_embedding_dim_consistency(&conn)?;
        }

        conn.pragma_update(None, "user_version", schema::SCHEMA_VERSION)?;

        // Check INDEX_VERSION (stored in application_id pragma).
        // When parser/indexer logic changes, INDEX_VERSION is bumped and
        // we clear all indexed data so the next ensure_indexed does a full rebuild.
        let stored_index_version: i32 = conn.pragma_query_value(None, "application_id", |row| row.get(0))?;
        if stored_index_version != 0 && stored_index_version != crate::domain::INDEX_VERSION {
            tracing::info!(
                "[index] Index version changed ({} → {}), clearing stale data for rebuild",
                stored_index_version, crate::domain::INDEX_VERSION
            );
            conn.execute_batch(
                "BEGIN; DELETE FROM edges; DELETE FROM nodes; DELETE FROM files; COMMIT;"
            )?;
        }
        conn.pragma_update(None, "application_id", crate::domain::INDEX_VERSION)?;

        Ok(Self { conn, vec_enabled: enable_vec })
    }

    /// Check if an error indicates SQLite database corruption.
    /// Used to decide whether to auto-delete and rebuild the index cache.
    fn is_corruption_error(e: &anyhow::Error) -> bool {
        let msg = e.to_string();
        if msg.contains("malformed") || msg.contains("corrupt") || msg.contains("not a database") {
            return true;
        }
        if let Some(sqlite_err) = e.downcast_ref::<rusqlite::Error>() {
            return matches!(
                sqlite_err,
                rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error { code: rusqlite::ffi::ErrorCode::DatabaseCorrupt, .. },
                    _
                ) | rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error { code: rusqlite::ffi::ErrorCode::NotADatabase, .. },
                    _
                )
            );
        }
        false
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn vec_enabled(&self) -> bool {
        self.vec_enabled
    }

    /// Run PRAGMA optimize to rebuild query planner statistics after bulk writes.
    pub fn run_optimize(&self) -> Result<()> {
        self.conn.execute_batch("PRAGMA optimize;")?;
        Ok(())
    }

    /// Compare the stored embedding dimension (meta table) against the
    /// compile-time EMBEDDING_DIM. Mismatch → drop node_vectors and recreate
    /// at the new dim so the next indexing run re-embeds cleanly.
    ///
    /// Three cases:
    ///   1. meta.embedding_dim == current: no-op.
    ///   2. meta.embedding_dim exists but != current: drop + rebuild.
    ///   3. meta.embedding_dim absent (fresh install OR first post-v7 open on a
    ///      v6 DB): introspect actual vec0 dim from sqlite_master.sql. If it
    ///      exists at a different dim than current, drop + rebuild; otherwise
    ///      just record the current dim. This catches the scenario where a
    ///      user built the binary at one EMBEDDING_DIM, generated a v6 DB,
    ///      then rebuilt the binary at a different EMBEDDING_DIM — without
    ///      this introspection, v6→v7 would silently stamp the wrong dim and
    ///      every subsequent INSERT into node_vectors would crash.
    fn ensure_embedding_dim_consistency(conn: &rusqlite::Connection) -> Result<()> {
        let current: i64 = crate::domain::EMBEDDING_DIM as i64;

        let stored: Option<i64> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [schema::META_KEY_EMBEDDING_DIM],
                |row| {
                    let v: String = row.get(0)?;
                    Ok(v.parse::<i64>().ok())
                },
            )
            .unwrap_or(None);

        let effective = stored.or_else(|| Self::vec0_dim_from_sqlite_master(conn));

        match effective {
            Some(dim) if dim == current => {} // match, nothing to do
            Some(dim) => {
                tracing::warn!(
                    "[vec] Embedding dim changed: on-disk={} current={}. \
                     Dropping node_vectors and rebuilding at the new dim. \
                     Existing vectors were invalid for the new model.",
                    dim, current
                );
                // Atomically drop + recreate so a mid-statement failure can't
                // leave the DB with no vec0 table at all.
                let tx = conn.unchecked_transaction()?;
                tx.execute_batch("DROP TABLE IF EXISTS node_vectors;")?;
                tx.execute_batch(&schema::create_vec_tables_sql())?;
                tx.commit()?;
            }
            None => {
                tracing::debug!("[vec] No prior vec0 table found; recording embedding_dim={}", current);
            }
        }

        // Upsert current dim (idempotent — same value on match).
        conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [schema::META_KEY_EMBEDDING_DIM, &current.to_string()],
        )?;
        Ok(())
    }

    /// Parse `float[N]` from the node_vectors DDL stored in sqlite_master.sql.
    /// Returns None when the table doesn't exist or the DDL shape is unexpected.
    fn vec0_dim_from_sqlite_master(conn: &rusqlite::Connection) -> Option<i64> {
        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='node_vectors'",
                [],
                |r| r.get(0),
            )
            .ok()?;
        let start = sql.find("float[")?;
        let remainder = &sql[start + 6..];
        let end = remainder.find(']')?;
        remainder[..end].trim().parse::<i64>().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_v7_records_embedding_dim_on_fresh_db() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        let db = Database::open_with_vec(&db_path).unwrap();

        let stored: String = db.conn()
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [schema::META_KEY_EMBEDDING_DIM],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, crate::domain::EMBEDDING_DIM.to_string());
    }

    #[test]
    fn test_embedding_dim_mismatch_rebuilds_vec_table() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        let current_dim = crate::domain::EMBEDDING_DIM as i64;

        // First open: normal path records dim
        drop(Database::open_with_vec(&db_path).unwrap());

        // Simulate a model swap by poisoning meta with a wrong dim.
        let fake_dim = current_dim + 1;
        {
            let c = Connection::open(&db_path).unwrap();
            c.execute(
                "UPDATE meta SET value = ?1 WHERE key = ?2",
                [&fake_dim.to_string(), schema::META_KEY_EMBEDDING_DIM],
            )
            .unwrap();
        }

        // Reopen: guard should detect mismatch, drop + recreate node_vectors,
        // and upsert current dim.
        let db = Database::open_with_vec(&db_path).unwrap();
        let stored: i64 = db.conn()
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM meta WHERE key = ?1",
                [schema::META_KEY_EMBEDDING_DIM],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            stored, current_dim,
            "stored dim must be upserted back to current EMBEDDING_DIM"
        );
        // node_vectors must exist and be empty (rebuilt)
        let count: i64 = db.conn()
            .query_row("SELECT COUNT(*) FROM node_vectors", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_v6_upgrade_rebuilds_vec_table_when_dim_differs() {
        // Reproduces the adversarial I1 scenario: a v6 DB already has a
        // node_vectors vec0 table at a dim different from the current
        // EMBEDDING_DIM (e.g., user rebuilt the binary with a new model).
        // Without sqlite_master introspection, the v6→v7 migration would
        // silently stamp the current dim into meta while leaving the old
        // vec0 in place — every subsequent INSERT would fail at runtime.
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        let current_dim = crate::domain::EMBEDDING_DIM as i64;
        let fake_dim = if current_dim == 128 { 256 } else { 128 };

        // Hand-craft a v6 DB with a wrong-dim vec0 table.
        {
            register_sqlite_vec();
            let c = Connection::open(&db_path).unwrap();
            c.execute_batch("PRAGMA journal_mode = WAL;").unwrap();
            c.execute_batch(&format!(
                "CREATE VIRTUAL TABLE node_vectors USING vec0(
                    node_id INTEGER PRIMARY KEY,
                    embedding float[{}]
                );",
                fake_dim
            ))
            .unwrap();
            c.pragma_update(None, "user_version", 6).unwrap();
        }

        // Reopen: guard must introspect actual vec0 dim, detect mismatch,
        // drop + rebuild at current dim, and stamp meta.
        let db = Database::open_with_vec(&db_path).unwrap();
        let actual = Database::vec0_dim_from_sqlite_master(db.conn()).unwrap();
        assert_eq!(
            actual, current_dim,
            "node_vectors must be rebuilt at current EMBEDDING_DIM after v6→v7 upgrade"
        );
        let stored: String = db.conn()
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [schema::META_KEY_EMBEDDING_DIM],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, current_dim.to_string());
    }

    #[test]
    fn test_v6_to_v7_migration_adds_meta_table() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");

        // Build a v6 database by hand, then verify open upgrades it to v7.
        {
            let c = Connection::open(&db_path).unwrap();
            c.execute_batch("PRAGMA journal_mode = WAL;").unwrap();
            // Minimal v6 shape: we don't need full tables for this test — just
            // set user_version=6 so the migration path fires on reopen.
            c.pragma_update(None, "user_version", 6).unwrap();
        }

        let db = Database::open_with_vec(&db_path).unwrap();
        // Meta table exists and has our dim recorded
        let stored: String = db.conn()
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [schema::META_KEY_EMBEDDING_DIM],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, crate::domain::EMBEDDING_DIM.to_string());

        let version: i32 = db.conn()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, schema::SCHEMA_VERSION);
    }

    #[test]
    fn test_open_readonly_rejects_writes() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        // Primary bootstrap: create the DB normally.
        drop(Database::open(&db_path).unwrap());

        // Secondary opens read-only.
        let ro = Database::open_readonly(&db_path).unwrap();

        // Reads work.
        let count: i64 = ro.conn()
            .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);

        // Writes must fail at the SQLite layer — not bubble up as silent no-ops.
        let err = ro.conn()
            .execute(
                "INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) \
                 VALUES ('a', 'b', 0, 'rust', 0)",
                [],
            )
            .unwrap_err();
        let msg = format!("{}", err);
        assert!(
            msg.contains("readonly") || msg.contains("read-only") || msg.contains("read only"),
            "Expected read-only error, got: {}",
            msg
        );
    }

    #[test]
    fn test_open_readonly_missing_file_errors() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("no-such.db");
        assert!(Database::open_readonly(&missing).is_err());
    }

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
        assert!(!tables.contains(&"context_sandbox".to_string()));
    }

    #[test]
    fn test_schema_version_is_set() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        let db = Database::open(&db_path).unwrap();

        let version: i32 = db.conn()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, schema::SCHEMA_VERSION);
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

    #[test]
    fn test_v1_to_v2_migration() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");

        // Create a v1 database manually (without the 3 new columns)
        {
            register_sqlite_vec();
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;").unwrap();
            conn.execute_batch(
                "CREATE TABLE files (
                    id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE,
                    blake3_hash TEXT NOT NULL, last_modified INTEGER NOT NULL,
                    language TEXT, indexed_at INTEGER NOT NULL
                );
                CREATE TABLE nodes (
                    id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                    type TEXT NOT NULL, name TEXT NOT NULL, qualified_name TEXT,
                    start_line INTEGER NOT NULL, end_line INTEGER NOT NULL,
                    code_content TEXT NOT NULL, signature TEXT, doc_comment TEXT, context_string TEXT
                );
                CREATE TABLE edges (
                    id INTEGER PRIMARY KEY,
                    source_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                    target_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                    relation TEXT NOT NULL, metadata TEXT, UNIQUE(source_id, target_id, relation)
                );
                CREATE VIRTUAL TABLE nodes_fts USING fts5(
                    name, qualified_name, code_content, context_string, doc_comment,
                    content='nodes', content_rowid='id'
                );"
            ).unwrap();
            // Insert test data to verify preservation
            conn.execute(
                "INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('test.ts', 'h1', 1, 'typescript', 0)",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) VALUES (1, 'function', 'hello', 'hello', 1, 5, 'function hello() {}')",
                [],
            ).unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
        }

        // Open with Database::open — should trigger v1→v2 migration
        let db = Database::open(&db_path).unwrap();

        // Verify schema version updated
        let version: i32 = db.conn()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, schema::SCHEMA_VERSION);

        // Verify new columns exist (can write to them)
        db.conn().execute(
            "UPDATE nodes SET name_tokens = 'hello', return_type = 'void', param_types = '()' WHERE id = 1",
            [],
        ).unwrap();

        // Verify FTS5 has 8 columns (insert trigger fires on UPDATE with new columns)
        let fts_count: i64 = db.conn().query_row(
            "SELECT COUNT(*) FROM nodes_fts WHERE nodes_fts MATCH 'hello'",
            [], |row| row.get(0),
        ).unwrap();
        assert!(fts_count >= 1, "FTS5 should find existing data after migration rebuild");

        // Verify existing data preserved
        let name: String = db.conn().query_row(
            "SELECT name FROM nodes WHERE id = 1", [], |row| row.get(0),
        ).unwrap();
        assert_eq!(name, "hello");
    }

    #[test]
    fn test_corrupt_db_auto_recovery() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        // Write garbage to simulate corruption
        std::fs::write(&db_path, b"this is not a valid sqlite database").unwrap();
        // Should auto-delete and recreate instead of crashing
        let db = Database::open(&db_path).unwrap();
        // Verify it works — tables were created
        let tables: Vec<String> = db.conn()
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(tables.contains(&"files".to_string()), "Expected 'files' table after recovery");
        assert!(tables.contains(&"nodes".to_string()), "Expected 'nodes' table after recovery");
    }

    #[test]
    fn test_corrupt_db_removes_wal_and_shm() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        let wal_path = db_path.with_extension("db-wal");
        let shm_path = db_path.with_extension("db-shm");
        // Create corrupt DB + stale WAL/SHM files
        std::fs::write(&db_path, b"not a database").unwrap();
        std::fs::write(&wal_path, b"stale wal").unwrap();
        std::fs::write(&shm_path, b"stale shm").unwrap();
        // Recovery should clean up stale WAL and SHM before recreating
        let _db = Database::open(&db_path).unwrap();
        // The new connection creates a fresh WAL (because we use PRAGMA journal_mode=WAL),
        // but the stale content must be gone — verify the WAL is not our sentinel value
        if wal_path.exists() {
            let content = std::fs::read(&wal_path).unwrap();
            assert_ne!(content, b"stale wal", "Stale WAL content should be replaced");
        }
        // SHM may or may not be recreated depending on WAL activity
        if shm_path.exists() {
            let content = std::fs::read(&shm_path).unwrap();
            assert_ne!(content, b"stale shm", "Stale SHM content should be replaced");
        }
    }

    #[test]
    fn test_non_corruption_error_still_propagates() {
        // Opening a path where the parent dir doesn't exist is not corruption
        let bad_path = Path::new("/nonexistent_dir_xyz/impossible/index.db");
        let result = Database::open(bad_path);
        assert!(result.is_err(), "Non-corruption errors should still propagate");
    }

    #[test]
    fn test_v2_to_v3_migration() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");

        // Create a v2 database manually:
        // - nodes has name_tokens, return_type, param_types (added in v1->v2)
        // - edges has UNIQUE(source_id, target_id, relation) -- old constraint without metadata
        // - FTS5 has 8 columns but NO porter stemmer
        {
            register_sqlite_vec();
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;").unwrap();
            conn.execute_batch(
                "CREATE TABLE files (
                    id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE,
                    blake3_hash TEXT NOT NULL, last_modified INTEGER NOT NULL,
                    language TEXT, indexed_at INTEGER NOT NULL
                );
                CREATE TABLE nodes (
                    id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                    type TEXT NOT NULL, name TEXT NOT NULL, qualified_name TEXT,
                    start_line INTEGER NOT NULL, end_line INTEGER NOT NULL,
                    code_content TEXT NOT NULL, signature TEXT, doc_comment TEXT, context_string TEXT,
                    name_tokens TEXT, return_type TEXT, param_types TEXT
                );
                CREATE TABLE edges (
                    id INTEGER PRIMARY KEY,
                    source_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                    target_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                    relation TEXT NOT NULL, metadata TEXT,
                    UNIQUE(source_id, target_id, relation)
                );
                CREATE VIRTUAL TABLE nodes_fts USING fts5(
                    name, qualified_name, code_content, context_string, doc_comment,
                    name_tokens, return_type, param_types,
                    content='nodes', content_rowid='id'
                );"
            ).unwrap();

            // Insert test data
            conn.execute(
                "INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('test.ts', 'h1', 1, 'typescript', 0)",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) VALUES (1, 'function', 'hello', 'hello', 1, 5, 'function hello() {}')",
                [],
            ).unwrap();
            // Insert an edge to verify data preservation through table recreation
            conn.execute(
                "INSERT INTO edges (source_id, target_id, relation, metadata) VALUES (1, 1, 'calls', 'GET /api')",
                [],
            ).unwrap();
            conn.pragma_update(None, "user_version", 2).unwrap();
        }

        // Open with Database::open -- triggers v2->v3 (and v3->v4, v4->v5) migration
        let db = Database::open(&db_path).unwrap();

        // Verify schema version updated to current
        let version: i32 = db.conn()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, schema::SCHEMA_VERSION);

        // Verify the new UNIQUE index exists on edges (includes metadata via COALESCE)
        let idx_exists: bool = db.conn().query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='index' AND name='idx_edges_unique'",
            [], |row| row.get(0),
        ).unwrap();
        assert!(idx_exists, "idx_edges_unique should exist after v2->v3 migration");

        // Verify that edges with same (source, target, relation) but different metadata are allowed
        // (this was the whole point of v3: metadata is part of the unique constraint)
        db.conn().execute(
            "INSERT INTO edges (source_id, target_id, relation, metadata) VALUES (1, 1, 'calls', 'POST /api')",
            [],
        ).unwrap();

        // Verify existing edge data preserved
        let edge_meta: String = db.conn().query_row(
            "SELECT metadata FROM edges WHERE source_id = 1 AND metadata = 'GET /api'",
            [], |row| row.get(0),
        ).unwrap();
        assert_eq!(edge_meta, "GET /api");

        // Verify existing node data preserved
        let name: String = db.conn().query_row(
            "SELECT name FROM nodes WHERE id = 1", [], |row| row.get(0),
        ).unwrap();
        assert_eq!(name, "hello");
    }

    #[test]
    fn test_v3_to_v4_migration() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");

        // Create a v3 database manually:
        // - nodes has name_tokens, return_type, param_types
        // - edges has the v3 UNIQUE constraint (includes metadata)
        // - FTS5 has 8 columns but NO porter stemmer (plain tokenizer)
        {
            register_sqlite_vec();
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;").unwrap();
            conn.execute_batch(
                "CREATE TABLE files (
                    id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE,
                    blake3_hash TEXT NOT NULL, last_modified INTEGER NOT NULL,
                    language TEXT, indexed_at INTEGER NOT NULL
                );
                CREATE TABLE nodes (
                    id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                    type TEXT NOT NULL, name TEXT NOT NULL, qualified_name TEXT,
                    start_line INTEGER NOT NULL, end_line INTEGER NOT NULL,
                    code_content TEXT NOT NULL, signature TEXT, doc_comment TEXT, context_string TEXT,
                    name_tokens TEXT, return_type TEXT, param_types TEXT
                );
                CREATE TABLE edges (
                    id INTEGER PRIMARY KEY,
                    source_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                    target_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                    relation TEXT NOT NULL, metadata TEXT
                );
                CREATE UNIQUE INDEX idx_edges_unique ON edges(source_id, target_id, relation, COALESCE(metadata, ''));
                CREATE VIRTUAL TABLE nodes_fts USING fts5(
                    name, qualified_name, code_content, context_string, doc_comment,
                    name_tokens, return_type, param_types,
                    content='nodes', content_rowid='id'
                );"
            ).unwrap();

            // Insert test data with a word that tests porter stemming
            conn.execute(
                "INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('test.ts', 'h1', 1, 'typescript', 0)",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) VALUES (1, 'function', 'running', 'running', 1, 5, 'function running() {}')",
                [],
            ).unwrap();
            conn.pragma_update(None, "user_version", 3).unwrap();
        }

        // Open with Database::open -- triggers v3->v4 (and v4->v5) migration
        let db = Database::open(&db_path).unwrap();

        // Verify schema version updated to current
        let version: i32 = db.conn()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, schema::SCHEMA_VERSION);

        // Verify porter stemming works: searching "run" should match "running"
        let fts_count: i64 = db.conn().query_row(
            "SELECT COUNT(*) FROM nodes_fts WHERE nodes_fts MATCH 'run'",
            [], |row| row.get(0),
        ).unwrap();
        assert!(fts_count >= 1, "Porter stemmer should allow 'run' to match 'running'");

        // Verify existing node data preserved
        let name: String = db.conn().query_row(
            "SELECT name FROM nodes WHERE id = 1", [], |row| row.get(0),
        ).unwrap();
        assert_eq!(name, "running");
    }

    #[test]
    fn test_v4_to_v5_migration() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");

        // Create a v4 database manually:
        // - nodes has name_tokens, return_type, param_types (but NO is_test column)
        // - edges has v3 UNIQUE constraint (includes metadata)
        // - FTS5 has porter stemmer
        {
            register_sqlite_vec();
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;").unwrap();
            conn.execute_batch(
                "CREATE TABLE files (
                    id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE,
                    blake3_hash TEXT NOT NULL, last_modified INTEGER NOT NULL,
                    language TEXT, indexed_at INTEGER NOT NULL
                );
                CREATE TABLE nodes (
                    id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                    type TEXT NOT NULL, name TEXT NOT NULL, qualified_name TEXT,
                    start_line INTEGER NOT NULL, end_line INTEGER NOT NULL,
                    code_content TEXT NOT NULL, signature TEXT, doc_comment TEXT, context_string TEXT,
                    name_tokens TEXT, return_type TEXT, param_types TEXT
                );
                CREATE TABLE edges (
                    id INTEGER PRIMARY KEY,
                    source_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                    target_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                    relation TEXT NOT NULL, metadata TEXT
                );
                CREATE UNIQUE INDEX idx_edges_unique ON edges(source_id, target_id, relation, COALESCE(metadata, ''));
                CREATE VIRTUAL TABLE nodes_fts USING fts5(
                    name, qualified_name, code_content, context_string, doc_comment,
                    name_tokens, return_type, param_types,
                    content='nodes', content_rowid='id',
                    tokenize='porter unicode61'
                );
                CREATE TRIGGER nodes_ai AFTER INSERT ON nodes BEGIN
                    INSERT INTO nodes_fts(rowid, name, qualified_name, code_content, context_string, doc_comment, name_tokens, return_type, param_types)
                    VALUES (new.id, new.name, new.qualified_name, new.code_content, new.context_string, new.doc_comment, new.name_tokens, new.return_type, new.param_types);
                END;
                CREATE TRIGGER nodes_ad AFTER DELETE ON nodes BEGIN
                    INSERT INTO nodes_fts(nodes_fts, rowid, name, qualified_name, code_content, context_string, doc_comment, name_tokens, return_type, param_types)
                    VALUES ('delete', old.id, old.name, old.qualified_name, old.code_content, old.context_string, old.doc_comment, old.name_tokens, old.return_type, old.param_types);
                END;
                CREATE TRIGGER nodes_au AFTER UPDATE ON nodes BEGIN
                    INSERT INTO nodes_fts(nodes_fts, rowid, name, qualified_name, code_content, context_string, doc_comment, name_tokens, return_type, param_types)
                    VALUES ('delete', old.id, old.name, old.qualified_name, old.code_content, old.context_string, old.doc_comment, old.name_tokens, old.return_type, old.param_types);
                    INSERT INTO nodes_fts(rowid, name, qualified_name, code_content, context_string, doc_comment, name_tokens, return_type, param_types)
                    VALUES (new.id, new.name, new.qualified_name, new.code_content, new.context_string, new.doc_comment, new.name_tokens, new.return_type, new.param_types);
                END;"
            ).unwrap();

            // Insert test data
            conn.execute(
                "INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('test.ts', 'h1', 1, 'typescript', 0)",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) VALUES (1, 'function', 'myFunc', 'myFunc', 1, 5, 'function myFunc() {}')",
                [],
            ).unwrap();
            conn.pragma_update(None, "user_version", 4).unwrap();
        }

        // Open with Database::open -- triggers v4->v5 migration
        let db = Database::open(&db_path).unwrap();

        // Verify schema version updated to current
        let version: i32 = db.conn()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, schema::SCHEMA_VERSION);

        // Verify is_test column exists and defaults to 0 for existing rows
        let is_test: i32 = db.conn().query_row(
            "SELECT is_test FROM nodes WHERE id = 1", [], |row| row.get(0),
        ).unwrap();
        assert_eq!(is_test, 0, "is_test should default to 0 for existing rows");

        // Verify we can set is_test to 1
        db.conn().execute("UPDATE nodes SET is_test = 1 WHERE id = 1", []).unwrap();
        let is_test_updated: i32 = db.conn().query_row(
            "SELECT is_test FROM nodes WHERE id = 1", [], |row| row.get(0),
        ).unwrap();
        assert_eq!(is_test_updated, 1);

        // Verify existing node data preserved
        let name: String = db.conn().query_row(
            "SELECT name FROM nodes WHERE id = 1", [], |row| row.get(0),
        ).unwrap();
        assert_eq!(name, "myFunc");
    }

    #[test]
    fn test_v5_to_v6_migration() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");

        // Create a v5 database manually:
        // - nodes has is_test column (added in v4->v5)
        // - NO idx_nodes_qualified_name index
        {
            register_sqlite_vec();
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;").unwrap();
            conn.execute_batch(
                "CREATE TABLE files (
                    id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE,
                    blake3_hash TEXT NOT NULL, last_modified INTEGER NOT NULL,
                    language TEXT, indexed_at INTEGER NOT NULL
                );
                CREATE TABLE nodes (
                    id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                    type TEXT NOT NULL, name TEXT NOT NULL, qualified_name TEXT,
                    start_line INTEGER NOT NULL, end_line INTEGER NOT NULL,
                    code_content TEXT NOT NULL, signature TEXT, doc_comment TEXT, context_string TEXT,
                    name_tokens TEXT, return_type TEXT, param_types TEXT,
                    is_test INTEGER NOT NULL DEFAULT 0
                );
                CREATE INDEX idx_nodes_file ON nodes(file_id);
                CREATE INDEX idx_nodes_type ON nodes(type);
                CREATE INDEX idx_nodes_name ON nodes(name);
                CREATE TABLE edges (
                    id INTEGER PRIMARY KEY,
                    source_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                    target_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
                    relation TEXT NOT NULL, metadata TEXT
                );
                CREATE UNIQUE INDEX idx_edges_unique ON edges(source_id, target_id, relation, COALESCE(metadata, ''));
                CREATE VIRTUAL TABLE nodes_fts USING fts5(
                    name, qualified_name, code_content, context_string, doc_comment,
                    name_tokens, return_type, param_types,
                    content='nodes', content_rowid='id',
                    tokenize='porter unicode61'
                );
                CREATE TRIGGER nodes_ai AFTER INSERT ON nodes BEGIN
                    INSERT INTO nodes_fts(rowid, name, qualified_name, code_content, context_string, doc_comment, name_tokens, return_type, param_types)
                    VALUES (new.id, new.name, new.qualified_name, new.code_content, new.context_string, new.doc_comment, new.name_tokens, new.return_type, new.param_types);
                END;
                CREATE TRIGGER nodes_ad AFTER DELETE ON nodes BEGIN
                    INSERT INTO nodes_fts(nodes_fts, rowid, name, qualified_name, code_content, context_string, doc_comment, name_tokens, return_type, param_types)
                    VALUES ('delete', old.id, old.name, old.qualified_name, old.code_content, old.context_string, old.doc_comment, old.name_tokens, old.return_type, old.param_types);
                END;
                CREATE TRIGGER nodes_au AFTER UPDATE ON nodes BEGIN
                    INSERT INTO nodes_fts(nodes_fts, rowid, name, qualified_name, code_content, context_string, doc_comment, name_tokens, return_type, param_types)
                    VALUES ('delete', old.id, old.name, old.qualified_name, old.code_content, old.context_string, old.doc_comment, old.name_tokens, old.return_type, old.param_types);
                    INSERT INTO nodes_fts(rowid, name, qualified_name, code_content, context_string, doc_comment, name_tokens, return_type, param_types)
                    VALUES (new.id, new.name, new.qualified_name, new.code_content, new.context_string, new.doc_comment, new.name_tokens, new.return_type, new.param_types);
                END;"
            ).unwrap();

            // Insert test data
            conn.execute(
                "INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('test.ts', 'h1', 1, 'typescript', 0)",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) VALUES (1, 'function', 'myFunc', 'MyModule.myFunc', 1, 5, 'function myFunc() {}')",
                [],
            ).unwrap();
            conn.pragma_update(None, "user_version", 5).unwrap();
        }

        // Open with Database::open -- triggers v5->v6 migration
        let db = Database::open(&db_path).unwrap();

        // Verify schema version updated to current
        let version: i32 = db.conn()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, schema::SCHEMA_VERSION);

        // Verify idx_nodes_qualified_name index exists
        let idx_exists: bool = db.conn().query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='index' AND name='idx_nodes_qualified_name'",
            [], |row| row.get(0),
        ).unwrap();
        assert!(idx_exists, "idx_nodes_qualified_name should exist after v5->v6 migration");

        // Verify existing node data preserved
        let qname: String = db.conn().query_row(
            "SELECT qualified_name FROM nodes WHERE id = 1", [], |row| row.get(0),
        ).unwrap();
        assert_eq!(qname, "MyModule.myFunc");
    }

    #[test]
    fn test_vec0_extension_loads() {
        let tmp = TempDir::new().unwrap();
        let db = Database::open_with_vec(&tmp.path().join("test.db")).unwrap();
        // Try creating a vec0 table
        db.conn().execute_batch(
            "CREATE VIRTUAL TABLE test_vec USING vec0(embedding float[4]);"
        ).unwrap();
        // Insert a vector
        let vec_data: Vec<f32> = vec![1.0, 0.0, 0.0, 0.0];
        let bytes: &[u8] = bytemuck::cast_slice(&vec_data);
        db.conn().execute(
            "INSERT INTO test_vec(rowid, embedding) VALUES (1, ?)",
            [bytes],
        ).unwrap();
    }

    #[test]
    fn test_vec0_vector_search() {
        let tmp = TempDir::new().unwrap();
        let db = Database::open_with_vec(&tmp.path().join("test.db")).unwrap();
        db.conn().execute_batch(
            "CREATE VIRTUAL TABLE test_vec USING vec0(embedding float[4]);"
        ).unwrap();

        // Insert vectors
        let vecs: Vec<Vec<f32>> = vec![
            vec![1.0, 0.0, 0.0, 0.0],
            vec![0.0, 1.0, 0.0, 0.0],
            vec![0.9, 0.1, 0.0, 0.0], // similar to first
        ];
        for (i, v) in vecs.iter().enumerate() {
            let bytes: &[u8] = bytemuck::cast_slice(v);
            db.conn().execute(
                "INSERT INTO test_vec(rowid, embedding) VALUES (?1, ?2)",
                rusqlite::params![i as i64 + 1, bytes],
            ).unwrap();
        }

        // Search for similar to [1,0,0,0]
        let query: Vec<f32> = vec![1.0, 0.0, 0.0, 0.0];
        let query_bytes: &[u8] = bytemuck::cast_slice(&query);
        let mut stmt = db.conn().prepare(
            "SELECT rowid, distance FROM test_vec WHERE embedding MATCH ?1 ORDER BY distance LIMIT 2"
        ).unwrap();
        let results: Vec<(i64, f64)> = stmt.query_map([query_bytes], |row| {
            Ok((row.get(0)?, row.get(1)?))
        }).unwrap().filter_map(|r| r.ok()).collect();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, 1); // exact match first
    }
}
