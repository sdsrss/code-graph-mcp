use anyhow::Result;
use rusqlite::Connection;
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

    fn open_impl(path: &Path, enable_vec: bool) -> Result<Self> {
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
            // Run migrations before CREATE_TABLES (which uses v2 schema)
            if existing_version < 2 {
                let tx = conn.unchecked_transaction()?;
                schema::migrate_v1_to_v2(&conn)?;
                tx.commit()?;
            }
        }

        conn.execute_batch(schema::CREATE_TABLES)?;

        if enable_vec {
            conn.execute_batch(&schema::create_vec_tables_sql())?;
        }

        conn.pragma_update(None, "user_version", schema::SCHEMA_VERSION)?;

        Ok(Self { conn, vec_enabled: enable_vec })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn vec_enabled(&self) -> bool {
        self.vec_enabled
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
