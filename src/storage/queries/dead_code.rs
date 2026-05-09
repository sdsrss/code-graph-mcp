use anyhow::Result;
use rusqlite::Connection;

/// Result from dead code analysis. Each entry is a node with no incoming usage edges.
#[derive(Debug)]
pub struct DeadCodeResult {
    pub id: i64,
    pub name: String,
    pub node_type: String,
    pub start_line: i64,
    pub end_line: i64,
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
        // Anonymous consts (`const _: () = assert!(...)`) are compile-time checks,
        // never callable; same pattern for anonymous `let _ = ...` bindings.
        "n.name != '_'".to_string(),
        "f.path != '<external>'".to_string(),
        "(n.end_line - n.start_line + 1) >= :min_lines".to_string(),
    ];

    if !include_tests {
        conditions.push("n.is_test = 0".to_string());
    }

    // Track how many type filter placeholders we need
    let normalized_types: Vec<&str> = node_type
        .map(crate::domain::normalize_type_filter)
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
           -- Check if the name appears as a standalone identifier in another
           -- function's code in the same file. Uses delimiter-aware matching
           -- to avoid false matches where the name is a prefix of a longer
           -- identifier (e.g., `get_x` matching inside `get_x_batch`).
           -- This catches references the parser doesn't track as edges:
           --   1. Struct instantiation, type usage
           --   2. Function pointers/callbacks (e.g., `query_map(params, map_fn)`)
           AND (
               length(n.name) < 3
               OR NOT EXISTS (
                   SELECT 1 FROM nodes n2
                   WHERE n2.file_id = n.file_id
                     AND n2.id != n.id
                     AND n2.type IN ('function', 'method')
                     AND (
                         instr(n2.code_content, n.name || '(') > 0
                         OR instr(n2.code_content, n.name || ')') > 0
                         OR instr(n2.code_content, n.name || ',') > 0
                         OR instr(n2.code_content, n.name || ' ') > 0
                         OR instr(n2.code_content, n.name || ';') > 0
                         OR instr(n2.code_content, n.name || char(10)) > 0
                         OR instr(n2.code_content, n.name || ':') > 0
                         OR instr(n2.code_content, n.name || '<') > 0
                         OR instr(n2.code_content, n.name || '.') > 0
                         OR instr(n2.code_content, n.name || '{{') > 0
                         OR instr(n2.code_content, n.name || '}}') > 0
                     )
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
    use super::super::edges::insert_edge;
    use super::super::files::{upsert_file, FileRecord};
    use super::super::helpers::test_db;
    use super::super::nodes::{insert_node, NodeRecord};

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

        // 8. callback_fn — no call edge, but name appears in another function's code
        //    (function pointer passed as argument) → should NOT be dead code
        insert_node(conn, &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "callback_fn".into(),
            qualified_name: None, start_line: 91, end_line: 105,
            code_content: "fn callback_fn(row: &Row) -> Result<Item> { Ok(row.get(0)?) }".into(),
            signature: None, doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();

        // 9. anonymous `_` constant — `const _: () = assert!(...)` is a compile-time
        //    check, never callable. Must be excluded by name filter.
        insert_node(conn, &NodeRecord {
            file_id: fid, node_type: "constant".into(), name: "_".into(),
            qualified_name: None, start_line: 110, end_line: 115,
            code_content: "const _: () = assert!(SOME_CONST <= 1500, \"budget\");".into(),
            signature: None, doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();

        // --- Create edges ---
        // Someone calls used_fn and passes callback_fn as a function pointer
        let caller_id = insert_node(conn, &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "caller".into(),
            qualified_name: None, start_line: 86, end_line: 90,
            code_content: "fn caller() { used_fn(); stmt.query_map(params, callback_fn).unwrap(); }".into(),
            signature: None, doc_comment: None, context_string: None,
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
        assert!(!names.contains(&"callback_fn"), "callback_fn should be excluded (referenced as function pointer in caller's code)");
        assert!(!names.contains(&"_"), "anonymous `_` constant (compile-time assert) should be excluded by name filter");

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
}
