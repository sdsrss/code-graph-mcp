use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashMap;

use super::helpers::{first_row, make_placeholders, MAX_IN_PARAMS};

pub(super) const NODE_SELECT: &str =
    "id, file_id, type, name, qualified_name, start_line, end_line, code_content, signature, doc_comment, context_string, name_tokens, return_type, param_types, is_test";

/// NODE_SELECT with `n.` table alias prefix on every column (for JOINs).
pub(super) const NODE_SELECT_ALIASED: &str =
    "n.id, n.file_id, n.type, n.name, n.qualified_name, n.start_line, n.end_line, n.code_content, n.signature, n.doc_comment, n.context_string, n.name_tokens, n.return_type, n.param_types, n.is_test";

pub(super) fn map_node_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<NodeResult> {
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

/// Result combining node info with its file path and language (for search results).
pub struct NodeWithFile {
    pub node: NodeResult,
    pub file_path: String,
    pub language: Option<String>,
}

/// Entry in a global name→node lookup: `(node_id, file_path, language)`.
pub type NameEntry = (i64, String, Option<String>);

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

/// Collect cross-file inbound edges before deleting a file's nodes.
/// Returns (source_id, target_name, relation, metadata) for edges where:
/// - target is in the given file (will be deleted)
/// - source is NOT in the given file (would lose edge on cascade delete)
#[allow(clippy::type_complexity)]
pub fn get_inbound_cross_file_edges(conn: &Connection, file_id: i64) -> Result<Vec<(i64, i64, String, String, Option<String>)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT e.source_id, ns.file_id, nt.name, e.relation, e.metadata
         FROM edges e
         JOIN nodes nt ON nt.id = e.target_id
         JOIN nodes ns ON ns.id = e.source_id
         WHERE nt.file_id = ?1 AND ns.file_id != ?1"
    )?;
    let rows = stmt.query_map([file_id], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub fn delete_nodes_by_file(conn: &Connection, file_id: i64) -> Result<()> {
    conn.execute("DELETE FROM nodes WHERE file_id = ?1", [file_id])?;
    Ok(())
}

/// Returns inbound REL_CALLS edges into nodes of the given file from callers
/// in OTHER files, projected as (source_id, target_name, source_language,
/// metadata) — exactly what `pending_unresolved_calls` needs to buffer.
///
/// Used right before Phase 0 cascade-deletes the target file's nodes. The
/// cascade strips B→A.foo edges via target_id FK; without buffering these
/// callers' bare-name calls into pending, B never gets a chance to re-resolve
/// them when A reappears later. Same shape of bug as the "callee added later"
/// case, just from the deletion direction.
#[allow(clippy::type_complexity)]
pub fn get_inbound_calls_for_pending(
    conn: &Connection,
    file_id: i64,
) -> Result<Vec<(i64, String, String, Option<String>)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT e.source_id, nt.name, COALESCE(fs.language, ''), e.metadata
         FROM edges e
         JOIN nodes nt ON nt.id = e.target_id
         JOIN nodes ns ON ns.id = e.source_id
         JOIN files fs ON fs.id = ns.file_id
         WHERE nt.file_id = ?1 AND ns.file_id != ?1 AND e.relation = 'calls'
           AND fs.language IS NOT NULL"
    )?;
    let rows = stmt.query_map([file_id], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;
    rows.filter_map(Result::ok)
        .filter(|(_, _, lang, _)| !lang.is_empty())
        .map(Ok)
        .collect()
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

/// Load ALL node (name -> [NameEntry]) into a HashMap.
/// Used for building a global name resolution map once before the batch loop.
/// `language` enables same-language-preferred edge resolution to avoid
/// cross-language bare-name collisions (e.g. Rust `hasher.update()` resolving
/// to a JS `function update`).
pub fn get_all_node_names_with_ids(conn: &Connection) -> Result<HashMap<String, Vec<NameEntry>>> {
    let mut stmt = conn.prepare_cached(
        "SELECT n.id, n.name, f.path, f.language FROM nodes n JOIN files f ON n.file_id = f.id"
    )?;
    let mut map: HashMap<String, Vec<(i64, String, Option<String>)>> = HashMap::new();
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;
    for row in rows {
        let (id, name, path, language) = row?;
        map.entry(name).or_default().push((id, path, language));
    }
    Ok(map)
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
    use crate::domain::REL_CALLS;
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

    // Order by caller_count DESC so high-value symbols surface first; without
    // this, `ORDER BY f.path` alphabetically truncated late-path files (e.g.
    // src/storage/queries.rs — 54 Result-returning fns) out of the top-N.
    let sql = format!(
        "SELECT {cols}, f.path, f.language \
         FROM nodes n JOIN files f ON f.id = n.file_id{where_clause} \
         ORDER BY (SELECT COUNT(*) FROM edges e WHERE e.target_id = n.id AND e.relation = '{rel}') DESC, \
                  f.path ASC, n.start_line ASC \
         LIMIT ?{limit_idx}",
        cols = NODE_SELECT_ALIASED,
        where_clause = where_clause,
        rel = REL_CALLS,
        limit_idx = params.len() + 1,
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

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::edges::insert_edge;
    use super::super::files::{upsert_file, FileRecord};
    use super::super::helpers::test_db;
    use super::super::search::fts5_search;

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
        let alpha_ids: Vec<i64> = alpha_entries.iter().map(|(id, _, _)| *id).collect();
        assert!(alpha_ids.contains(&nid1));
        assert!(alpha_ids.contains(&nid3));

        // "beta" should have 1 entry
        let beta_entries = map.get("beta").unwrap();
        assert_eq!(beta_entries.len(), 1);
        assert_eq!(beta_entries[0].0, nid2);
        assert_eq!(beta_entries[0].1, "src/b.ts");

        // Check paths are correct for alpha entries
        let alpha_paths: Vec<&str> = alpha_entries.iter().map(|(_, p, _)| p.as_str()).collect();
        assert!(alpha_paths.contains(&"src/a.ts"));
        assert!(alpha_paths.contains(&"src/b.ts"));
    }

    #[test]
    fn test_get_nodes_with_files_by_filters_ranks_by_caller_count() {
        // Regression: alphabetical ORDER BY silently truncated high-caller-count
        // symbols in late-path files. New ranking is caller_count DESC, path ASC.
        let (db, _tmp) = test_db();
        let early = upsert_file(db.conn(), &FileRecord {
            path: "a/early.rs".into(), blake3_hash: "h1".into(),
            last_modified: 1, language: Some("rust".into()),
        }).unwrap();
        let late = upsert_file(db.conn(), &FileRecord {
            path: "z/late.rs".into(), blake3_hash: "h2".into(),
            last_modified: 1, language: Some("rust".into()),
        }).unwrap();

        // Uncalled Result-fn in alphabetically-first file
        let cold = insert_node(db.conn(), &NodeRecord {
            file_id: early, node_type: "function".into(), name: "cold_fn".into(),
            qualified_name: None, start_line: 1, end_line: 3,
            code_content: "fn cold_fn() -> Result<()> {}".into(),
            signature: None, doc_comment: None, context_string: None,
            name_tokens: None, return_type: Some("Result<()>".into()),
            param_types: None, is_test: false,
        }).unwrap();

        // Hot Result-fn in alphabetically-last file, called 3×
        let hot = insert_node(db.conn(), &NodeRecord {
            file_id: late, node_type: "function".into(), name: "hot_fn".into(),
            qualified_name: None, start_line: 1, end_line: 3,
            code_content: "fn hot_fn() -> Result<i32> {}".into(),
            signature: None, doc_comment: None, context_string: None,
            name_tokens: None, return_type: Some("Result<i32>".into()),
            param_types: None, is_test: false,
        }).unwrap();

        for i in 0..3 {
            let caller = insert_node(db.conn(), &NodeRecord {
                file_id: early, node_type: "function".into(),
                name: format!("caller_{}", i), qualified_name: None,
                start_line: 10 + i as i64, end_line: 12 + i as i64,
                code_content: "fn c() {}".into(), signature: None,
                doc_comment: None, context_string: None, name_tokens: None,
                return_type: None, param_types: None, is_test: false,
            }).unwrap();
            insert_edge(db.conn(), caller, hot, "calls", None).unwrap();
        }
        insert_edge(db.conn(), cold, cold, "calls", None).unwrap(); // self-loop still = 1 caller

        let types: &[&str] = &["function"];
        let results = get_nodes_with_files_by_filters(
            db.conn(), Some(types), Some("Result"), None, 10,
        ).unwrap();

        assert_eq!(results[0].node.id, hot, "hot_fn (3 callers) must outrank cold_fn (1)");
        assert_eq!(results[0].file_path, "z/late.rs");
        assert_eq!(results[1].node.id, cold);

        // With limit=1, hot_fn still wins even though alphabetically-first file exists
        let top1 = get_nodes_with_files_by_filters(
            db.conn(), Some(types), Some("Result"), None, 1,
        ).unwrap();
        assert_eq!(top1.len(), 1);
        assert_eq!(top1[0].node.id, hot,
            "limit=1 with alphabetical ORDER BY would return cold_fn — regression guard");
    }
}
