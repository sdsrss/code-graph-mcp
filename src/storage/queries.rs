use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::HashMap;

/// Maximum number of parameters in a single IN clause to stay within SQLite limits.
const MAX_IN_PARAMS: usize = 500;

/// Edge info tuple: (relation, direction, target_name, metadata).
/// Used by batch edge queries and context string builders.
pub type EdgeInfo = (String, String, String, Option<String>);

// --- Shared helpers ---

const NODE_SELECT: &str =
    "id, file_id, type, name, qualified_name, start_line, end_line, code_content, signature, doc_comment, context_string, name_tokens, return_type, param_types, is_test";

/// NODE_SELECT with `n.` table alias prefix on every column (for JOINs).
const NODE_SELECT_ALIASED: &str =
    "n.id, n.file_id, n.type, n.name, n.qualified_name, n.start_line, n.end_line, n.code_content, n.signature, n.doc_comment, n.context_string, n.name_tokens, n.return_type, n.param_types, n.is_test";

fn map_node_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<NodeResult> {
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
        name_tokens: row.get(11)?,
        return_type: row.get(12)?,
        param_types: row.get(13)?,
        is_test: row.get::<_, i32>(14)? != 0,
    })
}

fn first_row<T>(
    mut rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> rusqlite::Result<Option<T>> {
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

fn make_placeholders(start: usize, count: usize) -> String {
    (start..start + count)
        .map(|i| format!("?{}", i))
        .collect::<Vec<_>>()
        .join(",")
}

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

pub fn get_index_status(conn: &Connection, is_watching: bool) -> Result<IndexStatus> {
    let files_count: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
    let nodes_count: i64 = conn.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))?;
    let edges_count: i64 = conn.query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))?;
    let last_indexed_at: Option<i64> = conn.query_row(
        "SELECT MAX(indexed_at) FROM files", [], |r| r.get(0)
    ).ok().flatten();
    let schema_version: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;

    // NOTE: page_count * page_size gives the main DB file size only.
    // In WAL mode, the -wal file adds additional overhead not reflected here.
    let page_count: i64 = conn.pragma_query_value(None, "page_count", |r| r.get(0))?;
    let page_size: i64 = conn.pragma_query_value(None, "page_size", |r| r.get(0))?;
    let db_size_bytes = page_count * page_size;

    Ok(IndexStatus {
        files_count,
        nodes_count,
        edges_count,
        last_indexed_at,
        is_watching,
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
    pub name_tokens: Option<String>,
    pub return_type: Option<String>,
    /// Full parameter text from AST (includes names + types, not just type annotations).
    pub param_types: Option<String>,
    /// True if this node is inside a test context (#[cfg(test)], mod tests, etc.)
    pub is_test: bool,
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
    pub name_tokens: Option<String>,
    pub return_type: Option<String>,
    pub param_types: Option<String>,
    /// Whether this node is inside a test context (stored in DB since schema v5).
    /// Stored as INTEGER in SQLite (0/1).
    pub is_test: bool,
}

// --- Edge records ---

pub struct EdgeRecord {
    pub source_id: i64,
    pub target_id: i64,
    pub relation: String,
    pub metadata: Option<String>,
}

pub fn upsert_file(conn: &Connection, file: &FileRecord) -> Result<i64> {
    let id: i64 = conn.query_row(
        "INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at)
         VALUES (?1, ?2, ?3, ?4, unixepoch())
         ON CONFLICT(path) DO UPDATE SET
            blake3_hash = excluded.blake3_hash,
            last_modified = excluded.last_modified,
            language = excluded.language,
            indexed_at = unixepoch()
         RETURNING id",
        (&file.path, &file.blake3_hash, file.last_modified, &file.language),
        |row| row.get(0),
    )?;
    Ok(id)
}

pub fn delete_files_by_paths(conn: &Connection, paths: &[String]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    for chunk in paths.chunks(MAX_IN_PARAMS) {
        let sql = format!("DELETE FROM files WHERE path IN ({})", make_placeholders(1, chunk.len()));
        let params: Vec<&dyn rusqlite::types::ToSql> =
            chunk.iter().map(|p| p as &dyn rusqlite::types::ToSql).collect();
        conn.execute(&sql, params.as_slice())?;
    }
    Ok(())
}

pub fn get_all_file_hashes(conn: &Connection) -> Result<HashMap<String, String>> {
    let mut stmt = conn.prepare("SELECT path, blake3_hash FROM files")?;
    let map = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?
    .collect::<Result<HashMap<_, _>, _>>()?;
    Ok(map)
}

// --- Node CRUD ---

pub fn insert_node(conn: &Connection, node: &NodeRecord) -> Result<i64> {
    let id: i64 = conn.query_row(
        "INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content, signature, doc_comment, context_string, name_tokens, return_type, param_types, is_test)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         RETURNING id",
        (
            node.file_id, &node.node_type, &node.name, &node.qualified_name,
            node.start_line, node.end_line, &node.code_content,
            &node.signature, &node.doc_comment, &node.context_string,
            &node.name_tokens, &node.return_type, &node.param_types,
            node.is_test as i32,
        ),
        |row| row.get(0),
    )?;
    Ok(id)
}

/// Insert a node using a cached prepared statement for better throughput in loops.
/// Same semantics as insert_node, but avoids re-preparing the SQL on each call.
pub fn insert_node_cached(conn: &Connection, node: &NodeRecord) -> Result<i64> {
    let mut stmt = conn.prepare_cached(
        "INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content, signature, doc_comment, context_string, name_tokens, return_type, param_types, is_test)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         RETURNING id"
    )?;
    let id: i64 = stmt.query_row(
        (
            node.file_id, &node.node_type, &node.name, &node.qualified_name,
            node.start_line, node.end_line, &node.code_content,
            &node.signature, &node.doc_comment, &node.context_string,
            &node.name_tokens, &node.return_type, &node.param_types,
            node.is_test as i32,
        ),
        |row| row.get(0),
    )?;
    Ok(id)
}

pub fn get_nodes_by_name(conn: &Connection, name: &str) -> Result<Vec<NodeResult>> {
    let mut stmt = conn.prepare(
        &format!("SELECT {} FROM nodes WHERE name = ?1", NODE_SELECT)
    )?;
    let rows = stmt.query_map([name], map_node_row)?;
    let results = rows.collect::<Result<Vec<_>, _>>()?;
    Ok(results)
}

/// Like `get_nodes_by_name` but JOINs with files to return file_path in one query.
/// Avoids N+1 `get_file_path` calls when filtering/displaying by file.
pub fn get_nodes_with_files_by_name(conn: &Connection, name: &str) -> Result<Vec<NodeWithFile>> {
    let sql = format!(
        "SELECT {}, f.path, f.language FROM nodes n JOIN files f ON f.id = n.file_id WHERE n.name = ?1",
        NODE_SELECT_ALIASED
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([name], |row| {
        Ok(NodeWithFile {
            node: map_node_row(row)?,
            file_path: row.get(15)?,
            language: row.get(16)?,
        })
    })?;
    let results = rows.collect::<Result<Vec<_>, _>>()?;
    Ok(results)
}

pub fn delete_nodes_by_file(conn: &Connection, file_id: i64) -> Result<()> {
    conn.execute("DELETE FROM nodes WHERE file_id = ?1", [file_id])?;
    Ok(())
}

#[cfg(test)]
pub fn update_context_string(conn: &Connection, node_id: i64, context_string: &str) -> Result<()> {
    conn.execute(
        "UPDATE nodes SET context_string = ?1 WHERE id = ?2",
        (context_string, node_id),
    )?;
    Ok(())
}

/// Batch update context strings using a single prepared statement.
pub fn update_context_strings_batch(conn: &Connection, updates: &[(i64, String)]) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "UPDATE nodes SET context_string = ?1 WHERE id = ?2"
    )?;
    for (node_id, ctx) in updates {
        stmt.execute((ctx.as_str(), node_id))?;
    }
    Ok(())
}

// --- Edge CRUD ---

/// Insert an edge, ignoring duplicates. Returns true if a new row was actually inserted.
pub fn insert_edge(conn: &Connection, source_id: i64, target_id: i64, relation: &str, metadata: Option<&str>) -> Result<bool> {
    conn.execute(
        "INSERT OR IGNORE INTO edges (source_id, target_id, relation, metadata)
         VALUES (?1, ?2, ?3, ?4)",
        (source_id, target_id, relation, metadata),
    )?;
    Ok(conn.changes() > 0)
}

/// Insert an edge using a cached prepared statement. Returns true if new row inserted.
pub fn insert_edge_cached(conn: &Connection, source_id: i64, target_id: i64, relation: &str, metadata: Option<&str>) -> Result<bool> {
    let mut stmt = conn.prepare_cached(
        "INSERT OR IGNORE INTO edges (source_id, target_id, relation, metadata)
         VALUES (?1, ?2, ?3, ?4)"
    )?;
    let rows = stmt.execute((source_id, target_id, relation, metadata))?;
    Ok(rows > 0)
}

pub fn get_edges_from(conn: &Connection, node_id: i64) -> Result<Vec<EdgeRecord>> {
    let mut stmt = conn.prepare(
        "SELECT source_id, target_id, relation, metadata FROM edges WHERE source_id = ?1"
    )?;
    let rows = stmt.query_map([node_id], |row| {
        Ok(EdgeRecord {
            source_id: row.get(0)?,
            target_id: row.get(1)?,
            relation: row.get(2)?,
            metadata: row.get(3)?,
        })
    })?;
    let results = rows.collect::<Result<Vec<_>, _>>()?;
    Ok(results)
}

// --- Graph query helpers ---

/// Get all node (name, id, file_path) tuples excluding nodes belonging to specified files.
/// Used for building cross-batch name resolution maps with file path awareness.
pub fn get_node_names_with_paths_excluding_files(conn: &Connection, exclude_file_ids: &[i64]) -> Result<Vec<(String, i64, String)>> {
    if exclude_file_ids.is_empty() {
        let mut stmt = conn.prepare(
            "SELECT n.name, n.id, f.path FROM nodes n JOIN files f ON f.id = n.file_id"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?))
        })?;
        return Ok(rows.collect::<Result<Vec<_>, _>>()?);
    }

    // Chunked NOT IN — avoids temp table concurrency issues
    if exclude_file_ids.len() <= MAX_IN_PARAMS {
        let placeholders = make_placeholders(1, exclude_file_ids.len());
        let sql = format!(
            "SELECT n.name, n.id, f.path FROM nodes n JOIN files f ON f.id = n.file_id \
             WHERE n.file_id NOT IN ({})", placeholders
        );
        let params: Vec<&dyn rusqlite::types::ToSql> = exclude_file_ids.iter().map(|id| id as &dyn rusqlite::types::ToSql).collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params.as_slice(), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?))
        })?;
        return Ok(rows.collect::<Result<Vec<_>, _>>()?);
    }

    // For large exclude sets, filter in Rust with HashSet
    let exclude_set: std::collections::HashSet<i64> = exclude_file_ids.iter().copied().collect();
    let mut stmt = conn.prepare(
        "SELECT n.name, n.id, n.file_id, f.path FROM nodes n JOIN files f ON f.id = n.file_id"
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, String>(3)?))
    })?;
    let mut results = Vec::new();
    for row in rows {
        let (name, id, file_id, path) = row?;
        if !exclude_set.contains(&file_id) {
            results.push((name, id, path));
        }
    }
    Ok(results)
}

