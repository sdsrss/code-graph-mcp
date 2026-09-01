use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashMap;

use super::helpers::{escape_like, first_row, make_placeholders, MAX_IN_PARAMS};

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
            node.file_id,
            &node.node_type,
            &node.name,
            &node.qualified_name,
            node.start_line,
            node.end_line,
            &node.code_content,
            &node.signature,
            &node.doc_comment,
            &node.context_string,
            &node.name_tokens,
            &node.return_type,
            &node.param_types,
            node.is_test as i32,
        ),
        |row| row.get(0),
    )?;
    Ok(id)
}

/// SQL fragment excluding the `<external>` pseudo-file from a name lookup.
///
/// Every production caller of the by-name lookups below is USER-FACING symbol
/// resolution — `show`, `impact`, `callgraph`, `refs`, `similar`, MCP
/// `get_ast_node` — and none of them can do anything with a sentinel: it has no
/// source, no line range, and its `file_path` cannot be passed back as `--file`
/// / `file_path`. Filtering here rather than at each surface is deliberate:
/// IDX v53 started binding Rust `use std::…` to sentinels, and the first attempt
/// at this fix patched two call sites, leaving `show HashMap` answering
/// `module <external>/HashMap` with exit 0 and `impact HashMap` answering
/// `Risk: UNKNOWN, 0 callers` with exit 0 — both of which had correctly reported
/// "Symbol not found" before v53. One filter at the source cannot be
/// half-applied.
///
/// Edge-oriented surfaces (`deps` disclosure, `find_references`' import rows)
/// do not go through these lookups and are unaffected.
const EXCLUDE_EXTERNAL_BY_NAME: &str =
    "AND file_id NOT IN (SELECT id FROM files WHERE path = '<external>')";

pub fn get_nodes_by_name(conn: &Connection, name: &str) -> Result<Vec<NodeResult>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM nodes WHERE name = ?1 {}",
        NODE_SELECT, EXCLUDE_EXTERNAL_BY_NAME
    ))?;
    let rows = stmt.query_map([name], map_node_row)?;
    let results = rows.collect::<Result<Vec<_>, _>>()?;
    Ok(results)
}

/// Like `get_nodes_by_name` but JOINs with files to return file_path in one query.
/// Avoids N+1 `get_file_path` calls when filtering/displaying by file.
pub fn get_nodes_with_files_by_name(conn: &Connection, name: &str) -> Result<Vec<NodeWithFile>> {
    let sql = format!(
        "SELECT {}, f.path, f.language FROM nodes n JOIN files f ON f.id = n.file_id \
         WHERE n.name = ?1 AND f.path <> '<external>'",
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
pub fn get_inbound_cross_file_edges(
    conn: &Connection,
    file_id: i64,
) -> Result<Vec<(i64, i64, String, String, Option<String>)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT e.source_id, ns.file_id, nt.name, e.relation, e.metadata
         FROM edges e
         JOIN nodes nt ON nt.id = e.target_id
         JOIN nodes ns ON ns.id = e.source_id
         WHERE nt.file_id = ?1 AND ns.file_id != ?1",
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
           AND fs.language IS NOT NULL
         ORDER BY e.source_id, nt.name",
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

/// Returns inbound NON-`calls` edges into nodes of the given file from sources
/// in OTHER files, projected as (source_id, source_path, source_language,
/// target_name, relation, metadata) — what the deferred pass needs to re-resolve
/// them against the whole tree.
///
/// The sibling of [`get_inbound_calls_for_pending`], and the reason it exists:
/// that one is hardcoded to `relation = 'calls'` because `pending_unresolved_calls`
/// is a calls-only buffer. Everything else (imports / implements / inherits /
/// references / exports / routes_to) was cascade-deleted with no recovery
/// channel at all, so deleting file A silently dropped B's `imports A.Base`
/// edge while B itself never changed — and no later run re-extracted it, because
/// B's hash still matched. A full rebuild of the same final tree kept the edge
/// (re-resolved to an `<external>` sentinel), so incremental and full diverged
/// permanently (indexing audit 2026-08-02 P1-5).
///
/// Rows are ORDERed for the same reason every other resolution input is sorted:
/// the deferred pass is first-wins in places, so a HashMap-order arrival would
/// make two indexes of one tree disagree.
#[allow(clippy::type_complexity)]
pub fn get_inbound_relations_for_requeue(
    conn: &Connection,
    file_id: i64,
) -> Result<Vec<(i64, String, String, String, String, Option<String>)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT e.source_id, fs.path, COALESCE(fs.language, ''), nt.name, e.relation, e.metadata
         FROM edges e
         JOIN nodes nt ON nt.id = e.target_id
         JOIN nodes ns ON ns.id = e.source_id
         JOIN files fs ON fs.id = ns.file_id
         WHERE nt.file_id = ?1 AND ns.file_id != ?1 AND e.relation != 'calls'
           AND fs.language IS NOT NULL
         ORDER BY e.source_id, e.relation, nt.name",
    )?;
    let rows = stmt.query_map([file_id], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<String>>(5)?,
        ))
    })?;
    rows.filter_map(Result::ok)
        .filter(|(_, path, lang, ..)| !lang.is_empty() && !path.is_empty())
        .map(Ok)
        .collect()
}

