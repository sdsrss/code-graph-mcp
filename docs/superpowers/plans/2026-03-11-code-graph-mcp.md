# code-graph-mcp Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a local, zero-config MCP server in Rust that provides intelligent code search, call graph analysis, and context-aware embeddings for Claude Code.

**Architecture:** Single-crate Rust binary communicating over JSON-RPC 2.0 stdio. SQLite for storage (FTS5 + sqlite-vec), Tree-sitter for AST parsing, candle for local ML inference. Three-phase indexing pipeline with Merkle tree change detection and graph-augmented embeddings.

**Tech Stack:** Rust, SQLite (rusqlite), Tree-sitter, candle (MiniLM-L6-v2), blake3, notify, serde_json, tokio

**Spec:** `docs/superpowers/specs/2026-03-11-code-graph-mcp-design.md`

---

## Chunk 1: Project Foundation + Storage Layer

### Task 1: Initialize Cargo Project

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`
- Create: `src/lib.rs`
- Create: `.gitignore`

- [ ] **Step 1: Create Cargo project**

```bash
cd /mnt/data_ssd/dev/projects/code-graph-mcp
cargo init --name code-graph-mcp
```

- [ ] **Step 2: Configure Cargo.toml with initial dependencies**

```toml
[package]
name = "code-graph-mcp"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }
rusqlite = { version = "0.31", features = ["bundled", "fts5", "functions", "loadable_extension"] }
blake3 = "1"
ignore = "0.4"
tracing = "0.1"
tracing-subscriber = "0.3"
anyhow = "1"
bytemuck = "1"

[dev-dependencies]
tempfile = "3"
```

Note: Start with minimal deps. tree-sitter, candle, notify, sqlite-vec added in later chunks when needed. `loadable_extension` is needed for sqlite-vec in Chunk 5. `bytemuck` for casting f32 slices to bytes for vec0.

- [ ] **Step 3: Create src/lib.rs with module declarations**

```rust
pub mod storage;
pub mod utils;
```

- [ ] **Step 4: Create stub modules**

Create these files with just `// TODO` placeholder:
- `src/storage/mod.rs`
- `src/utils/mod.rs`
- `src/utils/config.rs`
- `src/utils/gitignore.rs`

- [ ] **Step 5: Verify it compiles**

Run: `cargo build`
Expected: Compiles with no errors

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/ .gitignore
git commit -m "feat: initialize cargo project with core dependencies"
```

---

### Task 2: SQLite Database Manager + Schema

**Files:**
- Create: `src/storage/mod.rs`
- Create: `src/storage/db.rs`
- Create: `src/storage/schema.rs`

- [ ] **Step 1: Write failing test for database initialization**

In `src/storage/db.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_init_creates_db_and_tables() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("index.db");
        let db = Database::open(&db_path).unwrap();

        // Verify tables exist by querying sqlite_master
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib storage::db::tests`
Expected: FAIL — `Database` not defined

- [ ] **Step 3: Implement Database struct and schema**

In `src/storage/schema.rs` — all CREATE TABLE / CREATE INDEX / CREATE TRIGGER DDL statements from the design spec Section 4. Key constant:

```rust
pub const SCHEMA_VERSION: i32 = 1;

pub const CREATE_TABLES: &str = r#"
CREATE TABLE IF NOT EXISTS files (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    blake3_hash TEXT NOT NULL,
    last_modified INTEGER NOT NULL,
    language TEXT,
    indexed_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS nodes (
    id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    type TEXT NOT NULL,
    name TEXT NOT NULL,
    qualified_name TEXT,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    code_content TEXT NOT NULL,
    signature TEXT,
    doc_comment TEXT,
    context_string TEXT
);

CREATE INDEX IF NOT EXISTS idx_nodes_file ON nodes(file_id);
CREATE INDEX IF NOT EXISTS idx_nodes_type ON nodes(type);
CREATE INDEX IF NOT EXISTS idx_nodes_name ON nodes(name);

-- FTS5 virtual table
CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
    name, qualified_name, code_content, context_string, doc_comment,
    content='nodes', content_rowid='id'
);

-- FTS5 sync triggers
CREATE TRIGGER IF NOT EXISTS nodes_ai AFTER INSERT ON nodes BEGIN
    INSERT INTO nodes_fts(rowid, name, qualified_name, code_content, context_string, doc_comment)
    VALUES (new.id, new.name, new.qualified_name, new.code_content, new.context_string, new.doc_comment);
END;
CREATE TRIGGER IF NOT EXISTS nodes_ad AFTER DELETE ON nodes BEGIN
    INSERT INTO nodes_fts(nodes_fts, rowid, name, qualified_name, code_content, context_string, doc_comment)
    VALUES ('delete', old.id, old.name, old.qualified_name, old.code_content, old.context_string, old.doc_comment);
END;
CREATE TRIGGER IF NOT EXISTS nodes_au AFTER UPDATE ON nodes BEGIN
    INSERT INTO nodes_fts(nodes_fts, rowid, name, qualified_name, code_content, context_string, doc_comment)
    VALUES ('delete', old.id, old.name, old.qualified_name, old.code_content, old.context_string, old.doc_comment);
    INSERT INTO nodes_fts(rowid, name, qualified_name, code_content, context_string, doc_comment)
    VALUES (new.id, new.name, new.qualified_name, new.code_content, new.context_string, new.doc_comment);
END;

CREATE TABLE IF NOT EXISTS edges (
    id INTEGER PRIMARY KEY,
    source_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    target_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    relation TEXT NOT NULL,
    metadata TEXT,
    UNIQUE(source_id, target_id, relation)
);

CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source_id);
CREATE INDEX IF NOT EXISTS idx_edges_target ON edges(target_id);
CREATE INDEX IF NOT EXISTS idx_edges_relation ON edges(relation);

