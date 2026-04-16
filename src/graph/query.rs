use anyhow::{anyhow, Result};
use rusqlite::Connection;
use std::collections::HashMap;

use crate::domain::REL_CALLS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Callees,
    Callers,
}

impl Direction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Direction::Callees => "callees",
            Direction::Callers => "callers",
        }
    }
}

/// A node in a call graph traversal result.
pub struct CallGraphNode {
    pub node_id: i64,
    pub name: String,
    pub node_type: String,
    pub file_path: String,
    pub depth: i32,
    pub direction: Direction,
}

/// Traverse the call graph starting from a function by name.
///
/// `direction` must be one of: "callers", "callees", "both".
/// `depth` controls the maximum recursion depth.
/// `file_path` optionally disambiguates when multiple functions share the same name.
pub fn get_call_graph(
    conn: &Connection,
    function_name: &str,
    direction: &str,
    max_depth: i32,
    file_path: Option<&str>,
) -> Result<Vec<CallGraphNode>> {
    match direction {
        "callees" => query_direction(conn, function_name, max_depth, file_path, Direction::Callees),
        "callers" => query_direction(conn, function_name, max_depth, file_path, Direction::Callers),
        "both" => {
            let callees = query_direction(conn, function_name, max_depth, file_path, Direction::Callees)?;
            let callers = query_direction(conn, function_name, max_depth, file_path, Direction::Callers)?;
            Ok(merge_results(callees, callers))
        }
        other => Err(anyhow!("invalid direction '{}': must be callers, callees, or both", other)),
    }
}

fn query_direction(
    conn: &Connection,
    function_name: &str,
    max_depth: i32,
    file_path: Option<&str>,
    direction: Direction,
) -> Result<Vec<CallGraphNode>> {
    let max_depth = max_depth.min(10); // Hard cap to prevent CTE blowup on highly connected graphs
    // Use NULL sentinel: when file_path is None, pass NULL and the filter is always true
    let file_filter = "AND (?2 IS NULL OR f.path = ?2)";
    let file_path_param: Option<&str> = file_path;

    // In the recursive step:
    // - callees: follow edges forward (source_id = current, target_id = next)
    // - callers: follow edges backward (target_id = current, source_id = next)
    let (edge_join, next_node_join) = match direction {
        Direction::Callees => (
            "JOIN edges e ON e.source_id = cg.node_id AND e.relation = ?4",
            "JOIN nodes t ON t.id = e.target_id",
        ),
        Direction::Callers => (
            "JOIN edges e ON e.target_id = cg.node_id AND e.relation = ?4",
            "JOIN nodes t ON t.id = e.source_id",
        ),
    };

    let sql = format!(
        "WITH RECURSIVE call_graph(node_id, name, type, depth, visited) AS (
            SELECT n.id, n.name, n.type, 0, CAST(n.id AS TEXT)
            FROM nodes n
            JOIN files f ON f.id = n.file_id
            WHERE n.name = ?1
            {file_filter}

            UNION ALL

            SELECT t.id, t.name, t.type, cg.depth + 1,
                   cg.visited || ',' || CAST(t.id AS TEXT)
            FROM call_graph cg
            {edge_join}
            {next_node_join}
            WHERE cg.depth < ?3
            AND (',' || cg.visited || ',') NOT LIKE '%,' || CAST(t.id AS TEXT) || ',%'
        )
        SELECT DISTINCT cg.node_id, cg.name, cg.type, f.path, MIN(cg.depth) as depth
        FROM call_graph cg
        JOIN nodes n ON n.id = cg.node_id
        JOIN files f ON f.id = n.file_id
        GROUP BY cg.node_id
        ORDER BY depth
        LIMIT 200"
    );

    let mut stmt = conn.prepare(&sql)?;

    let map_row = move |row: &rusqlite::Row<'_>| -> rusqlite::Result<CallGraphNode> {
        Ok(CallGraphNode {
            node_id: row.get(0)?,
            name: row.get(1)?,
            node_type: row.get(2)?,
            file_path: row.get(3)?,
            depth: row.get(4)?,
            direction,
        })
    };

    let results: Vec<CallGraphNode> = stmt
        .query_map(rusqlite::params![function_name, file_path_param, max_depth, REL_CALLS], map_row)?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(results)
}