/// Load ALL node (name -> [(id, file_path)]) into a HashMap.
/// Used for building a global name resolution map once before the batch loop.
pub fn get_all_node_names_with_ids(conn: &Connection) -> Result<HashMap<String, Vec<(i64, String)>>> {
    let mut stmt = conn.prepare_cached(
        "SELECT n.id, n.name, f.path FROM nodes n JOIN files f ON n.file_id = f.id"
    )?;
    let mut map: HashMap<String, Vec<(i64, String)>> = HashMap::new();
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
    })?;
    for row in rows {
        let (id, name, path) = row?;
        map.entry(name).or_default().push((id, path));
    }
    Ok(map)
}

pub fn get_edge_target_names(conn: &Connection, source_id: i64, relation: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT n.name FROM edges e JOIN nodes n ON n.id = e.target_id
         WHERE e.source_id = ?1 AND e.relation = ?2"
    )?;
    let rows = stmt.query_map(rusqlite::params![source_id, relation], |row| {
        row.get::<_, String>(0)
    })?;
    let results = rows.collect::<Result<Vec<_>, _>>()?;
    Ok(results)
}

/// Batch-fetch edge target names for multiple source IDs in one query.
/// Returns a map from source_id to list of target names.
pub fn get_edge_target_names_batch(conn: &Connection, source_ids: &[i64], relation: &str) -> Result<HashMap<i64, Vec<String>>> {
    let mut result: HashMap<i64, Vec<String>> = HashMap::new();
    if source_ids.is_empty() {
        return Ok(result);
    }
    for chunk in source_ids.chunks(MAX_IN_PARAMS) {
        let placeholders = make_placeholders(2, chunk.len());
        let sql = format!(
            "SELECT e.source_id, n.name FROM edges e JOIN nodes n ON n.id = e.target_id
             WHERE e.source_id IN ({}) AND e.relation = ?1",
            placeholders
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut params: Vec<&dyn rusqlite::types::ToSql> = vec![&relation as &dyn rusqlite::types::ToSql];
        for id in chunk {
            params.push(id as &dyn rusqlite::types::ToSql);
        }
        let rows = stmt.query_map(params.as_slice(), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (src_id, name) = row?;
            result.entry(src_id).or_default().push(name);
        }
    }
    Ok(result)
}

pub fn get_edge_source_names(conn: &Connection, target_id: i64, relation: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT n.name FROM edges e JOIN nodes n ON n.id = e.source_id
         WHERE e.target_id = ?1 AND e.relation = ?2"
    )?;
    let rows = stmt.query_map(rusqlite::params![target_id, relation], |row| {
        row.get::<_, String>(0)
    })?;
    let results = rows.collect::<Result<Vec<_>, _>>()?;
    Ok(results)
}

/// Like get_edge_target_names but also returns the file path for each target node.
pub fn get_edge_targets_with_files(conn: &Connection, source_id: i64, relation: &str) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT n.name, COALESCE(f.path, '') FROM edges e
         JOIN nodes n ON n.id = e.target_id
         LEFT JOIN files f ON f.id = n.file_id
         WHERE e.source_id = ?1 AND e.relation = ?2"
    )?;
    let rows = stmt.query_map(rusqlite::params![source_id, relation], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Like get_edge_source_names but also returns the file path for each source node.
pub fn get_edge_sources_with_files(conn: &Connection, target_id: i64, relation: &str) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT n.name, COALESCE(f.path, '') FROM edges e
         JOIN nodes n ON n.id = e.source_id
         LEFT JOIN files f ON f.id = n.file_id
         WHERE e.target_id = ?1 AND e.relation = ?2"
    )?;
    let rows = stmt.query_map(rusqlite::params![target_id, relation], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Find all incoming references (source nodes) pointing to a target node, with file paths.
/// Optionally filter by relation type. Returns structured reference info.
pub fn get_incoming_references(
    conn: &Connection,
    target_id: i64,
    relation_filter: Option<&str>,
) -> Result<Vec<IncomingReference>> {
    let sql = if relation_filter.is_some() {
        "SELECT n.id, n.name, n.type, f.path, n.start_line, e.relation
         FROM edges e
         JOIN nodes n ON n.id = e.source_id
         LEFT JOIN files f ON f.id = n.file_id
         WHERE e.target_id = ?1 AND e.relation = ?2
         ORDER BY f.path, n.start_line"
    } else {
        "SELECT n.id, n.name, n.type, f.path, n.start_line, e.relation
         FROM edges e
         JOIN nodes n ON n.id = e.source_id
         LEFT JOIN files f ON f.id = n.file_id
         WHERE e.target_id = ?1
         ORDER BY e.relation, f.path, n.start_line"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = if let Some(rel) = relation_filter {
        stmt.query_map(rusqlite::params![target_id, rel], map_incoming_ref)?
    } else {
        stmt.query_map(rusqlite::params![target_id], map_incoming_ref)?
    };
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub struct IncomingReference {
    pub node_id: i64,
    pub name: String,
    pub node_type: String,
    pub file_path: String,
    pub start_line: i64,
    pub relation: String,
}

fn map_incoming_ref(row: &rusqlite::Row) -> rusqlite::Result<IncomingReference> {
    Ok(IncomingReference {
        node_id: row.get(0)?,
        name: row.get(1)?,
        node_type: row.get(2)?,
        file_path: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
        start_line: row.get(4)?,
        relation: row.get(5)?,
    })
}

/// Batch-fetch all edge info for a set of node IDs, grouped by node_id.
/// Each entry is an [`EdgeInfo`] tuple: (relation, direction, target_name, metadata).
/// Direction is "out" for outgoing edges (source=node), "in" for incoming edges (target=node).
pub fn get_edges_batch(conn: &Connection, node_ids: &[i64]) -> Result<HashMap<i64, Vec<EdgeInfo>>> {
    if node_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut result: HashMap<i64, Vec<EdgeInfo>> = HashMap::new();

    for chunk in node_ids.chunks(MAX_IN_PARAMS) {
        let placeholders = make_placeholders(1, chunk.len());
        let params: Vec<&dyn rusqlite::types::ToSql> = chunk.iter().map(|id| id as &dyn rusqlite::types::ToSql).collect();

        // Outgoing edges: node is source
        let sql_out = format!(
            "SELECT e.source_id, e.relation, n.name, e.metadata FROM edges e JOIN nodes n ON n.id = e.target_id WHERE e.source_id IN ({})",
            placeholders
        );
        let mut stmt = conn.prepare(&sql_out)?;
        let rows = stmt.query_map(params.as_slice(), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, Option<String>>(3)?))
        })?;
        for row in rows {
            let (source_id, relation, name, metadata) = row?;
            result.entry(source_id).or_default().push((relation, "out".into(), name, metadata));
        }

        // Incoming edges: node is target
        let sql_in = format!(
            "SELECT e.target_id, e.relation, n.name, e.metadata FROM edges e JOIN nodes n ON n.id = e.source_id WHERE e.target_id IN ({})",
            placeholders
        );
        let mut stmt = conn.prepare(&sql_in)?;
        let rows = stmt.query_map(params.as_slice(), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, Option<String>>(3)?))
        })?;
        for row in rows {
            let (target_id, relation, name, metadata) = row?;
            result.entry(target_id).or_default().push((relation, "in".into(), name, metadata));
        }
    }

    Ok(result)
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

/// Batch insert vectors using a single prepared statement.
/// For best performance, caller should wrap in a transaction (avoids per-statement fsync).
pub fn insert_node_vectors_batch(conn: &Connection, vectors: &[(i64, Vec<f32>)]) -> Result<()> {
    if vectors.is_empty() {
        return Ok(());
    }
    // vec0 virtual tables do not support INSERT OR REPLACE, so delete first.
    let mut del_stmt = conn.prepare_cached(
        "DELETE FROM node_vectors WHERE node_id = ?1"
    )?;
    let mut ins_stmt = conn.prepare_cached(
        "INSERT INTO node_vectors(node_id, embedding) VALUES (?1, ?2)"
    )?;
    for (node_id, embedding) in vectors {
        let bytes: &[u8] = bytemuck::cast_slice(embedding);
        del_stmt.execute(rusqlite::params![node_id])?;
        ins_stmt.execute(rusqlite::params![node_id, bytes])?;
    }
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
    let results = rows.collect::<Result<Vec<_>, _>>()?;
    Ok(results)
}

pub fn get_node_embedding(conn: &Connection, node_id: i64) -> Result<Vec<u8>> {
    let bytes: Vec<u8> = conn.query_row(
        "SELECT embedding FROM node_vectors WHERE node_id = ?1",
        [node_id],
        |row| row.get(0),
    )?;
    Ok(bytes)
}

// --- Additional node queries ---

