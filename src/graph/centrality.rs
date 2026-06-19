//! Betweenness centrality over the call graph — finds *architectural chokepoints*.
//!
//! `caller_count` (used by `project_map` hot functions) is **degree centrality**:
//! it answers "how many functions call this?". Betweenness answers a different,
//! orthogonal question: "how many shortest call paths between *other* functions
//! pass *through* this one?". A node can have a modest caller_count yet be a
//! structural bridge whose removal would fragment the graph — degree centrality
//! is blind to these, betweenness surfaces them.
//!
//! Algorithm: Brandes' algorithm (the standard O(V·E) exact betweenness for
//! unweighted graphs) over the directed `REL_CALLS` edge set. Directed graphs are
//! NOT halved (each ordered (s,t) pair is counted once). CLI-only and on-demand —
//! not wired into the MCP hot path or any per-query surface.

use anyhow::Result;
use rusqlite::Connection;
use std::collections::VecDeque;

use crate::domain::{is_test_symbol, REL_CALLS};

/// A function ranked by betweenness centrality.
pub struct CentralityNode {
    pub node_id: i64,
    pub name: String,
    pub node_type: String,
    pub file_path: String,
    /// Raw Brandes betweenness: the (fractional) number of shortest paths between
    /// other node pairs that route through this node.
    pub score: f64,
    /// `score` divided by the directed-graph maximum `(n-1)(n-2)`, giving a 0..1
    /// comparable figure across graphs of different sizes. 0 when `n < 3`.
    pub normalized: f64,
    /// In-degree on the `calls` graph (how many distinct callers) — shown
    /// alongside betweenness so users can see "bridge but few callers" cases.
    pub caller_count: u32,
}