/// Distinct source-file paths holding a non-`calls` relation INTO the given
/// file — its structural dependents (importers, subclasses, route sources).
///
/// The set `index_files` must re-extract when that file's EXISTENCE changes.
/// [`get_inbound_relations_for_requeue`] re-resolves those same edges by the
/// TARGET NODE's name, which is a lossy stand-in for what extraction would
/// produce: for Python `from a import target`, a rebuild of the tree without
/// `a.py` mints one `<external>` sentinel named after the SPECIFIER (`a`,
/// `external_module`), while the requeue mints one named after the imported
/// SYMBOL (`target`) and drops the module-level edge entirely, whose target
/// name is the useless `<module>`. Re-extracting the dependent is the only
/// mechanism that reproduces extraction's own shape, in both the vanished and
/// the restored state (audit 2026-08-29 PIPE-02).
///
/// `calls` is excluded because that direction already has a faithful channel:
/// `pending_unresolved_calls` buffers the caller's intent (name + language +
/// receiver metadata) rather than a resolved edge, and the Phase-2c sweep
/// rebinds it when the callee returns.
pub fn get_structural_dependent_files(conn: &Connection, path: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT fs.path
         FROM edges e
         JOIN nodes nt ON nt.id = e.target_id
         JOIN nodes ns ON ns.id = e.source_id
         JOIN files ft ON ft.id = nt.file_id
         JOIN files fs ON fs.id = ns.file_id
         WHERE ft.path = ?1
           AND fs.path <> ?1
           AND e.relation <> 'calls'
           AND fs.language IS NOT NULL
         ORDER BY fs.path",
    )?;
    let rows = stmt.query_map([path], |row| row.get::<_, String>(0))?;
    Ok(rows.filter_map(Result::ok).collect())
}

/// Every `(sentinel name, importing file)` pair currently bound to an
/// `<external>` sentinel by a non-`calls` relation.
///
/// The candidate pool for the OTHER half of the existence-change sweep: when a
/// file appears, the files that could not resolve a specifier before are the
/// ones whose extraction may now bind it to a real node. Matching the specifier
/// against the new file happens in Rust (`sentinel_name_matches_stem`) rather
/// than in SQL — the names are module specifiers (`./util`, `pkg.util`,
/// `@scope/util`) and a LIKE/GLOB pattern built from a file stem would need
/// metacharacter escaping to stay correct on a stem containing `*`, `[` or `_`.
///
/// Cardinality is one row per (specifier, importer) pair, i.e. proportional to
/// unresolved import statements: 989 rows on this 2,385-file repo. The caller
/// runs it only when a run actually introduces a file new to the index.
pub fn get_external_sentinel_importers(conn: &Connection) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT nt.name, fs.path
         FROM edges e
         JOIN nodes nt ON nt.id = e.target_id
         JOIN nodes ns ON ns.id = e.source_id
         JOIN files ft ON ft.id = nt.file_id
         JOIN files fs ON fs.id = ns.file_id
         WHERE ft.path = ?1
           AND e.relation <> 'calls'
           AND fs.language IS NOT NULL
         ORDER BY nt.name, fs.path",
    )?;
    let rows = stmt.query_map([crate::domain::EXTERNAL_FILE_PATH], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    Ok(rows.filter_map(Result::ok).collect())
}

/// Delete `<external>` sentinel nodes that no edge touches any more.
///
/// Sentinels are minted as edge TARGETS only, but pruning
/// (`prune_import_contradicted_call_edges`), re-resolution, and source-file
/// deletion can strip their last edge — and nothing ever deleted the node
/// (audit 2026-08-02 P1-9). A lingering orphan stays in the name-resolution
/// pool as a live candidate and makes an incrementally-grown node set diverge
/// from a fresh rebuild forever. Both edge directions are checked as a belt:
/// a sentinel should never source an edge, but if one ever does, reaping it
/// would silently drop that edge via cascade.
///
/// Returns the number of nodes deleted.
pub fn reap_orphan_external_nodes(conn: &Connection) -> Result<usize> {
    let n = conn.execute(
        "DELETE FROM nodes
         WHERE file_id = (SELECT id FROM files WHERE path = ?1)
           AND id NOT IN (SELECT target_id FROM edges)
           AND id NOT IN (SELECT source_id FROM edges)",
        [crate::domain::EXTERNAL_FILE_PATH],
    )?;
    Ok(n)
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
    let mut stmt = conn.prepare_cached("UPDATE nodes SET context_string = ?1 WHERE id = ?2")?;
    for (node_id, ctx) in updates {
        stmt.execute((ctx.as_str(), node_id))?;
    }
    Ok(())
}

// --- Graph query helpers ---

/// Get all node (name, id, file_path) tuples excluding nodes belonging to specified files.
/// Used for building cross-batch name resolution maps with file path awareness.
pub fn get_node_names_with_paths_excluding_files(
    conn: &Connection,
    exclude_file_ids: &[i64],
) -> Result<Vec<(String, i64, String)>> {
    if exclude_file_ids.is_empty() {
        let mut stmt = conn
            .prepare("SELECT n.name, n.id, f.path FROM nodes n JOIN files f ON f.id = n.file_id")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        return Ok(rows.collect::<Result<Vec<_>, _>>()?);
    }

    // Chunked NOT IN — avoids temp table concurrency issues
    if exclude_file_ids.len() <= MAX_IN_PARAMS {
        let placeholders = make_placeholders(1, exclude_file_ids.len());
        let sql = format!(
            "SELECT n.name, n.id, f.path FROM nodes n JOIN files f ON f.id = n.file_id \
             WHERE n.file_id NOT IN ({})",
            placeholders
        );
        let params: Vec<&dyn rusqlite::types::ToSql> = exclude_file_ids
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        return Ok(rows.collect::<Result<Vec<_>, _>>()?);
    }

    // For large exclude sets, filter in Rust with HashSet
    let exclude_set: std::collections::HashSet<i64> = exclude_file_ids.iter().copied().collect();
    let mut stmt = conn.prepare(
        "SELECT n.name, n.id, n.file_id, f.path FROM nodes n JOIN files f ON f.id = n.file_id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
        ))
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
        "SELECT n.id, n.name, f.path, f.language FROM nodes n JOIN files f ON n.file_id = f.id",
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
    let mut stmt = conn.prepare(&format!(
        "SELECT id FROM nodes WHERE name = ?1 {EXCLUDE_EXTERNAL_BY_NAME} LIMIT 1"
    ))?;
    let rows = stmt.query_map([name], |row| row.get::<_, i64>(0))?;
    Ok(first_row(rows)?)
}

