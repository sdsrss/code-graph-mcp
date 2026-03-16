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
    "id, file_id, type, name, qualified_name, start_line, end_line, code_content, signature, doc_comment, context_string, name_tokens, return_type, param_types";

/// NODE_SELECT with `n.` table alias prefix on every column (for JOINs).
const NODE_SELECT_ALIASED: &str =
    "n.id, n.file_id, n.type, n.name, n.qualified_name, n.start_line, n.end_line, n.code_content, n.signature, n.doc_comment, n.context_string, n.name_tokens, n.return_type, n.param_types";

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

    conn.execute_batch("CREATE TEMP TABLE IF NOT EXISTS _exclude_file_ids (id INTEGER PRIMARY KEY)")?;
    conn.execute("DELETE FROM _exclude_file_ids", [])?;

    for chunk in exclude_file_ids.chunks(MAX_IN_PARAMS) {
        let values = (1..=chunk.len()).map(|i| format!("(?{})", i)).collect::<Vec<_>>().join(",");
        let sql = format!("INSERT INTO _exclude_file_ids (id) VALUES {}", values);
        let params: Vec<&dyn rusqlite::types::ToSql> = chunk.iter().map(|id| id as &dyn rusqlite::types::ToSql).collect();
        conn.execute(&sql, params.as_slice())?;
    }

    let mut stmt = conn.prepare(
        "SELECT n.name, n.id, f.path FROM nodes n JOIN files f ON f.id = n.file_id \
         WHERE n.file_id NOT IN (SELECT id FROM _exclude_file_ids)"
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?))
    })?;
    let results = rows.collect::<Result<Vec<_>, _>>()?;

    conn.execute_batch("DROP TABLE IF EXISTS _exclude_file_ids")?;

    Ok(results)
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
        let _ = del_stmt.execute(rusqlite::params![node_id]);
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

// --- Additional node queries ---

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
                file_path: row.get(14)?,
                language: row.get(15)?,
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
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, meta) = row?;
            route_map.entry(id).or_insert(meta);
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

#[derive(Debug)]
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
                (SELECT COUNT(*) FROM edges e2 WHERE e2.target_id = n.id AND e2.relation = ?3) as caller_count
         FROM nodes n
         JOIN files f ON f.id = n.file_id
         JOIN edges e ON e.target_id = n.id AND e.relation = ?1
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
                (SELECT COUNT(*) FROM edges e2 WHERE e2.target_id = n.id AND e2.relation = ?2) as caller_count
         FROM nodes n
         JOIN files f ON f.id = n.file_id
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
    let mut seen: HashMap<(&str, &str), usize> = HashMap::new();
    let mut deduped: Vec<ModuleExport> = Vec::with_capacity(all.len());
    for export in &all {
        let key = (export.name.as_str(), export.file_path.as_str());
        if let Some(&prev_idx) = seen.get(&key) {
            if export.caller_count > deduped[prev_idx].caller_count {
                deduped[prev_idx] = ModuleExport {
                    node_id: export.node_id,
                    name: export.name.clone(),
                    node_type: export.node_type.clone(),
                    signature: export.signature.clone(),
                    file_path: export.file_path.clone(),
                    caller_count: export.caller_count,
                };
            }
        } else {
            seen.insert(key, deduped.len());
            deduped.push(ModuleExport {
                node_id: export.node_id,
                name: export.name.clone(),
                node_type: export.node_type.clone(),
                signature: export.signature.clone(),
                file_path: export.file_path.clone(),
                caller_count: export.caller_count,
            });
        }
    }
    Ok(deduped)
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
            SELECT dt.file_path, MIN(dt.depth) as min_depth, COUNT(*) as cnt
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
            SELECT dt.file_path, MIN(dt.depth) as min_depth, COUNT(*) as cnt
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
    let mut stmt = conn.prepare(
        "SELECT n.id, n.context_string FROM nodes n
         LEFT JOIN node_vectors v ON v.node_id = n.id
         WHERE n.context_string IS NOT NULL AND v.node_id IS NULL
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
        let sql = "SELECT tn.name, sf.path, e.metadata \
             FROM edges e \
             JOIN nodes sn ON sn.id = e.source_id \
             JOIN nodes tn ON tn.id = e.target_id \
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
             GROUP BY n.id \
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

pub fn fts5_search(conn: &Connection, query: &str, limit: i64) -> Result<Vec<NodeResult>> {
    // Preprocess query: filter stopwords, split identifiers (camelCase/snake_case),
    // then sanitize for FTS5. Porter stemming is handled by the FTS5 tokenizer.
    let sanitized: String = query
        .split_whitespace()
        .filter(|w| !FTS_STOP_WORDS.contains(&w.to_lowercase().as_str()))
        .flat_map(|word| {
            // Split camelCase/snake_case identifiers into constituent words
            let split = crate::search::tokenizer::split_identifier(word);
            split.split_whitespace().map(String::from).collect::<Vec<_>>()
        })
        .collect::<std::collections::BTreeSet<_>>() // deduplicate (sorted for deterministic queries)
        .into_iter()
        .map(|word| format!("\"{}\"", word.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ");
    // Empty/whitespace-only queries would cause FTS5 MATCH error
    if sanitized.is_empty() {
        return Ok(vec![]);
    }
    let sql = format!(
        "SELECT {} FROM nodes_fts f JOIN nodes n ON n.id = f.rowid WHERE nodes_fts MATCH ?1
         -- BM25 weights: name(5), qualified_name(3), code_content(2), context_string(2), doc_comment(1), name_tokens(5), return_type(1), param_types(1)
         ORDER BY bm25(nodes_fts, 5.0, 3.0, 2.0, 2.0, 1.0, 5.0, 1.0, 1.0) LIMIT ?2",
        NODE_SELECT_ALIASED
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![sanitized, limit], map_node_row)?;
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

        let results = fts5_search(db.conn(), "authentication token", 5).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "validateToken");
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
        // File A with function that imports from File B
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('src/a.ts', 'h1', 0, 'typescript', 0)", []).unwrap();
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('src/b.ts', 'h2', 0, 'typescript', 0)", []).unwrap();
        // Node in file A
        conn.execute("INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) VALUES (1, 'function', 'funcA', 'funcA', 1, 10, 'fn funcA()')", []).unwrap();
        // Node in file B
        conn.execute("INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) VALUES (2, 'function', 'funcB', 'funcB', 1, 10, 'fn funcB()')", []).unwrap();
        // funcA imports funcB
        conn.execute("INSERT INTO edges (source_id, target_id, relation) VALUES (1, 2, 'imports')", []).unwrap();

        let tree = get_import_tree(conn, "src/a.ts", "outgoing", 2).unwrap();
        assert!(!tree.is_empty());
        assert!(tree.iter().any(|d| d.file_path == "src/b.ts"));
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
        let results = fts5_search(db.conn(), "bar baz", 5).unwrap();
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
}