/// Merge callee and caller results, deduplicating by (node_id, direction) and keeping the minimum depth.
fn merge_results(callees: Vec<CallGraphNode>, callers: Vec<CallGraphNode>) -> Vec<CallGraphNode> {
    let mut by_key: HashMap<(i64, Direction), CallGraphNode> = HashMap::new();

    for node in callees.into_iter().chain(callers) {
        let key = (node.node_id, node.direction);
        by_key
            .entry(key)
            .and_modify(|existing| {
                if node.depth < existing.depth {
                    existing.depth = node.depth;
                }
            })
            .or_insert(node);
    }

    let mut results: Vec<CallGraphNode> = by_key.into_values().collect();
    results.sort_by_key(|n| n.depth);
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db::Database;
    use crate::storage::queries::{upsert_file, insert_node, insert_edge, FileRecord, NodeRecord};
    use crate::domain::REL_CALLS;
    use tempfile::TempDir;

    fn test_db() -> (Database, TempDir) {
        let tmp = TempDir::new().unwrap();
        let db = Database::open(&tmp.path().join("test.db")).unwrap();
        (db, tmp)
    }

    fn node(name: &str, file_id: i64) -> NodeRecord {
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
            name_tokens: None,
            return_type: None,
            param_types: None,
            is_test: false,
        }
    }

    /// Setup: A→calls→B→calls→C, D→calls→B
    /// Query callees of A depth 2 → should contain B and C
    #[test]
    fn test_get_callees() {
        let (db, _tmp) = test_db();
        let conn = db.conn();

        let fid = upsert_file(conn, &FileRecord {
            path: "test.ts".into(),
            blake3_hash: "h1".into(),
            last_modified: 1,
            language: Some("typescript".into()),
        }).unwrap();

        let a = insert_node(conn, &node("A", fid)).unwrap();
        let b = insert_node(conn, &node("B", fid)).unwrap();
        let c = insert_node(conn, &node("C", fid)).unwrap();
        let d = insert_node(conn, &node("D", fid)).unwrap();

        insert_edge(conn, a, b, REL_CALLS, None).unwrap();
        insert_edge(conn, b, c, REL_CALLS, None).unwrap();
        insert_edge(conn, d, b, REL_CALLS, None).unwrap();

        let result = get_call_graph(conn, "A", "callees", 2, None).unwrap();

        // Should include A (depth 0), B (depth 1), C (depth 2)
        let names: Vec<&str> = result.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"A"), "should contain root node A");
        assert!(names.contains(&"B"), "should contain callee B");
        assert!(names.contains(&"C"), "should contain callee C");
        assert!(!names.contains(&"D"), "should NOT contain D (not a callee of A)");

        // Verify depths
        let a_node = result.iter().find(|n| n.name == "A").unwrap();
        assert_eq!(a_node.depth, 0);
        let b_node = result.iter().find(|n| n.name == "B").unwrap();
        assert_eq!(b_node.depth, 1);
        let c_node = result.iter().find(|n| n.name == "C").unwrap();
        assert_eq!(c_node.depth, 2);
    }

    /// Query callers of B depth 2 → should contain A and D
    #[test]
    fn test_get_callers() {
        let (db, _tmp) = test_db();
        let conn = db.conn();

        let fid = upsert_file(conn, &FileRecord {
            path: "test.ts".into(),
            blake3_hash: "h1".into(),
            last_modified: 1,
            language: Some("typescript".into()),
        }).unwrap();

        let a = insert_node(conn, &node("A", fid)).unwrap();
        let b = insert_node(conn, &node("B", fid)).unwrap();
        let c = insert_node(conn, &node("C", fid)).unwrap();
        let d = insert_node(conn, &node("D", fid)).unwrap();

        insert_edge(conn, a, b, REL_CALLS, None).unwrap();
        insert_edge(conn, b, c, REL_CALLS, None).unwrap();
        insert_edge(conn, d, b, REL_CALLS, None).unwrap();

        let result = get_call_graph(conn, "B", "callers", 2, None).unwrap();

        let names: Vec<&str> = result.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"B"), "should contain root node B");
        assert!(names.contains(&"A"), "should contain caller A");
        assert!(names.contains(&"D"), "should contain caller D");
        assert!(!names.contains(&"C"), "should NOT contain C (C is a callee, not caller)");

        // Verify depths
        let b_node = result.iter().find(|n| n.name == "B").unwrap();
        assert_eq!(b_node.depth, 0);
        let a_node = result.iter().find(|n| n.name == "A").unwrap();
        assert_eq!(a_node.depth, 1);
        let d_node = result.iter().find(|n| n.name == "D").unwrap();
        assert_eq!(d_node.depth, 1);
    }

    /// A→B→A mutual recursion. Query callees of A depth 10 → should terminate with <=3 results.
    #[test]
    fn test_cycle_detection() {
        let (db, _tmp) = test_db();
        let conn = db.conn();

        let fid = upsert_file(conn, &FileRecord {
            path: "test.ts".into(),
            blake3_hash: "h1".into(),
            last_modified: 1,
            language: Some("typescript".into()),
        }).unwrap();

        let a = insert_node(conn, &node("A", fid)).unwrap();
        let b = insert_node(conn, &node("B", fid)).unwrap();

        insert_edge(conn, a, b, REL_CALLS, None).unwrap();
        insert_edge(conn, b, a, REL_CALLS, None).unwrap();

        let result = get_call_graph(conn, "A", "callees", 10, None).unwrap();

        // Should terminate and contain at most A and B
        assert!(result.len() <= 2, "cycle detection should limit results to <=2, got {}", result.len());

        let names: Vec<&str> = result.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"A"));
        assert!(names.contains(&"B"));
    }

    /// Query "both" on B → should contain A, D (callers) and C (callees)
    #[test]
    fn test_both_direction() {
        let (db, _tmp) = test_db();
        let conn = db.conn();

        let fid = upsert_file(conn, &FileRecord {
            path: "test.ts".into(),
            blake3_hash: "h1".into(),
            last_modified: 1,
            language: Some("typescript".into()),
        }).unwrap();

        let a = insert_node(conn, &node("A", fid)).unwrap();
        let b = insert_node(conn, &node("B", fid)).unwrap();
        let c = insert_node(conn, &node("C", fid)).unwrap();
        let d = insert_node(conn, &node("D", fid)).unwrap();

        insert_edge(conn, a, b, REL_CALLS, None).unwrap();
        insert_edge(conn, b, c, REL_CALLS, None).unwrap();
        insert_edge(conn, d, b, REL_CALLS, None).unwrap();

        let result = get_call_graph(conn, "B", "both", 2, None).unwrap();

        let names: Vec<&str> = result.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"B"), "should contain root node B");
        assert!(names.contains(&"A"), "should contain caller A");
        assert!(names.contains(&"D"), "should contain caller D");
        assert!(names.contains(&"C"), "should contain callee C");

        // B should be at depth 0
        let b_node = result.iter().find(|n| n.name == "B").unwrap();
        assert_eq!(b_node.depth, 0);
    }
}