pub fn get_node_by_id(conn: &Connection, node_id: i64) -> Result<Option<NodeResult>> {
    let mut stmt = conn.prepare(&format!("SELECT {} FROM nodes WHERE id = ?1", NODE_SELECT))?;
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

/// List nodes filtered by type/returns/params/name without FTS5 query.
/// Used by ast-search filter-only path AND as a fallback when FTS query returned
/// zero post-type-filter results (FTS ranking can drown structs/enums under
/// function-name hits — e.g. `query="Result" type=struct` returns 0 because the
/// top FTS hits for "Result" are functions like `compress_results`).
///
/// `name_filter` does case-insensitive substring match on `n.name`.
pub fn get_nodes_with_files_by_filters(
    conn: &Connection,
    type_filter: Option<&[&str]>,
    returns_filter: Option<&str>,
    params_filter: Option<&str>,
    name_filter: Option<&str>,
    limit: usize,
) -> Result<Vec<NodeWithFile>> {
    use crate::domain::REL_CALLS;
    let mut conditions = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut param_idx = 1;

    if let Some(types) = type_filter {
        let placeholders: Vec<String> = types
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", param_idx + i))
            .collect();
        conditions.push(format!("n.type IN ({})", placeholders.join(",")));
        for t in types {
            params.push(Box::new(t.to_string()));
        }
        param_idx += types.len();
    }
    // Escape LIKE wildcards in the user-supplied filter so a literal `_`/`%` (common
    // in code identifiers like `get_node`) matches literally instead of as a single-
    // char / any-run wildcard. Mirrors find_functions_by_fuzzy_name's escaping.
    if let Some(rt) = returns_filter {
        conditions.push(format!(
            "LOWER(n.return_type) LIKE ?{} ESCAPE '\\'",
            param_idx
        ));
        let escaped = escape_like(&rt.to_lowercase());
        params.push(Box::new(format!("%{}%", escaped)));
        param_idx += 1;
    }
    if let Some(pt) = params_filter {
        conditions.push(format!(
            "LOWER(n.param_types) LIKE ?{} ESCAPE '\\'",
            param_idx
        ));
        let escaped = escape_like(&pt.to_lowercase());
        params.push(Box::new(format!("%{}%", escaped)));
        param_idx += 1;
    }
    if let Some(nf) = name_filter {
        conditions.push(format!("LOWER(n.name) LIKE ?{} ESCAPE '\\'", param_idx));
        let escaped = escape_like(&nf.to_lowercase());
        params.push(Box::new(format!("%{}%", escaped)));
        let _ = param_idx;
    }

    // Always exclude the <module>/<external> placeholder nodes and test symbols:
    // ast_search (the only caller) must not surface graph internals or tests among
    // structural results, matching is_skippable_result on the FTS/query path and the
    // search/similar surfaces. These literals carry no bind params, so they don't
    // disturb the ?N indices assigned above. `is_test_node_sql` covers the stored
    // flag AND the name/path heuristic (a superset of is_skippable_result's check).
    conditions.push("NOT (n.type = 'module' AND n.name = '<module>')".to_string());
    conditions.push("f.path != '<external>'".to_string());
    conditions.push(format!("NOT {}", crate::domain::is_test_node_sql("n", "f")));

    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conditions.join(" AND "))
    };

    // Order by caller_count DESC so high-value symbols surface first; without
    // this, `ORDER BY f.path` alphabetically truncated late-path files (e.g.
    // src/storage/queries.rs — 54 Result-returning fns) out of the top-N.
    // Subquery uses domain helpers to filter source-side test edges so test-only
    // utility wrappers (e.g. `extract_relations` 64 test/0 prod) don't out-rank
    // real prod hot symbols. Aligned with project_map / get_module_exports.
    let prod_join = crate::domain::prod_source_join_sql("e");
    let prod_where = crate::domain::prod_source_filter_and();
    let sql = format!(
        "SELECT {cols}, f.path, f.language \
         FROM nodes n JOIN files f ON f.id = n.file_id{where_clause} \
         ORDER BY (\
             SELECT COUNT(*) FROM edges e \
             {prod_join} \
             WHERE e.target_id = n.id AND e.relation = '{rel}' \
               AND {prod_where} \
         ) DESC, \
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
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = doubled
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();

        let mut stmt = conn.prepare(&sql_callers)?;
        let rows = stmt.query_map(param_refs.as_slice(), |row| row.get::<_, i64>(0))?;
        for row in rows {
            results.push(row?);
        }

        let mut stmt = conn.prepare(&sql_callees)?;
        let rows = stmt.query_map(param_refs.as_slice(), |row| row.get::<_, i64>(0))?;
        for row in rows {
            results.push(row?);
        }
    }

    results.sort();
    results.dedup();
    Ok(results)
}

// --- Batch node queries ---

/// Batch-fetch nodes with their file path and language by node IDs.
/// Avoids N+1 queries when loading search results.
pub fn get_nodes_with_files_by_ids(
    conn: &Connection,
    node_ids: &[i64],
) -> Result<Vec<NodeWithFile>> {
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
        let params: Vec<&dyn rusqlite::types::ToSql> = chunk
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();
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

/// Batch-fetch a `node_id -> file path` map for the given IDs, chunked under
/// SQLite's bind limit (`MAX_IN_PARAMS` per IN-clause). Returns only the path
/// (not the full `NodeWithFile`) for callers that just need a proximity hint —
/// notably `resolve_pending_calls`, where a single source function with N
/// unresolved calls yields N pending rows sharing one `source_id`, so the raw
/// list can be ~2× the node count and a single unchunked `IN (...)` blows past
/// SQLite's variable cap. IDs absent from the DB are omitted from the map.
pub fn get_node_paths_by_ids(conn: &Connection, node_ids: &[i64]) -> Result<HashMap<i64, String>> {
    let mut map = HashMap::new();
    if node_ids.is_empty() {
        return Ok(map);
    }
    for chunk in node_ids.chunks(MAX_IN_PARAMS) {
        let placeholders = make_placeholders(1, chunk.len());
        let sql = format!(
            "SELECT n.id, f.path FROM nodes n JOIN files f ON f.id = n.file_id WHERE n.id IN ({})",
            placeholders
        );
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::types::ToSql> = chunk
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt.query_map(params.as_slice(), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, path) = row?;
            map.insert(id, path);
        }
    }
    Ok(map)
}