/// Get all node IDs matching an exact name, with file paths for filtering.
pub fn get_node_ids_by_name(conn: &Connection, name: &str) -> Result<Vec<(i64, String)>> {
    let mut stmt = conn.prepare(
        "SELECT n.id, COALESCE(f.path, '') FROM nodes n LEFT JOIN files f ON f.id = n.file_id WHERE n.name = ?1"
    )?;
    let rows = stmt.query_map([name], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub fn get_first_node_id_by_name(conn: &Connection, name: &str) -> Result<Option<i64>> {
    let mut stmt = conn.prepare("SELECT id FROM nodes WHERE name = ?1 LIMIT 1")?;
    let rows = stmt.query_map([name], |row| row.get::<_, i64>(0))?;
    Ok(first_row(rows)?)
}

pub fn get_node_by_id(conn: &Connection, node_id: i64) -> Result<Option<NodeResult>> {
    let mut stmt = conn.prepare(
        &format!("SELECT {} FROM nodes WHERE id = ?1", NODE_SELECT)
    )?;
    let rows = stmt.query_map([node_id], map_node_row)?;
    Ok(first_row(rows)?)
}

pub fn get_nodes_by_file_path(conn: &Connection, file_path: &str) -> Result<Vec<NodeResult>> {
    let sql = format!(
        "SELECT {} FROM nodes n JOIN files f ON f.id = n.file_id WHERE f.path = ?1 ORDER BY n.start_line",
        NODE_SELECT_ALIASED
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([file_path], map_node_row)?;
    let results = rows.collect::<Result<Vec<_>, _>>()?;
    Ok(results)
}

/// List nodes filtered by type/returns/params without FTS5 query.
/// Used by ast-search when no keyword is given but structural filters are present.
pub fn get_nodes_with_files_by_filters(
    conn: &Connection,
    type_filter: Option<&[&str]>,
    returns_filter: Option<&str>,
    params_filter: Option<&str>,
    limit: usize,
) -> Result<Vec<NodeWithFile>> {
    let mut conditions = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut param_idx = 1;

    if let Some(types) = type_filter {
        let placeholders: Vec<String> = types.iter().enumerate().map(|(i, _)| {
            format!("?{}", param_idx + i)
        }).collect();
        conditions.push(format!("n.type IN ({})", placeholders.join(",")));
        for t in types {
            params.push(Box::new(t.to_string()));
        }
        param_idx += types.len();
    }
    if let Some(rt) = returns_filter {
        conditions.push(format!("LOWER(n.return_type) LIKE ?{}", param_idx));
        params.push(Box::new(format!("%{}%", rt.to_lowercase())));
        param_idx += 1;
    }
    if let Some(pt) = params_filter {
        conditions.push(format!("LOWER(n.param_types) LIKE ?{}", param_idx));
        params.push(Box::new(format!("%{}%", pt.to_lowercase())));
        let _ = param_idx; // suppress unused warning
    }

    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conditions.join(" AND "))
    };

    let sql = format!(
        "SELECT {}, f.path, f.language FROM nodes n JOIN files f ON f.id = n.file_id{} ORDER BY f.path, n.start_line LIMIT ?{}",
        NODE_SELECT_ALIASED, where_clause, params.len() + 1
    );
    params.push(Box::new(limit as i64));

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(param_refs.as_slice(), |row| {
        Ok(NodeWithFile {
            node: map_node_row(row)?,
            file_path: row.get(15)?,
            language: row.get(16)?,
        })
    })?;
    let results = rows.collect::<Result<Vec<_>, _>>()?;
    Ok(results)
}

/// Fetch a single node with its file path/language by node ID (JOIN, single query).
pub fn get_node_with_file_by_id(conn: &Connection, node_id: i64) -> Result<Option<NodeWithFile>> {
    let sql = format!(
        "SELECT {}, f.path, f.language FROM nodes n JOIN files f ON f.id = n.file_id WHERE n.id = ?1",
        NODE_SELECT_ALIASED
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([node_id], |row| {
        Ok(NodeWithFile {
            node: map_node_row(row)?,
            file_path: row.get(15)?,
            language: row.get(16)?,
        })
    })?;
    Ok(first_row(rows)?)
}

pub fn get_file_path(conn: &Connection, file_id: i64) -> Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT path FROM files WHERE id = ?1")?;
    let rows = stmt.query_map([file_id], |row| row.get::<_, String>(0))?;
    Ok(first_row(rows)?)
}

pub fn get_file_language(conn: &Connection, file_id: i64) -> Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT language FROM files WHERE id = ?1")?;
    let rows = stmt.query_map([file_id], |row| row.get::<_, Option<String>>(0))?;
    match first_row(rows)? {
        Some(lang) => Ok(lang),
        None => Ok(None),
    }
}

/// Find node IDs in other files that have edges pointing to/from nodes in the given file IDs.
/// Bidirectional: finds both callers (outgoing edges into changed files) and callees
/// (incoming edges from changed files) to ensure context strings stay consistent.
/// Used for dirty-node propagation during incremental indexing.
pub fn get_dirty_node_ids(conn: &Connection, changed_file_ids: &[i64]) -> Result<Vec<i64>> {
    if changed_file_ids.is_empty() {
        return Ok(vec![]);
    }
    let mut results = Vec::new();

    for chunk in changed_file_ids.chunks(MAX_IN_PARAMS / 2) {
        let n = chunk.len();
        let changed_ph = make_placeholders(1, n);
        let exclude_ph = make_placeholders(n + 1, n);

        let sql_callers = format!(
            "SELECT DISTINCT e.source_id FROM edges e
             JOIN nodes n ON n.id = e.target_id
             WHERE n.file_id IN ({})
             AND e.source_id NOT IN (SELECT id FROM nodes WHERE file_id IN ({}))",
            changed_ph, exclude_ph
        );
        let sql_callees = format!(
            "SELECT DISTINCT e.target_id FROM edges e
             JOIN nodes n ON n.id = e.source_id
             WHERE n.file_id IN ({})
             AND e.target_id NOT IN (SELECT id FROM nodes WHERE file_id IN ({}))",
            changed_ph, exclude_ph
        );

        let doubled: Vec<i64> = chunk.iter().chain(chunk.iter()).copied().collect();
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = doubled.iter().map(|id| id as &dyn rusqlite::types::ToSql).collect();

        let mut stmt = conn.prepare(&sql_callers)?;
        let rows = stmt.query_map(param_refs.as_slice(), |row| row.get::<_, i64>(0))?;
        for row in rows { results.push(row?); }

        let mut stmt = conn.prepare(&sql_callees)?;
        let rows = stmt.query_map(param_refs.as_slice(), |row| row.get::<_, i64>(0))?;
        for row in rows { results.push(row?); }
    }

    results.sort();
    results.dedup();
    Ok(results)
}

// --- Batch node queries ---

/// Result combining node info with its file path and language (for search results).
pub struct NodeWithFile {
    pub node: NodeResult,
    pub file_path: String,
    pub language: Option<String>,
}

/// Batch-fetch nodes with their file path and language by node IDs.
/// Avoids N+1 queries when loading search results.
pub fn get_nodes_with_files_by_ids(conn: &Connection, node_ids: &[i64]) -> Result<Vec<NodeWithFile>> {
    if node_ids.is_empty() {
        return Ok(vec![]);
    }
    let mut all_results = Vec::new();
    for chunk in node_ids.chunks(MAX_IN_PARAMS) {
        let placeholders = make_placeholders(1, chunk.len());
        let sql = format!(
            "SELECT {}, f.path, f.language FROM nodes n JOIN files f ON f.id = n.file_id WHERE n.id IN ({})",
            NODE_SELECT_ALIASED, placeholders
        );
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::types::ToSql> =
            chunk.iter().map(|id| id as &dyn rusqlite::types::ToSql).collect();
        let rows = stmt.query_map(params.as_slice(), |row| {
            Ok(NodeWithFile {
                node: map_node_row(row)?,
                file_path: row.get(15)?,
                language: row.get(16)?,
            })
        })?;
        for row in rows {
            all_results.push(row?);
        }
    }
    Ok(all_results)
}

// --- Route queries ---

pub struct RouteMatch {
    pub node_id: i64,
    pub metadata: Option<String>,
    pub handler_name: String,
    pub handler_type: String,
    pub file_path: String,
    pub start_line: i64,
    pub end_line: i64,
}

pub fn find_routes_by_path(conn: &Connection, route_path: &str, relation: &str) -> Result<Vec<RouteMatch>> {
    // Use json_extract for precise path matching instead of LIKE substring.
    // Match if the route_path is a prefix of the stored path (handles both exact and prefix matches).
    let mut stmt = conn.prepare(
        "SELECT e.source_id, e.metadata, n.name, n.type, f.path, n.start_line, n.end_line
         FROM edges e
         JOIN nodes n ON n.id = e.source_id
         JOIN files f ON f.id = n.file_id
         WHERE e.relation = ?2
         AND e.metadata IS NOT NULL
         AND (json_extract(e.metadata, '$.path') = ?1
              OR json_extract(e.metadata, '$.path') LIKE ?3 ESCAPE '\\')"
    )?;

    // Support both exact match and prefix match with path boundary
    // (e.g., "/api/users" matches "/api/users/:id" but not "/api/userservices")
    let escaped = route_path.replace('%', "\\%").replace('_', "\\_");
    let prefix_pattern = format!("{}/%", escaped);
    let rows = stmt.query_map(rusqlite::params![route_path, relation, prefix_pattern], |row| {
        Ok(RouteMatch {
            node_id: row.get(0)?,
            metadata: row.get(1)?,
            handler_name: row.get(2)?,
            handler_type: row.get(3)?,
            file_path: row.get(4)?,
            start_line: row.get(5)?,
            end_line: row.get(6)?,
        })
    })?;
    let results = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(results)
}

// --- Caller + route info query ---

#[derive(Debug)]
pub struct CallerWithRouteInfo {
    pub node_id: i64,
    pub name: String,
    pub node_type: String,
    pub file_path: String,
    pub depth: i32,
    pub route_info: Option<String>, // JSON metadata from routes_to edge
}

/// Get all callers of a symbol, annotating any that are HTTP route handlers.
pub fn get_callers_with_route_info(
    conn: &Connection,
    symbol_name: &str,
    file_path: Option<&str>,
    max_depth: i32,
) -> Result<Vec<CallerWithRouteInfo>> {
    use crate::graph::query::get_call_graph;
    use crate::domain::REL_ROUTES_TO;

    let callers = get_call_graph(conn, symbol_name, "callers", max_depth, file_path)?;

    if callers.is_empty() {
        return Ok(vec![]);
    }

    // Batch fetch route metadata for all callers (avoids N+1 queries)
    let mut route_map: HashMap<i64, String> = HashMap::new();
    let caller_ids: Vec<i64> = callers.iter().map(|c| c.node_id).collect();
    for chunk in caller_ids.chunks(MAX_IN_PARAMS) {
        let placeholders = make_placeholders(1, chunk.len());
        let sql = format!(
            "SELECT e.source_id, e.metadata FROM edges e WHERE e.source_id IN ({}) AND e.relation = ?{}",
            placeholders,
            chunk.len() + 1
        );
        let mut params: Vec<&dyn rusqlite::types::ToSql> = chunk.iter().map(|id| id as &dyn rusqlite::types::ToSql).collect();
        let rel: &dyn rusqlite::types::ToSql = &REL_ROUTES_TO;
        params.push(rel);
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params.as_slice(), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
        })?;
        for row in rows {
            let (id, meta) = row?;
            if let Some(meta) = meta {
                route_map.entry(id).or_insert(meta);
            }
        }
    }

    let results = callers
        .iter()
        .map(|caller| CallerWithRouteInfo {
            node_id: caller.node_id,
            name: caller.name.clone(),
            node_type: caller.node_type.clone(),
            file_path: caller.file_path.clone(),
            depth: caller.depth,
            route_info: route_map.get(&caller.node_id).cloned(),
        })
        .collect();
    Ok(results)
}

// --- Module queries ---

#[derive(Debug, Clone)]
pub struct ModuleExport {
    pub node_id: i64,
    pub name: String,
    pub node_type: String,
    pub signature: Option<String>,
    pub file_path: String,
    pub caller_count: i64,
}

