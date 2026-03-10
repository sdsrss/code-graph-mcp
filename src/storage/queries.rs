use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::HashMap;

// --- Index status ---

#[derive(Debug, Serialize)]
pub struct IndexStatus {
    pub files_count: i64,
    pub nodes_count: i64,
    pub edges_count: i64,
    pub last_indexed_at: Option<i64>,
    pub is_watching: bool,
    pub schema_version: i32,
    pub db_size_bytes: i64,
}

pub fn get_index_status(conn: &Connection) -> Result<IndexStatus> {
    let files_count: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
    let nodes_count: i64 = conn.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))?;
    let edges_count: i64 = conn.query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))?;
    let last_indexed_at: Option<i64> = conn.query_row(
        "SELECT MAX(indexed_at) FROM files", [], |r| r.get(0)
    ).ok().flatten();
    let schema_version: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;

    // For in-memory DBs, page_count * page_size gives size
    let page_count: i64 = conn.pragma_query_value(None, "page_count", |r| r.get(0))?;
    let page_size: i64 = conn.pragma_query_value(None, "page_size", |r| r.get(0))?;
    let db_size_bytes = page_count * page_size;

    Ok(IndexStatus {
        files_count,
        nodes_count,
        edges_count,
        last_indexed_at,
        is_watching: false,
        schema_version,
        db_size_bytes,
    })
}

// --- File records ---

pub struct FileRecord {
    pub path: String,
    pub blake3_hash: String,
    pub last_modified: i64,
    pub language: Option<String>,
}

// --- Node records ---

pub struct NodeRecord {
    pub file_id: i64,
    pub node_type: String,
    pub name: String,
    pub qualified_name: Option<String>,
    pub start_line: i64,
    pub end_line: i64,
    pub code_content: String,
    pub signature: Option<String>,
    pub doc_comment: Option<String>,
    pub context_string: Option<String>,
}

pub struct NodeResult {
    pub id: i64,
    pub file_id: i64,
    pub node_type: String,
    pub name: String,
    pub qualified_name: Option<String>,
    pub start_line: i64,
    pub end_line: i64,
    pub code_content: String,
    pub signature: Option<String>,
    pub doc_comment: Option<String>,
    pub context_string: Option<String>,
}

// --- Edge records ---

