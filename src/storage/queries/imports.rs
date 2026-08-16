use anyhow::Result;
use rusqlite::Connection;
use std::collections::{BTreeMap, HashSet};

#[derive(Debug)]
pub struct FileDependency {
    pub file_path: String,
    pub direction: String, // "outgoing" (this file imports) or "incoming" (imports this file)
    pub symbol_count: i64,
    pub depth: i32,
}

/// Which way a file-level hop follows an edge.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FileHop {
    /// Edge source is in the current file → the files it depends on.
    Outgoing,
    /// Edge target is in the current file → the files that depend on it.
    Incoming,
}

/// Max file ids bound into one frontier query. Keeps the `IN (...)` list well
/// under SQLite's variable cap on a repo whose frontier is thousands of files.
const FRONTIER_CHUNK: usize = 400;

/// Breadth-first file-level dependency closure from `root_path`, following
/// `relations` in the direction given by `hop`, out to `max_depth` hops.
///
/// Replaces the `WITH RECURSIVE … visited_ids NOT LIKE '%|id|%'` construct that
/// used to drive these traversals. That guard is *per path*, so the CTE
/// enumerated every simple path and only collapsed them at the final
/// `GROUP BY … MIN(depth)`; on a densely connected repo the row count grows
/// exponentially with depth (measured: `affected src/domain.rs` took 14.7s at
/// depth 10 vs 1.0s at depth 6 for an identical result set). One SQL query per
/// BFS level with a GLOBAL visited set visits each file once, so the cost is
/// linear in edges regardless of depth.
///
/// Output is identical to the CTE's: BFS assigns each file the shortest hop
/// distance, which is exactly what `MIN(dt.depth)` computed. Returns
/// `(file_path, shortest_depth)` sorted by `(depth ASC, path ASC)`; the root
/// path itself is excluded (matching the CTE's `f.path != root`).
fn file_closure(
    conn: &Connection,
    root_path: &str,
    max_depth: i32,
    relations: &[&str],
    hop: FileHop,
) -> Result<Vec<(String, i32)>> {
    // Relation IN-list built from trusted constants (no user input → no injection).
    let in_list = relations
        .iter()
        .map(|r| format!("'{r}'"))
        .collect::<Vec<_>>()
        .join(", ");
    // `n_from` is the node on the frontier side of the edge, `n_to` the node on
    // the side we are expanding towards; the edge column each binds to is what
    // makes the hop outgoing or incoming.
    let (from_col, to_col) = match hop {
        FileHop::Outgoing => ("source_id", "target_id"),
        FileHop::Incoming => ("target_id", "source_id"),
    };

    let mut frontier: Vec<i64> = {
        let mut stmt = conn.prepare("SELECT id FROM files WHERE path = ?1 ORDER BY id")?;
        let rows = stmt.query_map(rusqlite::params![root_path], |row| row.get::<_, i64>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    if frontier.is_empty() {
        return Ok(Vec::new());
    }
    let mut visited: HashSet<i64> = frontier.iter().copied().collect();
    // path → shortest depth, standing in for the CTE's `GROUP BY dt.file_path`.
    // `files.path` is UNIQUE, so grouping never actually merges two ids; keeping
    // the map keyed by path (rather than by id) is what makes the result
    // insensitive to that assumption.
    let mut found: BTreeMap<String, i32> = BTreeMap::new();

    let mut depth = 1;
    while depth <= max_depth && !frontier.is_empty() {
        let mut next: Vec<i64> = Vec::new();
        for chunk in frontier.chunks(FRONTIER_CHUNK) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT DISTINCT f_to.id, f_to.path
                 FROM nodes n_from
                 JOIN edges e ON e.{from_col} = n_from.id AND e.relation IN ({in_list})
                 JOIN nodes n_to ON n_to.id = e.{to_col}
                 JOIN files f_to ON f_to.id = n_to.file_id
                 WHERE n_from.file_id IN ({placeholders})"
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
                let (file_id, path) = row?;
                if path == root_path || !visited.insert(file_id) {
                    continue;
                }
                found
                    .entry(path)
                    .and_modify(|d| {
                        if depth < *d {
                            *d = depth
                        }
                    })
                    .or_insert(depth);
                next.push(file_id);
            }
        }
        next.sort_unstable();
        frontier = next;
        depth += 1;
    }

    // BTreeMap iterates path-ascending; a STABLE sort by depth then yields
    // (depth ASC, path ASC) — the CTE's `ORDER BY min_depth, file_path`.
    let mut out: Vec<(String, i32)> = found.into_iter().collect();
    out.sort_by_key(|(_, d)| *d);
    Ok(out)
}

