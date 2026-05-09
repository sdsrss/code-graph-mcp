use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashMap;

use super::helpers::{make_placeholders, MAX_IN_PARAMS};

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

    if callers.nodes.is_empty() {
        return Ok(vec![]);
    }

    // Batch fetch route metadata for all callers (avoids N+1 queries)
    let mut route_map: HashMap<i64, String> = HashMap::new();
    let caller_ids: Vec<i64> = callers.nodes.iter().map(|c| c.node_id).collect();
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
        .nodes
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
    pub start_line: i64,
    pub end_line: i64,
}

/// Get all exported symbols from files under a directory prefix.
/// For JS/TS, uses explicit `exports` edges. For other languages (Rust, Go, Python, etc.),
/// falls back to returning all named top-level symbols (functions, structs, classes, etc.).
pub fn get_module_exports(conn: &Connection, dir_prefix: &str) -> Result<Vec<ModuleExport>> {
    use crate::domain::{REL_EXPORTS, REL_CALLS};
    let escaped_prefix = dir_prefix.replace('%', "\\%").replace('_', "\\_");
    let prefix_pattern = format!("{}%", escaped_prefix);

    // Phase 1: Try explicit exports (JS/TS)
    // Filter n.is_test=0 — AST-level flag catches inline `#[cfg(test)] mod tests`
    // whose names don't match the name-heuristic in is_test_symbol.
    let sql_exports =
        "SELECT DISTINCT n.id, n.name, n.type, n.signature, f.path,
                COALESCE(cc.cnt, 0) as caller_count,
                n.start_line, n.end_line
         FROM nodes n
         JOIN files f ON f.id = n.file_id
         JOIN edges e ON e.target_id = n.id AND e.relation = ?1
         LEFT JOIN (SELECT target_id, COUNT(*) as cnt FROM edges WHERE relation = ?3 GROUP BY target_id) cc
           ON cc.target_id = n.id
         WHERE f.path LIKE ?2 ESCAPE '\\'
           AND n.is_test = 0
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
            start_line: row.get(6)?,
            end_line: row.get(7)?,
        })
    })?;
    let results: Vec<ModuleExport> = rows.collect::<std::result::Result<Vec<_>, _>>()?;

    if !results.is_empty() {
        return Ok(results);
    }

    // Phase 2: Fallback for non-JS/TS — all named top-level symbols in matching files
    let sql_fallback =
        "SELECT DISTINCT n.id, n.name, n.type, n.signature, f.path,
                COALESCE(cc.cnt, 0) as caller_count,
                n.start_line, n.end_line
         FROM nodes n
         JOIN files f ON f.id = n.file_id
         LEFT JOIN (SELECT target_id, COUNT(*) as cnt FROM edges WHERE relation = ?2 GROUP BY target_id) cc
           ON cc.target_id = n.id
         WHERE f.path LIKE ?1 ESCAPE '\\'
           AND n.type != 'module'
           AND n.name != '<module>'
           AND n.is_test = 0
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
            start_line: row.get(6)?,
            end_line: row.get(7)?,
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

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::helpers::test_db;

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
    fn test_get_module_exports_filters_is_test_nodes() {
        // Rust fallback path: inline `#[cfg(test)] mod tests { #[test] fn foo }`
        // whose names don't prefix-match `test_` must still be excluded via the
        // AST-level n.is_test flag. See feedback_test_filter_propagation.md.
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute(
            "INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at)
             VALUES ('src/foo.rs', 'h1', 0, 'rust', 0)",
            [],
        ).unwrap();
        // Real export — name doesn't match is_test_symbol heuristic
        conn.execute(
            "INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content, is_test)
             VALUES (1, 'function', 'compute_thing', 'compute_thing', 1, 5, 'fn compute_thing(){}', 0)",
            [],
        ).unwrap();
        // Inline test fn — name doesn't match heuristic either, but is_test=1
        conn.execute(
            "INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content, is_test)
             VALUES (1, 'function', 'arrays_are_homogeneous', 'arrays_are_homogeneous', 10, 20, 'fn arrays_are_homogeneous(){}', 1)",
            [],
        ).unwrap();

        let exports = get_module_exports(conn, "src/foo.rs").unwrap();
        let names: Vec<&str> = exports.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"compute_thing"), "real export missing: {:?}", names);
        assert!(
            !names.contains(&"arrays_are_homogeneous"),
            "is_test=1 node leaked into module exports: {:?}", names,
        );
    }
}