/// Compute betweenness centrality over the `calls` graph and return the top
/// `limit` functions by raw score (descending). Test symbols are excluded from
/// the graph entirely unless `include_tests` is set — both as endpoints and as
/// intermediate hops — so test helpers don't inflate production chokepoints. The
/// test classification reuses [`is_test_symbol`] (the canonical Rust classifier),
/// not a parallel SQL heuristic.
pub fn betweenness_centrality(
    conn: &Connection,
    include_tests: bool,
    limit: usize,
) -> Result<Vec<CentralityNode>> {
    // 1. Load node metadata, applying the test filter to decide membership.
    struct NodeMeta {
        node_id: i64,
        name: String,
        node_type: String,
        file_path: String,
    }
    let mut stmt = conn.prepare(
        "SELECT n.id, n.name, n.type, f.path, n.is_test \
         FROM nodes n JOIN files f ON f.id = n.file_id",
    )?;
    let rows = stmt.query_map([], |row| {
        let node_id: i64 = row.get(0)?;
        let name: String = row.get(1)?;
        let node_type: String = row.get(2)?;
        let file_path: String = row.get(3)?;
        let is_test_flag: i64 = row.get(4)?;
        Ok((node_id, name, node_type, file_path, is_test_flag != 0))
    })?;

    // Dense index assignment: node_id -> 0..n. Only kept (non-test, unless
    // include_tests) nodes get an index; edges touching excluded nodes are dropped.
    let mut metas: Vec<NodeMeta> = Vec::new();
    let mut index_of: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    for row in rows {
        let (node_id, name, node_type, file_path, is_test_flag) = row?;
        let excluded = !include_tests && (is_test_flag || is_test_symbol(&name, &file_path));
        if excluded {
            continue;
        }
        index_of.insert(node_id, metas.len());
        metas.push(NodeMeta { node_id, name, node_type, file_path });
    }

    let n = metas.len();
    if n == 0 {
        return Ok(Vec::new());
    }

    // 2. Load `calls` edges into a forward adjacency list over dense indices.
    // Self-loops and edges to/from excluded nodes are skipped. Parallel edges are
    // deduped per (u,v) so multiplicity doesn't distort shortest-path counts.
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut caller_count: Vec<u32> = vec![0; n];
    let mut edge_stmt =
        conn.prepare("SELECT source_id, target_id FROM edges WHERE relation = ?1")?;
    let edge_rows = edge_stmt.query_map([REL_CALLS], |row| {
        let s: i64 = row.get(0)?;
        let t: i64 = row.get(1)?;
        Ok((s, t))
    })?;
    let mut seen_edges: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    for row in edge_rows {
        let (s, t) = row?;
        let (Some(&u), Some(&v)) = (index_of.get(&s), index_of.get(&t)) else {
            continue;
        };
        if u == v || !seen_edges.insert((u, v)) {
            continue;
        }
        adj[u].push(v);
        caller_count[v] += 1;
    }

    // 3. Brandes' algorithm (unweighted, directed). For each source s, a BFS
    // builds the shortest-path DAG (sigma = #shortest paths, P = predecessors),
    // then a reverse-order accumulation back-propagates the dependency delta.
    let mut betweenness: Vec<f64> = vec![0.0; n];
    let mut sigma: Vec<f64> = vec![0.0; n];
    let mut dist: Vec<i64> = vec![-1; n];
    let mut delta: Vec<f64> = vec![0.0; n];
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut stack: Vec<usize> = Vec::with_capacity(n);

    for s in 0..n {
        // Reset per-source scratch (only touched entries, but full reset is simpler
        // and the cost is dominated by the BFS/accumulation anyway).
        for v in 0..n {
            sigma[v] = 0.0;
            dist[v] = -1;
            delta[v] = 0.0;
            preds[v].clear();
        }
        stack.clear();

        sigma[s] = 1.0;
        dist[s] = 0;
        let mut queue: VecDeque<usize> = VecDeque::new();
        queue.push_back(s);

        while let Some(v) = queue.pop_front() {
            stack.push(v);
            for &w in &adj[v] {
                // First time we reach w: it's on a shortest path; enqueue it.
                if dist[w] < 0 {
                    dist[w] = dist[v] + 1;
                    queue.push_back(w);
                }
                // w found on another shortest path of the same length via v.
                if dist[w] == dist[v] + 1 {
                    sigma[w] += sigma[v];
                    preds[w].push(v);
                }
            }
        }

        // Back-propagation in order of non-increasing distance from s.
        while let Some(w) = stack.pop() {
            let coeff = (1.0 + delta[w]) / sigma[w];
            for &v in &preds[w] {
                delta[v] += sigma[v] * coeff;
            }
            if w != s {
                betweenness[w] += delta[w];
            }
        }
    }

    // 4. Normalization factor for directed graphs: (n-1)(n-2).
    let norm_denom = if n >= 3 {
        ((n - 1) as f64) * ((n - 2) as f64)
    } else {
        0.0
    };

    // 5. Rank by raw score desc, break ties by caller_count then name for stable
    // output, take top `limit`.
    let mut ranked: Vec<usize> = (0..n).collect();
    ranked.sort_by(|&a, &b| {
        betweenness[b]
            .partial_cmp(&betweenness[a])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(caller_count[b].cmp(&caller_count[a]))
            .then(metas[a].name.cmp(&metas[b].name))
    });

    let out = ranked
        .into_iter()
        .filter(|&i| betweenness[i] > 0.0)
        .take(limit)
        .map(|i| CentralityNode {
            node_id: metas[i].node_id,
            name: metas[i].name.clone(),
            node_type: metas[i].node_type.clone(),
            file_path: metas[i].file_path.clone(),
            score: betweenness[i],
            normalized: if norm_denom > 0.0 {
                betweenness[i] / norm_denom
            } else {
                0.0
            },
            caller_count: caller_count[i],
        })
        .collect();

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::REL_CALLS;
    use crate::storage::db::Database;
    use crate::storage::queries::{insert_edge, insert_node, upsert_file, FileRecord, NodeRecord};
    use tempfile::TempDir;

    fn test_db() -> (Database, TempDir) {
        let tmp = TempDir::new().unwrap();
        let db = Database::open(&tmp.path().join("test.db")).unwrap();
        (db, tmp)
    }

    fn mk_node(name: &str, file_id: i64, is_test: bool) -> NodeRecord {
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
            is_test,
        }
    }

    fn file(conn: &Connection, path: &str) -> i64 {
        upsert_file(
            conn,
            &FileRecord {
                path: path.into(),
                blake3_hash: format!("h-{path}"),
                last_modified: 1,
                language: Some("typescript".into()),
            },
        )
        .unwrap()
    }

    /// Path graph A→B→C→D→E. B, C, D are all on shortest paths between others;
    /// the middle node C lies on the most (A→D, A→E, B→D, B→E pass through C),
    /// so C must rank highest. Endpoints A and E lie on zero paths.
    #[test]
    fn test_path_graph_middle_is_chokepoint() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        let fid = file(conn, "src/lib.ts");

        let a = insert_node(conn, &mk_node("A", fid, false)).unwrap();
        let b = insert_node(conn, &mk_node("B", fid, false)).unwrap();
        let c = insert_node(conn, &mk_node("C", fid, false)).unwrap();
        let d = insert_node(conn, &mk_node("D", fid, false)).unwrap();
        let e = insert_node(conn, &mk_node("E", fid, false)).unwrap();

        insert_edge(conn, a, b, REL_CALLS, None).unwrap();
        insert_edge(conn, b, c, REL_CALLS, None).unwrap();
        insert_edge(conn, c, d, REL_CALLS, None).unwrap();
        insert_edge(conn, d, e, REL_CALLS, None).unwrap();

        let result = betweenness_centrality(conn, false, 10).unwrap();

        // Top node must be C (the geometric middle of the chain).
        assert_eq!(result[0].name, "C", "middle node C must be the top chokepoint");

        // A and E (endpoints) lie on no shortest path → score 0 → filtered out.
        let names: Vec<&str> = result.iter().map(|r| r.name.as_str()).collect();
        assert!(!names.contains(&"A"), "endpoint A has zero betweenness");
        assert!(!names.contains(&"E"), "endpoint E has zero betweenness");

        // Exact directed-betweenness scores on a 5-node path: C=4, B=3, D=3.
        let c_node = result.iter().find(|r| r.name == "C").unwrap();
        assert_eq!(c_node.score, 4.0, "C is on 4 shortest paths");
    }

    /// A bridge node with LOW caller_count can still dominate betweenness. Two
    /// clusters {L1,L2,L3} and {R1,R2,R3} connected only through bridge node X.
    /// X has just one caller (L1) but every left→right shortest path crosses it.
    #[test]
    fn test_bridge_beats_high_degree_hub() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        let fid = file(conn, "src/lib.ts");

        let l1 = insert_node(conn, &mk_node("L1", fid, false)).unwrap();
        let l2 = insert_node(conn, &mk_node("L2", fid, false)).unwrap();
        let l3 = insert_node(conn, &mk_node("L3", fid, false)).unwrap();
        let x = insert_node(conn, &mk_node("X", fid, false)).unwrap();
        let r1 = insert_node(conn, &mk_node("R1", fid, false)).unwrap();
        let r2 = insert_node(conn, &mk_node("R2", fid, false)).unwrap();
        let r3 = insert_node(conn, &mk_node("R3", fid, false)).unwrap();

        // Left cluster funnels into L1, L1 → X (single bridge), X fans to right.
        insert_edge(conn, l2, l1, REL_CALLS, None).unwrap();
        insert_edge(conn, l3, l1, REL_CALLS, None).unwrap();
        insert_edge(conn, l1, x, REL_CALLS, None).unwrap();
        insert_edge(conn, x, r1, REL_CALLS, None).unwrap();
        insert_edge(conn, r1, r2, REL_CALLS, None).unwrap();
        insert_edge(conn, r1, r3, REL_CALLS, None).unwrap();

        let result = betweenness_centrality(conn, false, 10).unwrap();

        let x_node = result.iter().find(|r| r.name == "X").unwrap();
        // X has exactly one caller (L1) ...
        assert_eq!(x_node.caller_count, 1, "bridge X has a single caller");
        // ... yet it is the top chokepoint despite the low degree.
        assert_eq!(result[0].name, "X", "single-caller bridge must outrank by betweenness");
    }

    /// Test nodes are excluded by default (both endpoints and intermediate hops),
    /// included with `include_tests`.
    #[test]
    fn test_test_nodes_excluded_by_default() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        let src = file(conn, "src/lib.ts");
        let tst = file(conn, "tests/lib_test.ts");

        let a = insert_node(conn, &mk_node("A", src, false)).unwrap();
        // T is a test helper bridging A→B; flagged is_test AND in a test path.
        let t = insert_node(conn, &mk_node("test_helper", tst, true)).unwrap();
        let b = insert_node(conn, &mk_node("B", src, false)).unwrap();
        let c = insert_node(conn, &mk_node("C", src, false)).unwrap();

        insert_edge(conn, a, t, REL_CALLS, None).unwrap();
        insert_edge(conn, t, b, REL_CALLS, None).unwrap();
        insert_edge(conn, b, c, REL_CALLS, None).unwrap();

        let default_run = betweenness_centrality(conn, false, 10).unwrap();
        assert!(
            !default_run.iter().any(|r| r.name == "test_helper"),
            "test node must be excluded by default"
        );

        let with_tests = betweenness_centrality(conn, true, 10).unwrap();
        assert!(
            with_tests.iter().any(|r| r.name == "test_helper"),
            "test node must appear with include_tests"
        );
    }

    /// Empty graph and a graph with no paths both yield an empty ranking, never panic.
    #[test]
    fn test_empty_and_edgeless() {
        let (db, _tmp) = test_db();
        let conn = db.conn();

        // Empty DB.
        assert!(betweenness_centrality(conn, false, 10).unwrap().is_empty());

        // Nodes but no edges → every score 0 → empty ranking.
        let fid = file(conn, "src/lib.ts");
        insert_node(conn, &mk_node("A", fid, false)).unwrap();
        insert_node(conn, &mk_node("B", fid, false)).unwrap();
        assert!(
            betweenness_centrality(conn, false, 10).unwrap().is_empty(),
            "edgeless graph has no chokepoints"
        );
    }

    /// A 2-cycle A↔B must terminate (cycle safety) and produce no chokepoint
    /// (each lies on no shortest path between a distinct pair).
    #[test]
    fn test_cycle_terminates() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        let fid = file(conn, "src/lib.ts");
        let a = insert_node(conn, &mk_node("A", fid, false)).unwrap();
        let b = insert_node(conn, &mk_node("B", fid, false)).unwrap();
        insert_edge(conn, a, b, REL_CALLS, None).unwrap();
        insert_edge(conn, b, a, REL_CALLS, None).unwrap();

        let result = betweenness_centrality(conn, false, 10).unwrap();
        assert!(result.is_empty(), "2-cycle has no intermediary nodes");
    }
}