/// Batch-fetch a `node_id -> COALESCE(qualified_name, "")` map for the given
/// IDs, chunked under `MAX_IN_PARAMS` like [`get_node_paths_by_ids`]. Used by
/// `path_filter_candidates`, whose candidate set is every node sharing a bare
/// name in one language — a single unchunked `IN (...)` would risk SQLite's
/// variable cap on pathological repos (issue #30). IDs absent from the DB are
/// omitted from the map.
pub fn get_node_qualified_names_by_ids(
    conn: &Connection,
    node_ids: &[i64],
) -> Result<HashMap<i64, String>> {
    let mut map = HashMap::new();
    if node_ids.is_empty() {
        return Ok(map);
    }
    for chunk in node_ids.chunks(MAX_IN_PARAMS) {
        let placeholders = make_placeholders(1, chunk.len());
        let sql = format!(
            "SELECT id, COALESCE(qualified_name, '') FROM nodes WHERE id IN ({})",
            placeholders
        );
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::types::ToSql> = chunk
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt.query_map(params.as_slice(), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, qn) = row?;
            map.insert(id, qn);
        }
    }
    Ok(map)
}

/// Filter `node_ids` to those whose `qualified_name` denotes a method, chunked
/// under `MAX_IN_PARAMS`. `of_type = Some("Type")` keeps only `Type.*` methods
/// (the impl-type gate); `of_type = None` keeps any `*.*` qualified_name (the
/// receiver-call gate that excludes same-named free functions). NULL
/// qualified_name is excluded by the LIKE. Replaces the unchunked `IN (...)`
/// clauses in `self_filter_candidates` / `method_candidates` (issue #30).
pub fn filter_method_ids(
    conn: &Connection,
    node_ids: &[i64],
    of_type: Option<&str>,
) -> Result<Vec<i64>> {
    let mut kept = Vec::new();
    if node_ids.is_empty() {
        return Ok(kept);
    }
    // Escape LIKE metacharacters in the type name so a literal `_`/`%` (legal in
    // identifiers like `my_widget` or `Foo_Bar`) matches exactly instead of as a
    // wildcard that would also capture a sibling type's methods (`Data_X` else
    // matching `DataYX.run`). `.` is not a LIKE metacharacter. The `None => %.%`
    // gate keeps its intentional wildcards. Mirrors nodes.rs return/param/name
    // filters and lesson #1533.
    let like = match of_type {
        Some(t) => format!("{}.%", escape_like(t)),
        None => "%.%".to_string(),
    };
    for chunk in node_ids.chunks(MAX_IN_PARAMS) {
        let placeholders = make_placeholders(1, chunk.len());
        let sql = format!(
            "SELECT id FROM nodes WHERE id IN ({}) AND qualified_name LIKE ?{} ESCAPE '\\'",
            placeholders,
            chunk.len() + 1
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut params: Vec<&dyn rusqlite::types::ToSql> = chunk
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();
        params.push(&like as &dyn rusqlite::types::ToSql);
        let rows = stmt.query_map(params.as_slice(), |row| row.get::<_, i64>(0))?;
        for row in rows {
            kept.push(row?);
        }
    }
    Ok(kept)
}

/// Find nodes that are missing context strings (likely from a failed Phase 3).
/// Excludes external pseudo-nodes which never have context strings.
pub fn get_nodes_missing_context(conn: &Connection) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT n.id FROM nodes n
         JOIN files f ON f.id = n.file_id
         WHERE n.context_string IS NULL
         AND f.path != '<external>'
         LIMIT 10000",
    )?;
    let ids: Vec<i64> = stmt
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::super::edges::insert_edge;
    use super::super::files::{upsert_file, FileRecord};
    use super::super::helpers::test_db;
    use super::super::search::fts5_search;
    use super::*;

    #[test]
    fn test_insert_and_query_node() {
        let (db, _tmp) = test_db();
        let file_id = upsert_file(
            db.conn(),
            &FileRecord {
                path: "test.ts".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: Some("typescript".into()),
            },
        )
        .unwrap();

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
        let fid = upsert_file(
            db.conn(),
            &FileRecord {
                path: "t.ts".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: None,
            },
        )
        .unwrap();
        let nid = insert_node(
            db.conn(),
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "foo".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 5,
                code_content: "fn foo(){}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

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
        let fid1 = upsert_file(
            conn,
            &FileRecord {
                path: "a.ts".into(),
                blake3_hash: "h1".into(),
                last_modified: 1,
                language: None,
            },
        )
        .unwrap();
        let fid2 = upsert_file(
            conn,
            &FileRecord {
                path: "b.ts".into(),
                blake3_hash: "h2".into(),
                last_modified: 1,
                language: None,
            },
        )
        .unwrap();
        let fid3 = upsert_file(
            conn,
            &FileRecord {
                path: "c.ts".into(),
                blake3_hash: "h3".into(),
                last_modified: 1,
                language: None,
            },
        )
        .unwrap();

        insert_node(
            conn,
            &NodeRecord {
                file_id: fid1,
                node_type: "function".into(),
                name: "alpha".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 5,
                code_content: "fn alpha(){}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        insert_node(
            conn,
            &NodeRecord {
                file_id: fid2,
                node_type: "function".into(),
                name: "beta".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 5,
                code_content: "fn beta(){}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        insert_node(
            conn,
            &NodeRecord {
                file_id: fid3,
                node_type: "function".into(),
                name: "gamma".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 5,
                code_content: "fn gamma(){}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

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
    fn test_get_node_paths_by_ids_crosses_chunk_boundary() {
        // Regression for issue #30: resolve_pending_calls built one IN(...) over
        // every (un-deduped) source_id, exceeding SQLite's bind cap on large
        // repos. The chunked helper must return every path across the
        // MAX_IN_PARAMS boundary in a single logical lookup.
        let (db, _tmp) = test_db();
        let conn = db.conn();
        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "src/big.rs".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();

        let n = MAX_IN_PARAMS + 1; // force >1 chunk
        let mut ids = Vec::with_capacity(n);
        for i in 0..n {
            let id = insert_node(
                conn,
                &NodeRecord {
                    file_id: fid,
                    node_type: "function".into(),
                    name: format!("f{i}"),
                    qualified_name: None,
                    start_line: i as i64 + 1,
                    end_line: i as i64 + 1,
                    code_content: String::new(),
                    signature: None,
                    doc_comment: None,
                    context_string: None,
                    name_tokens: None,
                    return_type: None,
                    param_types: None,
                    is_test: false,
                },
            )
            .unwrap();
            ids.push(id);
        }

        let map = get_node_paths_by_ids(conn, &ids).unwrap();
        assert_eq!(
            map.len(),
            n,
            "all ids across the chunk boundary must resolve"
        );
        for id in &ids {
            assert_eq!(map.get(id).map(String::as_str), Some("src/big.rs"));
        }

        // Empty input is a no-op, not an error.
        assert!(get_node_paths_by_ids(conn, &[]).unwrap().is_empty());
    }

    #[test]
    fn test_qn_helpers_cross_chunk_boundary() {
        // Regression for issue #30: the resolve.rs candidate filters
        // (path/self/method) bound one parameter per same-name candidate in an
        // unchunked IN(...). The chunked helpers must behave identically across
        // the MAX_IN_PARAMS boundary.
        let (db, _tmp) = test_db();
        let conn = db.conn();
        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "src/big.rs".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();

        // > MAX_IN_PARAMS methods of type "T", plus two free functions (no dot).
        let n_methods = MAX_IN_PARAMS + 1;
        let mut method_ids = Vec::with_capacity(n_methods);
        let mut all_ids = Vec::with_capacity(n_methods + 2);
        let mk = |name: String, qn: Option<String>, line: i64| NodeRecord {
            file_id: fid,
            node_type: "function".into(),
            name,
            qualified_name: qn,
            start_line: line,
            end_line: line,
            code_content: String::new(),
            signature: None,
            doc_comment: None,
            context_string: None,
            name_tokens: None,
            return_type: None,
            param_types: None,
            is_test: false,
        };
        for i in 0..n_methods {
            let id = insert_node(
                conn,
                &mk(format!("m{i}"), Some(format!("T.m{i}")), i as i64 + 1),
            )
            .unwrap();
            method_ids.push(id);
            all_ids.push(id);
        }
        for i in 0..2 {
            let id = insert_node(
                conn,
                &mk(format!("free{i}"), None, n_methods as i64 + i as i64 + 1),
            )
            .unwrap();
            all_ids.push(id);
        }

        // qualified_name map covers every id (free functions map to "").
        let qns = get_node_qualified_names_by_ids(conn, &all_ids).unwrap();
        assert_eq!(qns.len(), all_ids.len());
        assert_eq!(qns.get(&method_ids[0]).map(String::as_str), Some("T.m0"));

        // None gate keeps only `*.*` (methods); free functions excluded.
        let mut methods = filter_method_ids(conn, &all_ids, None).unwrap();
        methods.sort_unstable();
        let mut expected = method_ids.clone();
        expected.sort_unstable();
        assert_eq!(
            methods, expected,
            "method gate must span the chunk boundary"
        );

        // Some("T") gate keeps only T.* — same set here.
        let mut of_type = filter_method_ids(conn, &all_ids, Some("T")).unwrap();
        of_type.sort_unstable();
        assert_eq!(of_type, expected);

        // A non-matching type keeps nothing; empty input is a no-op.
        assert!(filter_method_ids(conn, &all_ids, Some("Other"))
            .unwrap()
            .is_empty());
        assert!(filter_method_ids(conn, &[], None).unwrap().is_empty());
        assert!(get_node_qualified_names_by_ids(conn, &[])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn test_filter_method_ids_escapes_like_wildcards() {
        // `of_type` is a receiver/impl type name lifted from source, which may
        // legally contain `_` (Python `class my_widget:`, a Rust `Foo_Bar`
        // struct, …). SQLite LIKE treats `_` as a single-char wildcard, so an
        // unescaped `Data_X.%` pattern also matches `DataYX.run` and would bind a
        // call to a sibling type's method. The type gate must match literally.
        let (db, _tmp) = test_db();
        let conn = db.conn();
        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "src/w.py".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: Some("python".into()),
            },
        )
        .unwrap();
        let mk = |name: &str, qn: &str, line: i64| NodeRecord {
            file_id: fid,
            node_type: "function".into(),
            name: name.into(),
            qualified_name: Some(qn.into()),
            start_line: line,
            end_line: line,
            code_content: String::new(),
            signature: None,
            doc_comment: None,
            context_string: None,
            name_tokens: None,
            return_type: None,
            param_types: None,
            is_test: false,
        };
        let target = insert_node(conn, &mk("run", "Data_X.run", 1)).unwrap();
        let _sibling = insert_node(conn, &mk("run", "DataYX.run", 2)).unwrap();

        // `Data_X` must keep ONLY Data_X.run — the `_` is a literal, not a
        // wildcard that also captures DataYX.run.
        let kept = filter_method_ids(conn, &[target, _sibling], Some("Data_X")).unwrap();
        assert_eq!(
            kept,
            vec![target],
            "type filter must treat `_` literally, not as a LIKE wildcard"
        );
    }

    #[test]
    fn test_get_nodes_missing_context() {
        let (db, _tmp) = test_db();
        let conn = db.conn();

        // Create a normal file and an external pseudo-file
        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "src/app.ts".into(),
                blake3_hash: "h1".into(),
                last_modified: 1,
                language: Some("typescript".into()),
            },
        )
        .unwrap();
        let fid_ext = upsert_file(
            conn,
            &FileRecord {
                path: "<external>".into(),
                blake3_hash: "ext".into(),
                last_modified: 0,
                language: None,
            },
        )
        .unwrap();

        // Node with context_string set (healthy)
        insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "healthy".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 5,
                code_content: "function healthy() {}".into(),
                signature: None,
                doc_comment: None,
                context_string: Some("function healthy".into()),
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        // Node with NULL context_string (broken -- should be found)
        let broken_id = insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "broken".into(),
                qualified_name: None,
                start_line: 6,
                end_line: 10,
                code_content: "function broken() {}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        // External pseudo-node with NULL context_string (should be excluded)
        insert_node(
            conn,
            &NodeRecord {
                file_id: fid_ext,
                node_type: "function".into(),
                name: "ext_func".into(),
                qualified_name: None,
                start_line: 0,
                end_line: 0,
                code_content: "".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        let missing = get_nodes_missing_context(conn).unwrap();
        assert_eq!(
            missing.len(),
            1,
            "should find exactly 1 broken node (not external)"
        );
        assert_eq!(missing[0], broken_id);
    }

    #[test]
    fn test_get_all_node_names_with_ids() {
        let (db, _tmp) = test_db();
        let conn = db.conn();

        // Create 2 files with nodes
        let fid1 = upsert_file(
            conn,
            &FileRecord {
                path: "src/a.ts".into(),
                blake3_hash: "h1".into(),
                last_modified: 1,
                language: None,
            },
        )
        .unwrap();
        let fid2 = upsert_file(
            conn,
            &FileRecord {
                path: "src/b.ts".into(),
                blake3_hash: "h2".into(),
                last_modified: 1,
                language: None,
            },
        )
        .unwrap();

        let nid1 = insert_node(
            conn,
            &NodeRecord {
                file_id: fid1,
                node_type: "function".into(),
                name: "alpha".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 5,
                code_content: "fn alpha(){}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        let nid2 = insert_node(
            conn,
            &NodeRecord {
                file_id: fid2,
                node_type: "function".into(),
                name: "beta".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 5,
                code_content: "fn beta(){}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        // Same name in different file
        let nid3 = insert_node(
            conn,
            &NodeRecord {
                file_id: fid2,
                node_type: "function".into(),
                name: "alpha".into(),
                qualified_name: None,
                start_line: 6,
                end_line: 10,
                code_content: "fn alpha(){}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

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
        let early = upsert_file(
            db.conn(),
            &FileRecord {
                path: "a/early.rs".into(),
                blake3_hash: "h1".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();
        let late = upsert_file(
            db.conn(),
            &FileRecord {
                path: "z/late.rs".into(),
                blake3_hash: "h2".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();

        // Uncalled Result-fn in alphabetically-first file
        let cold = insert_node(
            db.conn(),
            &NodeRecord {
                file_id: early,
                node_type: "function".into(),
                name: "cold_fn".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 3,
                code_content: "fn cold_fn() -> Result<()> {}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: Some("Result<()>".into()),
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        // Hot Result-fn in alphabetically-last file, called 3×
        let hot = insert_node(
            db.conn(),
            &NodeRecord {
                file_id: late,
                node_type: "function".into(),
                name: "hot_fn".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 3,
                code_content: "fn hot_fn() -> Result<i32> {}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: Some("Result<i32>".into()),
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        for i in 0..3 {
            let caller = insert_node(
                db.conn(),
                &NodeRecord {
                    file_id: early,
                    node_type: "function".into(),
                    name: format!("caller_{}", i),
                    qualified_name: None,
                    start_line: 10 + i as i64,
                    end_line: 12 + i as i64,
                    code_content: "fn c() {}".into(),
                    signature: None,
                    doc_comment: None,
                    context_string: None,
                    name_tokens: None,
                    return_type: None,
                    param_types: None,
                    is_test: false,
                },
            )
            .unwrap();
            insert_edge(db.conn(), caller, hot, "calls", None).unwrap();
        }
        insert_edge(db.conn(), cold, cold, "calls", None).unwrap(); // self-loop still = 1 caller

        let types: &[&str] = &["function"];
        let results =
            get_nodes_with_files_by_filters(db.conn(), Some(types), Some("Result"), None, None, 10)
                .unwrap();

        assert_eq!(
            results[0].node.id, hot,
            "hot_fn (3 callers) must outrank cold_fn (1)"
        );
        assert_eq!(results[0].file_path, "z/late.rs");
        assert_eq!(results[1].node.id, cold);

        // With limit=1, hot_fn still wins even though alphabetically-first file exists
        let top1 =
            get_nodes_with_files_by_filters(db.conn(), Some(types), Some("Result"), None, None, 1)
                .unwrap();
        assert_eq!(top1.len(), 1);
        assert_eq!(
            top1[0].node.id, hot,
            "limit=1 with alphabetical ORDER BY would return cold_fn — regression guard"
        );
    }

    #[test]
    fn test_get_nodes_with_files_by_filters_excludes_test_sources_in_ranking() {
        // Aligned with project_map hot_functions / get_module_exports caller_count:
        // ranking subquery must filter test/bench source nodes so test-only utility
        // wrappers (e.g. extract_relations 0 prod / 64 test) don't out-rank prod hot fns.
        let (db, _tmp) = test_db();
        let prod_file = upsert_file(
            db.conn(),
            &FileRecord {
                path: "src/prod.rs".into(),
                blake3_hash: "h1".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();
        let test_file = upsert_file(
            db.conn(),
            &FileRecord {
                path: "src/prod_tests.rs".into(),
                blake3_hash: "h2".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();
        let bench_file = upsert_file(
            db.conn(),
            &FileRecord {
                path: "benches/foo.rs".into(),
                blake3_hash: "h3".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();
        let integ_file = upsert_file(
            db.conn(),
            &FileRecord {
                path: "tests/integration.rs".into(),
                blake3_hash: "h4".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();

        // Target #1: real prod hot fn (1 prod caller)
        let real_hot = insert_node(
            db.conn(),
            &NodeRecord {
                file_id: prod_file,
                node_type: "function".into(),
                name: "real_hot".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 3,
                code_content: "fn real_hot() -> Result<()> {}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: Some("Result<()>".into()),
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        // Target #2: test-only wrapper (0 prod, 4 test/bench callers)
        let fake_hot = insert_node(
            db.conn(),
            &NodeRecord {
                file_id: prod_file,
                node_type: "function".into(),
                name: "fake_hot".into(),
                qualified_name: None,
                start_line: 5,
                end_line: 7,
                code_content: "fn fake_hot() -> Result<()> {}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: Some("Result<()>".into()),
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        // 1 prod caller for real_hot
        let prod_caller = insert_node(
            db.conn(),
            &NodeRecord {
                file_id: prod_file,
                node_type: "function".into(),
                name: "prod_caller".into(),
                qualified_name: None,
                start_line: 10,
                end_line: 12,
                code_content: "fn prod_caller(){}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        insert_edge(db.conn(), prod_caller, real_hot, "calls", None).unwrap();

        // 4 callers for fake_hot — all test/bench sources
        let inline_test = insert_node(
            db.conn(),
            &NodeRecord {
                file_id: prod_file,
                node_type: "function".into(),
                name: "inline_test".into(),
                qualified_name: None,
                start_line: 20,
                end_line: 22,
                code_content: "fn inline_test(){}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: true, // AST-flag inline test
            },
        )
        .unwrap();
        let test_prefix = insert_node(
            db.conn(),
            &NodeRecord {
                file_id: test_file,
                node_type: "function".into(),
                name: "test_foo".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 3,
                code_content: "fn test_foo(){}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        let bench_caller = insert_node(
            db.conn(),
            &NodeRecord {
                file_id: bench_file,
                node_type: "function".into(),
                name: "bench_foo".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 3,
                code_content: "fn bench_foo(){}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        let integ_caller = insert_node(
            db.conn(),
            &NodeRecord {
                file_id: integ_file,
                node_type: "function".into(),
                name: "verify_path".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 3,
                code_content: "fn verify_path(){}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        insert_edge(db.conn(), inline_test, fake_hot, "calls", None).unwrap();
        insert_edge(db.conn(), test_prefix, fake_hot, "calls", None).unwrap();
        insert_edge(db.conn(), bench_caller, fake_hot, "calls", None).unwrap();
        insert_edge(db.conn(), integ_caller, fake_hot, "calls", None).unwrap();

        let types: &[&str] = &["function"];
        let results =
            get_nodes_with_files_by_filters(db.conn(), Some(types), Some("Result"), None, None, 10)
                .unwrap();

        // Both targets returned but real_hot (1 prod) outranks fake_hot (4 test/bench)
        let real_pos = results
            .iter()
            .position(|nf| nf.node.id == real_hot)
            .expect("real_hot must appear");
        let fake_pos = results
            .iter()
            .position(|nf| nf.node.id == fake_hot)
            .expect("fake_hot must appear");
        assert!(
            real_pos < fake_pos,
            "real_hot (1 prod caller) must rank above fake_hot (4 test/bench callers); \
             got positions real={} fake={}",
            real_pos,
            fake_pos,
        );
    }

    /// Regression (real-user QA): ast_search's filter-only path (this fn) leaked the
    /// <module>/<external> placeholder nodes and test symbols into structural results
    /// — an `<external>:0-0` stub and `<module>` file nodes surfaced alongside real
    /// symbols, unlike search/similar. Now always excluded here, covering all three
    /// legs: the `<module>` placeholder, the `<external>` path, and tests via both the
    /// name/path heuristic AND the stored is_test flag.
    #[test]
    fn test_get_nodes_with_files_by_filters_excludes_module_external_test() {
        let (db, _tmp) = test_db();
        let src = upsert_file(
            db.conn(),
            &FileRecord {
                path: "src/lib.rs".into(),
                blake3_hash: "h1".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();
        let ext = upsert_file(
            db.conn(),
            &FileRecord {
                path: "<external>".into(),
                blake3_hash: "h2".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();
        let tests = upsert_file(
            db.conn(),
            &FileRecord {
                path: "tests/mod_test.rs".into(),
                blake3_hash: "h3".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();
        let mk = |file_id: i64, name: &str, ty: &str, is_test: bool| {
            insert_node(
                db.conn(),
                &NodeRecord {
                    file_id,
                    node_type: ty.into(),
                    name: name.into(),
                    qualified_name: None,
                    start_line: 1,
                    end_line: 1,
                    code_content: String::new(),
                    signature: None,
                    doc_comment: None,
                    context_string: None,
                    name_tokens: None,
                    return_type: None,
                    param_types: None,
                    is_test,
                },
            )
            .unwrap()
        };
        mk(src, "module_loader", "function", false); // the ONLY keeper
        mk(src, "<module>", "module", false); // <module> placeholder
        mk(ext, "modulejs", "external_module", false); // <external> stub (path leg)
        mk(tests, "test_module_helper", "function", false); // test_ name + tests/ path
        mk(src, "InlineModuleTest", "function", true); // AST is_test flag leg

        // name LIKE %module% matches ALL five; only the prod fn must survive.
        let r = get_nodes_with_files_by_filters(db.conn(), None, None, None, Some("module"), 20)
            .unwrap();
        let names: Vec<&str> = r.iter().map(|nwf| nwf.node.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["module_loader"],
            "only the real prod symbol survives; <module>/<external>/test excluded, got: {names:?}"
        );
    }

    /// `name_filter` does case-insensitive substring on n.name. Underwrites the
    /// ast_search FTS-rank fallback (query="Result" type=struct must surface
    /// IndexResult/CallGraphResult/etc instead of zero hits because top FTS
    /// hits for "Result" are functions like `compress_results`).
    #[test]
    fn test_get_nodes_with_files_by_filters_name_filter() {
        let (db, _tmp) = test_db();
        let file_id = upsert_file(
            db.conn(),
            &FileRecord {
                path: "src/lib.rs".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();

        let mk = |name: &str, ty: &str| -> i64 {
            insert_node(
                db.conn(),
                &NodeRecord {
                    file_id,
                    node_type: ty.into(),
                    name: name.into(),
                    qualified_name: None,
                    start_line: 1,
                    end_line: 1,
                    code_content: String::new(),
                    signature: None,
                    doc_comment: None,
                    context_string: None,
                    name_tokens: None,
                    return_type: None,
                    param_types: None,
                    is_test: false,
                },
            )
            .unwrap()
        };
        let idx_struct = mk("IndexResult", "struct");
        let cg_struct = mk("CallGraphResult", "struct");
        let _fn1 = mk("compress_results", "function");
        let _other = mk("FooBar", "struct");

        let struct_types: &[&str] = &["struct"];
        let r = get_nodes_with_files_by_filters(
            db.conn(),
            Some(struct_types),
            None,
            None,
            Some("Result"),
            10,
        )
        .unwrap();
        let ids: Vec<i64> = r.iter().map(|nwf| nwf.node.id).collect();
        assert!(ids.contains(&idx_struct));
        assert!(ids.contains(&cg_struct));
        assert_eq!(r.len(), 2, "name LIKE %Result% under type=struct must match exactly 2 structs (FooBar excluded, compress_results excluded by type)");

        // Case-insensitive
        let r_lower = get_nodes_with_files_by_filters(
            db.conn(),
            Some(struct_types),
            None,
            None,
            Some("result"),
            10,
        )
        .unwrap();
        assert_eq!(r_lower.len(), 2, "name_filter must be case-insensitive");

        // type=function + same name_filter excludes structs
        let fn_types: &[&str] = &["function"];
        let r_fn = get_nodes_with_files_by_filters(
            db.conn(),
            Some(fn_types),
            None,
            None,
            Some("Result"),
            10,
        )
        .unwrap();
        assert_eq!(
            r_fn.len(),
            1,
            "type=function + name=Result matches only compress_results"
        );
    }

    /// Regression: LIKE-escape helpers must escape the backslash ITSELF before the
    /// `%`/`_` metachars. Under `ESCAPE '\'` a literal `\` in the query is the escape
    /// char, so an unescaped `a\b` pattern degrades to `ab` (the `\b` escapes `b` to a
    /// literal), and a trailing `\` escapes the closing wildcard — wrong matches both
    /// ways. `escape_like` prepends `\` → `\\`, restoring literal semantics. This path
    /// (`get_nodes_with_files_by_filters` name_filter) is pure LIKE with no fuzzy
    /// fallback, so the assertions isolate the escape behavior.
    #[test]
    fn test_get_nodes_with_files_by_filters_name_filter_escapes_backslash() {
        let (db, _tmp) = test_db();
        let file_id = upsert_file(
            db.conn(),
            &FileRecord {
                path: "src/lib.rs".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();

        let mk = |name: &str| -> i64 {
            insert_node(
                db.conn(),
                &NodeRecord {
                    file_id,
                    node_type: "function".into(),
                    name: name.into(),
                    qualified_name: None,
                    start_line: 1,
                    end_line: 1,
                    code_content: String::new(),
                    signature: None,
                    doc_comment: None,
                    context_string: None,
                    name_tokens: None,
                    return_type: None,
                    param_types: None,
                    is_test: false,
                },
            )
            .unwrap()
        };
        let backslash_node = mk("a\\b"); // literal name: a, backslash, b
        let plain_node = mk("ab");
        let trailing_node = mk("x\\"); // literal name: x, backslash

        let fn_types: &[&str] = &["function"];

        // Query `a\b` must match the literal `a\b` node and NOT `ab`. Pre-fix the
        // pattern degraded to `%a\b%` where `\b` = literal `b`, matching `ab` instead.
        let r = get_nodes_with_files_by_filters(
            db.conn(),
            Some(fn_types),
            None,
            None,
            Some("a\\b"),
            10,
        )
        .unwrap();
        let ids: Vec<i64> = r.iter().map(|nwf| nwf.node.id).collect();
        assert!(
            ids.contains(&backslash_node),
            "query `a\\b` must match the literal `a\\b` node; got ids {ids:?}"
        );
        assert!(!ids.contains(&plain_node),
            "query `a\\b` must NOT match `ab` (the backslash is a literal, not an escape); got ids {ids:?}");

        // Trailing backslash: query `x\` must match `x\`. Pre-fix `%x\%` escaped the
        // closing wildcard (`\%` = literal `%`) and matched nothing.
        let r_tail =
            get_nodes_with_files_by_filters(db.conn(), Some(fn_types), None, None, Some("x\\"), 10)
                .unwrap();
        let tail_ids: Vec<i64> = r_tail.iter().map(|nwf| nwf.node.id).collect();
        assert!(tail_ids.contains(&trailing_node),
            "query `x\\` (trailing backslash) must match the literal `x\\` node; got ids {tail_ids:?}");

        // Control: a wildcard-free query `ab` matches only `ab`, never `a\b`.
        let r_plain =
            get_nodes_with_files_by_filters(db.conn(), Some(fn_types), None, None, Some("ab"), 10)
                .unwrap();
        let plain_ids: Vec<i64> = r_plain.iter().map(|nwf| nwf.node.id).collect();
        assert!(
            plain_ids.contains(&plain_node) && !plain_ids.contains(&backslash_node),
            "query `ab` must match `ab` and not `a\\b`; got ids {plain_ids:?}"
        );
    }
}
