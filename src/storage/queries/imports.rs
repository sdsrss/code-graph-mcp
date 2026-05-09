use anyhow::Result;
use rusqlite::Connection;

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
            "WITH RECURSIVE dep_tree(file_id, file_path, depth, visited_ids) AS (
                -- Seed: the starting file (use file ID for cycle detection to avoid LIKE metacharacter issues)
                SELECT f0.id, f0.path, 0, CAST(f0.id AS TEXT)
                FROM files f0 WHERE f0.path = ?2

                UNION ALL

                -- Recurse: find files that the current-depth files depend on
                SELECT DISTINCT f2.id, f2.path, dt.depth + 1,
                       dt.visited_ids || '|' || CAST(f2.id AS TEXT)
                FROM dep_tree dt
                JOIN nodes n1 ON n1.file_id = dt.file_id
                JOIN edges e ON e.source_id = n1.id AND e.relation IN (?1, ?3)
                JOIN nodes n2 ON n2.id = e.target_id
                JOIN files f2 ON f2.id = n2.file_id
                WHERE dt.depth < ?4
                  AND f2.path != ?2
                  AND ('|' || dt.visited_ids || '|') NOT LIKE '%|' || CAST(f2.id AS TEXT) || '|%'
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
            "WITH RECURSIVE dep_tree(file_id, file_path, depth, visited_ids) AS (
                SELECT f0.id, f0.path, 0, CAST(f0.id AS TEXT)
                FROM files f0 WHERE f0.path = ?2

                UNION ALL

                SELECT DISTINCT f1.id, f1.path, dt.depth + 1,
                       dt.visited_ids || '|' || CAST(f1.id AS TEXT)
                FROM dep_tree dt
                JOIN nodes n2 ON n2.file_id = dt.file_id
                JOIN edges e ON e.target_id = n2.id AND e.relation IN (?1, ?3)
                JOIN nodes n1 ON n1.id = e.source_id
                JOIN files f1 ON f1.id = n1.file_id
                WHERE dt.depth < ?4
                  AND f1.path != ?2
                  AND ('|' || dt.visited_ids || '|') NOT LIKE '%|' || CAST(f1.id AS TEXT) || '|%'
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

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::helpers::test_db;

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
}