/// Cross-file symbol counts between `root_path` and every other file, over
/// `relations`. `hop` picks the side the root sits on: `Outgoing` counts
/// distinct symbols the root's nodes point AT (keyed by the target's file),
/// `Incoming` counts distinct symbols pointed at IN the root (keyed by the
/// source's file). One grouped query replaces the per-row correlated subquery
/// the CTE ran for every result file.
fn cross_file_symbol_counts(
    conn: &Connection,
    root_path: &str,
    relations: &[&str],
    hop: FileHop,
) -> Result<BTreeMap<String, i64>> {
    let in_list = relations
        .iter()
        .map(|r| format!("'{r}'"))
        .collect::<Vec<_>>()
        .join(", ");
    // The counted symbol is always the edge TARGET (`nb`), as in the CTE's
    // `COUNT(DISTINCT nb.id)`; only which end is pinned to the root changes.
    let (pinned, grouped) = match hop {
        FileHop::Outgoing => ("fa", "fb"),
        FileHop::Incoming => ("fb", "fa"),
    };
    let sql = format!(
        "SELECT {grouped}.path, COUNT(DISTINCT nb.id)
         FROM nodes na
         JOIN files fa ON fa.id = na.file_id
         JOIN edges ea ON ea.source_id = na.id AND ea.relation IN ({in_list})
         JOIN nodes nb ON nb.id = ea.target_id
         JOIN files fb ON fb.id = nb.file_id
         WHERE {pinned}.path = ?1
         GROUP BY {grouped}.path"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![root_path], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut out = BTreeMap::new();
    for row in rows {
        let (path, count) = row?;
        out.insert(path, count);
    }
    Ok(out)
}

/// Get file-level import/export dependencies with breadth-first depth traversal.
/// direction: "outgoing" (what this file depends on), "incoming" (what depends on this file), "both"
///
/// Both directions share one traversal ([`file_closure`]) parameterised by hop
/// direction; they used to be two hand-mirrored recursive CTEs.
pub fn get_import_tree(
    conn: &Connection,
    file_path: &str,
    direction: &str,
    max_depth: i32,
) -> Result<Vec<FileDependency>> {
    use crate::domain::{REL_CALLS, REL_IMPORTS};
    if !matches!(direction, "outgoing" | "incoming" | "both") {
        anyhow::bail!(
            "invalid direction '{}': expected outgoing, incoming, or both",
            direction
        );
    }
    let max_depth = max_depth.clamp(1, 10);
    // Dependency-graph view: a file "depends on" another when it imports from it
    // or calls into it. (`affected` deliberately walks a wider relation set —
    // see `get_reverse_dependents`.)
    let relations = [REL_IMPORTS, REL_CALLS];
    let mut results = Vec::new();

    for (dir_name, hop) in [
        ("outgoing", FileHop::Outgoing),
        ("incoming", FileHop::Incoming),
    ] {
        if direction != dir_name && direction != "both" {
            continue;
        }
        let closure = file_closure(conn, file_path, max_depth, &relations, hop)?;
        let counts = cross_file_symbol_counts(conn, file_path, &relations, hop)?;
        let mut deps: Vec<FileDependency> = closure
            .into_iter()
            .map(|(path, depth)| {
                let symbol_count = counts.get(&path).copied().unwrap_or(0);
                FileDependency {
                    file_path: path,
                    direction: dir_name.into(),
                    symbol_count,
                    depth,
                }
            })
            .collect();
        // `depth ASC, symbol_count DESC` is the CTE's ordering; the `file_path`
        // tail is new — it makes ties deterministic instead of leaving them to
        // the query plan.
        deps.sort_by(|a, b| {
            a.depth
                .cmp(&b.depth)
                .then(b.symbol_count.cmp(&a.symbol_count))
                .then(a.file_path.cmp(&b.file_path))
        });
        results.extend(deps);
    }

    Ok(results)
}

/// True when `file_path` has a row in the `files` table (i.e. it is indexed).
/// Lets `affected` distinguish "no dependents" from "never indexed".
pub fn file_is_indexed(conn: &Connection, file_path: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM files WHERE path = ?1",
        rusqlite::params![file_path],
        |row| row.get(0),
    )?;
    Ok(n > 0)
}