/// Get all exported symbols from files under a directory prefix.
/// For JS/TS, uses explicit `exports` edges. For other languages (Rust, Go, Python, etc.),
/// falls back to returning all named top-level symbols (functions, structs, classes, etc.).
pub fn get_module_exports(conn: &Connection, dir_prefix: &str) -> Result<Vec<ModuleExport>> {
    use crate::domain::{REL_EXPORTS, REL_CALLS};
    let escaped_prefix = dir_prefix.replace('%', "\\%").replace('_', "\\_");
    let prefix_pattern = format!("{}%", escaped_prefix);

    // Phase 1: Try explicit exports (JS/TS)
    let sql_exports =
        "SELECT DISTINCT n.id, n.name, n.type, n.signature, f.path,
                COALESCE(cc.cnt, 0) as caller_count
         FROM nodes n
         JOIN files f ON f.id = n.file_id
         JOIN edges e ON e.target_id = n.id AND e.relation = ?1
         LEFT JOIN (SELECT target_id, COUNT(*) as cnt FROM edges WHERE relation = ?3 GROUP BY target_id) cc
           ON cc.target_id = n.id
         WHERE f.path LIKE ?2 ESCAPE '\\'
         ORDER BY caller_count DESC";
    let mut stmt = conn.prepare(sql_exports)?;
    let rows = stmt.query_map(rusqlite::params![REL_EXPORTS, &prefix_pattern, REL_CALLS], |row| {
        Ok(ModuleExport {
            node_id: row.get(0)?,
            name: row.get(1)?,
            node_type: row.get(2)?,
            signature: row.get(3)?,
            file_path: row.get(4)?,
            caller_count: row.get(5)?,
        })
    })?;
    let results: Vec<ModuleExport> = rows.collect::<std::result::Result<Vec<_>, _>>()?;

    if !results.is_empty() {
        return Ok(results);
    }

    // Phase 2: Fallback for non-JS/TS — all named top-level symbols in matching files
    let sql_fallback =
        "SELECT DISTINCT n.id, n.name, n.type, n.signature, f.path,
                COALESCE(cc.cnt, 0) as caller_count
         FROM nodes n
         JOIN files f ON f.id = n.file_id
         LEFT JOIN (SELECT target_id, COUNT(*) as cnt FROM edges WHERE relation = ?2 GROUP BY target_id) cc
           ON cc.target_id = n.id
         WHERE f.path LIKE ?1 ESCAPE '\\'
           AND n.type != 'module'
           AND n.name != '<module>'
         ORDER BY caller_count DESC";
    let mut stmt2 = conn.prepare(sql_fallback)?;
    let rows2 = stmt2.query_map(rusqlite::params![&prefix_pattern, REL_CALLS], |row| {
        Ok(ModuleExport {
            node_id: row.get(0)?,
            name: row.get(1)?,
            node_type: row.get(2)?,
            signature: row.get(3)?,
            file_path: row.get(4)?,
            caller_count: row.get(5)?,
        })
    })?;
    let all: Vec<ModuleExport> = rows2.collect::<std::result::Result<Vec<_>, _>>()?;

    // Deduplicate by (name, file_path) — keeps highest caller_count.
    // Handles feature-gated duplicates (e.g. #[cfg(feature)] producing two nodes for same symbol).
    let mut best: HashMap<(String, String), ModuleExport> = HashMap::with_capacity(all.len());
    for export in all {
        let key = (export.name.clone(), export.file_path.clone());
        best.entry(key)
            .and_modify(|existing| {
                if export.caller_count > existing.caller_count {
                    *existing = export.clone();
                }
            })
            .or_insert(export);
    }
    Ok(best.into_values().collect())
}

// --- Fuzzy name resolution ---

/// Candidate result from fuzzy function name matching.
#[derive(Debug)]
pub struct NameCandidate {
    pub name: String,
    pub file_path: String,
    pub node_type: String,
}

/// Find function/method names that contain the given substring.
/// Used as a fallback when exact name match returns no results.
pub fn find_functions_by_fuzzy_name(conn: &Connection, partial_name: &str) -> Result<Vec<NameCandidate>> {
    let escaped = partial_name.replace('%', "\\%").replace('_', "\\_");
    let pattern = format!("%{}%", escaped);

    // Tokenize input for cross-convention matching (camelCase ↔ snake_case).
    let tokens_only = crate::search::tokenizer::split_identifier_tokens(partial_name);
    let token_escaped = tokens_only.replace('%', "\\%").replace('_', "\\_");
    let token_pattern = format!("%{}%", token_escaped);

    let sql =
        "SELECT DISTINCT n.name, f.path, n.type
         FROM nodes n
         JOIN files f ON f.id = n.file_id
         WHERE (n.name LIKE ?1 ESCAPE '\\' OR n.name_tokens LIKE ?3 ESCAPE '\\')
           AND n.type IN ('function', 'method')
         ORDER BY
           CASE WHEN n.name = ?2 THEN 0
                WHEN n.name LIKE ?2 || '%' THEN 1
                ELSE 2
           END,
           LENGTH(n.name)
         LIMIT 10";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(rusqlite::params![pattern, partial_name, token_pattern], |row| {
        Ok(NameCandidate {
            name: row.get(0)?,
            file_path: row.get(1)?,
            node_type: row.get(2)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
}

// --- Import tree queries ---

#[derive(Debug)]
pub struct FileDependency {
    pub file_path: String,
    pub direction: String, // "outgoing" (this file imports) or "incoming" (imports this file)
    pub symbol_count: i64,
    pub depth: i32,
}

/// Get file-level import/export dependencies with recursive depth traversal.
/// direction: "outgoing" (what this file depends on), "incoming" (what depends on this file), "both"
pub fn get_import_tree(
    conn: &Connection,
    file_path: &str,
    direction: &str,
    max_depth: i32,
) -> Result<Vec<FileDependency>> {
    use crate::domain::{REL_CALLS, REL_IMPORTS};
    if !matches!(direction, "outgoing" | "incoming" | "both") {
        anyhow::bail!("invalid direction '{}': expected outgoing, incoming, or both", direction);
    }
    let max_depth = max_depth.clamp(1, 10);
    let mut results = Vec::new();

    if direction == "outgoing" || direction == "both" {
        let mut stmt = conn.prepare(
            "WITH RECURSIVE dep_tree(file_path, depth, visited) AS (
                -- Seed: the starting file
                SELECT ?2, 0, ?2

                UNION ALL

                -- Recurse: find files that the current-depth files depend on
                SELECT DISTINCT f2.path, dt.depth + 1,
                       dt.visited || '|' || f2.path
                FROM dep_tree dt
                JOIN files f1 ON f1.path = dt.file_path
                JOIN nodes n1 ON n1.file_id = f1.id
                JOIN edges e ON e.source_id = n1.id AND e.relation IN (?1, ?3)
                JOIN nodes n2 ON n2.id = e.target_id
                JOIN files f2 ON f2.id = n2.file_id
                WHERE dt.depth < ?4
                  AND f2.path != ?2
                  AND ('|' || dt.visited || '|') NOT LIKE '%|' || f2.path || '|%'
            )
            SELECT dt.file_path, MIN(dt.depth) as min_depth,
                -- Count actual cross-file edges from root to this file
                (SELECT COUNT(*)
                 FROM nodes na JOIN files fa ON fa.id = na.file_id
                 JOIN edges ea ON ea.source_id = na.id AND ea.relation IN (?1, ?3)
                 JOIN nodes nb ON nb.id = ea.target_id
                 JOIN files fb ON fb.id = nb.file_id
                 WHERE fa.path = ?2 AND fb.path = dt.file_path) as cnt
            FROM dep_tree dt
            WHERE dt.depth > 0
            GROUP BY dt.file_path
            ORDER BY min_depth, cnt DESC"
        )?;
        let rows = stmt.query_map(
            rusqlite::params![REL_IMPORTS, file_path, REL_CALLS, max_depth],
            |row| {
                Ok(FileDependency {
                    file_path: row.get(0)?,
                    direction: "outgoing".into(),
                    symbol_count: row.get(2)?,
                    depth: row.get(1)?,
                })
            },
        )?;
        for row in rows {
            results.push(row?);
        }
    }

    if direction == "incoming" || direction == "both" {
        let mut stmt = conn.prepare(
            "WITH RECURSIVE dep_tree(file_path, depth, visited) AS (
                SELECT ?2, 0, ?2

                UNION ALL

                SELECT DISTINCT f1.path, dt.depth + 1,
                       dt.visited || '|' || f1.path
                FROM dep_tree dt
                JOIN files f2 ON f2.path = dt.file_path
                JOIN nodes n2 ON n2.file_id = f2.id
                JOIN edges e ON e.target_id = n2.id AND e.relation IN (?1, ?3)
                JOIN nodes n1 ON n1.id = e.source_id
                JOIN files f1 ON f1.id = n1.file_id
                WHERE dt.depth < ?4
                  AND f1.path != ?2
                  AND ('|' || dt.visited || '|') NOT LIKE '%|' || f1.path || '|%'
            )
            SELECT dt.file_path, MIN(dt.depth) as min_depth,
                -- Count actual cross-file edges from this file to root
                (SELECT COUNT(*)
                 FROM nodes na JOIN files fa ON fa.id = na.file_id
                 JOIN edges ea ON ea.source_id = na.id AND ea.relation IN (?1, ?3)
                 JOIN nodes nb ON nb.id = ea.target_id
                 JOIN files fb ON fb.id = nb.file_id
                 WHERE fa.path = dt.file_path AND fb.path = ?2) as cnt
            FROM dep_tree dt
            WHERE dt.depth > 0
            GROUP BY dt.file_path
            ORDER BY min_depth, cnt DESC"
        )?;
        let rows = stmt.query_map(
            rusqlite::params![REL_IMPORTS, file_path, REL_CALLS, max_depth],
            |row| {
                Ok(FileDependency {
                    file_path: row.get(0)?,
                    direction: "incoming".into(),
                    symbol_count: row.get(2)?,
                    depth: row.get(1)?,
                })
            },
        )?;
        for row in rows {
            results.push(row?);
        }
    }

    Ok(results)
}

// --- Unembedded nodes ---

/// Get (node_id, context_string) for nodes that have context strings but no vectors.
/// Returns at most `limit` rows per call to bound memory usage.
pub fn get_unembedded_nodes(conn: &Connection, limit: usize) -> Result<Vec<(i64, String)>> {
    // Priority: embed hot-path nodes first (most referenced = highest value for search)
    // Uses LEFT JOIN + GROUP BY instead of correlated subquery for better performance
    let mut stmt = conn.prepare(
        "SELECT n.id, n.context_string
         FROM nodes n
         LEFT JOIN node_vectors nv ON n.id = nv.node_id
         LEFT JOIN edges e ON e.target_id = n.id
         WHERE nv.node_id IS NULL AND n.context_string IS NOT NULL
         GROUP BY n.id
         ORDER BY COUNT(e.target_id) DESC
         LIMIT ?1"
    )?;
    let rows = stmt.query_map([limit as i64], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Count nodes with embeddings vs total embeddable nodes.
/// Returns (with_vectors, total_embeddable).
pub fn count_nodes_with_vectors(conn: &Connection) -> Result<(i64, i64)> {
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM nodes WHERE context_string IS NOT NULL", [], |r| r.get(0)
    )?;
    // node_vectors table may not exist when embed-model feature is disabled; return 0 in that case
    let with_vectors: i64 = conn.query_row(
        "SELECT COUNT(*) FROM node_vectors", [], |r| r.get(0)
    ).unwrap_or(0);
    Ok((with_vectors, total))
}

// --- Project Architecture Map ---

use crate::domain::{REL_CALLS, REL_IMPORTS, REL_ROUTES_TO, REL_EXPORTS};

/// Per-module (directory) statistics for the project map.
pub struct ModuleStats {
    pub path: String,
    pub files: usize,
    pub functions: usize,
    pub classes: usize,
    pub interfaces_traits: usize,
    pub languages: Vec<String>,
    pub key_symbols: Vec<String>,
}

/// Cross-module dependency edge.
pub struct ModuleDep {
    pub from: String,
    pub to: String,
    pub import_count: usize,
}

/// HTTP entry point.
pub struct EntryPoint {
    pub route: String,
    pub handler: String,
    pub file: String,
}

/// Hot function (most callers).
pub struct HotFunction {
    pub name: String,
    pub node_type: String,
    pub file: String,
    pub caller_count: usize,
}

/// Get the directory part of a file path (everything before the last '/').
fn dir_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "<root>",
    }
}