CREATE TABLE IF NOT EXISTS context_sandbox (
    id INTEGER PRIMARY KEY,
    query_hash TEXT NOT NULL,
    summary TEXT NOT NULL,
    pointers TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_sandbox_query ON context_sandbox(query_hash);

CREATE TABLE IF NOT EXISTS merkle_state (
    dir_path TEXT PRIMARY KEY,
    tree_hash TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

-- node_vectors cleanup trigger (vec0 virtual tables skip CASCADE)
-- Note: The vec0 virtual table itself is created in Task 17 (sqlite-vec integration)
-- This trigger is created at that time, but we define a placeholder here as a reminder.
-- Actual trigger: CREATE TRIGGER nodes_vectors_ad AFTER DELETE ON nodes BEGIN
--   DELETE FROM node_vectors WHERE node_id = old.id;
-- END;
"#;
```

In `src/storage/db.rs`:

```rust
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

        // Set PRAGMAs
        conn.execute_batch("
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA cache_size = -64000;
            PRAGMA mmap_size = 268435456;
            PRAGMA temp_store = MEMORY;
            PRAGMA foreign_keys = ON;
        ")?;

        // Create tables
        conn.execute_batch(schema::CREATE_TABLES)?;

        // Set schema version
        conn.pragma_update(None, "user_version", schema::SCHEMA_VERSION)?;

        Ok(Self { conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}
```

In `src/storage/mod.rs`:

```rust
pub mod db;
pub mod schema;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib storage::db::tests`
Expected: All 3 tests PASS

- [ ] **Step 5: Commit**

```bash
git add src/storage/
git commit -m "feat(storage): add SQLite database init with full schema and WAL mode"
```

---

### Task 3: Storage CRUD Operations — Files Table

**Files:**
- Create: `src/storage/queries.rs`

- [ ] **Step 1: Write failing tests for file CRUD**

In `src/storage/queries.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db::Database;
    use tempfile::TempDir;

    fn test_db() -> (Database, TempDir) {
        let tmp = TempDir::new().unwrap();
        let db = Database::open(&tmp.path().join("test.db")).unwrap();
        (db, tmp) // TempDir must outlive Database to keep files on disk
    }

    #[test]
    fn test_upsert_file() {
        let (db, _tmp) = test_db();
        let file = FileRecord {
            path: "src/main.rs".into(),
            blake3_hash: "abc123".into(),
            last_modified: 1000,
            language: Some("rust".into()),
        };
        let id = upsert_file(db.conn(), &file).unwrap();
        assert!(id > 0);

        // Upsert same path updates hash
        let file2 = FileRecord {
            blake3_hash: "def456".into(),
            ..file
        };
        let id2 = upsert_file(db.conn(), &file2).unwrap();
        assert_eq!(id, id2);
    }

    #[test]
    fn test_delete_files_by_paths() {
        let (db, _tmp) = test_db();
        let f1 = FileRecord { path: "a.rs".into(), blake3_hash: "h1".into(), last_modified: 1, language: None };
        let f2 = FileRecord { path: "b.rs".into(), blake3_hash: "h2".into(), last_modified: 1, language: None };
        upsert_file(db.conn(), &f1).unwrap();
        upsert_file(db.conn(), &f2).unwrap();

        delete_files_by_paths(db.conn(), &["a.rs".into()]).unwrap();

        let count: i64 = db.conn()
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_get_all_file_hashes() {
        let (db, _tmp) = test_db();
        let f = FileRecord { path: "x.rs".into(), blake3_hash: "h1".into(), last_modified: 1, language: None };
        upsert_file(db.conn(), &f).unwrap();

        let hashes = get_all_file_hashes(db.conn()).unwrap();
        assert_eq!(hashes.len(), 1);
        assert_eq!(hashes.get("x.rs").unwrap(), "h1");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib storage::queries::tests`
Expected: FAIL

- [ ] **Step 3: Implement file CRUD functions**

```rust
use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashMap;

pub struct FileRecord {
    pub path: String,
    pub blake3_hash: String,
    pub last_modified: i64,
    pub language: Option<String>,
}

pub fn upsert_file(conn: &Connection, file: &FileRecord) -> Result<i64> {
    conn.execute(
        "INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at)
         VALUES (?1, ?2, ?3, ?4, unixepoch())
         ON CONFLICT(path) DO UPDATE SET
            blake3_hash = excluded.blake3_hash,
            last_modified = excluded.last_modified,
            language = excluded.language,
            indexed_at = unixepoch()",
        (&file.path, &file.blake3_hash, file.last_modified, &file.language),
    )?;
    // IMPORTANT: last_insert_rowid() returns 0 on ON CONFLICT UPDATE path.
    // Always SELECT to get the correct id.
    let id: i64 = conn.query_row(
        "SELECT id FROM files WHERE path = ?1",
        [&file.path],
        |row| row.get(0),
    )?;
    Ok(id)
}

pub fn delete_files_by_paths(conn: &Connection, paths: &[String]) -> Result<()> {
    for path in paths {
        conn.execute("DELETE FROM files WHERE path = ?1", [path])?;
    }
    Ok(())
}

pub fn get_all_file_hashes(conn: &Connection) -> Result<HashMap<String, String>> {
    let mut stmt = conn.prepare("SELECT path, blake3_hash FROM files")?;
    let map = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?
    .filter_map(|r| r.ok())
    .collect();
    Ok(map)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib storage::queries::tests`
Expected: All PASS

- [ ] **Step 5: Commit**

```bash
git add src/storage/queries.rs src/storage/mod.rs
git commit -m "feat(storage): add file table CRUD operations"
```

---

### Task 4: Storage CRUD — Nodes + Edges Tables

**Files:**
- Modify: `src/storage/queries.rs`

- [ ] **Step 1: Write failing tests for node and edge CRUD**

Add to `src/storage/queries.rs` tests:

```rust
#[test]
fn test_insert_and_query_node() {
    let db = test_db();
    let file_id = upsert_file(db.conn(), &FileRecord {
        path: "test.ts".into(), blake3_hash: "h".into(), last_modified: 1, language: Some("typescript".into()),
    }).unwrap();

    let node = NodeRecord {
        file_id,
        node_type: "function".into(),
        name: "handleLogin".into(),
        qualified_name: Some("auth.handleLogin".into()),
        start_line: 10,
        end_line: 25,
        code_content: "function handleLogin() {}".into(),
        signature: Some("(req, res) -> void".into()),
        doc_comment: None,
        context_string: None,
    };
    let node_id = insert_node(db.conn(), &node).unwrap();
    assert!(node_id > 0);

    let found = get_nodes_by_name(db.conn(), "handleLogin").unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name, "handleLogin");
}

#[test]
fn test_insert_edge_and_cascade_delete() {
    let db = test_db();
    let fid = upsert_file(db.conn(), &FileRecord {
        path: "t.ts".into(), blake3_hash: "h".into(), last_modified: 1, language: None,
    }).unwrap();
    let n1 = insert_node(db.conn(), &NodeRecord {
        file_id: fid, node_type: "function".into(), name: "a".into(),
        qualified_name: None, start_line: 1, end_line: 5,
        code_content: "fn a(){}".into(), signature: None,
        doc_comment: None, context_string: None,
    }).unwrap();
    let n2 = insert_node(db.conn(), &NodeRecord {
        file_id: fid, node_type: "function".into(), name: "b".into(),
        qualified_name: None, start_line: 6, end_line: 10,
        code_content: "fn b(){}".into(), signature: None,
        doc_comment: None, context_string: None,
    }).unwrap();

    insert_edge(db.conn(), n1, n2, "calls", None).unwrap();

    let edges = get_edges_from(db.conn(), n1).unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].relation, "calls");

    // Delete the file → CASCADE deletes nodes → CASCADE deletes edges
    delete_files_by_paths(db.conn(), &["t.ts".into()]).unwrap();
    let edges_after = get_edges_from(db.conn(), n1).unwrap();
    assert_eq!(edges_after.len(), 0);
}

#[test]
fn test_fts5_search() {
    let db = test_db();
    let fid = upsert_file(db.conn(), &FileRecord {
        path: "t.ts".into(), blake3_hash: "h".into(), last_modified: 1, language: None,
    }).unwrap();
    insert_node(db.conn(), &NodeRecord {
        file_id: fid, node_type: "function".into(), name: "validateToken".into(),
        qualified_name: None, start_line: 1, end_line: 5,
        code_content: "function validateToken(token) { jwt.verify(token); }".into(),
        signature: None, doc_comment: None,
        context_string: Some("validates JWT authentication token".into()),
    }).unwrap();

    let results = fts5_search(db.conn(), "authentication token", 5).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "validateToken");
}

#[test]
fn test_update_context_string() {
    let db = test_db();
    let fid = upsert_file(db.conn(), &FileRecord {
        path: "t.ts".into(), blake3_hash: "h".into(), last_modified: 1, language: None,
    }).unwrap();
    let nid = insert_node(db.conn(), &NodeRecord {
        file_id: fid, node_type: "function".into(), name: "foo".into(),
        qualified_name: None, start_line: 1, end_line: 5,
        code_content: "fn foo(){}".into(), signature: None,
        doc_comment: None, context_string: None,
    }).unwrap();

    update_context_string(db.conn(), nid, "function foo\ncalls: bar, baz").unwrap();

    // Verify FTS5 picks up updated context_string
    let results = fts5_search(db.conn(), "bar baz", 5).unwrap();
    assert_eq!(results.len(), 1);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib storage::queries::tests`
Expected: FAIL — new types/functions not defined

- [ ] **Step 3: Implement NodeRecord, EdgeRecord structs and CRUD functions**

Add structs `NodeRecord`, `NodeResult`, `EdgeRecord` and functions:
- `insert_node(conn, &NodeRecord) -> Result<i64>`
- `get_nodes_by_name(conn, &str) -> Result<Vec<NodeResult>>`
- `insert_edge(conn, source_id, target_id, relation, metadata) -> Result<i64>`
- `get_edges_from(conn, node_id) -> Result<Vec<EdgeRecord>>`
- `fts5_search(conn, query, limit) -> Result<Vec<NodeResult>>`
- `update_context_string(conn, node_id, context_string) -> Result<()>`
- `delete_nodes_by_file(conn, file_id) -> Result<()>`

Key: `fts5_search` uses `SELECT n.* FROM nodes_fts f JOIN nodes n ON n.id = f.rowid WHERE nodes_fts MATCH ?1 ORDER BY rank LIMIT ?2`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib storage::queries::tests`
Expected: All PASS

- [ ] **Step 5: Commit**

```bash
git add src/storage/queries.rs
git commit -m "feat(storage): add node/edge CRUD and FTS5 search"
```

---

### Task 5: Merkle Tree Change Detection

**Files:**
- Create: `src/indexer/mod.rs`
- Create: `src/indexer/merkle.rs`

- [ ] **Step 1: Write failing tests for Merkle diff**

In `src/indexer/merkle.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::fs;

    #[test]
    fn test_hash_file() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("test.txt");
        fs::write(&file, "hello world").unwrap();
        let hash = hash_file(&file).unwrap();
        assert!(!hash.is_empty());
        assert_eq!(hash.len(), 64); // blake3 hex = 64 chars
    }

    #[test]
    fn test_diff_detects_new_files() {
        let old: HashMap<String, String> = HashMap::new();
        let mut current = HashMap::new();
        current.insert("a.rs".into(), "hash1".into());

        let diff = compute_diff(&old, &current);
        assert_eq!(diff.new_files.len(), 1);
        assert_eq!(diff.changed_files.len(), 0);
        assert_eq!(diff.deleted_files.len(), 0);
    }

    #[test]
    fn test_diff_detects_changed_files() {
        let mut old = HashMap::new();
        old.insert("a.rs".into(), "hash1".into());

        let mut current = HashMap::new();
        current.insert("a.rs".into(), "hash2".into());

        let diff = compute_diff(&old, &current);
        assert_eq!(diff.new_files.len(), 0);
        assert_eq!(diff.changed_files.len(), 1);
        assert_eq!(diff.deleted_files.len(), 0);
    }

    #[test]
    fn test_diff_detects_deleted_files() {
        let mut old = HashMap::new();
        old.insert("a.rs".into(), "hash1".into());
        let current: HashMap<String, String> = HashMap::new();

        let diff = compute_diff(&old, &current);
        assert_eq!(diff.deleted_files.len(), 1);
    }

    #[test]
    fn test_scan_directory_respects_gitignore() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(".gitignore"), "node_modules/\n*.log").unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("src/main.rs"), "fn main(){}").unwrap();
        fs::create_dir_all(tmp.path().join("node_modules")).unwrap();
        fs::write(tmp.path().join("node_modules/pkg.js"), "x").unwrap();
        fs::write(tmp.path().join("debug.log"), "log").unwrap();

        let hashes = scan_directory(tmp.path()).unwrap();
        assert!(hashes.contains_key("src/main.rs"));
        assert!(!hashes.contains_key("node_modules/pkg.js"));
        assert!(!hashes.contains_key("debug.log"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib indexer::merkle::tests`
Expected: FAIL

- [ ] **Step 3: Implement Merkle module**

Key types and functions:
```rust
use std::collections::HashMap;
use std::path::Path;
use anyhow::Result;

pub struct DiffResult {
    pub new_files: Vec<String>,
    pub changed_files: Vec<String>,
    pub deleted_files: Vec<String>,
}

pub fn hash_file(path: &Path) -> Result<String> {
    let content = std::fs::read(path)?;
    Ok(blake3::hash(&content).to_hex().to_string())
}

pub fn scan_directory(root: &Path) -> Result<HashMap<String, String>> {
    // Use ignore::WalkBuilder to respect .gitignore
    // For each file, compute blake3 hash
    // Return map of relative_path -> hash
}

pub fn compute_diff(
    old: &HashMap<String, String>,
    current: &HashMap<String, String>,
) -> DiffResult {
    // Compare old vs current hash maps
    // new = in current but not in old
    // changed = in both but different hash
    // deleted = in old but not in current
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib indexer::merkle::tests`
Expected: All PASS

- [ ] **Step 5: Update lib.rs module declarations**

Add `pub mod indexer;` to `src/lib.rs`.

- [ ] **Step 6: Commit**

```bash
git add src/indexer/ src/lib.rs
git commit -m "feat(indexer): add Merkle tree change detection with gitignore support"
```

---

### Task 6: Utility Module — Language Detection + Config

**Files:**
- Create: `src/utils/config.rs`
- Create: `src/utils/gitignore.rs`
- Modify: `src/utils/mod.rs`

- [ ] **Step 1: Write failing tests**

In `src/utils/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_language_from_extension() {
        assert_eq!(detect_language("src/main.rs"), Some("rust"));
        assert_eq!(detect_language("app.ts"), Some("typescript"));
        assert_eq!(detect_language("app.tsx"), Some("typescript"));
        assert_eq!(detect_language("index.js"), Some("javascript"));
        assert_eq!(detect_language("main.go"), Some("go"));
        assert_eq!(detect_language("app.py"), Some("python"));
        assert_eq!(detect_language("Main.java"), Some("java"));
        assert_eq!(detect_language("main.c"), Some("c"));
        assert_eq!(detect_language("main.cpp"), Some("cpp"));
        assert_eq!(detect_language("index.html"), Some("html"));
        assert_eq!(detect_language("style.css"), Some("css"));
        assert_eq!(detect_language("image.png"), None);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib utils::config::tests`
Expected: FAIL

- [ ] **Step 3: Implement**

```rust
pub fn detect_language(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next()?;
    match ext {
        "rs" => Some("rust"),
        "ts" | "tsx" => Some("typescript"),
        "js" | "jsx" | "mjs" | "cjs" => Some("javascript"),
        "go" => Some("go"),
        "py" | "pyi" => Some("python"),
        "java" => Some("java"),
        "c" | "h" => Some("c"),
        "cpp" | "cc" | "cxx" | "hpp" => Some("cpp"),
        "html" | "htm" => Some("html"),
        "css" => Some("css"),
        _ => None,
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib utils::config::tests`
Expected: All PASS

- [ ] **Step 5: Commit**

```bash
git add src/utils/
git commit -m "feat(utils): add language detection from file extension"
```

---

## Chunk 2: MCP Protocol Layer

### Task 7: JSON-RPC 2.0 Protocol Types

**Files:**
- Create: `src/mcp/mod.rs`
- Create: `src/mcp/protocol.rs`
- Create: `src/mcp/types.rs`

- [ ] **Step 1: Write failing tests for JSON-RPC parsing**

In `src/mcp/protocol.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_initialize_request() {
        let json = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{}}}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.method, "initialize");
        assert_eq!(req.id, Some(serde_json::Value::Number(1.into())));
    }

    #[test]
    fn test_parse_tool_call_request() {
        let json = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_index_status","arguments":{}}}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.method, "tools/call");
    }

    #[test]
    fn test_serialize_response() {
        let resp = JsonRpcResponse::success(
            Some(serde_json::Value::Number(1.into())),
            serde_json::json!({"status": "ok"}),
        );
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"result\""));
    }

    #[test]
    fn test_serialize_error_response() {
        let resp = JsonRpcResponse::error(
            Some(serde_json::Value::Number(1.into())),
            -32601,
            "Method not found".into(),
        );
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"error\""));
        assert!(json.contains("-32601"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib mcp::protocol::tests`
Expected: FAIL

- [ ] **Step 3: Implement JSON-RPC types**

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<serde_json::Value>,
    pub method: String,
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
}

impl JsonRpcResponse {
    pub fn success(id: Option<serde_json::Value>, result: serde_json::Value) -> Self { ... }
    pub fn error(id: Option<serde_json::Value>, code: i32, message: String) -> Self { ... }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib mcp::protocol::tests`
Expected: All PASS

- [ ] **Step 5: Commit**

```bash
git add src/mcp/
git commit -m "feat(mcp): add JSON-RPC 2.0 request/response types"
```

---

### Task 8: MCP Tool Registration Framework

**Files:**
- Create: `src/mcp/tools.rs`
- Modify: `src/mcp/types.rs`

- [ ] **Step 1: Write failing test for tool registration**

In `src/mcp/tools.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_registry_lists_all_tools() {
        let registry = ToolRegistry::new();
        let tools = registry.list_tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"semantic_code_search"));
        assert!(names.contains(&"get_call_graph"));
        assert!(names.contains(&"find_http_route"));
        assert!(names.contains(&"get_ast_node"));
        assert!(names.contains(&"read_snippet"));
        assert!(names.contains(&"start_watch"));
        assert!(names.contains(&"stop_watch"));
        assert!(names.contains(&"get_index_status"));
        assert!(names.contains(&"rebuild_index"));
        assert_eq!(tools.len(), 9);
    }

    #[test]
    fn test_tool_schema_has_description() {
        let registry = ToolRegistry::new();
        let tools = registry.list_tools();
        for tool in &tools {
            assert!(!tool.description.is_empty(), "Tool {} has no description", tool.name);
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib mcp::tools::tests`
Expected: FAIL

- [ ] **Step 3: Implement ToolRegistry**

Define `ToolDefinition` struct with `name`, `description`, `input_schema` (JSON Schema as `serde_json::Value`). `ToolRegistry::new()` registers all 9 tools with their schemas from the spec. Tool handlers will be connected in later chunks.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib mcp::tools::tests`
Expected: All PASS

- [ ] **Step 5: Commit**

```bash
git add src/mcp/tools.rs src/mcp/types.rs
git commit -m "feat(mcp): add tool registry with 9 tool definitions and JSON schemas"
```

---

### Task 9: MCP Server — stdio Transport + Initialize/ListTools Handlers

**Files:**
- Create: `src/mcp/server.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Write failing test for MCP initialize handshake**

In `src/mcp/server.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handle_initialize() {
        let server = McpServer::new_test();
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"claude-code","version":"1.0"}}}"#;
        let resp = server.handle_message(req).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert!(parsed["result"]["capabilities"]["tools"].is_object());
        assert_eq!(parsed["result"]["protocolVersion"], "2024-11-05");
    }

    #[test]
    fn test_handle_tools_list() {
        let server = McpServer::new_test();
        let req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
        let resp = server.handle_message(req).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        let tools = parsed["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 9);
    }

    #[test]
    fn test_handle_unknown_method() {
        let server = McpServer::new_test();
        let req = r#"{"jsonrpc":"2.0","id":3,"method":"unknown/method","params":{}}"#;
        let resp = server.handle_message(req).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert!(parsed["error"].is_object());
        assert_eq!(parsed["error"]["code"], -32601);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib mcp::server::tests`
Expected: FAIL

- [ ] **Step 3: Implement McpServer**

`McpServer` struct holds `ToolRegistry`, `Database` (via `Arc`), and state. `handle_message(&self, line: &str) -> Result<String>` parses JSON-RPC, dispatches to handlers:
- `initialize` → return capabilities + protocol version
- `tools/list` → return `registry.list_tools()` as JSON
- `tools/call` → dispatch to tool handler (stub for now — returns "not implemented")
- `notifications/initialized` → no-op notification, return None
- unknown → return JSON-RPC error -32601

`main.rs`: Read lines from stdin, call `handle_message`, write to stdout. Using `tokio::io::BufReader` on stdin.

`McpServer::new_test()` creates an instance with an in-memory database for testing.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib mcp::server::tests`
Expected: All PASS

- [ ] **Step 5: Test the binary manually**

```bash
cargo build
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}' | cargo run 2>/dev/null
```

Expected: JSON response with capabilities

- [ ] **Step 6: Commit**

```bash
git add src/mcp/server.rs src/main.rs src/lib.rs
git commit -m "feat(mcp): add MCP server with stdio transport, initialize, and tools/list"
```

---

### Task 10: Implement get_index_status Tool Handler

**Files:**
- Modify: `src/mcp/server.rs`
- Modify: `src/storage/queries.rs`

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn test_get_index_status_tool() {
    let server = McpServer::new_test();
    // Insert some test data
    {
        let db = server.db();
        upsert_file(db.conn(), &FileRecord {
            path: "a.rs".into(), blake3_hash: "h".into(),
            last_modified: 1, language: Some("rust".into()),
        }).unwrap();
    }

    let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_index_status","arguments":{}}}"#;
    let resp = server.handle_message(req).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    let content = &parsed["result"]["content"][0]["text"];
    let status: serde_json::Value = serde_json::from_str(content.as_str().unwrap()).unwrap();
    assert_eq!(status["files_count"], 1);
    assert_eq!(status["schema_version"], 1);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib mcp::server::tests::test_get_index_status`
Expected: FAIL

- [ ] **Step 3: Implement**

Add `get_index_status()` query in `storage/queries.rs`:
```rust
pub fn get_index_status(conn: &Connection) -> Result<IndexStatus> {
    // SELECT COUNT(*) from files, nodes, edges
    // PRAGMA user_version
    // Check db file size
}
```

Wire the tool handler in `McpServer::handle_tool_call()`.

MCP tool results use the format:
```json
{"content": [{"type": "text", "text": "<json string>"}]}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib mcp::server::tests::test_get_index_status`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/mcp/server.rs src/storage/queries.rs
git commit -m "feat(mcp): implement get_index_status tool handler"
```

---

## Chunk 3: Tree-sitter Parser

### Task 11: Tree-sitter Setup + TypeScript/JavaScript Parsing

**Files:**
- Create: `src/parser/mod.rs`
- Create: `src/parser/treesitter.rs`
- Create: `src/parser/languages.rs`
- Modify: `Cargo.toml` — add tree-sitter dependencies

- [ ] **Step 1: Add tree-sitter dependencies to Cargo.toml**

Add to `[dependencies]`:
```toml
tree-sitter = "0.24"
tree-sitter-typescript = "0.23"
tree-sitter-javascript = "0.23"
```

Start with TS/JS only. Add other languages incrementally.

Run: `cargo build` to verify deps resolve.

- [ ] **Step 2: Write failing test for parsing TypeScript functions**

In `src/parser/treesitter.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_typescript_functions() {
        let code = r#"
function handleLogin(req: Request, res: Response): void {
    validateToken(req.token);
    res.send(200);
}

const processPayment = async (amount: number): Promise<void> => {
    await chargeCard(amount);
};

class UserService {
    async findUser(id: string): Promise<User> {
        return db.query(id);
    }
}
"#;
        let nodes = parse_code(code, "typescript").unwrap();
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"handleLogin"));
        assert!(names.contains(&"processPayment"));
        assert!(names.contains(&"UserService"));
        assert!(names.contains(&"findUser"));
    }

    #[test]
    fn test_parse_extracts_signatures() {
        let code = "function add(a: number, b: number): number { return a + b; }";
        let nodes = parse_code(code, "typescript").unwrap();
        assert_eq!(nodes.len(), 1);
        assert!(nodes[0].signature.is_some());
    }

    #[test]
    fn test_parse_extracts_line_numbers() {
        let code = "// line 1\nfunction foo() {\n  return 1;\n}\n";
        let nodes = parse_code(code, "typescript").unwrap();
        assert_eq!(nodes[0].start_line, 2); // 1-indexed
        assert_eq!(nodes[0].end_line, 4);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --lib parser::treesitter::tests`
Expected: FAIL

- [ ] **Step 4: Implement parser**

In `src/parser/languages.rs`:
```rust
use tree_sitter::Language;

pub fn get_language(name: &str) -> Option<Language> {
    match name {
        "typescript" | "tsx" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        "javascript" | "jsx" => Some(tree_sitter_javascript::LANGUAGE.into()),
        _ => None,
    }
}
```

In `src/parser/treesitter.rs`:
```rust
pub struct ParsedNode {
    pub node_type: String,    // "function" | "class" | "method" | ...
    pub name: String,
    pub qualified_name: Option<String>,
    pub start_line: u32,
    pub end_line: u32,
    pub code_content: String,
    pub signature: Option<String>,
    pub doc_comment: Option<String>,
}

pub fn parse_code(source: &str, language: &str) -> Result<Vec<ParsedNode>> {
    let lang = get_language(language).ok_or_else(|| anyhow!("unsupported language: {}", language))?;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang)?;
    let tree = parser.parse(source, None).ok_or_else(|| anyhow!("parse failed"))?;
    let root = tree.root_node();

    let mut nodes = Vec::new();
    extract_nodes(root, source, &mut nodes);
    Ok(nodes)
}

fn extract_nodes(node: tree_sitter::Node, source: &str, results: &mut Vec<ParsedNode>) {
    // Walk AST tree, match node kinds:
    // "function_declaration" | "arrow_function" (named) | "class_declaration"
    // | "method_definition" | "interface_declaration"
    // Extract name from child nodes, signature from parameters + return type
    // Recurse into children for nested definitions
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib parser::treesitter::tests`
Expected: All PASS

- [ ] **Step 6: Commit**

```bash
git add src/parser/ Cargo.toml Cargo.lock
git commit -m "feat(parser): add Tree-sitter TypeScript/JavaScript parser"
```

---

### Task 12: Relation Extraction (Calls, Imports, Inherits)

**Files:**
- Create: `src/parser/relations.rs`

- [ ] **Step 1: Write failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_call_relations() {
        let code = r#"
function handleLogin(req) {
    const user = validateToken(req.token);
    sendResponse(req, user);
}
"#;
        let relations = extract_relations(code, "typescript").unwrap();
        let calls: Vec<&str> = relations.iter()
            .filter(|r| r.relation == "calls")
            .map(|r| r.target_name.as_str())
            .collect();
        assert!(calls.contains(&"validateToken"));
        assert!(calls.contains(&"sendResponse"));
    }

    #[test]
    fn test_extract_import_relations() {
        let code = r#"
import { UserService } from './services/user';
import jwt from 'jsonwebtoken';
"#;
        let relations = extract_relations(code, "typescript").unwrap();
        let imports: Vec<&str> = relations.iter()
            .filter(|r| r.relation == "imports")
            .map(|r| r.target_name.as_str())
            .collect();
        assert!(imports.contains(&"UserService"));
    }

    #[test]
    fn test_extract_inherits_relations() {
        let code = r#"
class AdminService extends UserService {
    getPermissions() { return []; }
}
"#;
        let relations = extract_relations(code, "typescript").unwrap();
        let inherits: Vec<&str> = relations.iter()
            .filter(|r| r.relation == "inherits")
            .map(|r| r.target_name.as_str())
            .collect();
        assert!(inherits.contains(&"UserService"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib parser::relations::tests`
Expected: FAIL

- [ ] **Step 3: Implement relation extraction**

```rust
pub struct ParsedRelation {
    pub source_name: String,   // function/class that contains the relation
    pub target_name: String,   // called/imported/inherited symbol
    pub relation: String,      // "calls" | "imports" | "inherits" | "routes_to"
    pub metadata: Option<String>,
}

pub fn extract_relations(source: &str, language: &str) -> Result<Vec<ParsedRelation>> {
    // Parse with tree-sitter
    // Walk AST, within each function/method body:
    //   - call_expression → extract callee name → "calls" relation
    // At module level:
    //   - import_statement → extract imported names → "imports" relation
    // In class declarations:
    //   - extends_clause → "inherits" relation
    //   - implements_clause → "implements" relation
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib parser::relations::tests`
Expected: All PASS

- [ ] **Step 5: Commit**

```bash
git add src/parser/relations.rs src/parser/mod.rs
git commit -m "feat(parser): add relation extraction for calls, imports, inherits"
```

---

### Task 13: Add Remaining Language Grammars

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/parser/languages.rs`
- Add test fixtures

- [ ] **Step 1: Add remaining tree-sitter grammar dependencies**

```toml
tree-sitter-go = "0.23"
tree-sitter-python = "0.23"
tree-sitter-rust = "0.23"
tree-sitter-java = "0.23"
tree-sitter-c = "0.23"
tree-sitter-cpp = "0.23"
tree-sitter-html = "0.23"
tree-sitter-css = "0.23"
```

- [ ] **Step 2: Write failing tests for each language**

One test per language verifying basic function/class extraction:

```rust
#[test]
fn test_parse_go_functions() {
    let code = "package main\nfunc handleRequest(w http.ResponseWriter, r *http.Request) {\n}\n";
    let nodes = parse_code(code, "go").unwrap();
    assert!(nodes.iter().any(|n| n.name == "handleRequest"));
}

#[test]
fn test_parse_python_functions() {
    let code = "def process_data(items: list) -> dict:\n    return {}\n\nclass DataProcessor:\n    def run(self):\n        pass\n";
    let nodes = parse_code(code, "python").unwrap();
    assert!(nodes.iter().any(|n| n.name == "process_data"));
    assert!(nodes.iter().any(|n| n.name == "DataProcessor"));
}

#[test]
fn test_parse_rust_functions() {
    let code = "pub fn calculate(x: i32, y: i32) -> i32 { x + y }\nstruct Config { name: String }\n";
    let nodes = parse_code(code, "rust").unwrap();
    assert!(nodes.iter().any(|n| n.name == "calculate"));
    assert!(nodes.iter().any(|n| n.name == "Config"));
}

#[test]
fn test_parse_java_methods() {
    let code = "public class UserController {\n    public void getUser(String id) {}\n}\n";
    let nodes = parse_code(code, "java").unwrap();
    assert!(nodes.iter().any(|n| n.name == "UserController"));
}

#[test]
fn test_parse_c_functions() {
    let code = "int main(int argc, char *argv[]) { return 0; }\n";
    let nodes = parse_code(code, "c").unwrap();
    assert!(nodes.iter().any(|n| n.name == "main"));
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --lib parser::treesitter::tests`
Expected: FAIL — languages not registered

- [ ] **Step 4: Extend languages.rs with all grammar mappings**

```rust
pub fn get_language(name: &str) -> Option<Language> {
    match name {
        "typescript" | "tsx" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        "javascript" | "jsx" => Some(tree_sitter_javascript::LANGUAGE.into()),
        "go" => Some(tree_sitter_go::LANGUAGE.into()),
        "python" => Some(tree_sitter_python::LANGUAGE.into()),
        "rust" => Some(tree_sitter_rust::LANGUAGE.into()),
        "java" => Some(tree_sitter_java::LANGUAGE.into()),
        "c" => Some(tree_sitter_c::LANGUAGE.into()),
        "cpp" => Some(tree_sitter_cpp::LANGUAGE.into()),
        "html" => Some(tree_sitter_html::LANGUAGE.into()),
        "css" => Some(tree_sitter_css::LANGUAGE.into()),
        _ => None,
    }
}
```

Extend `extract_nodes` in `treesitter.rs` with language-specific node kind mappings for Go/Python/Rust/Java/C/C++.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib parser`
Expected: All PASS

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/parser/
git commit -m "feat(parser): add Tree-sitter grammars for Go, Python, Rust, Java, C/C++, HTML, CSS"
```

---

## Chunk 4: Indexing Pipeline

### Task 14: Three-Phase Indexing Pipeline

**Files:**
- Create: `src/indexer/pipeline.rs`
- Modify: `src/indexer/mod.rs`

- [ ] **Step 1: Write failing integration test**

In `src/indexer/pipeline.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::fs;

    #[test]
    fn test_full_index_pipeline() {
        let project_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();

        // Create test files
        fs::create_dir_all(project_dir.path().join("src")).unwrap();
        fs::write(project_dir.path().join("src/auth.ts"), r#"
function validateToken(token: string): boolean {
    return jwt.verify(token);
}

function handleLogin(req: Request) {
    if (validateToken(req.token)) {
        return createSession(req.userId);
    }
}
"#).unwrap();

        let db = Database::open(&db_dir.path().join("index.db")).unwrap();
        let result = run_full_index(&db, project_dir.path()).unwrap();

        assert!(result.files_indexed > 0);
        assert!(result.nodes_created > 0);
        assert!(result.edges_created > 0);

        // Verify nodes are in DB
        let nodes = get_nodes_by_name(db.conn(), "handleLogin").unwrap();
        assert_eq!(nodes.len(), 1);

        // Verify edges: handleLogin → calls → validateToken
        let edges = get_edges_from(db.conn(), nodes[0].id).unwrap();
        let call_targets: Vec<&str> = edges.iter()
            .filter(|e| e.relation == "calls")
            .map(|e| e.target_name.as_str())
            .collect();
        assert!(call_targets.contains(&"validateToken"));
    }

    #[test]
    fn test_incremental_index() {
        let project_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        let db = Database::open(&db_dir.path().join("index.db")).unwrap();

        // Initial index
        fs::write(project_dir.path().join("a.ts"), "function foo() {}").unwrap();
        run_full_index(&db, project_dir.path()).unwrap();

        // Modify file
        fs::write(project_dir.path().join("a.ts"), "function bar() {}").unwrap();

        // Incremental index
        let result = run_incremental_index(&db, project_dir.path()).unwrap();
        assert_eq!(result.files_indexed, 1); // only the changed file

        // Old node gone, new node present
        let foo = get_nodes_by_name(db.conn(), "foo").unwrap();
        assert_eq!(foo.len(), 0);
        let bar = get_nodes_by_name(db.conn(), "bar").unwrap();
        assert_eq!(bar.len(), 1);
    }

    #[test]
    fn test_deleted_file_cleanup() {
        let project_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        let db = Database::open(&db_dir.path().join("index.db")).unwrap();

        fs::write(project_dir.path().join("a.ts"), "function foo() {}").unwrap();
        run_full_index(&db, project_dir.path()).unwrap();

        // Delete file
        fs::remove_file(project_dir.path().join("a.ts")).unwrap();
        run_incremental_index(&db, project_dir.path()).unwrap();

        let foo = get_nodes_by_name(db.conn(), "foo").unwrap();
        assert_eq!(foo.len(), 0);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib indexer::pipeline::tests`
Expected: FAIL

- [ ] **Step 3: Implement pipeline**

```rust
pub struct IndexResult {
    pub files_indexed: usize,
    pub nodes_created: usize,
    pub edges_created: usize,
}

pub fn run_full_index(db: &Database, project_root: &Path) -> Result<IndexResult> {
    let current_hashes = scan_directory(project_root)?;
    index_files(db, project_root, &current_hashes.keys().cloned().collect::<Vec<_>>(), &current_hashes)
}

pub fn run_incremental_index(db: &Database, project_root: &Path) -> Result<IndexResult> {
    let stored_hashes = get_all_file_hashes(db.conn())?;
    let current_hashes = scan_directory(project_root)?;
    let diff = compute_diff(&stored_hashes, &current_hashes);

    // Phase 0: Clean up deleted files (CASCADE handles nodes + edges)
    delete_files_by_paths(db.conn(), &diff.deleted_files)?;

    // Index changed + new files
    let to_index: Vec<String> = [diff.new_files, diff.changed_files].concat();
    index_files(db, project_root, &to_index, &current_hashes)
}

fn index_files(db: &Database, root: &Path, files: &[String], hashes: &HashMap<String, String>) -> Result<IndexResult> {
    // Phase 1: Parse all files, insert nodes with context_string = NULL
    // Phase 2: Extract relations, resolve cross-file references, insert edges
    // Phase 3: Build context_strings from edges, update nodes
    //          (Embeddings deferred to Chunk 5 — for now, context_string only)
    // Update merkle_state and files table
}
```

Note: Embedding generation (Phase 3 part 2) is deferred to Chunk 5. This task builds the pipeline with Phases 1-3 excluding vector embeddings.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib indexer::pipeline::tests`
Expected: All PASS

- [ ] **Step 5: Commit**

```bash
git add src/indexer/pipeline.rs src/indexer/mod.rs
git commit -m "feat(indexer): implement 3-phase indexing pipeline with incremental support"
```

---

### Task 15: Graph Context String Builder

**Files:**
- Create: `src/embedding/mod.rs`
- Create: `src/embedding/context.rs`

- [ ] **Step 1: Write failing test**

In `src/embedding/context.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_context_string() {
        let info = NodeContext {
            node_type: "function".into(),
            name: "validateToken".into(),
            file_path: "src/auth/middleware.ts".into(),
            signature: Some("(token: string) -> Promise<User | null>".into()),
            routes: vec!["POST /api/login".into(), "GET /api/profile".into()],
            callees: vec!["jwt.verify".into(), "UserRepo.findById".into()],
            callers: vec!["authMiddleware".into(), "handleLogin".into()],
            inherits: vec![],
            doc_comment: Some("Validates JWT token and returns the associated user".into()),
        };

        let ctx = build_context_string(&info);
        assert!(ctx.contains("function validateToken"));
        assert!(ctx.contains("in src/auth/middleware.ts"));
        assert!(ctx.contains("calls: jwt.verify, UserRepo.findById"));
        assert!(ctx.contains("called_by: authMiddleware, handleLogin"));
        assert!(ctx.contains("routes: POST /api/login, GET /api/profile"));
    }

    #[test]
    fn test_build_context_string_minimal() {
        let info = NodeContext {
            node_type: "function".into(),
            name: "helper".into(),
            file_path: "utils.ts".into(),
            signature: None,
            routes: vec![],
            callees: vec![],
            callers: vec![],
            inherits: vec![],
            doc_comment: None,
        };

        let ctx = build_context_string(&info);
        assert!(ctx.contains("function helper"));
        assert!(ctx.contains("in utils.ts"));
        // Empty sections should be omitted
        assert!(!ctx.contains("calls:"));
        assert!(!ctx.contains("routes:"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib embedding::context::tests`
Expected: FAIL

- [ ] **Step 3: Implement**

```rust
pub struct NodeContext {
    pub node_type: String,
    pub name: String,
    pub file_path: String,
    pub signature: Option<String>,
    pub routes: Vec<String>,
    pub callees: Vec<String>,
    pub callers: Vec<String>,
    pub inherits: Vec<String>,
    pub doc_comment: Option<String>,
}

pub fn build_context_string(info: &NodeContext) -> String {
    let mut parts = vec![format!("{} {}", info.node_type, info.name)];
    parts.push(format!("in {}", info.file_path));
    if let Some(sig) = &info.signature {
        parts.push(format!("signature: {}", sig));
    }
    if !info.routes.is_empty() {
        parts.push(format!("routes: {}", info.routes.join(", ")));
    }
    if !info.callees.is_empty() {
        parts.push(format!("calls: {}", info.callees.join(", ")));
    }
    if !info.callers.is_empty() {
        parts.push(format!("called_by: {}", info.callers.join(", ")));
    }
    if !info.inherits.is_empty() {
        parts.push(format!("inherits: {}", info.inherits.join(", ")));
    }
    if let Some(doc) = &info.doc_comment {
        parts.push(format!("doc: {}", doc));
    }
    parts.join("\n")
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib embedding::context::tests`
Expected: All PASS

- [ ] **Step 5: Wire context builder into indexing pipeline Phase 3**

In `pipeline.rs`, after edges are inserted, query edges for each node and call `build_context_string()`, then `update_context_string()` in the DB.

- [ ] **Step 6: Commit**

```bash
git add src/embedding/ src/indexer/pipeline.rs src/lib.rs
git commit -m "feat(embedding): add graph context string builder with pipeline integration"
```

---

### Task 16: File Watcher (notify)

**Files:**
- Create: `src/indexer/watcher.rs`
- Modify: `Cargo.toml` — add `notify = "6"`

- [ ] **Step 1: Add notify dependency**

```toml
notify = "6"
```

- [ ] **Step 2: Write failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::{fs, time::Duration, thread};

    #[test]
    fn test_watcher_detects_file_changes() {
        let tmp = TempDir::new().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let watcher = FileWatcher::start(tmp.path(), tx).unwrap();

        // Create a file
        fs::write(tmp.path().join("test.ts"), "function foo() {}").unwrap();
        thread::sleep(Duration::from_millis(200));

        let events: Vec<WatchEvent> = rx.try_iter().collect();
        assert!(!events.is_empty());

        watcher.stop();
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --lib indexer::watcher::tests`
Expected: FAIL

- [ ] **Step 4: Implement FileWatcher**

```rust
use notify::{Watcher, RecursiveMode, Event};
use std::sync::mpsc;

pub enum WatchEvent {
    Changed(Vec<String>),
}

pub struct FileWatcher {
    _watcher: notify::RecommendedWatcher,
}

impl FileWatcher {
    pub fn start(root: &Path, tx: mpsc::Sender<WatchEvent>) -> Result<Self> {
        let mut watcher = notify::recommended_watcher(move |res: Result<Event, _>| {
            if let Ok(event) = res {
                let paths: Vec<String> = event.paths.iter()
                    .filter_map(|p| p.to_str().map(String::from))
                    .collect();
                if !paths.is_empty() {
                    let _ = tx.send(WatchEvent::Changed(paths));
                }
            }
        })?;
        watcher.watch(root, RecursiveMode::Recursive)?;
        Ok(Self { _watcher: watcher })
    }

    pub fn stop(self) {
        // Drop watcher to stop watching
        drop(self._watcher);
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib indexer::watcher::tests`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/indexer/watcher.rs Cargo.toml Cargo.lock
git commit -m "feat(indexer): add file system watcher using notify crate"
```

---

## Chunk 5: Embedding Engine + Vector Search

### Task 17: sqlite-vec Build Integration

**Files:**
- Create: `build.rs`
- Create: `vendor/sqlite-vec/` (download vec0.c)
- Modify: `Cargo.toml` — add `cc` to build-deps

- [ ] **Step 1: Download sqlite-vec source**

```bash
mkdir -p vendor/sqlite-vec
# Download vec0.c and sqlite-vec.h from github.com/asg017/sqlite-vec/releases
curl -L -o vendor/sqlite-vec/sqlite-vec.c https://github.com/asg017/sqlite-vec/releases/download/v0.1.6/sqlite-vec.c
curl -L -o vendor/sqlite-vec/sqlite-vec.h https://github.com/asg017/sqlite-vec/releases/download/v0.1.6/sqlite-vec.h
```

Note: Check latest stable release. Pin the version.

- [ ] **Step 2: Add build.rs**

```rust
fn main() {
    cc::Build::new()
        .file("vendor/sqlite-vec/sqlite-vec.c")
        .include("vendor/sqlite-vec")
        .define("SQLITE_CORE", None)
        .compile("sqlite_vec");
}
```

Add to Cargo.toml:
```toml
[build-dependencies]
cc = "1"
```

- [ ] **Step 3: Write failing test for vec0 virtual table**

```rust
#[test]
fn test_vec0_extension_loads() {
    let tmp = TempDir::new().unwrap();
    let db = Database::open_with_vec(&tmp.path().join("test.db")).unwrap();
    // Try creating a vec0 table
    db.conn().execute_batch(
        "CREATE VIRTUAL TABLE test_vec USING vec0(embedding float[4]);"
    ).unwrap();
    // Insert a vector — vec0 expects embedding as raw bytes
    let vec: Vec<f32> = vec![1.0, 0.0, 0.0, 0.0];
    let bytes: &[u8] = bytemuck::cast_slice(&vec);
    db.conn().execute(
        "INSERT INTO test_vec(rowid, embedding) VALUES (1, ?)",
        [bytes],
    ).unwrap();
}
```

- [ ] **Step 4: Run test to verify it fails**

Run: `cargo test --lib storage::db::tests::test_vec0`
Expected: FAIL

- [ ] **Step 5: Implement vec0 loading in Database**

In `db.rs`, add `open_with_vec()` using rusqlite's safe extension loading API:

```rust
pub fn open_with_vec(path: &Path) -> Result<Self> {
    let conn = Connection::open(path)?;

    // Enable extension loading (disabled by default for security)
    unsafe { conn.load_extension_enable()? };

    // Load the statically compiled sqlite-vec extension
    // The compiled .so/.dylib is linked by build.rs, but we still need
    // to call the init function. Use the path to our compiled extension.
    //
    // Alternative approach: Use rusqlite's bundled SQLite and register
    // sqlite-vec as a compile-time extension via build.rs by adding
    // sqlite3_vec_init to the auto-extension list in a custom SQLite build.
    //
    // For the MVP, the simplest working approach is:
    conn.load_extension(
        std::env::current_exe()?.parent().unwrap().join("libsqlite_vec"),
        Some("sqlite3_vec_init"),
    )?;

    // Disable extension loading after we're done
    unsafe { conn.load_extension_disable()? };

    // Set PRAGMAs and create schema (same as open())
    // ...
    Ok(Self { conn })
}
```

**IMPORTANT:** The exact extension loading path depends on how `build.rs` produces the artifact. The `cc` crate compiles a static library, but `load_extension` expects a shared library. Two viable approaches:

**Approach A (Recommended):** Compile sqlite-vec as a shared library in `build.rs` using `cc::Build::new().shared_flag(true)`, then `load_extension()` from the output path.

**Approach B:** Patch rusqlite's bundled SQLite to call `sqlite3_vec_init` during `sqlite3_open`. This requires modifying `build.rs` to add `-DSQLITE_EXTRA_INIT=sqlite3_vec_init` and linking the static lib. More complex but truly zero-runtime-dependency.

Test both approaches during implementation and pick whichever compiles cleanly.

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test --lib storage::db::tests::test_vec0`
Expected: PASS

- [ ] **Step 7: Add node_vectors virtual table to schema**

Add to `schema.rs` CREATE_TABLES (conditionally, only when vec0 is available):

```sql
CREATE VIRTUAL TABLE IF NOT EXISTS node_vectors USING vec0(
    node_id INTEGER PRIMARY KEY,
    embedding float[384]
);
```

Add the cleanup trigger:
```sql
CREATE TRIGGER IF NOT EXISTS nodes_vectors_ad AFTER DELETE ON nodes BEGIN
    DELETE FROM node_vectors WHERE node_id = old.id;
END;
```

- [ ] **Step 8: Commit**

```bash
git add build.rs vendor/ Cargo.toml Cargo.lock src/storage/
git commit -m "feat(storage): integrate sqlite-vec via static compilation for vector search"
```

---

### Task 18: Candle Model Loading + Inference

**Files:**
- Create: `src/embedding/model.rs`
- Create: `models/` directory for model files
- Modify: `Cargo.toml`

- [ ] **Step 1: Add candle dependencies**

```toml
candle-core = "0.8"
candle-nn = "0.8"
candle-transformers = "0.8"
tokenizers = "0.20"
```

- [ ] **Step 2: Download model files**

```bash
mkdir -p models
# Download MiniLM-L6-v2 safetensors + tokenizer
# From huggingface.co/sentence-transformers/all-MiniLM-L6-v2
# Files needed: model.safetensors (~22MB), tokenizer.json (~700KB)
```

Note: For development, load from file path. Switch to `include_bytes!` for release builds later.

- [ ] **Step 3: Write failing test for model loading and inference**

In `src/embedding/model.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embed_single_text() {
        let model = EmbeddingModel::load().unwrap();
        let embedding = model.embed("function validateToken authenticates JWT").unwrap();
        assert_eq!(embedding.len(), 384);
        // Verify it's normalized (L2 norm ≈ 1.0)
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_embed_batch() {
        let model = EmbeddingModel::load().unwrap();
        let texts = vec![
            "function foo handles user login",
            "class UserService manages database queries",
            "route POST /api/auth validates credentials",
        ];
        let embeddings = model.embed_batch(&texts).unwrap();
        assert_eq!(embeddings.len(), 3);
        for emb in &embeddings {
            assert_eq!(emb.len(), 384);
        }
    }

    #[test]
    fn test_similar_texts_have_high_cosine_similarity() {
        let model = EmbeddingModel::load().unwrap();
        let e1 = model.embed("user authentication login").unwrap();
        let e2 = model.embed("authenticate user credentials").unwrap();
        let e3 = model.embed("database migration schema").unwrap();

        let sim_related = cosine_similarity(&e1, &e2);
        let sim_unrelated = cosine_similarity(&e1, &e3);
        assert!(sim_related > sim_unrelated);
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
```

- [ ] **Step 4: Run tests to verify they fail**

Run: `cargo test --lib embedding::model::tests`
Expected: FAIL

- [ ] **Step 5: Implement EmbeddingModel**

```rust
use candle_core::{Device, Tensor};
use candle_nn::VarBuilder;
use tokenizers::Tokenizer;

pub struct EmbeddingModel {
    model: candle_transformers::models::bert::BertModel,
    tokenizer: Tokenizer,
    device: Device,
}

impl EmbeddingModel {
    pub fn load() -> Result<Self> {
        let device = Device::Cpu;
        // Load model weights from file (dev) or include_bytes! (release)
        let model_bytes = std::fs::read("models/model.safetensors")?;
        let tokenizer = Tokenizer::from_file("models/tokenizer.json")
            .map_err(|e| anyhow!("tokenizer load failed: {}", e))?;

        let vb = VarBuilder::from_buffered_safetensors(model_bytes, candle_core::DType::F32, &device)?;
        let config = /* BertConfig for MiniLM-L6-v2 */;
        let model = candle_transformers::models::bert::BertModel::load(vb, &config)?;

        Ok(Self { model, tokenizer, device })
    }

    pub fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let batch = self.embed_batch(&[text])?;
        Ok(batch.into_iter().next().unwrap())
    }

    pub fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        // Tokenize all texts
        // Run model forward pass
        // Mean pooling over token embeddings
        // L2 normalize
        // Return Vec<Vec<f32>>
    }
}
```

Note: The exact candle API may vary by version. Consult candle documentation. The key operations are: tokenize → encode → mean pool → normalize.

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --lib embedding::model::tests`
Expected: All PASS (requires model files in `models/` directory)

- [ ] **Step 7: Commit**

```bash
git add src/embedding/model.rs Cargo.toml Cargo.lock
git commit -m "feat(embedding): add candle-based MiniLM-L6-v2 embedding model"
```

---

### Task 19: Vector Search + RRF Fusion

**Files:**
- Create: `src/search/mod.rs`
- Create: `src/search/fts.rs`
- Create: `src/search/vector.rs`
- Create: `src/search/fusion.rs`

- [ ] **Step 1: Write failing test for RRF fusion**

In `src/search/fusion.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rrf_fusion_basic() {
        // FTS5 results: doc A (rank 1), doc B (rank 2), doc C (rank 3)
        let fts_results = vec![
            SearchResult { node_id: 1, score: 0.0 }, // rank 1
            SearchResult { node_id: 2, score: 0.0 }, // rank 2
            SearchResult { node_id: 3, score: 0.0 }, // rank 3
        ];
        // Vec0 results: doc B (rank 1), doc D (rank 2), doc A (rank 3)
        let vec_results = vec![
            SearchResult { node_id: 2, score: 0.0 }, // rank 1
            SearchResult { node_id: 4, score: 0.0 }, // rank 2
            SearchResult { node_id: 1, score: 0.0 }, // rank 3
        ];

        let fused = rrf_fusion(&fts_results, &vec_results, 60, 3);

        // Doc B should rank highest (rank 2 in FTS + rank 1 in vec)
        // Doc A should rank second (rank 1 in FTS + rank 3 in vec)
        assert_eq!(fused[0].node_id, 2); // B
        assert_eq!(fused[1].node_id, 1); // A
        assert_eq!(fused.len(), 3);
    }

    #[test]
    fn test_rrf_with_no_overlap() {
        let fts = vec![SearchResult { node_id: 1, score: 0.0 }];
        let vec = vec![SearchResult { node_id: 2, score: 0.0 }];

        let fused = rrf_fusion(&fts, &vec, 60, 5);
        assert_eq!(fused.len(), 2);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib search::fusion::tests`
Expected: FAIL

- [ ] **Step 3: Implement RRF fusion**

```rust
pub struct SearchResult {
    pub node_id: i64,
    pub score: f64,
}

pub fn rrf_fusion(
    fts_results: &[SearchResult],
    vec_results: &[SearchResult],
    k: i32,
    top_k: usize,
) -> Vec<SearchResult> {
    let mut scores: HashMap<i64, f64> = HashMap::new();

    for (rank, r) in fts_results.iter().enumerate() {
        *scores.entry(r.node_id).or_default() += 1.0 / (k as f64 + rank as f64 + 1.0);
    }
    for (rank, r) in vec_results.iter().enumerate() {
        *scores.entry(r.node_id).or_default() += 1.0 / (k as f64 + rank as f64 + 1.0);
    }

    let mut results: Vec<SearchResult> = scores.into_iter()
        .map(|(id, score)| SearchResult { node_id: id, score })
        .collect();
    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
    results.truncate(top_k);
    results
}
```

- [ ] **Step 4: Implement FTS5 search wrapper**

In `src/search/fts.rs`:
```rust
pub fn fts5_search(db: &Database, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
    // Use the existing fts5_search from storage/queries.rs
    // Convert to SearchResult format with rank as score
}
```

- [ ] **Step 5: Implement vector search wrapper**

In `src/search/vector.rs`:
```rust
pub fn vector_search(db: &Database, embedding: &[f32], limit: usize) -> Result<Vec<SearchResult>> {
    // Query node_vectors using vec0's MATCH syntax
    // SELECT node_id, distance FROM node_vectors WHERE embedding MATCH ? ORDER BY distance LIMIT ?
}
```

- [ ] **Step 6: Run all search tests**

Run: `cargo test --lib search`
Expected: All PASS

- [ ] **Step 7: Commit**

```bash
git add src/search/ src/lib.rs
git commit -m "feat(search): add FTS5, vector search, and RRF fusion"
```

---

### Task 20: Wire Embeddings into Indexing Pipeline

**Files:**
- Modify: `src/indexer/pipeline.rs`

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn test_pipeline_generates_embeddings() {
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open_with_vec(&db_dir.path().join("index.db")).unwrap();
    let model = EmbeddingModel::load().unwrap();

    fs::write(project_dir.path().join("a.ts"), "function foo() { bar(); }").unwrap();
    run_full_index_with_embeddings(&db, project_dir.path(), &model).unwrap();

    // Verify vectors exist
    let count: i64 = db.conn()
        .query_row("SELECT COUNT(*) FROM node_vectors", [], |r| r.get(0)).unwrap();
    assert!(count > 0);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib indexer::pipeline::tests::test_pipeline_generates_embeddings`
Expected: FAIL

- [ ] **Step 3: Add embedding generation to Phase 3 of pipeline**

After building context_strings and updating nodes, generate embeddings in batches of 32:

```rust
// Phase 3 addition: Generate embeddings
let nodes_needing_embeddings = get_nodes_with_context(db.conn())?;
for batch in nodes_needing_embeddings.chunks(32) {
    let texts: Vec<&str> = batch.iter().map(|n| n.context_string.as_str()).collect();
    let embeddings = model.embed_batch(&texts)?;
    for (node, embedding) in batch.iter().zip(embeddings) {
        insert_node_vector(db.conn(), node.id, &embedding)?;
    }
}
```

Add `insert_node_vector()` to `storage/queries.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib indexer::pipeline::tests::test_pipeline_generates_embeddings`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/indexer/pipeline.rs src/storage/queries.rs
git commit -m "feat(indexer): integrate embedding generation into Phase 3 of pipeline"
```

---

## Chunk 6: Knowledge Graph + Sandbox + MCP Tool Handlers + Integration

### Task 21: Recursive CTE Call Graph Queries

**Files:**
- Create: `src/graph/mod.rs`
- Create: `src/graph/query.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn setup_graph(db: &Database) {
        // Create: A → calls → B → calls → C, D → calls → B (diamond)
        let fid = upsert_file(db.conn(), &FileRecord {
            path: "t.ts".into(), blake3_hash: "h".into(), last_modified: 1, language: None,
        }).unwrap();
        let a = insert_node(db.conn(), &node("A", fid)).unwrap();
        let b = insert_node(db.conn(), &node("B", fid)).unwrap();
        let c = insert_node(db.conn(), &node("C", fid)).unwrap();
        let d = insert_node(db.conn(), &node("D", fid)).unwrap();
        insert_edge(db.conn(), a, b, "calls", None).unwrap();
        insert_edge(db.conn(), b, c, "calls", None).unwrap();
        insert_edge(db.conn(), d, b, "calls", None).unwrap();
    }

    #[test]
    fn test_get_callees() {
        let (db, _tmp) = test_db();
        setup_graph(&db);
        let result = get_call_graph(db.conn(), "A", "callees", 2, None).unwrap();
        let names: Vec<&str> = result.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"B"));
        assert!(names.contains(&"C"));
    }

    #[test]
    fn test_get_callers() {
        let (db, _tmp) = test_db();
        setup_graph(&db);
        let result = get_call_graph(db.conn(), "B", "callers", 2, None).unwrap();
        let names: Vec<&str> = result.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"A"));
        assert!(names.contains(&"D"));
    }

    #[test]
    fn test_cycle_detection() {
        let (db, _tmp) = test_db();
        let fid = upsert_file(db.conn(), &FileRecord {
            path: "t.ts".into(), blake3_hash: "h".into(), last_modified: 1, language: None,
        }).unwrap();
        let a = insert_node(db.conn(), &node("A", fid)).unwrap();
        let b = insert_node(db.conn(), &node("B", fid)).unwrap();
        // Mutual recursion: A → B → A
        insert_edge(db.conn(), a, b, "calls", None).unwrap();
        insert_edge(db.conn(), b, a, "calls", None).unwrap();

        // Should not infinite loop, should return finite results
        let result = get_call_graph(db.conn(), "A", "callees", 10, None).unwrap();
        assert!(result.len() <= 3); // A, B, and that's it (cycle detected)
    }

    #[test]
    fn test_both_direction() {
        let (db, _tmp) = test_db();
        setup_graph(&db);
        let result = get_call_graph(db.conn(), "B", "both", 2, None).unwrap();
        let names: Vec<&str> = result.iter().map(|r| r.name.as_str()).collect();
        // Callers: A, D; Callees: C
        assert!(names.contains(&"A"));
        assert!(names.contains(&"D"));
        assert!(names.contains(&"C"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib graph::query::tests`
Expected: FAIL

- [ ] **Step 3: Implement call graph queries**

Use the CTE SQL from the design spec (with visited-path cycle detection). Implement:
```rust
pub fn get_call_graph(
    conn: &Connection,
    function_name: &str,
    direction: &str,  // "callers" | "callees" | "both"
    depth: i32,
    file_path: Option<&str>,
) -> Result<Vec<CallGraphNode>> {
    match direction {
        "callees" => query_callees(conn, function_name, depth, file_path),
        "callers" => query_callers(conn, function_name, depth, file_path),
        "both" => {
            let mut results = query_callees(conn, function_name, depth, file_path)?;
            let callers = query_callers(conn, function_name, depth, file_path)?;
            // Merge and deduplicate
            for c in callers {
                if !results.iter().any(|r| r.node_id == c.node_id) {
                    results.push(c);
                }
            }
            Ok(results)
        }
        _ => Err(anyhow!("invalid direction: {}", direction)),
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib graph::query::tests`
Expected: All PASS

- [ ] **Step 5: Commit**

```bash
git add src/graph/ src/lib.rs
git commit -m "feat(graph): add recursive CTE call graph queries with cycle detection"
```

---

### Task 22: Context Sandbox (Compressor + read_snippet)

**Files:**
- Create: `src/sandbox/mod.rs`
- Create: `src/sandbox/compressor.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_compress_small_results() {
        let results = vec![NodeResult { code_content: "short".into(), ..default_node() }];
        assert!(!should_compress(&results, 2000));
    }

    #[test]
    fn test_should_compress_large_results() {
        let results = vec![NodeResult {
            code_content: "x".repeat(3000),
            ..default_node()
        }];
        assert!(should_compress(&results, 2000));
    }

    #[test]
    fn test_compress_returns_summaries_with_node_ids() {
        let results = vec![
            NodeResult { id: 1, name: "foo".into(), signature: Some("() -> i32".into()), code_content: "x".repeat(500), ..default_node() },
            NodeResult { id: 2, name: "bar".into(), signature: Some("(x: str) -> bool".into()), code_content: "y".repeat(500), ..default_node() },
        ];
        let compressed = compress_results(&results);
        assert_eq!(compressed.len(), 2);
        assert_eq!(compressed[0].node_id, 1);
        assert!(compressed[0].summary.contains("foo"));
        assert!(compressed[0].summary.contains("() -> i32"));
    }

    #[test]
    fn test_sandbox_cleanup_expired() {
        let (db, _tmp) = test_db();
        // Insert an expired entry
        db.conn().execute(
            "INSERT INTO context_sandbox (query_hash, summary, pointers, created_at, expires_at) VALUES ('h', 's', '[]', 0, 0)",
            [],
        ).unwrap();
        cleanup_expired_sandbox(db.conn()).unwrap();
        let count: i64 = db.conn().query_row("SELECT COUNT(*) FROM context_sandbox", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib sandbox::compressor::tests`
Expected: FAIL

- [ ] **Step 3: Implement**

```rust
pub struct CompressedResult {
    pub node_id: i64,
    pub summary: String,
    pub relevance_score: f64,
}

pub fn should_compress(results: &[NodeResult], token_threshold: usize) -> bool {
    let total_chars: usize = results.iter().map(|r| r.code_content.len()).sum();
    // Rough estimate: 1 token ≈ 4 chars
    total_chars / 4 > token_threshold
}

pub fn compress_results(results: &[NodeResult]) -> Vec<CompressedResult> {
    results.iter().map(|r| {
        let summary = format!(
            "{} {} ({}:{}-{}){}",
            r.node_type, r.name, r.file_path, r.start_line, r.end_line,
            r.signature.as_ref().map(|s| format!(" {}", s)).unwrap_or_default(),
        );
        CompressedResult { node_id: r.id, summary, relevance_score: r.score }
    }).collect()
}

pub fn cleanup_expired_sandbox(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM context_sandbox WHERE expires_at < unixepoch()", [])?;
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib sandbox`
Expected: All PASS

- [ ] **Step 5: Commit**

```bash
git add src/sandbox/
git commit -m "feat(sandbox): add context compression and expired entry cleanup"
```

---

### Task 23: Wire All MCP Tool Handlers

**Files:**
- Modify: `src/mcp/server.rs`

This is the integration task. Wire each of the 9 tools to its backend implementation.

- [ ] **Step 1: Write integration tests for each tool**

Test each tool via `server.handle_message()`:

```rust
#[test]
fn test_semantic_code_search_tool() {
    let server = setup_indexed_server(); // helper that creates server + indexes test files
    let req = tool_call_json("semantic_code_search", json!({"query": "user login", "top_k": 3}));
    let resp = server.handle_message(&req).unwrap();
    let parsed = parse_tool_result(&resp);
    assert!(parsed.is_array());
}

#[test]
fn test_get_call_graph_tool() {
    let server = setup_indexed_server();
    let req = tool_call_json("get_call_graph", json!({"function_name": "handleLogin", "direction": "callees", "depth": 2}));
    let resp = server.handle_message(&req).unwrap();
    let parsed = parse_tool_result(&resp);
    assert!(parsed["callees"].is_array());
}

#[test]
fn test_find_http_route_tool() { ... }
#[test]
fn test_get_ast_node_tool() { ... }
#[test]
fn test_read_snippet_tool() { ... }
#[test]
fn test_start_stop_watch_tools() { ... }
#[test]
fn test_rebuild_index_tool() { ... }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib mcp::server::tests`
Expected: FAIL — tools return "not implemented"

- [ ] **Step 3: Implement each tool handler**

In `server.rs`, in `handle_tool_call()`:

```rust
fn handle_tool_call(&self, name: &str, args: &serde_json::Value) -> Result<serde_json::Value> {
    match name {
        "semantic_code_search" => {
            let query = args["query"].as_str().ok_or(anyhow!("query required"))?;
            let top_k = args["top_k"].as_u64().unwrap_or(5) as usize;
            // 1. Trigger incremental index if needed
            // 2. Embed query via model
            // 3. FTS5 search + vec0 search
            // 4. RRF fusion
            // 5. Check if should_compress → return full or compressed
        }
        "get_call_graph" => {
            let name = args["function_name"].as_str().ok_or(anyhow!("function_name required"))?;
            let direction = args["direction"].as_str().unwrap_or("both");
            let depth = args["depth"].as_i64().unwrap_or(2) as i32;
            let file_path = args["file_path"].as_str();
            // Call graph::query::get_call_graph()
        }
        "find_http_route" => {
            // Query edges where relation = 'routes_to' and metadata matches route_path
        }
        "get_ast_node" => {
            // Query nodes by file_path + name, optionally include references from edges
        }
        "read_snippet" => {
            // Query node by id, read surrounding context from original file
        }
        "start_watch" => {
            // Start FileWatcher, store in server state
        }
        "stop_watch" => {
            // Stop FileWatcher
        }
        "get_index_status" => { /* already implemented */ }
        "rebuild_index" => {
            // Verify confirm=true, delete .code-graph/index.db, run full index
        }
        _ => Err(anyhow!("unknown tool: {}", name)),
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib mcp::server::tests`
Expected: All PASS

- [ ] **Step 5: Commit**

```bash
git add src/mcp/server.rs
git commit -m "feat(mcp): wire all 9 tool handlers to backend implementations"
```

---

### Task 24: Project Initialization Flow

**Files:**
- Modify: `src/mcp/server.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Write test for auto-initialization**

```rust
#[test]
fn test_first_tool_call_triggers_init() {
    let project_dir = TempDir::new().unwrap();
    fs::write(project_dir.path().join("a.ts"), "function foo() {}").unwrap();

    // No .code-graph directory exists yet
    assert!(!project_dir.path().join(".code-graph").exists());

    let server = McpServer::new(project_dir.path()).unwrap();
    let req = tool_call_json("get_index_status", json!({}));
    let resp = server.handle_message(&req).unwrap();

    // .code-graph should now exist
    assert!(project_dir.path().join(".code-graph/index.db").exists());

    // .gitignore should contain .code-graph/
    if project_dir.path().join(".gitignore").exists() {
        let gitignore = fs::read_to_string(project_dir.path().join(".gitignore")).unwrap();
        assert!(gitignore.contains(".code-graph/"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib mcp::server::tests::test_first_tool_call_triggers_init`
Expected: FAIL

- [ ] **Step 3: Implement auto-initialization**

In `McpServer::new(project_root)`:
1. Check if `.code-graph/` exists
2. If not: create directory, initialize DB, detect project root from CWD
3. Append `.code-graph/` to `.gitignore` if it exists and doesn't already contain the entry
4. On first tool call that needs index: run `run_full_index()`

In `main.rs`:
```rust
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::init();
    let project_root = std::env::current_dir()?;
    let server = McpServer::new(&project_root)?;
    // Read stdin line by line, handle messages, write to stdout
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    // ... message loop
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib mcp::server::tests::test_first_tool_call_triggers_init`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/mcp/server.rs src/main.rs
git commit -m "feat: add auto-initialization on first tool call with gitignore integration"
```

---

### Task 25: End-to-End Integration Test

**Files:**
- Create: `tests/integration.rs`

- [ ] **Step 1: Write end-to-end test**

```rust
use std::fs;
use tempfile::TempDir;

#[test]
fn test_e2e_index_and_search() {
    let project = TempDir::new().unwrap();

    // Create a realistic project structure
    fs::create_dir_all(project.path().join("src/auth")).unwrap();
    fs::create_dir_all(project.path().join("src/api")).unwrap();

    fs::write(project.path().join("src/auth/token.ts"), r#"
import jwt from 'jsonwebtoken';
import { UserRepo } from '../db/user';

export function validateToken(token: string): User | null {
    const decoded = jwt.verify(token, process.env.SECRET);
    return UserRepo.findById(decoded.userId);
}
"#).unwrap();

    fs::write(project.path().join("src/api/login.ts"), r#"
import { validateToken } from '../auth/token';

export function handleLogin(req: Request, res: Response) {
    const user = validateToken(req.headers.authorization);
    if (!user) { res.status(401); return; }
    res.json({ userId: user.id });
}
"#).unwrap();

    let server = McpServer::new(project.path()).unwrap();

    // Initialize
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();

    // Search for auth-related code
    let search = tool_call("semantic_code_search", json!({"query": "user authentication login", "top_k": 3}));
    let resp = server.handle_message(&search).unwrap();
    let results = parse_tool_result(&resp);
    // Should find handleLogin and/or validateToken
    let names: Vec<&str> = results.as_array().unwrap().iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    assert!(names.contains(&"handleLogin") || names.contains(&"validateToken"));

    // Get call graph for handleLogin
    let graph = tool_call("get_call_graph", json!({"function_name": "handleLogin", "direction": "callees", "depth": 2}));
    let resp = server.handle_message(&graph).unwrap();
    let result = parse_tool_result(&resp);
    // Should show handleLogin → validateToken chain
    let callees: Vec<&str> = result["callees"].as_array().unwrap().iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    assert!(callees.contains(&"validateToken"));

    // Get index status
    let status = tool_call("get_index_status", json!({}));
    let resp = server.handle_message(&status).unwrap();
    let result = parse_tool_result(&resp);
    assert!(result["files_count"].as_i64().unwrap() >= 2);
    assert!(result["nodes_count"].as_i64().unwrap() >= 2);
}
```

- [ ] **Step 2: Run test**

Run: `cargo test --test integration`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add tests/
git commit -m "test: add end-to-end integration test for index + search + call graph"
```

---

### Task 26: Final Cleanup + Release Build Test

**Files:**
- Modify: `Cargo.toml` — optimize release profile
- Verify binary size and startup time

- [ ] **Step 1: Add release profile optimizations**

```toml
[profile.release]
opt-level = "z"      # Optimize for size
lto = true           # Link-time optimization
strip = true         # Strip debug symbols
codegen-units = 1    # Single codegen unit for better optimization
```

- [ ] **Step 2: Build release binary**

```bash
cargo build --release
ls -lh target/release/code-graph-mcp
```

Expected: Binary size < 80MB (may vary based on embedded model inclusion)

- [ ] **Step 3: Run all tests**

```bash
cargo test
```

Expected: All tests PASS

- [ ] **Step 4: Smoke test the binary**

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}' | target/release/code-graph-mcp 2>/dev/null
```

Expected: Valid JSON response with capabilities

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml
git commit -m "chore: add release profile optimizations"
```

- [ ] **Step 6: Tag release**

```bash
git tag -a v0.1.0 -m "Initial MVP release: semantic search + call graph + incremental indexing"
```

---

## Addendum: Reviewer Fixes (Post-Review)

The following additions address issues found during plan review.

### Task 27: Route Extraction for find_http_route

**Files:**
- Modify: `src/parser/relations.rs`

The `find_http_route` tool requires parser support for extracting route decorators/registrations. This is missing from Tasks 12-13.

- [ ] **Step 1: Write failing tests for route extraction**

```rust
#[test]
fn test_extract_express_routes() {
    let code = r#"
app.post('/api/login', handleLogin);
app.get('/api/users/:id', getUser);
router.use('/api/admin', adminMiddleware, adminRouter);
"#;
    let relations = extract_relations(code, "typescript").unwrap();
    let routes: Vec<(&str, &str)> = relations.iter()
        .filter(|r| r.relation == "routes_to")
        .map(|r| (r.metadata.as_deref().unwrap_or(""), r.target_name.as_str()))
        .collect();
    assert!(routes.iter().any(|(meta, target)| meta.contains("/api/login") && *target == "handleLogin"));
}

#[test]
fn test_extract_python_flask_routes() {
    let code = r#"
@app.route('/api/users', methods=['GET'])
def get_users():
    return jsonify(users)
"#;
    let relations = extract_relations(code, "python").unwrap();
    let routes: Vec<&str> = relations.iter()
        .filter(|r| r.relation == "routes_to")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(routes.contains(&"get_users"));
}

#[test]
fn test_extract_go_http_routes() {
    let code = r#"
func main() {
    http.HandleFunc("/api/health", healthCheck)
    http.Handle("/api/users", userHandler)
}
"#;
    let relations = extract_relations(code, "go").unwrap();
    assert!(relations.iter().any(|r| r.relation == "routes_to" && r.target_name == "healthCheck"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib parser::relations::tests`
Expected: FAIL

- [ ] **Step 3: Implement route extraction per language**

In `relations.rs`, extend `extract_relations` to detect:
- **TypeScript/JS**: `app.get/post/put/delete(path, handler)`, `router.use(path, ...)`
- **Python**: `@app.route(path)`, `@app.get(path)` decorators
- **Go**: `http.HandleFunc(path, handler)`, `http.Handle(path, handler)`
- **Java**: `@RequestMapping`, `@GetMapping`, `@PostMapping` annotations

Store the route path in `metadata` as JSON: `{"method": "POST", "path": "/api/login"}`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib parser::relations::tests`
Expected: All PASS

- [ ] **Step 5: Commit**

```bash
git add src/parser/relations.rs
git commit -m "feat(parser): add route extraction for Express/Flask/Go/Spring"
```

---

### Task 28: Dirty-Node Propagation in Incremental Index

**Files:**
- Modify: `src/indexer/pipeline.rs`

Per spec Section 6: when file A changes, nodes in other files that reference A's nodes must have their `context_string` regenerated.

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn test_incremental_propagates_dirty_context() {
    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db = Database::open(&db_dir.path().join("index.db")).unwrap();

    // Initial: B calls A
    fs::write(project_dir.path().join("a.ts"), "function alpha() {}").unwrap();
    fs::write(project_dir.path().join("b.ts"), "function beta() { alpha(); }").unwrap();
    run_full_index(&db, project_dir.path()).unwrap();

    let beta_ctx_before = get_nodes_by_name(db.conn(), "beta").unwrap()[0]
        .context_string.clone().unwrap_or_default();

    // Change A: rename function
    fs::write(project_dir.path().join("a.ts"), "function alphaRenamed() {}").unwrap();
    run_incremental_index(&db, project_dir.path()).unwrap();

    // beta's context_string should be updated (calls list changed)
    let beta_ctx_after = get_nodes_by_name(db.conn(), "beta").unwrap()[0]
        .context_string.clone().unwrap_or_default();
    assert_ne!(beta_ctx_before, beta_ctx_after);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib indexer::pipeline::tests::test_incremental_propagates_dirty_context`
Expected: FAIL

- [ ] **Step 3: Implement dirty propagation**

In `run_incremental_index`, after re-indexing changed files:

```rust
// Dirty propagation: find nodes referencing changed nodes
let changed_node_ids: Vec<i64> = /* ids of nodes in changed files */;
let dirty_node_ids: HashSet<i64> = conn.prepare(
    "SELECT DISTINCT source_id FROM edges WHERE target_id IN (SELECT id FROM nodes WHERE file_id IN (SELECT id FROM files WHERE path IN (?)))"
)?.query_map(...)?.collect();

// Regenerate context_string for dirty nodes (1-level propagation only)
for node_id in &dirty_node_ids {
    let ctx = build_context_for_node(conn, *node_id)?;
    update_context_string(conn, *node_id, &ctx)?;
    // Re-embed if embedding model available
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib indexer::pipeline::tests::test_incremental_propagates_dirty_context`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/indexer/pipeline.rs
git commit -m "feat(indexer): add dirty-node context propagation for incremental updates"
```

---

### Task 29: Embed Model via include_bytes! for Release

**Files:**
- Modify: `src/embedding/model.rs`

- [ ] **Step 1: Add conditional compilation for model loading**

```rust
impl EmbeddingModel {
    pub fn load() -> Result<Self> {
        let device = Device::Cpu;

        // In release mode: load from embedded bytes (zero-dependency)
        // In dev mode: load from files (faster compilation)
        #[cfg(not(debug_assertions))]
        let (model_bytes, tokenizer_bytes) = {
            static MODEL: &[u8] = include_bytes!("../../models/model.safetensors");
            static TOKENIZER: &[u8] = include_bytes!("../../models/tokenizer.json");
            (MODEL.to_vec(), TOKENIZER)
        };

        #[cfg(debug_assertions)]
        let (model_bytes, tokenizer_bytes) = {
            let model = std::fs::read("models/model.safetensors")?;
            let tok = std::fs::read("models/tokenizer.json")?;
            (model, tok.as_slice())
        };

        let tokenizer = Tokenizer::from_bytes(tokenizer_bytes)
            .map_err(|e| anyhow!("tokenizer load failed: {}", e))?;
        let vb = VarBuilder::from_buffered_safetensors(model_bytes, DType::F32, &device)?;
        // ... rest of model loading
    }
}
```

- [ ] **Step 2: Verify release build includes model**

```bash
cargo build --release
ls -lh target/release/code-graph-mcp
# Should be ~60-80MB (includes 22MB model + 700KB tokenizer)
```

- [ ] **Step 3: Commit**

```bash
git add src/embedding/model.rs
git commit -m "feat(embedding): embed model weights via include_bytes! for release builds"
```

---

### Shared Test Utilities

Create `tests/helpers.rs` or `src/test_utils.rs` (behind `#[cfg(test)]`) with these helpers used across tests:

```rust
#[cfg(test)]
pub mod test_utils {
    use crate::storage::queries::*;
    use crate::mcp::server::McpServer;
    use tempfile::TempDir;

    /// Create a minimal NodeRecord for testing
    pub fn node(name: &str, file_id: i64) -> NodeRecord {
        NodeRecord {
            file_id,
            node_type: "function".into(),
            name: name.into(),
            qualified_name: None,
            start_line: 1,
            end_line: 5,
            code_content: format!("function {}() {{}}", name),
            signature: None,
            doc_comment: None,
            context_string: None,
        }
    }

    /// Create a default NodeResult for sandbox tests
    pub fn default_node() -> NodeResult {
        NodeResult {
            id: 0,
            file_id: 0,
            node_type: "function".into(),
            name: "default".into(),
            qualified_name: None,
            file_path: "test.ts".into(),
            start_line: 1,
            end_line: 5,
            code_content: "".into(),
            signature: None,
            doc_comment: None,
            context_string: None,
            score: 0.0,
        }
    }

    /// Build a JSON-RPC tools/call request
    pub fn tool_call_json(tool_name: &str, args: serde_json::Value) -> String {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": args
            }
        }).to_string()
    }

    /// Alias for tool_call_json
    pub fn tool_call(tool_name: &str, args: serde_json::Value) -> String {
        tool_call_json(tool_name, args)
    }

    /// Parse the text content from an MCP tool result response
    pub fn parse_tool_result(response: &str) -> serde_json::Value {
        let parsed: serde_json::Value = serde_json::from_str(response).unwrap();
        let text = parsed["result"]["content"][0]["text"].as_str().unwrap();
        serde_json::from_str(text).unwrap()
    }

    /// Create a McpServer with indexed test files for integration tests
    pub fn setup_indexed_server() -> (McpServer, TempDir) {
        let project_dir = TempDir::new().unwrap();
        std::fs::create_dir_all(project_dir.path().join("src")).unwrap();
        std::fs::write(
            project_dir.path().join("src/auth.ts"),
            r#"
function validateToken(token: string): boolean {
    return jwt.verify(token);
}

function handleLogin(req: Request) {
    if (validateToken(req.token)) {
        return createSession(req.userId);
    }
}
"#,
        ).unwrap();
        let server = McpServer::new(project_dir.path()).unwrap();
        // Trigger initialization
        let init = tool_call_json("get_index_status", serde_json::json!({}));
        server.handle_message(&init).unwrap();
        (server, project_dir)
    }
}
```

### BertConfig for MiniLM-L6-v2

Reference for Task 18 Step 5 — the `/* BertConfig */` placeholder:

```rust
use candle_transformers::models::bert::Config as BertConfig;

let config = BertConfig {
    vocab_size: 30522,
    hidden_size: 384,
    num_hidden_layers: 6,
    num_attention_heads: 12,
    intermediate_size: 1536,
    hidden_act: candle_nn::Activation::Gelu,
    hidden_dropout_prob: 0.1,
    max_position_embeddings: 512,
    type_vocab_size: 2,
    initializer_range: 0.02,
    layer_norm_eps: 1e-12,
    ..Default::default()
};
```

Note: Verify exact config values against the model's `config.json` from HuggingFace. The values above are for `sentence-transformers/all-MiniLM-L6-v2`.