/// Reverse transitive dependents of `file_path` over EVERY "A depends on B" relation
/// (imports ∪ calls ∪ references ∪ implements ∪ inherits), file-level, breadth-first
/// (see [`file_closure`] — the global visited set is what bounds a cyclic graph).
/// Returns (dependent_file_path, min_depth). Unlike [`get_import_tree`] (imports ∪ calls
/// only — correct for a *dependency graph* view), `affected` needs the full relation
/// set so a test that only `references`/`implements`/`inherits` a changed symbol is not
/// silently dropped from the "tests to re-run" set. No `symbol_count` subquery — callers
/// here only need the file set and depth.
pub fn get_reverse_dependents(
    conn: &Connection,
    file_path: &str,
    max_depth: i32,
) -> Result<Vec<(String, i32)>> {
    use crate::domain::{REL_CALLS, REL_IMPLEMENTS, REL_IMPORTS, REL_INHERITS, REL_REFERENCES};
    let max_depth = max_depth.clamp(1, 10);
    let relations = [
        REL_IMPORTS,
        REL_CALLS,
        REL_REFERENCES,
        REL_IMPLEMENTS,
        REL_INHERITS,
    ];
    file_closure(conn, file_path, max_depth, &relations, FileHop::Incoming)
}

/// All distinct cross-file `imports` edges as `(source_file, target_file)` pairs
/// (source imports target), for whole-graph circular-dependency detection.
///
/// Excludes self-file edges and the synthetic `<external>` pseudo-file (unresolved
/// external/builtin imports, mirroring `project_map`). `calls` is intentionally
/// excluded: a call cycle is mutual recursion, not a circular *import*.
pub fn all_file_import_edges(conn: &Connection) -> Result<Vec<(String, String)>> {
    use crate::domain::REL_IMPORTS;
    let mut stmt = conn.prepare(
        "SELECT DISTINCT sf.path, tf.path \
         FROM edges e \
         JOIN nodes ns ON ns.id = e.source_id \
         JOIN files sf ON sf.id = ns.file_id \
         JOIN nodes nt ON nt.id = e.target_id \
         JOIN files tf ON tf.id = nt.file_id \
         WHERE e.relation = ?1 \
           AND sf.id != tf.id \
           AND sf.path != '<external>' AND tf.path != '<external>'",
    )?;
    let rows = stmt.query_map(rusqlite::params![REL_IMPORTS], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::helpers::test_db;
    use super::*;

    /// The pre-BFS recursive-CTE traversal, kept ONLY as a differential oracle:
    /// the tests below assert the BFS implementation returns byte-identical
    /// results on generated graphs (diamonds, cycles, multi-parent fan-in).
    /// Reproduced verbatim from the shipped v0.116.0 implementation so a future
    /// change to `file_closure` cannot silently redefine what "same result"
    /// means. Only usable on small fixtures — on a real repo the per-path
    /// `visited_ids NOT LIKE` guard makes it exponential in depth, which is why
    /// it is no longer the production path.
    fn legacy_reverse_dependents_cte(
        conn: &Connection,
        file_path: &str,
        max_depth: i32,
    ) -> Result<Vec<(String, i32)>> {
        use crate::domain::{REL_CALLS, REL_IMPLEMENTS, REL_IMPORTS, REL_INHERITS, REL_REFERENCES};
        let max_depth = max_depth.clamp(1, 10);
        let in_list = [
            REL_IMPORTS,
            REL_CALLS,
            REL_REFERENCES,
            REL_IMPLEMENTS,
            REL_INHERITS,
        ]
        .iter()
        .map(|r| format!("'{r}'"))
        .collect::<Vec<_>>()
        .join(", ");
        let sql = format!(
            "WITH RECURSIVE dep_tree(file_id, file_path, depth, visited_ids) AS (
                SELECT f0.id, f0.path, 0, CAST(f0.id AS TEXT)
                FROM files f0 WHERE f0.path = ?1

                UNION ALL

                SELECT DISTINCT f1.id, f1.path, dt.depth + 1,
                       dt.visited_ids || '|' || CAST(f1.id AS TEXT)
                FROM dep_tree dt
                JOIN nodes n2 ON n2.file_id = dt.file_id
                JOIN edges e ON e.target_id = n2.id AND e.relation IN ({in_list})
                JOIN nodes n1 ON n1.id = e.source_id
                JOIN files f1 ON f1.id = n1.file_id
                WHERE dt.depth < ?2
                  AND f1.path != ?1
                  AND ('|' || dt.visited_ids || '|') NOT LIKE '%|' || CAST(f1.id AS TEXT) || '|%'
            )
            SELECT dt.file_path, MIN(dt.depth) AS min_depth
            FROM dep_tree dt
            WHERE dt.depth > 0
            GROUP BY dt.file_path
            ORDER BY min_depth, dt.file_path"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![file_path, max_depth], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i32>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Deterministic 32-bit LCG — fixture generation must be reproducible, and a
    /// `rand` dev-dependency is not worth one test helper.
    struct Lcg(u32);
    impl Lcg {
        fn next(&mut self, bound: usize) -> usize {
            self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (self.0 >> 8) as usize % bound
        }
    }

    /// `files` files × `per_file` nodes, plus `edges` pseudorandom cross-file
    /// edges drawn from the five dependency relations. Dense enough that the
    /// path count between two files is large (that is the point: it is what the
    /// old CTE enumerated), while the file count stays small enough for the
    /// oracle to finish.
    fn seeded_graph(conn: &Connection, files: usize, per_file: usize, edges: usize, seed: u32) {
        for i in 0..files {
            conn.execute(
                "INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES (?1, 'h', 0, 'rust', 0)",
                rusqlite::params![format!("src/f{i}.rs")],
            )
            .unwrap();
            for j in 0..per_file {
                conn.execute(
                    "INSERT INTO nodes (file_id, type, name, start_line, end_line, code_content) VALUES (?1, 'function', ?2, 1, 2, '')",
                    rusqlite::params![i as i64 + 1, format!("f{i}_n{j}")],
                )
                .unwrap();
            }
        }
        let relations = ["imports", "calls", "references", "implements", "inherits"];
        let total_nodes = files * per_file;
        let mut rng = Lcg(seed);
        for _ in 0..edges {
            let a = rng.next(total_nodes) as i64 + 1;
            let b = rng.next(total_nodes) as i64 + 1;
            let r = relations[rng.next(relations.len())];
            if a == b {
                continue;
            }
            // INSERT OR IGNORE: idx_edges_unique rejects duplicates, which the
            // generator will produce.
            conn.execute(
                "INSERT OR IGNORE INTO edges (source_id, target_id, relation) VALUES (?1, ?2, ?3)",
                rusqlite::params![a, b, r],
            )
            .unwrap();
        }
    }

    #[test]
    fn reverse_dependents_matches_recursive_cte_oracle() {
        // Three independent graphs so a single lucky topology cannot pass this.
        for seed in [7u32, 4242, 90_210] {
            let (db, _tmp) = test_db();
            let conn = db.conn();
            seeded_graph(conn, 8, 3, 120, seed);
            for depth in 1..=10 {
                for root in 0..8 {
                    let path = format!("src/f{root}.rs");
                    let bfs = get_reverse_dependents(conn, &path, depth).unwrap();
                    let cte = legacy_reverse_dependents_cte(conn, &path, depth).unwrap();
                    assert_eq!(
                        bfs, cte,
                        "seed={seed} root={path} depth={depth}: BFS closure must equal the recursive-CTE oracle"
                    );
                }
            }
        }
    }

    #[test]
    fn import_tree_matches_recursive_cte_oracle() {
        for seed in [11u32, 5150] {
            let (db, _tmp) = test_db();
            let conn = db.conn();
            seeded_graph(conn, 8, 3, 120, seed);
            for depth in 1..=10 {
                for dir in ["outgoing", "incoming", "both"] {
                    for root in 0..8 {
                        let path = format!("src/f{root}.rs");
                        let bfs = get_import_tree(conn, &path, dir, depth).unwrap();
                        let cte = legacy_import_tree_cte(conn, &path, dir, depth).unwrap();
                        // The CTE leaves (depth, symbol_count) ties to the query
                        // plan; BFS breaks them by path. Compare as sorted sets
                        // of the full row so a genuine field difference still
                        // fails, then assert the tie-broken order separately.
                        let key = |d: &FileDependency| {
                            (
                                d.depth,
                                -d.symbol_count,
                                d.direction.clone(),
                                d.file_path.clone(),
                            )
                        };
                        let mut a: Vec<_> = bfs.iter().map(key).collect();
                        let mut b: Vec<_> = cte.iter().map(key).collect();
                        a.sort();
                        b.sort();
                        assert_eq!(
                            a, b,
                            "seed={seed} root={path} dir={dir} depth={depth}: import tree must equal the recursive-CTE oracle"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn closure_visits_a_diamond_node_once_at_its_shortest_depth() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        // root ← b ← d and root ← c ← d (two paths to d), plus a direct
        // root ← d shortcut added afterwards to pin "shortest wins".
        for name in ["root", "b", "c", "d"] {
            conn.execute(
                "INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES (?1, 'h', 0, 'rust', 0)",
                rusqlite::params![format!("src/{name}.rs")],
            ).unwrap();
        }
        for i in 1..=4 {
            conn.execute(
                "INSERT INTO nodes (file_id, type, name, start_line, end_line, code_content) VALUES (?1, 'function', ?2, 1, 2, '')",
                rusqlite::params![i, format!("n{i}")],
            ).unwrap();
        }
        // n1=root, n2=b, n3=c, n4=d. b→root, c→root, d→b, d→c.
        for (s, t) in [(2, 1), (3, 1), (4, 2), (4, 3)] {
            conn.execute(
                "INSERT INTO edges (source_id, target_id, relation) VALUES (?1, ?2, 'calls')",
                rusqlite::params![s, t],
            )
            .unwrap();
        }
        let deps = get_reverse_dependents(conn, "src/root.rs", 10).unwrap();
        assert_eq!(
            deps,
            vec![
                ("src/b.rs".to_string(), 1),
                ("src/c.rs".to_string(), 1),
                ("src/d.rs".to_string(), 2),
            ],
            "d is reachable by two paths and must appear exactly once, at depth 2"
        );

        // Add the direct shortcut d→root: d must now be reported at depth 1.
        conn.execute(
            "INSERT INTO edges (source_id, target_id, relation) VALUES (4, 1, 'references')",
            [],
        )
        .unwrap();
        let deps = get_reverse_dependents(conn, "src/root.rs", 10).unwrap();
        assert_eq!(
            deps.iter().find(|(p, _)| p == "src/d.rs").map(|(_, d)| *d),
            Some(1),
            "the shorter path must win"
        );
    }

    #[test]
    fn closure_terminates_on_a_cycle_and_respects_max_depth() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        // A 4-file ring a→b→c→d→a: every file is reachable from every other, so
        // a per-path guard would enumerate every rotation.
        for name in ["a", "b", "c", "d"] {
            conn.execute(
                "INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES (?1, 'h', 0, 'rust', 0)",
                rusqlite::params![format!("src/{name}.rs")],
            ).unwrap();
        }
        for i in 1..=4 {
            conn.execute(
                "INSERT INTO nodes (file_id, type, name, start_line, end_line, code_content) VALUES (?1, 'function', ?2, 1, 2, '')",
                rusqlite::params![i, format!("n{i}")],
            ).unwrap();
        }
        for (s, t) in [(1, 2), (2, 3), (3, 4), (4, 1)] {
            conn.execute(
                "INSERT INTO edges (source_id, target_id, relation) VALUES (?1, ?2, 'imports')",
                rusqlite::params![s, t],
            )
            .unwrap();
        }
        // Depth 10 on a 4-cycle terminates and reports each other file once.
        let deps = get_import_tree(conn, "src/a.rs", "outgoing", 10).unwrap();
        let mut paths: Vec<_> = deps.iter().map(|d| d.file_path.clone()).collect();
        paths.sort();
        assert_eq!(
            paths,
            vec!["src/b.rs", "src/c.rs", "src/d.rs"],
            "the ring must terminate with each file reported exactly once"
        );
        assert_eq!(
            deps.iter()
                .find(|d| d.file_path == "src/d.rs")
                .unwrap()
                .depth,
            3
        );

        // Depth limiting: at 2 hops d is out of range.
        let deps = get_import_tree(conn, "src/a.rs", "outgoing", 2).unwrap();
        let mut paths: Vec<_> = deps.iter().map(|d| d.file_path.clone()).collect();
        paths.sort();
        assert_eq!(
            paths,
            vec!["src/b.rs", "src/c.rs"],
            "depth 2 must stop before d"
        );
        let deps = get_reverse_dependents(conn, "src/a.rs", 1).unwrap();
        assert_eq!(
            deps,
            vec![("src/d.rs".to_string(), 1)],
            "depth 1 reverse = direct dependents only"
        );
    }

    #[test]
    fn closure_spans_multiple_frontier_chunks_matching_oracle() {
        // A frontier wider than FRONTIER_CHUNK is split across several `IN (...)`
        // queries. Neither this repo (≈230 files) nor the fixtures above reach
        // that width, so without this test the chunk-stitching code is dark.
        let (db, _tmp) = test_db();
        let conn = db.conn();
        let wide = FRONTIER_CHUNK + 50;
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('src/root.rs','h',0,'rust',0)", []).unwrap();
        conn.execute("INSERT INTO nodes (file_id,type,name,start_line,end_line,code_content) VALUES (1,'function','root',1,2,'')", []).unwrap();
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('src/leaf.rs','h',0,'rust',0)", []).unwrap();
        conn.execute("INSERT INTO nodes (file_id,type,name,start_line,end_line,code_content) VALUES (2,'function','leaf',1,2,'')", []).unwrap();
        for i in 0..wide {
            conn.execute(
                "INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES (?1,'h',0,'rust',0)",
                rusqlite::params![format!("src/mid{i}.rs")],
            ).unwrap();
            let file_id = 3 + i as i64;
            conn.execute(
                "INSERT INTO nodes (file_id,type,name,start_line,end_line,code_content) VALUES (?1,'function',?2,1,2,'')",
                rusqlite::params![file_id, format!("mid{i}")],
            ).unwrap();
            let node_id = 3 + i as i64;
            // mid_i → root (depth 1), leaf → mid_i (so leaf is depth 2 and only
            // reachable THROUGH the oversized frontier).
            conn.execute(
                "INSERT INTO edges (source_id,target_id,relation) VALUES (?1,1,'calls')",
                rusqlite::params![node_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO edges (source_id,target_id,relation) VALUES (2,?1,'references')",
                rusqlite::params![node_id],
            )
            .unwrap();
        }
        let bfs = get_reverse_dependents(conn, "src/root.rs", 5).unwrap();
        assert_eq!(bfs.len(), wide + 1, "every mid file plus the leaf");
        assert_eq!(
            bfs.iter()
                .find(|(p, _)| p == "src/leaf.rs")
                .map(|(_, d)| *d),
            Some(2),
            "the leaf is only reachable through a frontier wider than one chunk"
        );
        let cte = legacy_reverse_dependents_cte(conn, "src/root.rs", 5).unwrap();
        assert_eq!(
            bfs, cte,
            "chunked traversal must equal the recursive-CTE oracle"
        );
    }

    #[test]
    fn closure_output_is_deterministic_across_runs() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        seeded_graph(conn, 8, 3, 120, 31_337);
        let first = get_reverse_dependents(conn, "src/f0.rs", 10).unwrap();
        let first_tree = get_import_tree(conn, "src/f0.rs", "both", 10).unwrap();
        for _ in 0..5 {
            assert_eq!(
                get_reverse_dependents(conn, "src/f0.rs", 10).unwrap(),
                first
            );
            let tree = get_import_tree(conn, "src/f0.rs", "both", 10).unwrap();
            assert_eq!(tree.len(), first_tree.len());
            for (a, b) in tree.iter().zip(first_tree.iter()) {
                assert_eq!(
                    (&a.file_path, &a.direction, a.depth, a.symbol_count),
                    (&b.file_path, &b.direction, b.depth, b.symbol_count),
                    "repeated calls must return an identical, identically-ordered result"
                );
            }
        }
    }

    /// Pre-BFS recursive-CTE `get_import_tree`, verbatim from v0.116.0, kept as a
    /// differential oracle for the BFS rewrite (see the reverse-dependents oracle
    /// above for why).
    fn legacy_import_tree_cte(
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
                    -- Count distinct cross-file target symbols from root to this file
                    -- (a symbol both imported and called is one symbol, not two).
                    (SELECT COUNT(DISTINCT nb.id)
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
                    -- Count distinct cross-file target symbols from this file to root
                    -- (a symbol both imported and called is one symbol, not two).
                    (SELECT COUNT(DISTINCT nb.id)
                     FROM nodes na JOIN files fa ON fa.id = na.file_id
                     JOIN edges ea ON ea.source_id = na.id AND ea.relation IN (?1, ?3)
                     JOIN nodes nb ON nb.id = ea.target_id
                     JOIN files fb ON fb.id = nb.file_id
                     WHERE fa.path = dt.file_path AND fb.path = ?2) as cnt
                FROM dep_tree dt
                WHERE dt.depth > 0
                GROUP BY dt.file_path
                ORDER BY min_depth, cnt DESC",
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
        conn.execute(
            "INSERT INTO edges (source_id, target_id, relation) VALUES (1, 3, 'imports')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO edges (source_id, target_id, relation) VALUES (2, 4, 'calls')",
            [],
        )
        .unwrap();

        let tree = get_import_tree(conn, "src/a.ts", "outgoing", 2).unwrap();
        assert!(!tree.is_empty());
        let b_dep = tree.iter().find(|d| d.file_path == "src/b.ts").unwrap();
        assert_eq!(
            b_dep.symbol_count, 2,
            "symbol_count should reflect actual cross-file edges"
        );
        assert_eq!(b_dep.depth, 1);

        // Incoming: from B's perspective, A depends on it with 2 symbols
        let tree_in = get_import_tree(conn, "src/b.ts", "incoming", 2).unwrap();
        let a_dep = tree_in.iter().find(|d| d.file_path == "src/a.ts").unwrap();
        assert_eq!(a_dep.symbol_count, 2, "incoming symbol_count should match");
    }

    #[test]
    fn file_is_indexed_detects_presence() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute(
            "INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('src/a.rs', 'h', 0, 'rust', 0)",
            [],
        ).unwrap();
        assert!(file_is_indexed(conn, "src/a.rs").unwrap());
        assert!(!file_is_indexed(conn, "src/missing.rs").unwrap());
    }

    #[test]
    fn reverse_dependents_includes_non_import_relations() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('a.ts','h1',0,'typescript',0)", []).unwrap();
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('b.ts','h2',0,'typescript',0)", []).unwrap();
        conn.execute("INSERT INTO nodes (file_id,type,name,start_line,end_line,code_content) VALUES (1,'function','a',1,2,'')", []).unwrap();
        conn.execute("INSERT INTO nodes (file_id,type,name,start_line,end_line,code_content) VALUES (2,'function','b',1,2,'')", []).unwrap();
        // a (file 1) references b (file 2) via a 'references' edge — NOT imports/calls.
        conn.execute(
            "INSERT INTO edges (source_id,target_id,relation) VALUES (1,2,'references')",
            [],
        )
        .unwrap();

        // get_import_tree walks only imports∪calls → must MISS the references-only dep.
        let imp = get_import_tree(conn, "b.ts", "incoming", 5).unwrap();
        assert!(
            imp.iter().all(|d| d.file_path != "a.ts"),
            "import_tree (imports∪calls) should not see a references-only dependent"
        );
        // get_reverse_dependents walks all dependency relations → must INCLUDE a.ts.
        let rev = get_reverse_dependents(conn, "b.ts", 5).unwrap();
        assert!(
            rev.iter().any(|(p, _)| p == "a.ts"),
            "reverse_dependents must include the references dependent; got {rev:?}"
        );
    }

    #[test]
    fn all_file_import_edges_returns_cross_file_imports_only() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('a.ts','h1',0,'typescript',0)", []).unwrap();
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('b.ts','h2',0,'typescript',0)", []).unwrap();
        conn.execute("INSERT INTO nodes (file_id,type,name,start_line,end_line,code_content) VALUES (1,'function','fa',1,2,'')", []).unwrap();
        conn.execute("INSERT INTO nodes (file_id,type,name,start_line,end_line,code_content) VALUES (2,'function','fb',1,2,'')", []).unwrap();
        // a imports b AND b imports a → a file-level cycle.
        conn.execute(
            "INSERT INTO edges (source_id,target_id,relation) VALUES (1,2,'imports')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO edges (source_id,target_id,relation) VALUES (2,1,'imports')",
            [],
        )
        .unwrap();
        // A 'calls' edge must NOT appear — cycles are imports-only (call cycles = recursion).
        conn.execute(
            "INSERT INTO edges (source_id,target_id,relation) VALUES (1,2,'calls')",
            [],
        )
        .unwrap();

        let mut edges = all_file_import_edges(conn).unwrap();
        edges.sort();
        assert_eq!(
            edges,
            vec![
                ("a.ts".to_string(), "b.ts".to_string()),
                ("b.ts".to_string(), "a.ts".to_string()),
            ],
            "exactly the two cross-file import edges; the calls edge is excluded"
        );
    }

    #[test]
    fn all_file_import_edges_excludes_self_file_and_external() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('a.ts','h1',0,'typescript',0)", []).unwrap();
        // Synthetic <external> bucket for unresolved imports (mirrors project_map).
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('<external>','h2',0,'typescript',0)", []).unwrap();
        conn.execute("INSERT INTO nodes (file_id,type,name,start_line,end_line,code_content) VALUES (1,'function','f1',1,2,'')", []).unwrap();
        conn.execute("INSERT INTO nodes (file_id,type,name,start_line,end_line,code_content) VALUES (1,'function','f2',3,4,'')", []).unwrap();
        conn.execute("INSERT INTO nodes (file_id,type,name,start_line,end_line,code_content) VALUES (2,'function','ext',1,2,'')", []).unwrap();
        // Intra-file import (same file) and an import of the <external> bucket — both excluded.
        conn.execute(
            "INSERT INTO edges (source_id,target_id,relation) VALUES (1,2,'imports')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO edges (source_id,target_id,relation) VALUES (1,3,'imports')",
            [],
        )
        .unwrap();

        let edges = all_file_import_edges(conn).unwrap();
        assert!(
            edges.is_empty(),
            "self-file and <external> imports must be excluded; got {edges:?}"
        );
    }
}