/// Build a project architecture map from the knowledge graph.
#[allow(clippy::type_complexity)]
pub fn get_project_map(conn: &Connection) -> Result<(Vec<ModuleStats>, Vec<ModuleDep>, Vec<EntryPoint>, Vec<HotFunction>)> {
    // 1. Module map: SQL-level aggregation (C3: use constants, I1: GROUP BY in SQL)
    let sql = "SELECT f.path, \
                SUM(CASE WHEN n.type = 'function' THEN 1 ELSE 0 END), \
                SUM(CASE WHEN n.type IN ('class', 'struct', 'enum') THEN 1 ELSE 0 END), \
                SUM(CASE WHEN n.type IN ('interface', 'trait') THEN 1 ELSE 0 END), \
                GROUP_CONCAT(DISTINCT f.language) \
         FROM nodes n JOIN files f ON f.id = n.file_id \
         WHERE n.type != 'module' AND n.name != '<module>' \
           AND n.is_test = 0 \
         GROUP BY f.path"
        .to_string();
    let mut dir_files: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
    let mut dir_funcs: HashMap<String, usize> = HashMap::new();
    let mut dir_classes: HashMap<String, usize> = HashMap::new();
    let mut dir_ifaces: HashMap<String, usize> = HashMap::new();
    let mut dir_langs: HashMap<String, std::collections::BTreeSet<String>> = HashMap::new();
    {
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)? as usize,
                row.get::<_, i64>(2)? as usize,
                row.get::<_, i64>(3)? as usize,
                row.get::<_, Option<String>>(4)?,
            ))
        })?;
        for row in rows {
            let (path, funcs, classes, ifaces, langs) = row?;
            let dir = dir_of(&path).to_string();
            dir_files.entry(dir.clone()).or_default().insert(path);
            *dir_funcs.entry(dir.clone()).or_default() += funcs;
            *dir_classes.entry(dir.clone()).or_default() += classes;
            *dir_ifaces.entry(dir.clone()).or_default() += ifaces;
            if let Some(l) = langs {
                for lang in l.split(',').filter(|s| !s.is_empty()) {
                    dir_langs.entry(dir.clone()).or_default().insert(lang.to_string());
                }
            }
        }
    }

    // 2. Key symbols per module (C2: language-agnostic — use most-called functions per module)
    let mut dir_symbols: HashMap<String, Vec<String>> = HashMap::new();
    {
        let sql = "SELECT n.name, f.path, COUNT(e.id) as cnt \
             FROM nodes n \
             JOIN files f ON f.id = n.file_id \
             JOIN edges e ON e.target_id = n.id \
             WHERE e.relation = ?1 AND n.type != 'module' AND n.name != '<module>' \
               AND n.is_test = 0 \
               AND n.name NOT LIKE 'test\\_%' ESCAPE '\\' \
               AND f.path NOT LIKE 'tests/%' \
               AND f.path NOT LIKE '%_test.%' \
             GROUP BY n.id \
             ORDER BY cnt DESC \
             LIMIT 200";
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([REL_CALLS], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (name, path) = row?;
            let dir = dir_of(&path).to_string();
            let syms = dir_symbols.entry(dir).or_default();
            if syms.len() < 6 && !syms.contains(&name) {
                syms.push(name);
            }
        }
    }

    // Also add explicit exports (JS/TS) where available
    {
        let sql = "SELECT DISTINCT n.name, f.path FROM edges e \
             JOIN nodes n ON n.id = e.target_id \
             JOIN files f ON f.id = n.file_id \
             WHERE e.relation = ?1 AND n.name != '<module>'";
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([REL_EXPORTS], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (name, path) = row?;
            let dir = dir_of(&path).to_string();
            let syms = dir_symbols.entry(dir).or_default();
            if syms.len() < 8 && !syms.contains(&name) {
                syms.push(name);
            }
        }
    }

    // Assemble module stats (sorted by function count descending)
    let mut modules: Vec<ModuleStats> = dir_files.keys().map(|dir| {
        ModuleStats {
            path: dir.clone(),
            files: dir_files.get(dir).map(|s| s.len()).unwrap_or(0),
            functions: *dir_funcs.get(dir).unwrap_or(&0),
            classes: *dir_classes.get(dir).unwrap_or(&0),
            interfaces_traits: *dir_ifaces.get(dir).unwrap_or(&0),
            languages: dir_langs.get(dir).map(|s| s.iter().cloned().collect()).unwrap_or_default(),
            key_symbols: dir_symbols.remove(dir).unwrap_or_default(),
        }
    }).collect();
    modules.sort_by(|a, b| b.functions.cmp(&a.functions));

    // 3. Cross-module dependencies (C3: use REL_IMPORTS constant)
    let mut dep_map: HashMap<(String, String), usize> = HashMap::new();
    {
        let sql = "SELECT sf.path, tf.path, COUNT(*) \
             FROM edges e \
             JOIN nodes sn ON sn.id = e.source_id \
             JOIN nodes tn ON tn.id = e.target_id \
             JOIN files sf ON sf.id = sn.file_id \
             JOIN files tf ON tf.id = tn.file_id \
             WHERE e.relation = ?1 AND sf.path != tf.path \
             GROUP BY sf.path, tf.path";
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([REL_IMPORTS], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)? as usize))
        })?;
        for row in rows {
            let (from_file, to_file, count) = row?;
            let from_dir = dir_of(&from_file).to_string();
            let to_dir = dir_of(&to_file).to_string();
            if from_dir != to_dir {
                *dep_map.entry((from_dir, to_dir)).or_default() += count;
            }
        }
    }
    let mut deps: Vec<ModuleDep> = dep_map.into_iter()
        .map(|((from, to), count)| ModuleDep { from, to, import_count: count })
        .collect();
    deps.sort_by(|a, b| b.import_count.cmp(&a.import_count));

    // 4. HTTP entry points (C3: use REL_ROUTES_TO constant)
    let mut entry_points = Vec::new();
    {
        let sql = "SELECT sn.name, sf.path, e.metadata \
             FROM edges e \
             JOIN nodes sn ON sn.id = e.source_id \
             JOIN files sf ON sf.id = sn.file_id \
             WHERE e.relation = ?1 \
             LIMIT 20";
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([REL_ROUTES_TO], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?))
        })?;
        for row in rows {
            let (handler, file, metadata) = row?;
            let route = if let Some(ref meta) = metadata {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(meta) {
                    let method = v["method"].as_str().unwrap_or("ALL");
                    let path = v["path"].as_str().unwrap_or("?");
                    format!("{} {}", method, path)
                } else {
                    "?".into()
                }
            } else {
                "?".into()
            };
            entry_points.push(EntryPoint { route, handler, file });
        }
    }

    // 4b. Program entry points: main functions with no callers (Rust/Go/C/Python/Java)
    if entry_points.is_empty() {
        let sql = "SELECT n.name, f.path FROM nodes n \
             JOIN files f ON f.id = n.file_id \
             WHERE n.name = 'main' AND n.type = 'function' \
               AND NOT EXISTS (SELECT 1 FROM edges e WHERE e.target_id = n.id AND e.relation = ?1) \
             LIMIT 5";
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([REL_CALLS], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (name, file) = row?;
            entry_points.push(EntryPoint { route: "main".into(), handler: name, file });
        }
    }

    // 5. Hot functions (C1: filter test code, C3: use REL_CALLS constant)
    let mut hot_functions = Vec::new();
    {
        let sql = "SELECT n.name, n.type, f.path, COUNT(e.id) as cnt \
             FROM nodes n \
             JOIN files f ON f.id = n.file_id \
             JOIN edges e ON e.target_id = n.id \
             WHERE e.relation = ?1 AND n.type != 'module' AND n.name != '<module>' \
               AND n.is_test = 0 \
               AND n.name NOT LIKE 'test\\_%' ESCAPE '\\' \
               AND f.path NOT LIKE 'tests/%' \
               AND f.path NOT LIKE '%_test.%' \
             GROUP BY n.name, n.type, f.path \
             ORDER BY cnt DESC \
             LIMIT 15";
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([REL_CALLS], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, i64>(3)? as usize))
        })?;
        for row in rows {
            let (name, node_type, file, count) = row?;
            hot_functions.push(HotFunction { name, node_type, file, caller_count: count });
        }
    }

    Ok((modules, deps, entry_points, hot_functions))
}

// --- FTS5 Search ---

/// Stopwords filtered from FTS5 queries to reduce noise.
const FTS_STOP_WORDS: &[&str] = &[
    "a", "an", "and", "the", "or", "in", "of", "for", "to", "with",
    "is", "it", "this", "that", "by", "from", "on", "at", "as", "be",
    "are", "was", "were", "been", "all", "each", "how", "what", "when",
];

/// FTS5 search result with quality metadata.
pub struct FtsResult {
    pub nodes: Vec<NodeResult>,
    /// Raw BM25 scores (negated so higher = better match), parallel to `nodes`.
    pub bm25_scores: Vec<f64>,
    /// True if AND mode failed and OR fallback was used (weaker match).
    pub or_fallback: bool,
}

pub fn fts5_search(conn: &Connection, query: &str, limit: i64) -> Result<FtsResult> {
    fts5_search_impl(conn, query, limit, true)
}

/// FTS5 search including test symbols (for test-aware callers).
#[cfg(test)]
pub fn fts5_search_with_tests(conn: &Connection, query: &str, limit: i64) -> Result<FtsResult> {
    fts5_search_impl(conn, query, limit, false)
}