pub struct EdgeRecord {
    pub id: i64,
    pub source_id: i64,
    pub target_id: i64,
    pub relation: String,
    pub metadata: Option<String>,
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

// --- Node CRUD ---

pub fn insert_node(conn: &Connection, node: &NodeRecord) -> Result<i64> {
    conn.execute(
        "INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content, signature, doc_comment, context_string)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        (
            node.file_id, &node.node_type, &node.name, &node.qualified_name,
            node.start_line, node.end_line, &node.code_content,
            &node.signature, &node.doc_comment, &node.context_string,
        ),
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn get_nodes_by_name(conn: &Connection, name: &str) -> Result<Vec<NodeResult>> {
    let mut stmt = conn.prepare(
        "SELECT id, file_id, type, name, qualified_name, start_line, end_line, code_content, signature, doc_comment, context_string
         FROM nodes WHERE name = ?1"
    )?;
    let rows = stmt.query_map([name], |row| {
        Ok(NodeResult {
            id: row.get(0)?,
            file_id: row.get(1)?,
            node_type: row.get(2)?,
            name: row.get(3)?,
            qualified_name: row.get(4)?,
            start_line: row.get(5)?,
            end_line: row.get(6)?,
            code_content: row.get(7)?,
            signature: row.get(8)?,
            doc_comment: row.get(9)?,
            context_string: row.get(10)?,
        })
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

pub fn delete_nodes_by_file(conn: &Connection, file_id: i64) -> Result<()> {
    conn.execute("DELETE FROM nodes WHERE file_id = ?1", [file_id])?;
    Ok(())
}

pub fn update_context_string(conn: &Connection, node_id: i64, context_string: &str) -> Result<()> {
    conn.execute(
        "UPDATE nodes SET context_string = ?1 WHERE id = ?2",
        (context_string, node_id),
    )?;
    Ok(())
}

// --- Edge CRUD ---

pub fn insert_edge(conn: &Connection, source_id: i64, target_id: i64, relation: &str, metadata: Option<&str>) -> Result<i64> {
    conn.execute(
        "INSERT OR IGNORE INTO edges (source_id, target_id, relation, metadata)
         VALUES (?1, ?2, ?3, ?4)",
        (source_id, target_id, relation, metadata),
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn get_edges_from(conn: &Connection, node_id: i64) -> Result<Vec<EdgeRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, source_id, target_id, relation, metadata FROM edges WHERE source_id = ?1"
    )?;
    let rows = stmt.query_map([node_id], |row| {
        Ok(EdgeRecord {
            id: row.get(0)?,
            source_id: row.get(1)?,
            target_id: row.get(2)?,
            relation: row.get(3)?,
            metadata: row.get(4)?,
        })
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

// --- Graph query helpers ---

pub fn get_all_node_names(conn: &Connection) -> Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare("SELECT name, id FROM nodes")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

pub fn get_edge_target_names(conn: &Connection, source_id: i64, relation: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT n.name FROM edges e JOIN nodes n ON n.id = e.target_id
         WHERE e.source_id = ?1 AND e.relation = ?2"
    )?;
    let rows = stmt.query_map(rusqlite::params![source_id, relation], |row| {
        row.get::<_, String>(0)
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

pub fn get_edge_source_names(conn: &Connection, target_id: i64, relation: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT n.name FROM edges e JOIN nodes n ON n.id = e.source_id
         WHERE e.target_id = ?1 AND e.relation = ?2"
    )?;
    let rows = stmt.query_map(rusqlite::params![target_id, relation], |row| {
        row.get::<_, String>(0)
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

// --- Vector operations ---

pub fn insert_node_vector(conn: &Connection, node_id: i64, embedding: &[f32]) -> Result<()> {
    let bytes: &[u8] = bytemuck::cast_slice(embedding);
    conn.execute(
        "INSERT OR REPLACE INTO node_vectors(node_id, embedding) VALUES (?1, ?2)",
        rusqlite::params![node_id, bytes],
    )?;
    Ok(())
}

pub fn vector_search(conn: &Connection, query_embedding: &[f32], limit: i64) -> Result<Vec<(i64, f64)>> {
    let bytes: &[u8] = bytemuck::cast_slice(query_embedding);
    let mut stmt = conn.prepare(
        "SELECT node_id, distance FROM node_vectors WHERE embedding MATCH ?1 ORDER BY distance LIMIT ?2"
    )?;
    let rows = stmt.query_map(rusqlite::params![bytes, limit], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

// --- FTS5 Search ---

pub fn fts5_search(conn: &Connection, query: &str, limit: i64) -> Result<Vec<NodeResult>> {
    let mut stmt = conn.prepare(
        "SELECT n.id, n.file_id, n.type, n.name, n.qualified_name, n.start_line, n.end_line,
                n.code_content, n.signature, n.doc_comment, n.context_string
         FROM nodes_fts f JOIN nodes n ON n.id = f.rowid
         WHERE nodes_fts MATCH ?1
         ORDER BY rank
         LIMIT ?2"
    )?;
    let rows = stmt.query_map(rusqlite::params![query, limit], |row| {
        Ok(NodeResult {
            id: row.get(0)?,
            file_id: row.get(1)?,
            node_type: row.get(2)?,
            name: row.get(3)?,
            qualified_name: row.get(4)?,
            start_line: row.get(5)?,
            end_line: row.get(6)?,
            code_content: row.get(7)?,
            signature: row.get(8)?,
            doc_comment: row.get(9)?,
            context_string: row.get(10)?,
        })
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db::Database;
    use tempfile::TempDir;

    fn test_db() -> (Database, TempDir) {
        let tmp = TempDir::new().unwrap();
        let db = Database::open(&tmp.path().join("test.db")).unwrap();
        (db, tmp)
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

    #[test]
    fn test_insert_and_query_node() {
        let (db, _tmp) = test_db();
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
        let (db, _tmp) = test_db();
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
        let (db, _tmp) = test_db();
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
        let (db, _tmp) = test_db();
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
}