fn fts5_search_impl(conn: &Connection, query: &str, limit: i64, exclude_tests: bool) -> Result<FtsResult> {
    // Preprocess query: filter stopwords, split identifiers (camelCase/snake_case),
    // then sanitize for FTS5. Porter stemming is handled by the FTS5 tokenizer.
    let terms: Vec<String> = query
        .split_whitespace()
        .filter(|w| !FTS_STOP_WORDS.contains(&w.to_lowercase().as_str()))
        .flat_map(|word| {
            // Split camelCase/snake_case identifiers into constituent words
            let split = crate::search::tokenizer::split_identifier(word);
            split.split_whitespace().map(String::from).collect::<Vec<_>>()
        })
        .collect::<std::collections::BTreeSet<_>>() // deduplicate (sorted for deterministic queries)
        .into_iter()
        .map(|word| {
            // Strip FTS5 metacharacters to prevent query injection
            // (operators: * ^ : + - ~ ( ) { } " can alter FTS5 semantics)
            let sanitized: String = word.chars()
                .filter(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            sanitized
        })
        .filter(|w| !w.is_empty())
        .collect();
    // Empty/whitespace-only queries would cause FTS5 MATCH error
    if terms.is_empty() {
        return Ok(FtsResult { nodes: vec![], bm25_scores: vec![], or_fallback: false });
    }

    let test_filter = if exclude_tests { " AND n.is_test = 0" } else { "" };
    // Include BM25 score in SELECT for raw score blending in RRF fusion
    let bm25_expr = "bm25(nodes_fts, 5.0, 3.0, 2.0, 2.0, 1.0, 5.0, 1.0, 1.0)";
    let sql = format!(
        "SELECT {}, {} FROM nodes_fts fts JOIN nodes n ON n.id = fts.rowid WHERE nodes_fts MATCH ?1{}
         ORDER BY {} LIMIT ?2",
        NODE_SELECT_ALIASED, bm25_expr, test_filter, bm25_expr
    );

    // Row mapper: map_node_row for columns 0..14 (including is_test), BM25 score at column 15
    let map_row_with_bm25 = |row: &rusqlite::Row<'_>| -> rusqlite::Result<(NodeResult, f64)> {
        let node = map_node_row(row)?;
        // BM25 returns negative values (more negative = better); negate for positive scores
        let bm25: f64 = row.get(15)?;
        Ok((node, -bm25))
    };

    // Strategy: AND-first for multi-term queries (higher precision), fallback to OR
    if terms.len() > 1 {
        let and_query = terms.join(" AND ");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![and_query, limit], map_row_with_bm25)?;
        let pairs: Vec<(NodeResult, f64)> = rows.collect::<Result<Vec<_>, _>>()?;
        if pairs.len() >= std::cmp::max(3, limit as usize / 10) {
            let (nodes, bm25_scores): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
            return Ok(FtsResult { nodes, bm25_scores, or_fallback: false });
        }
        // Fallback: OR gives broader recall
    }

    let or_query = terms.join(" OR ");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![or_query, limit], map_row_with_bm25)?;
    let pairs: Vec<(NodeResult, f64)> = rows.collect::<Result<Vec<_>, _>>()?;
    let (nodes, bm25_scores): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
    Ok(FtsResult { nodes, bm25_scores, or_fallback: terms.len() > 1 })
}

/// Find nodes that are missing context strings (likely from a failed Phase 3).
/// Excludes external pseudo-nodes which never have context strings.
pub fn get_nodes_missing_context(conn: &Connection) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT n.id FROM nodes n
         JOIN files f ON f.id = n.file_id
         WHERE n.context_string IS NULL
         AND f.path != '<external>'
         LIMIT 10000"
    )?;
    let ids: Vec<i64> = stmt.query_map([], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ids)
}

// --- Dead code detection ---

/// Result from dead code analysis. Each entry is a node with no incoming usage edges.
#[derive(Debug)]
pub struct DeadCodeResult {
    pub id: i64,
    pub name: String,
    pub node_type: String,
    pub start_line: u32,
    pub end_line: u32,
    pub file_path: String,
    pub code_content: String,
    /// True if the node has an incoming `exports` edge (exported but never called).
    pub has_export_edge: bool,
}

/// Find potentially dead code: nodes with no incoming usage edges.
///
/// Excludes modules, `<module>` pseudo-nodes, `main` entry points, and (optionally) test nodes.
/// Route handlers with a `routes_to` self-edge are also excluded.
///
/// Returns at most `limit` results ordered by line count descending (largest unused code first).
pub fn find_dead_code(
    conn: &Connection,
    path_prefix: Option<&str>,
    node_type: Option<&str>,
    include_tests: bool,
    min_lines: u32,
    limit: i64,
) -> Result<Vec<DeadCodeResult>> {
    use crate::domain::{REL_CALLS, REL_IMPORTS, REL_INHERITS, REL_IMPLEMENTS, REL_ROUTES_TO, REL_EXPORTS};

    let mut conditions = vec![
        "n.type != 'module'".to_string(),
        "n.name != '<module>'".to_string(),
        "n.name != 'main'".to_string(),
        "(n.end_line - n.start_line + 1) >= :min_lines".to_string(),
    ];

    if !include_tests {
        conditions.push("n.is_test = 0".to_string());
    }

    // Track how many type filter placeholders we need
    let normalized_types: Vec<&str> = node_type
        .map(|t| crate::domain::normalize_type_filter(t))
        .unwrap_or_default();

    if node_type.is_some() {
        if normalized_types.is_empty() {
            // Unknown filter — pass as-is for backward compatibility
            conditions.push("n.type = :node_type".to_string());
        } else if normalized_types.len() == 1 {
            conditions.push("n.type = :type_0".to_string());
        } else {
            let placeholders: Vec<String> = (0..normalized_types.len())
                .map(|i| format!(":type_{}", i))
                .collect();
            conditions.push(format!("n.type IN ({})", placeholders.join(", ")));
        }
    }

    if path_prefix.is_some() {
        conditions.push("f.path LIKE :path_pattern ESCAPE '\\'".to_string());
    }

    let where_clause = conditions.join(" AND ");

    let sql = format!(
        "SELECT n.id, n.name, n.type, n.start_line, n.end_line, f.path, n.code_content,
                EXISTS(SELECT 1 FROM edges WHERE target_id = n.id AND relation = :rel_exports) as has_export
         FROM nodes n
         JOIN files f ON f.id = n.file_id
         WHERE {where_clause}
           AND NOT EXISTS (
               SELECT 1 FROM edges
               WHERE target_id = n.id
                 AND relation IN (:rel_calls, :rel_imports, :rel_inherits, :rel_implements)
           )
           AND NOT EXISTS (
               SELECT 1 FROM edges
               WHERE source_id = n.id AND target_id = n.id
                 AND relation = :rel_routes_to
           )
           -- For non-callable types (struct/enum/type/const/interface), also check
           -- if the name appears in any function's code in the same file.
           -- This catches struct instantiation, type usage, etc. that the parser
           -- doesn't track as graph edges.
           AND (
               n.type IN ('function', 'method')
               OR length(n.name) < 3
               OR NOT EXISTS (
                   SELECT 1 FROM nodes n2
                   WHERE n2.file_id = n.file_id
                     AND n2.id != n.id
                     AND n2.type IN ('function', 'method')
                     AND instr(n2.code_content, n.name) > 0
               )
           )
         ORDER BY (n.end_line - n.start_line + 1) DESC
         LIMIT :limit"
    );

    let mut stmt = conn.prepare(&sql)?;

    let path_pattern = path_prefix.map(|pp| {
        let escaped = pp.replace('%', "\\%").replace('_', "\\_");
        format!("{}%", escaped)
    });

    let mut params: Vec<(&str, &dyn rusqlite::types::ToSql)> = vec![
        (":min_lines", &min_lines),
        (":limit", &limit),
        (":rel_exports", &REL_EXPORTS),
        (":rel_calls", &REL_CALLS),
        (":rel_imports", &REL_IMPORTS),
        (":rel_inherits", &REL_INHERITS),
        (":rel_implements", &REL_IMPLEMENTS),
        (":rel_routes_to", &REL_ROUTES_TO),
    ];

    // Bind type filter placeholders (parameterized to prevent SQL injection)
    let type_param_names: Vec<String> = (0..normalized_types.len())
        .map(|i| format!(":type_{}", i))
        .collect();
    for (i, name) in type_param_names.iter().enumerate() {
        params.push((name.as_str(), &normalized_types[i] as &dyn rusqlite::types::ToSql));
    }

    // Only bind :node_type when the value was not recognized by normalize_type_filter
    let node_type_owned: Option<String> = node_type
        .filter(|_| normalized_types.is_empty())
        .map(|t| t.to_string());
    if let Some(ref t) = node_type_owned {
        params.push((":node_type", t));
    }

    if let Some(ref pattern) = path_pattern {
        params.push((":path_pattern", pattern));
    }

    let rows = stmt.query_map(params.as_slice(), |row| {
        Ok(DeadCodeResult {
            id: row.get(0)?,
            name: row.get(1)?,
            node_type: row.get(2)?,
            start_line: row.get(3)?,
            end_line: row.get(4)?,
            file_path: row.get(5)?,
            code_content: row.get(6)?,
            has_export_edge: row.get::<_, i32>(7)? != 0,
        })
    })?;

    let results = rows.collect::<Result<Vec<_>, _>>()?;
    Ok(results)
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
            name_tokens: None,
            return_type: None,
            param_types: None,
            is_test: false,
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
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();
        let n2 = insert_node(db.conn(), &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "b".into(),
            qualified_name: None, start_line: 6, end_line: 10,
            code_content: "fn b(){}".into(), signature: None,
            doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
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
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();

        let results = fts5_search(db.conn(), "authentication token", 5).unwrap().nodes;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "validateToken");
    }

    #[test]
    fn test_fts5_search_excludes_test_nodes() {
        let (db, _tmp) = test_db();
        let fid = upsert_file(db.conn(), &FileRecord {
            path: "t.ts".into(), blake3_hash: "h".into(), last_modified: 1, language: None,
        }).unwrap();
        // Production function
        insert_node(db.conn(), &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "validateToken".into(),
            qualified_name: None, start_line: 1, end_line: 5,
            code_content: "function validateToken(token) { jwt.verify(token); }".into(),
            signature: None, doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();
        // Test function (should be excluded by default)
        insert_node(db.conn(), &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "test_validateToken".into(),
            qualified_name: None, start_line: 10, end_line: 15,
            code_content: "function test_validateToken() { assert(validateToken('x')); }".into(),
            signature: None, doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: true,
        }).unwrap();

        // Default search excludes test nodes
        let results = fts5_search(db.conn(), "validateToken", 10).unwrap().nodes;
        assert_eq!(results.len(), 1, "should exclude test node");
        assert_eq!(results[0].name, "validateToken");

        // With tests included
        let results_all = fts5_search_with_tests(db.conn(), "validateToken", 10).unwrap().nodes;
        assert_eq!(results_all.len(), 2, "should include test node");
    }

    #[test]
    fn test_fts5_and_then_or_strategy() {
        let (db, _tmp) = test_db();
        let fid = upsert_file(db.conn(), &FileRecord {
            path: "t.ts".into(), blake3_hash: "h".into(), last_modified: 1, language: None,
        }).unwrap();
        // Node with both "validate" and "token" in content
        insert_node(db.conn(), &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "validateToken".into(),
            qualified_name: None, start_line: 1, end_line: 5,
            code_content: "function validateToken(token) { return true; }".into(),
            signature: None, doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();
        // Node with only "validate" (not "token")
        insert_node(db.conn(), &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "validateEmail".into(),
            qualified_name: None, start_line: 10, end_line: 15,
            code_content: "function validateEmail(email) { return true; }".into(),
            signature: None, doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();

        // Multi-term query: AND should match validateToken; if not enough results, OR adds validateEmail
        let fts = fts5_search(db.conn(), "validate token", 10).unwrap();
        assert!(!fts.nodes.is_empty(), "should find results");
        // validateToken matches both terms so should rank first
        assert_eq!(fts.nodes[0].name, "validateToken");
    }

    #[test]
    fn test_callers_with_routes() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        // Insert test data: file -> handler node -> route edge, caller -> calls -> handler
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('test.ts', 'h1', 0, 'typescript', 0)", []).unwrap();
        conn.execute("INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) VALUES (1, 'function', 'handler', 'handler', 1, 10, 'fn handler()')", []).unwrap();
        conn.execute("INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) VALUES (1, 'function', 'caller', 'caller', 11, 20, 'fn caller()')", []).unwrap();
        // caller (node 2) calls handler (node 1)
        conn.execute("INSERT INTO edges (source_id, target_id, relation, metadata) VALUES (2, 1, 'calls', NULL)", []).unwrap();
        // caller (node 2) is also a route handler
        conn.execute("INSERT INTO edges (source_id, target_id, relation, metadata) VALUES (2, 2, 'routes_to', '{\"method\":\"GET\",\"path\":\"/api/test\"}')", []).unwrap();

        let results = get_callers_with_route_info(db.conn(), "handler", None, 3).unwrap();
        assert!(!results.is_empty());
        // Verify route info is attached to the caller that is a route handler
        assert!(results.iter().any(|r| r.route_info.is_some()));
    }

    #[test]
    fn test_get_module_exports() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('src/auth/validator.ts', 'h1', 0, 'typescript', 0)", []).unwrap();
        conn.execute("INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content, signature) VALUES (1, 'function', 'validateUser', 'validateUser', 1, 10, 'function validateUser() {}', '(token: string) => User')", []).unwrap();
        // Add an export edge (module-level node exports this function)
        conn.execute("INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) VALUES (1, 'module', 'validator', 'validator', 0, 0, '')", []).unwrap();
        conn.execute("INSERT INTO edges (source_id, target_id, relation) VALUES (2, 1, 'exports')", []).unwrap();

        let exports = get_module_exports(conn, "src/auth/").unwrap();
        assert_eq!(exports.len(), 1);
        assert_eq!(exports[0].name, "validateUser");
    }

    #[test]
    fn test_get_import_tree() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        // File A with two functions, File B with two functions
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('src/a.ts', 'h1', 0, 'typescript', 0)", []).unwrap();
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('src/b.ts', 'h2', 0, 'typescript', 0)", []).unwrap();
        // Nodes in file A
        conn.execute("INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) VALUES (1, 'function', 'funcA1', 'funcA1', 1, 10, 'fn funcA1()')", []).unwrap();
        conn.execute("INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) VALUES (1, 'function', 'funcA2', 'funcA2', 11, 20, 'fn funcA2()')", []).unwrap();
        // Nodes in file B
        conn.execute("INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) VALUES (2, 'function', 'funcB1', 'funcB1', 1, 10, 'fn funcB1()')", []).unwrap();
        conn.execute("INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) VALUES (2, 'function', 'funcB2', 'funcB2', 11, 20, 'fn funcB2()')", []).unwrap();
        // funcA1 imports funcB1, funcA2 calls funcB2 — 2 cross-file edges
        conn.execute("INSERT INTO edges (source_id, target_id, relation) VALUES (1, 3, 'imports')", []).unwrap();
        conn.execute("INSERT INTO edges (source_id, target_id, relation) VALUES (2, 4, 'calls')", []).unwrap();

        let tree = get_import_tree(conn, "src/a.ts", "outgoing", 2).unwrap();
        assert!(!tree.is_empty());
        let b_dep = tree.iter().find(|d| d.file_path == "src/b.ts").unwrap();
        assert_eq!(b_dep.symbol_count, 2, "symbol_count should reflect actual cross-file edges");
        assert_eq!(b_dep.depth, 1);

        // Incoming: from B's perspective, A depends on it with 2 symbols
        let tree_in = get_import_tree(conn, "src/b.ts", "incoming", 2).unwrap();
        let a_dep = tree_in.iter().find(|d| d.file_path == "src/a.ts").unwrap();
        assert_eq!(a_dep.symbol_count, 2, "incoming symbol_count should match");
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
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();

        update_context_string(db.conn(), nid, "function foo\ncalls: bar, baz").unwrap();

        // Verify FTS5 picks up updated context_string
        let results = fts5_search(db.conn(), "bar baz", 5).unwrap().nodes;
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_get_node_names_with_paths_excluding_files_correctness() {
        let (db, _tmp) = test_db();
        let conn = db.conn();

        // Create 3 files with 1 node each
        let fid1 = upsert_file(conn, &FileRecord {
            path: "a.ts".into(), blake3_hash: "h1".into(), last_modified: 1, language: None,
        }).unwrap();
        let fid2 = upsert_file(conn, &FileRecord {
            path: "b.ts".into(), blake3_hash: "h2".into(), last_modified: 1, language: None,
        }).unwrap();
        let fid3 = upsert_file(conn, &FileRecord {
            path: "c.ts".into(), blake3_hash: "h3".into(), last_modified: 1, language: None,
        }).unwrap();

        insert_node(conn, &NodeRecord {
            file_id: fid1, node_type: "function".into(), name: "alpha".into(),
            qualified_name: None, start_line: 1, end_line: 5,
            code_content: "fn alpha(){}".into(), signature: None,
            doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();
        insert_node(conn, &NodeRecord {
            file_id: fid2, node_type: "function".into(), name: "beta".into(),
            qualified_name: None, start_line: 1, end_line: 5,
            code_content: "fn beta(){}".into(), signature: None,
            doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();
        insert_node(conn, &NodeRecord {
            file_id: fid3, node_type: "function".into(), name: "gamma".into(),
            qualified_name: None, start_line: 1, end_line: 5,
            code_content: "fn gamma(){}".into(), signature: None,
            doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();

        // Exclude 2 files → only 3rd file's node remains
        let result = get_node_names_with_paths_excluding_files(conn, &[fid1, fid2]).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "gamma");
        assert_eq!(result[0].2, "c.ts"); // also returns file path

        // Exclude all 3 → empty
        let result = get_node_names_with_paths_excluding_files(conn, &[fid1, fid2, fid3]).unwrap();
        assert!(result.is_empty());

        // Exclude none → all 3
        let result = get_node_names_with_paths_excluding_files(conn, &[]).unwrap();
        assert_eq!(result.len(), 3);
        let names: Vec<&str> = result.iter().map(|(n, _, _)| n.as_str()).collect();
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"beta"));
        assert!(names.contains(&"gamma"));
    }

    #[test]
    fn test_get_nodes_missing_context() {
        let (db, _tmp) = test_db();
        let conn = db.conn();

        // Create a normal file and an external pseudo-file
        let fid = upsert_file(conn, &FileRecord {
            path: "src/app.ts".into(), blake3_hash: "h1".into(), last_modified: 1, language: Some("typescript".into()),
        }).unwrap();
        let fid_ext = upsert_file(conn, &FileRecord {
            path: "<external>".into(), blake3_hash: "ext".into(), last_modified: 0, language: None,
        }).unwrap();

        // Node with context_string set (healthy)
        insert_node(conn, &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "healthy".into(),
            qualified_name: None, start_line: 1, end_line: 5,
            code_content: "function healthy() {}".into(), signature: None,
            doc_comment: None, context_string: Some("function healthy".into()),
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();

        // Node with NULL context_string (broken -- should be found)
        let broken_id = insert_node(conn, &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "broken".into(),
            qualified_name: None, start_line: 6, end_line: 10,
            code_content: "function broken() {}".into(), signature: None,
            doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();

        // External pseudo-node with NULL context_string (should be excluded)
        insert_node(conn, &NodeRecord {
            file_id: fid_ext, node_type: "function".into(), name: "ext_func".into(),
            qualified_name: None, start_line: 0, end_line: 0,
            code_content: "".into(), signature: None,
            doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();

        let missing = get_nodes_missing_context(conn).unwrap();
        assert_eq!(missing.len(), 1, "should find exactly 1 broken node (not external)");
        assert_eq!(missing[0], broken_id);
    }

    #[test]
    fn test_find_dead_code() {
        use crate::domain::{REL_CALLS, REL_ROUTES_TO, REL_EXPORTS};

        let (db, _tmp) = test_db();
        let conn = db.conn();

        let fid = upsert_file(conn, &FileRecord {
            path: "src/app.ts".into(), blake3_hash: "h1".into(), last_modified: 1,
            language: Some("typescript".into()),
        }).unwrap();

        // 1. main function — excluded by name filter
        insert_node(conn, &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "main".into(),
            qualified_name: None, start_line: 1, end_line: 10,
            code_content: "function main() { ... }".into(), signature: None,
            doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();

        // 2. used_fn — has incoming "calls" edge → excluded
        let used_fn_id = insert_node(conn, &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "used_fn".into(),
            qualified_name: None, start_line: 11, end_line: 20,
            code_content: "function used_fn() { ... }".into(), signature: None,
            doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();

        // 3. orphan_fn — no edges at all → should be found as dead code
        let _orphan_id = insert_node(conn, &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "orphan_fn".into(),
            qualified_name: None, start_line: 21, end_line: 40,
            code_content: "function orphan_fn() { /* lots of code */ }".into(), signature: None,
            doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();

        // 4. exported_unused — has "exports" edge but no callers → found as exported-unused
        let exported_id = insert_node(conn, &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "exported_unused".into(),
            qualified_name: None, start_line: 41, end_line: 55,
            code_content: "export function exported_unused() { ... }".into(), signature: None,
            doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();

        // 5. module node — excluded by type filter
        insert_node(conn, &NodeRecord {
            file_id: fid, node_type: "module".into(), name: "app".into(),
            qualified_name: None, start_line: 0, end_line: 100,
            code_content: "".into(), signature: None,
            doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();

        // 6. test_something — is_test=1 → excluded by default, included with include_tests=true
        let _test_id = insert_node(conn, &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "test_something".into(),
            qualified_name: None, start_line: 60, end_line: 70,
            code_content: "function test_something() { assert(true); }".into(), signature: None,
            doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: true,
        }).unwrap();

        // 7. handle_login — has routes_to self-edge → excluded
        let handler_id = insert_node(conn, &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "handle_login".into(),
            qualified_name: None, start_line: 71, end_line: 85,
            code_content: "function handle_login(req, res) { ... }".into(), signature: None,
            doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();

        // --- Create edges ---
        // Someone calls used_fn
        let caller_id = insert_node(conn, &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "caller".into(),
            qualified_name: None, start_line: 86, end_line: 90,
            code_content: "function caller() { used_fn(); }".into(), signature: None,
            doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();
        insert_edge(conn, caller_id, used_fn_id, REL_CALLS, None).unwrap();

        // Module exports exported_unused
        let module_id = insert_node(conn, &NodeRecord {
            file_id: fid, node_type: "module".into(), name: "<module>".into(),
            qualified_name: None, start_line: 0, end_line: 0,
            code_content: "".into(), signature: None,
            doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();
        insert_edge(conn, module_id, exported_id, REL_EXPORTS, None).unwrap();

        // handle_login has routes_to self-edge
        insert_edge(conn, handler_id, handler_id, REL_ROUTES_TO, Some("{\"method\":\"POST\",\"path\":\"/login\"}")).unwrap();

        // --- Test default (exclude tests) ---
        let results = find_dead_code(conn, None, None, false, 1, 100).unwrap();
        let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();

        // orphan_fn and exported_unused should be found
        assert!(names.contains(&"orphan_fn"), "orphan_fn should be found, got: {:?}", names);
        assert!(names.contains(&"exported_unused"), "exported_unused should be found, got: {:?}", names);

        // These should be excluded
        assert!(!names.contains(&"main"), "main should be excluded");
        assert!(!names.contains(&"used_fn"), "used_fn should be excluded (has callers)");
        assert!(!names.contains(&"app"), "module should be excluded");
        assert!(!names.contains(&"test_something"), "test node should be excluded by default");
        assert!(!names.contains(&"handle_login"), "route handler should be excluded");
        assert!(!names.contains(&"<module>"), "<module> should be excluded");

        // Verify has_export_edge classification
        let orphan = results.iter().find(|r| r.name == "orphan_fn").unwrap();
        assert!(!orphan.has_export_edge, "orphan_fn should not have export edge");

        let exported = results.iter().find(|r| r.name == "exported_unused").unwrap();
        assert!(exported.has_export_edge, "exported_unused should have export edge");

        // Verify ordering: largest (most lines) first
        // orphan_fn: 40-21+1=20 lines, exported_unused: 55-41+1=15 lines
        assert_eq!(results[0].name, "orphan_fn", "largest function should be first");

        // --- Test include_tests=true ---
        let results_with_tests = find_dead_code(conn, None, None, true, 1, 100).unwrap();
        let names_with_tests: Vec<&str> = results_with_tests.iter().map(|r| r.name.as_str()).collect();
        assert!(names_with_tests.contains(&"test_something"), "test node should be included when include_tests=true");

        // --- Test path_prefix filter ---
        let results_filtered = find_dead_code(conn, Some("src/"), None, false, 1, 100).unwrap();
        assert!(!results_filtered.is_empty(), "path prefix 'src/' should match");

        let results_no_match = find_dead_code(conn, Some("lib/"), None, false, 1, 100).unwrap();
        assert!(results_no_match.is_empty(), "path prefix 'lib/' should not match any");

        // --- Test node_type filter ---
        let results_fn = find_dead_code(conn, None, Some("fn"), false, 1, 100).unwrap();
        for r in &results_fn {
            assert!(r.node_type == "function" || r.node_type == "method",
                "fn filter should only return function/method, got: {}", r.node_type);
        }

        // --- Test min_lines filter ---
        let results_big = find_dead_code(conn, None, None, false, 18, 100).unwrap();
        let big_names: Vec<&str> = results_big.iter().map(|r| r.name.as_str()).collect();
        assert!(big_names.contains(&"orphan_fn"), "orphan_fn (20 lines) should pass min_lines=18");
        assert!(!big_names.contains(&"exported_unused"), "exported_unused (15 lines) should fail min_lines=18");
    }

    #[test]
    fn test_fts5_and_threshold_no_unnecessary_or_fallback() {
        // Verify that a small number of high-quality AND results don't trigger OR fallback.
        // With limit=20: new threshold = max(3, 20/10) = 3
        // So 4 AND results >= 3 means no fallback.
        let (db, _tmp) = test_db();
        let fid = upsert_file(db.conn(), &FileRecord {
            path: "t.ts".into(), blake3_hash: "h".into(), last_modified: 1, language: None,
        }).unwrap();
        // Create 4 nodes that match BOTH "parse" and "json" as separate tokens
        for i in 0..4 {
            insert_node(db.conn(), &NodeRecord {
                file_id: fid, node_type: "function".into(),
                name: format!("handler{}", i),
                qualified_name: None, start_line: i * 10 + 1, end_line: i * 10 + 5,
                code_content: format!("function handler{}() {{ parse json data }}", i),
                signature: None, doc_comment: None, context_string: None,
                name_tokens: None, return_type: None, param_types: None, is_test: false,
            }).unwrap();
        }
        // Create a node that only matches "parse" (not "json")
        insert_node(db.conn(), &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "parseXml".into(),
            qualified_name: None, start_line: 50, end_line: 55,
            code_content: "function parseXml(xml) { parse xml data }".into(),
            signature: None, doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();

        // With limit=20: old threshold was 20/2=10 (4 < 10 => fallback to OR)
        // New threshold: max(3, 20/10)=3, so 4 >= 3 => no OR fallback
        let fts = fts5_search(db.conn(), "parse json", 20).unwrap();
        assert!(!fts.or_fallback, "4 AND results >= threshold 3, should NOT fall back to OR");
        // All 4 handler nodes match both terms
        assert_eq!(fts.nodes.len(), 4);
    }

    #[test]
    fn test_get_unembedded_nodes_priority_order() {
        // Verify that get_unembedded_nodes returns nodes ordered by edge reference count (most referenced first)
        let (db, _tmp) = test_db();
        let conn = db.conn();
        let fid = upsert_file(conn, &FileRecord {
            path: "t.ts".into(), blake3_hash: "h".into(), last_modified: 1, language: None,
        }).unwrap();

        // Create 3 nodes with context strings
        let nid1 = insert_node(conn, &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "popular".into(),
            qualified_name: None, start_line: 1, end_line: 5,
            code_content: "function popular() {}".into(),
            signature: None, doc_comment: None, context_string: Some("function popular".into()),
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();
        let nid2 = insert_node(conn, &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "moderate".into(),
            qualified_name: None, start_line: 10, end_line: 15,
            code_content: "function moderate() {}".into(),
            signature: None, doc_comment: None, context_string: Some("function moderate".into()),
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();
        let nid3 = insert_node(conn, &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "lonely".into(),
            qualified_name: None, start_line: 20, end_line: 25,
            code_content: "function lonely() {}".into(),
            signature: None, doc_comment: None, context_string: Some("function lonely".into()),
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();

        // Create a caller node (no context string so it won't appear in results)
        let caller = insert_node(conn, &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "caller".into(),
            qualified_name: None, start_line: 30, end_line: 35,
            code_content: "function caller() {}".into(),
            signature: None, doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();

        // "popular" gets 3 incoming edges, "moderate" gets 1, "lonely" gets 0
        for _ in 0..3 {
            // Use different callers for unique edges - but we only have one caller node
            // Use different relations to make them unique
            conn.execute(
                "INSERT OR IGNORE INTO edges (source_id, target_id, relation) VALUES (?1, ?2, ?3)",
                rusqlite::params![caller, nid1, "calls"],
            ).unwrap();
        }
        // Add additional edges with different metadata to make them unique
        conn.execute(
            "INSERT INTO edges (source_id, target_id, relation, metadata) VALUES (?1, ?2, 'calls', 'a')",
            rusqlite::params![caller, nid1],
        ).unwrap();
        conn.execute(
            "INSERT INTO edges (source_id, target_id, relation, metadata) VALUES (?1, ?2, 'calls', 'b')",
            rusqlite::params![caller, nid1],
        ).unwrap();
        conn.execute(
            "INSERT INTO edges (source_id, target_id, relation) VALUES (?1, ?2, 'calls')",
            rusqlite::params![caller, nid2],
        ).unwrap();

        // Create vec tables for the LEFT JOIN to work
        conn.execute_batch(&crate::storage::schema::create_vec_tables_sql()).unwrap();

        let results = get_unembedded_nodes(conn, 10).unwrap();
        assert_eq!(results.len(), 3, "should return all 3 nodes with context strings");

        // First result should be "popular" (most referenced: 3 edges)
        assert_eq!(results[0].0, nid1, "most referenced node should be first");
        // Second should be "moderate" (1 edge)
        assert_eq!(results[1].0, nid2, "moderately referenced node should be second");
        // Third should be "lonely" (0 edges)
        assert_eq!(results[2].0, nid3, "unreferenced node should be last");
    }

    #[test]
    fn test_get_all_node_names_with_ids() {
        let (db, _tmp) = test_db();
        let conn = db.conn();

        // Create 2 files with nodes
        let fid1 = upsert_file(conn, &FileRecord {
            path: "src/a.ts".into(), blake3_hash: "h1".into(), last_modified: 1, language: None,
        }).unwrap();
        let fid2 = upsert_file(conn, &FileRecord {
            path: "src/b.ts".into(), blake3_hash: "h2".into(), last_modified: 1, language: None,
        }).unwrap();

        let nid1 = insert_node(conn, &NodeRecord {
            file_id: fid1, node_type: "function".into(), name: "alpha".into(),
            qualified_name: None, start_line: 1, end_line: 5,
            code_content: "fn alpha(){}".into(), signature: None,
            doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();
        let nid2 = insert_node(conn, &NodeRecord {
            file_id: fid2, node_type: "function".into(), name: "beta".into(),
            qualified_name: None, start_line: 1, end_line: 5,
            code_content: "fn beta(){}".into(), signature: None,
            doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();
        // Same name in different file
        let nid3 = insert_node(conn, &NodeRecord {
            file_id: fid2, node_type: "function".into(), name: "alpha".into(),
            qualified_name: None, start_line: 6, end_line: 10,
            code_content: "fn alpha(){}".into(), signature: None,
            doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();

        let map = get_all_node_names_with_ids(conn).unwrap();
        // "alpha" should have 2 entries (from both files)
        let alpha_entries = map.get("alpha").unwrap();
        assert_eq!(alpha_entries.len(), 2, "alpha should have 2 entries");
        let alpha_ids: Vec<i64> = alpha_entries.iter().map(|(id, _)| *id).collect();
        assert!(alpha_ids.contains(&nid1));
        assert!(alpha_ids.contains(&nid3));

        // "beta" should have 1 entry
        let beta_entries = map.get("beta").unwrap();
        assert_eq!(beta_entries.len(), 1);
        assert_eq!(beta_entries[0].0, nid2);
        assert_eq!(beta_entries[0].1, "src/b.ts");

        // Check paths are correct for alpha entries
        let alpha_paths: Vec<&str> = alpha_entries.iter().map(|(_, p)| p.as_str()).collect();
        assert!(alpha_paths.contains(&"src/a.ts"));
        assert!(alpha_paths.contains(&"src/b.ts"));
    }
}
