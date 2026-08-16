use anyhow::{anyhow, Result};
use rusqlite::Connection;
use std::collections::HashMap;

use crate::domain::REL_CALLS;

/// Hard cap on recursive CTE depth — protects against CTE blowup on highly
/// connected graphs. Caller-requested depth is clamped to this value silently;
/// `CallGraphResult::depth_capped` flags when that clamp fires so downstream
/// surfaces (MCP / CLI) can warn the agent that deeper chains may exist.
pub const CALL_GRAPH_MAX_DEPTH: i32 = 10;

/// Hard cap on rows returned per direction — keeps wide fan-outs from
/// returning megabytes of JSON. `CallGraphResult::limit_hit` flags when the
/// SQL query returned exactly this many rows (there may be more).
pub const CALL_GRAPH_ROW_LIMIT: usize = 200;

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
    /// node_id of the immediate parent in the traversal (the caller for
    /// `Direction::Callers`, the callee for `Direction::Callees`). `None` for
    /// the root (depth=0). When a node is reachable via multiple paths, this
    /// records one parent on the shortest path.
    pub parent_id: Option<i64>,
    /// The node's authoritative AST-level test flag (`nodes.is_test`), set by the
    /// parser for `#[cfg(test)] mod tests` / `#[test]` / `@Test` etc. Carried so
    /// caller-partitioning surfaces (impact risk, covering-tests) can classify an
    /// inline unit test whose descriptive snake_case name the `is_test_symbol`
    /// name/path heuristic misses (see [`crate::domain::is_test_symbol`]).
    pub is_test: bool,
}

/// Wraps `Vec<CallGraphNode>` with truncation provenance. Returned by
/// `get_call_graph` so MCP / CLI surfaces can tell agents when results are
/// incomplete instead of silently presenting a partial view as the full picture.
pub struct CallGraphResult {
    pub nodes: Vec<CallGraphNode>,
    /// True when at least one direction's recursive CTE hit `CALL_GRAPH_ROW_LIMIT`
    /// — more nodes may exist beyond the returned set. For "both", true if either
    /// callees or callers saturated.
    pub limit_hit: bool,
    /// True when the caller requested depth > `CALL_GRAPH_MAX_DEPTH`; the result
    /// only reflects the first `CALL_GRAPH_MAX_DEPTH` levels, deeper chains may exist.
    pub depth_capped: bool,
    /// Depth actually used by the SQL query (after clamping).
    pub effective_max_depth: i32,
    /// Depth originally requested by the caller (pre-clamp).
    pub requested_max_depth: i32,
    /// Count of the seed symbol's DIRECT edges (in the queried direction(s))
    /// pruned because their confidence ranked below the requested
    /// `min_confidence` floor — the by-name `ambiguous` fan-out class. Lets a
    /// surface disclose the hidden edges and point at `--min-confidence ambiguous`
    /// (CLI) / `min_confidence:"ambiguous"` (MCP) instead of silently dropping
    /// them. Always 0 when the floor is `ambiguous` (rank 0 — nothing is below it).
    pub suppressed_ambiguous: usize,
}

/// Traverse the call graph starting from a function by name.
///
/// `direction` must be one of: "callers", "callees", "both".
/// `depth` controls the maximum recursion depth (clamped to `CALL_GRAPH_MAX_DEPTH`;
/// `CallGraphResult::depth_capped` flags when the clamp fires).
/// `file_path` optionally disambiguates when multiple functions share the same name.
/// Back-compat entry point: traverses the full call graph with NO confidence
/// filtering — every edge is followed, including the `ambiguous` bare-name
/// fan-out class. Equivalent to `get_call_graph_filtered(.., 0)`. Callers that
/// want the low-noise default (hide ambiguous fan-out) call `_filtered` with a
/// higher rank; kept as a thin wrapper so existing callers (trace, route
/// resolution) and their tests are unchanged.
pub fn get_call_graph(
    conn: &Connection,
    function_name: &str,
    direction: &str,
    max_depth: i32,
    file_path: Option<&str>,
) -> Result<CallGraphResult> {
    // rank 0 = ambiguous floor = follow every edge (historical behavior).
    get_call_graph_filtered(conn, function_name, direction, max_depth, file_path, 0)
}

/// Traverse the call graph, following only edges whose resolution confidence
/// ranks at or above `min_confidence_rank` (per `domain::confidence_rank`:
/// extracted=2, inferred=1, ambiguous=0). The filter is applied INSIDE the
/// recursive CTE, so a sub-threshold edge is never expanded — this is what stops
/// the depth-N blowup from `ambiguous` bare-name edges (e.g. a `.execute()` call
/// resolving to every same-named def) rather than post-filtering after the
/// fan-out already exploded. `CallGraphResult::suppressed_ambiguous` reports how
/// many direct seed edges the floor pruned.
pub fn get_call_graph_filtered(
    conn: &Connection,
    function_name: &str,
    direction: &str,
    max_depth: i32,
    file_path: Option<&str>,
    min_confidence_rank: u8,
) -> Result<CallGraphResult> {
    let requested_max_depth = max_depth;
    let effective_max_depth = max_depth.min(CALL_GRAPH_MAX_DEPTH);
    let depth_capped = max_depth > CALL_GRAPH_MAX_DEPTH;

    let (nodes, limit_hit) = match direction {
        "callees" => query_direction(
            conn,
            function_name,
            effective_max_depth,
            file_path,
            Direction::Callees,
            min_confidence_rank,
        )?,
        "callers" => query_direction(
            conn,
            function_name,
            effective_max_depth,
            file_path,
            Direction::Callers,
            min_confidence_rank,
        )?,
        "both" => {
            let (callees, c1) = query_direction(
                conn,
                function_name,
                effective_max_depth,
                file_path,
                Direction::Callees,
                min_confidence_rank,
            )?;
            let (callers, c2) = query_direction(
                conn,
                function_name,
                effective_max_depth,
                file_path,
                Direction::Callers,
                min_confidence_rank,
            )?;
            (merge_results(callees, callers), c1 || c2)
        }
        other => {
            return Err(anyhow!(
                "invalid direction '{}': must be callers, callees, or both",
                other
            ))
        }
    };

    // Disclose, rather than silently drop, the pruned fan-out: count the seed's
    // direct sub-threshold edges in the queried direction(s).
    let suppressed_ambiguous = match direction {
        "callees" => count_suppressed_seed_edges(
            conn,
            function_name,
            file_path,
            Direction::Callees,
            min_confidence_rank,
        )?,
        "callers" => count_suppressed_seed_edges(
            conn,
            function_name,
            file_path,
            Direction::Callers,
            min_confidence_rank,
        )?,
        "both" => {
            count_suppressed_seed_edges(
                conn,
                function_name,
                file_path,
                Direction::Callees,
                min_confidence_rank,
            )? + count_suppressed_seed_edges(
                conn,
                function_name,
                file_path,
                Direction::Callers,
                min_confidence_rank,
            )?
        }
        _ => 0,
    };

    Ok(CallGraphResult {
        nodes,
        limit_hit,
        depth_capped,
        effective_max_depth,
        requested_max_depth,
        suppressed_ambiguous,
    })
}

/// Max node ids bound into one frontier / metadata query. Keeps every
/// `IN (...)` list well under SQLite's variable cap on a wide fan-out.
const FRONTIER_CHUNK: usize = 400;

/// Returns `(nodes, limit_hit)`. `limit_hit` is true when the traversal found at
/// least `CALL_GRAPH_ROW_LIMIT` nodes — more nodes may exist beyond the
/// returned set.
///
/// Traversal is a breadth-first walk driven from Rust, one SQL query per level,
/// with a GLOBAL visited set. It replaces a `WITH RECURSIVE` CTE whose cycle
/// guard was a per-path visited STRING
/// (`(',' || cg.visited || ',') NOT LIKE '%,id,%'`). That guard only stops a
/// path from revisiting its OWN nodes, so the CTE enumerated every simple path
/// in the graph and deduplicated them afterwards with
/// `ROW_NUMBER() … PARTITION BY node_id ORDER BY depth`. On a densely connected
/// graph the path count is exponential in depth: a synthetic layered graph of
/// 55 nodes / 250 edges took 22.8s at depth 10, and 66 nodes did not finish in
/// two minutes.
///
/// The output is the same set: a node's shortest path is a simple path, so the
/// CTE's `MIN(depth)` per node is exactly the BFS distance, and a node
/// reachable within N steps by any walk is reachable within N steps by a simple
/// path. What BFS drops is the redundant re-derivation of longer paths.
///
/// `parent_id` records one parent on a shortest path. The CTE's
/// `ORDER BY cg.depth` had no tiebreaker, so among several shortest-path
/// parents SQLite kept whichever row its queue produced first; BFS keeps the
/// parent discovered first, with the frontier held in discovery order and each
/// level's children ordered by `(parent discovery rank, node id)`. Same class of
/// answer, now pinned by the code rather than by the query plan.
fn query_direction(
    conn: &Connection,
    function_name: &str,
    max_depth: i32,
    file_path: Option<&str>,
    direction: Direction,
    min_confidence_rank: u8,
) -> Result<(Vec<CallGraphNode>, bool)> {
    let max_depth = max_depth.min(CALL_GRAPH_MAX_DEPTH); // Hard cap on traversal depth
                                                         // Use NULL sentinel: when file_path is None, pass NULL and the filter is always true
    let file_path_param: Option<&str> = file_path;

    // Seed. Never SEED on an `<external>` sentinel: it has no outgoing calls, so
    // the traversal returns a one-node graph whose root prints as
    // `HashMap (<external>)` — a call graph for a symbol that is not in the
    // project. The by-name lookups in `queries/nodes.rs` carry the same
    // exclusion. `ORDER BY n.id` fixes multi-seed order (same-named defs in
    // several files) so depth-0 output does not depend on the query plan.
    let mut frontier: Vec<i64> = {
        let mut stmt = conn.prepare(
            "SELECT n.id FROM nodes n
             JOIN files f ON f.id = n.file_id
             WHERE n.name = ?1 AND f.path <> '<external>'
               AND (?2 IS NULL OR f.path = ?2)
             ORDER BY n.id",
        )?;
        let rows = stmt.query_map(rusqlite::params![function_name, file_path_param], |row| {
            row.get::<_, i64>(0)
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    if frontier.is_empty() {
        return Ok((Vec::new(), false));
    }

    // node_id → (depth, parent). Seeds are depth 0 with no parent.
    let mut seen: HashMap<i64, (i32, Option<i64>)> =
        frontier.iter().map(|id| (*id, (0, None))).collect();

    // In the recursive step:
    // - callees: follow edges forward (source_id = current, target_id = next)
    // - callers: follow edges backward (target_id = current, source_id = next)
    let (from_col, to_col) = match direction {
        Direction::Callees => ("source_id", "target_id"),
        Direction::Callers => ("target_id", "source_id"),
    };
    // Confidence gate: only FOLLOW edges whose resolution-confidence rank is >=
    // the requested floor. The CASE mirrors `domain::confidence_rank`
    // (extracted=2, inferred=1, ambiguous/unknown=0) — a test pins the two in
    // sync. Applied to the frontier expansion so a sub-threshold edge is never
    // walked, which is what kills the ambiguous fan-out (one `.execute()` → 56
    // same-named defs) at the source instead of after. Placeholders here are
    // POSITIONAL (`?`), because the frontier `IN (...)` list ahead of them is,
    // and SQLite numbers a bare `?` one past the highest index seen so far —
    // mixing `?1` into the same statement silently aliases parameter 1.
    let conf_gate =
        "AND (CASE e.confidence WHEN 'extracted' THEN 2 WHEN 'inferred' THEN 1 ELSE 0 END) >= ?";

    let mut depth = 1;
    while depth <= max_depth && !frontier.is_empty() {
        // (child, rank of the parent that reached it) — lowest rank wins, so the
        // recorded parent is the earliest-discovered one, and the next frontier
        // inherits a deterministic order.
        let mut discovered: Vec<(i64, usize, i64)> = Vec::new(); // (child, parent_rank, parent)
        for (offset, chunk) in frontier.chunks(FRONTIER_CHUNK).enumerate() {
            let base = offset * FRONTIER_CHUNK;
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            // The `nodes t` join mirrors the CTE's: an edge pointing at a row
            // with no `nodes` entry is not expanded.
            let sql = format!(
                "SELECT e.{from_col}, e.{to_col}
                 FROM edges e
                 JOIN nodes t ON t.id = e.{to_col}
                 WHERE e.{from_col} IN ({placeholders}) AND e.relation = ? {conf_gate}"
            );
            let rank_of: HashMap<i64, usize> = chunk
                .iter()
                .enumerate()
                .map(|(i, id)| (*id, base + i))
                .collect();
            let mut stmt = conn.prepare(&sql)?;
            let mut params: Vec<&dyn rusqlite::types::ToSql> = Vec::with_capacity(chunk.len() + 2);
            let rank_param: i64 = min_confidence_rank as i64;
            for id in chunk {
                params.push(id as &dyn rusqlite::types::ToSql);
            }
            params.push(&REL_CALLS);
            params.push(&rank_param);
            let rows = stmt.query_map(params.as_slice(), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (parent, child) = row?;
                if seen.contains_key(&child) {
                    continue;
                }
                discovered.push((child, rank_of[&parent], parent));
            }
        }
        // Order by (parent discovery rank, child id) and keep the first hit per
        // child: that is the earliest-discovered parent, deterministically.
        discovered.sort_unstable_by_key(|(child, rank, _)| (*rank, *child));
        let mut next: Vec<i64> = Vec::new();
        for (child, _, parent) in discovered {
            if seen.contains_key(&child) {
                continue;
            }
            seen.insert(child, (depth, Some(parent)));
            next.push(child);
        }
        frontier = next;
        depth += 1;
    }

    // Node metadata. The `files` INNER JOIN is the CTE's: a node with no file
    // row is expanded during traversal but never emitted.
    let ids: Vec<i64> = seen.keys().copied().collect();
    let mut meta: HashMap<i64, (String, String, String, bool)> = HashMap::new();
    for chunk in ids.chunks(FRONTIER_CHUNK) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT n.id, n.name, n.type, f.path, n.is_test
             FROM nodes n JOIN files f ON f.id = n.file_id
             WHERE n.id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::types::ToSql> = chunk
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt.query_map(params.as_slice(), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, bool>(4)?,
            ))
        })?;
        for row in rows {
            let (id, name, ty, path, is_test) = row?;
            meta.insert(id, (name, ty, path, is_test));
        }
    }

    // Truncation ordering: when a hot function (e.g. `conn` in this repo with
    // 51 callers + 72 test) saturates CALL_GRAPH_ROW_LIMIT at depth=3, the
    // pre-truncation sort is `depth ASC, caller_count DESC, node_id ASC` so
    // high-connectivity subtrees survive. `caller_count DESC` keeps the most
    // relevant subtree; the `node_id ASC` tail is a UNIQUE tiebreaker so a band
    // of equal-caller_count siblings truncates deterministically instead of
    // dropping a query-plan-dependent subset. Counting is one grouped scan over
    // the inbound `calls` edges of the visited set (idx_edges_target_rel covers
    // the predicate) — confidence-unfiltered, as in the CTE's `caller_counts`.
    let mut caller_counts: HashMap<i64, i64> = HashMap::new();
    for chunk in ids.chunks(FRONTIER_CHUNK) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT target_id, COUNT(*) FROM edges
             WHERE target_id IN ({placeholders}) AND relation = ?{rel}
             GROUP BY target_id",
            rel = chunk.len() + 1,
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut params: Vec<&dyn rusqlite::types::ToSql> = chunk
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();
        params.push(&REL_CALLS);
        let rows = stmt.query_map(params.as_slice(), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (id, n) = row?;
            caller_counts.insert(id, n);
        }
    }

    let mut rows: Vec<(i32, i64, i64, CallGraphNode)> = Vec::new(); // (depth, -callers, id, node)
    for (id, (node_depth, parent_id)) in seen {
        let Some((name, node_type, file_path, is_test)) = meta.get(&id).cloned() else {
            continue; // no `files` row → not emitted, same as the CTE's inner join
        };
        let callers = caller_counts.get(&id).copied().unwrap_or(0);
        rows.push((
            node_depth,
            -callers,
            id,
            CallGraphNode {
                node_id: id,
                name,
                node_type,
                file_path,
                depth: node_depth,
                direction,
                parent_id,
                is_test,
            },
        ));
    }
    rows.sort_unstable_by_key(|(d, neg_callers, id, _)| (*d, *neg_callers, *id));

    // The CTE applied `LIMIT CALL_GRAPH_ROW_LIMIT` to this same ordering, so the
    // retained rows are identical; truncating in Rust keeps `limit_hit`
    // meaning "the cap was reached", exactly as before.
    let limit_hit = rows.len() >= CALL_GRAPH_ROW_LIMIT;
    rows.truncate(CALL_GRAPH_ROW_LIMIT);
    let results: Vec<CallGraphNode> = rows.into_iter().map(|(_, _, _, n)| n).collect();

    Ok((results, limit_hit))
}

/// Count the seed symbol's DIRECT edges (in `direction`) whose confidence rank
/// is below `min_confidence_rank` — exactly the edges `query_direction`'s
/// recursive step pruned one level out. Mirrors the CTE's seed selection
/// (`name` + optional `file_path`) and the same rank CASE, so the number is the
/// true count of hidden direct fan-out. Returns 0 for a rank-0 (ambiguous) floor
/// — nothing ranks below it, so no query is run.
///
/// `pub` so impact surfaces can disclose how many ambiguous callers their
/// confidence floor excluded from the risk assessment (impact folds ambiguous
/// by default like callgraph, but there a hidden edge could be a real caller, so
/// the count must be surfaced — risk is never silently under-stated).
pub fn count_suppressed_seed_edges(
    conn: &Connection,
    function_name: &str,
    file_path: Option<&str>,
    direction: Direction,
    min_confidence_rank: u8,
) -> Result<usize> {
    if min_confidence_rank == 0 {
        return Ok(0);
    }
    // Callees: the seed is the edge SOURCE (edges out). Callers: the seed is the
    // edge TARGET (edges in). Matches `query_direction`'s edge orientation.
    let seed_col = match direction {
        Direction::Callees => "source_id",
        Direction::Callers => "target_id",
    };
    let sql = format!(
        "SELECT COUNT(*) FROM edges e
         JOIN nodes n ON n.id = e.{seed_col}
         JOIN files f ON f.id = n.file_id
         WHERE n.name = ?1 AND f.path <> '<external>'
           AND (?2 IS NULL OR f.path = ?2) AND e.relation = ?3
           AND (CASE e.confidence WHEN 'extracted' THEN 2 WHEN 'inferred' THEN 1 ELSE 0 END) < ?4"
    );
    let count: i64 = conn.query_row(
        &sql,
        rusqlite::params![
            function_name,
            file_path,
            REL_CALLS,
            min_confidence_rank as i64
        ],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

/// Count sub-floor `calls` edges INTO any node in `target_ids` — the callers the
/// confidence floor pruned from the traversal FRONTIER. Impact passes the whole
/// returned caller set (seed + kept callers) so a TRANSITIVE ambiguous caller —
/// one whose parent was kept but whose own inbound edge was sub-floor — is
/// disclosed too. Without this, a uniquely-named symbol (clean direct callers, so
/// `count_suppressed_seed_edges` returns 0 → no disclosure) whose deeper callers
/// are ambiguous would under-state risk with ZERO disclosure. Returns 0 for a
/// rank-0 floor or an empty set. `target_ids` is bounded by CALL_GRAPH_ROW_LIMIT,
/// well under SQLite's variable cap, so no chunking is needed.
pub fn count_suppressed_into(
    conn: &Connection,
    target_ids: &[i64],
    min_confidence_rank: u8,
) -> Result<usize> {
    if min_confidence_rank == 0 || target_ids.is_empty() {
        return Ok(0);
    }
    let placeholders = std::iter::repeat_n("?", target_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    // CASE mirrors domain::confidence_rank (extracted=2, inferred=1, else=0), same
    // as query_direction's conf_gate; `< rank` is the complement of the `>= rank`
    // traversal gate, so this counts exactly the edges that gate pruned.
    let sql = format!(
        "SELECT COUNT(*) FROM edges e \
         WHERE e.target_id IN ({placeholders}) \
           AND e.relation = ?{rel} \
           AND (CASE e.confidence WHEN 'extracted' THEN 2 WHEN 'inferred' THEN 1 ELSE 0 END) < ?{rank}",
        rel = target_ids.len() + 1,
        rank = target_ids.len() + 2,
    );
    let rel_param: &dyn rusqlite::types::ToSql = &REL_CALLS;
    let rank_param: i64 = min_confidence_rank as i64;
    let mut params: Vec<&dyn rusqlite::types::ToSql> = target_ids
        .iter()
        .map(|id| id as &dyn rusqlite::types::ToSql)
        .collect();
    params.push(rel_param);
    params.push(&rank_param);
    let count: i64 = conn.query_row(&sql, params.as_slice(), |row| row.get(0))?;
    Ok(count as usize)
}

/// Merge callee and caller results into one deterministic, relevance-ordered list.
///
/// Each input is already deduped by `node_id` (the SQL
/// `ROW_NUMBER() OVER (PARTITION BY node_id ...) WHERE rn = 1`) and ordered by
/// `(depth ASC, caller_count DESC, node_id ASC)` — its relevance order. The two
/// inputs carry disjoint `Direction` values and each is node_id-unique, so the
/// `(node_id, direction)` key is globally unique across the concatenation: there
/// is nothing to collapse. A node reachable both as a callee and a caller (e.g.
/// mutual recursion A→B, B→A) is intentionally kept once per direction so it shows
/// in both sections. We concatenate and STABLE-sort by depth, which preserves each
/// direction's relevance order within every depth band.
///
/// This must NOT round-trip through a `HashMap`: `HashMap::into_values()` iterates
/// in the map's per-instance random-seed order, so a stable-sort-by-depth
/// afterward left same-depth ties in a different order on every call. That made
/// the DEFAULT `callgraph <symbol>` (direction=both, both CLI and MCP) print the
/// same caller/callee set in a different order on every run, and left the JSON
/// `results[]` order unstable — defeating diff/reproducibility.
fn merge_results(
    mut callees: Vec<CallGraphNode>,
    callers: Vec<CallGraphNode>,
) -> Vec<CallGraphNode> {
    callees.extend(callers);
    callees.sort_by_key(|n| n.depth);
    callees
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::REL_CALLS;
    use crate::storage::db::Database;
    use crate::storage::queries::{insert_edge, insert_node, upsert_file, FileRecord, NodeRecord};
    use tempfile::TempDir;

    /// The pre-BFS recursive-CTE traversal, verbatim from the shipped v0.116.0
    /// implementation, kept ONLY as a differential oracle for the tests below. Its
    /// per-path `visited NOT LIKE` guard enumerates simple paths, so it is usable
    /// only on the small fixtures here — that cost is exactly why production no
    /// longer runs it.
    fn legacy_query_direction_cte(
        conn: &Connection,
        function_name: &str,
        max_depth: i32,
        file_path: Option<&str>,
        direction: Direction,
        min_confidence_rank: u8,
    ) -> Result<(Vec<CallGraphNode>, bool)> {
        let max_depth = max_depth.min(CALL_GRAPH_MAX_DEPTH); // Hard cap to prevent CTE blowup on highly connected graphs
                                                             // Use NULL sentinel: when file_path is None, pass NULL and the filter is always true
        let file_filter = "AND (?2 IS NULL OR f.path = ?2)";
        let file_path_param: Option<&str> = file_path;

        // In the recursive step:
        // - callees: follow edges forward (source_id = current, target_id = next)
        // - callers: follow edges backward (target_id = current, source_id = next)
        // Confidence gate (?5): only FOLLOW edges whose resolution-confidence rank
        // is >= the requested floor. The CASE mirrors `domain::confidence_rank`
        // (extracted=2, inferred=1, ambiguous/unknown=0) — a test pins the two in
        // sync. Spliced into the recursive step's edge JOIN so a sub-threshold edge
        // is pruned BEFORE it expands, which is what kills the ambiguous fan-out
        // (one `.execute()` → 56 same-named defs) at the source instead of after.
        let conf_gate =
            "AND (CASE e.confidence WHEN 'extracted' THEN 2 WHEN 'inferred' THEN 1 ELSE 0 END) >= ?5";
        let (edge_join, next_node_join) = match direction {
            Direction::Callees => (
                format!("JOIN edges e ON e.source_id = cg.node_id AND e.relation = ?4 {conf_gate}"),
                "JOIN nodes t ON t.id = e.target_id",
            ),
            Direction::Callers => (
                format!("JOIN edges e ON e.target_id = cg.node_id AND e.relation = ?4 {conf_gate}"),
                "JOIN nodes t ON t.id = e.source_id",
            ),
        };

        // The CTE tracks `parent_id` (the cg row that produced each new node) so
        // the renderer can show real tree edges instead of inferring nesting from
        // depth alone (which collapses sibling subtrees under the last depth-N
        // entry). On dedup we keep the parent on the shortest path via
        // ROW_NUMBER() ... ORDER BY depth.
        //
        // Truncation ordering: when a hot function (e.g. `conn` in this repo with
        // 51 callers + 72 test) saturates CALL_GRAPH_ROW_LIMIT at depth=3, the
        // pre-LIMIT sort is `depth ASC, caller_count DESC, node_id ASC` so
        // high-connectivity subtrees survive the truncation. `caller_count DESC` keeps
        // the most-relevant subtree; the `node_id ASC` tail is a UNIQUE tiebreaker so a
        // band of equal-caller_count siblings truncates deterministically instead of
        // dropping a query-plan-dependent subset (without it, alphabetical / id-order
        // ties would silently drop an arbitrary part of the most-relevant band). The
        // `caller_count` LEFT JOIN is a single non-correlated GROUP BY scan over edges
        // (idx_edges_target_rel covers the predicate); rowcount is bounded by node
        // count, not edge count.
        let sql = format!(
            "WITH RECURSIVE call_graph(node_id, name, type, depth, visited, parent_id) AS (
                SELECT n.id, n.name, n.type, 0, CAST(n.id AS TEXT), NULL
                FROM nodes n
                JOIN files f ON f.id = n.file_id
                -- Never SEED on an `<external>` sentinel. It has no outgoing calls,
                -- so the traversal returns a one-node graph whose root prints as
                -- `HashMap (<external>)` — a call graph for a symbol that is not in
                -- the project. The by-name lookups in `queries/nodes.rs` carry the
                -- same exclusion; this CTE seeds itself and needed its own.
                WHERE n.name = ?1 AND f.path <> '<external>'
                {file_filter}

                UNION ALL

                SELECT t.id, t.name, t.type, cg.depth + 1,
                       cg.visited || ',' || CAST(t.id AS TEXT),
                       cg.node_id
                FROM call_graph cg
                {edge_join}
                {next_node_join}
                WHERE cg.depth < ?3
                AND (',' || cg.visited || ',') NOT LIKE '%,' || CAST(t.id AS TEXT) || ',%'
            ),
            caller_counts AS (
                SELECT target_id AS node_id, COUNT(*) AS callers
                FROM edges
                WHERE relation = ?4
                GROUP BY target_id
            )
            SELECT node_id, name, type, file_path, depth, parent_id, is_test FROM (
                SELECT cg.node_id, cg.name, cg.type, f.path AS file_path, cg.depth, cg.parent_id,
                       n.is_test AS is_test,
                       COALESCE(cc.callers, 0) AS caller_count,
                       ROW_NUMBER() OVER (PARTITION BY cg.node_id ORDER BY cg.depth) AS rn
                FROM call_graph cg
                JOIN nodes n ON n.id = cg.node_id
                JOIN files f ON f.id = n.file_id
                LEFT JOIN caller_counts cc ON cc.node_id = cg.node_id
            ) WHERE rn = 1
            ORDER BY depth ASC, caller_count DESC, node_id ASC
            LIMIT {row_limit}",
            row_limit = CALL_GRAPH_ROW_LIMIT,
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
                parent_id: row.get(5)?,
                is_test: row.get(6)?,
            })
        };

        let results: Vec<CallGraphNode> = stmt
            .query_map(
                rusqlite::params![
                    function_name,
                    file_path_param,
                    max_depth,
                    REL_CALLS,
                    min_confidence_rank as i64
                ],
                map_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;

        let limit_hit = results.len() == CALL_GRAPH_ROW_LIMIT;
        Ok((results, limit_hit))
    }
    /// Deterministic 32-bit LCG — fixture generation must be reproducible.
    struct Lcg(u32);
    impl Lcg {
        fn next(&mut self, bound: usize) -> usize {
            self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (self.0 >> 8) as usize % bound
        }
    }

    /// `n` functions in one file plus `edges` pseudorandom `calls` edges. Dense
    /// and cyclic on purpose: that is the shape whose simple-path count the old
    /// CTE enumerated.
    fn seeded_call_graph(conn: &Connection, n: usize, edges: usize, seed: u32) -> Vec<i64> {
        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "src/g.rs".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();
        let ids: Vec<i64> = (0..n)
            .map(|i| insert_node(conn, &node(&format!("fn{i}"), fid)).unwrap())
            .collect();
        let mut rng = Lcg(seed);
        for _ in 0..edges {
            let a = ids[rng.next(n)];
            let b = ids[rng.next(n)];
            if a != b {
                // Duplicate (a,b) pairs are expected from the generator; the
                // unique index rejects them and `insert_edge` reports Ok.
                let _ = insert_edge(conn, a, b, REL_CALLS, None);
            }
        }
        ids
    }

    /// Every result node's `parent_id` must be a real edge from a node one level
    /// up — the property the CTE's `ROW_NUMBER … ORDER BY depth` provided and
    /// that the BFS must keep. (`parent_id` itself is not compared against the
    /// oracle row-for-row: with several shortest-path parents the CTE kept
    /// whichever its queue emitted first, an unpinned choice.)
    fn assert_parents_are_shortest_path_edges(
        conn: &Connection,
        results: &[CallGraphNode],
        direction: Direction,
    ) {
        let depth_of: HashMap<i64, i32> = results.iter().map(|n| (n.node_id, n.depth)).collect();
        for n in results {
            match n.parent_id {
                None => assert_eq!(n.depth, 0, "only seeds may have no parent"),
                Some(p) => {
                    assert_eq!(
                        depth_of.get(&p),
                        Some(&(n.depth - 1)),
                        "parent of {} must sit one level up",
                        n.node_id
                    );
                    let (s, t) = match direction {
                        Direction::Callees => (p, n.node_id),
                        Direction::Callers => (n.node_id, p),
                    };
                    let cnt: i64 = conn
                        .query_row(
                            "SELECT COUNT(*) FROM edges WHERE source_id=?1 AND target_id=?2 AND relation=?3",
                            rusqlite::params![s, t, REL_CALLS],
                            |r| r.get(0),
                        )
                        .unwrap();
                    assert!(cnt > 0, "parent link {s}->{t} must be a real calls edge");
                }
            }
        }
    }

    fn row_keys(nodes: &[CallGraphNode]) -> Vec<(i64, String, String, String, i32, bool)> {
        nodes
            .iter()
            .map(|n| {
                (
                    n.node_id,
                    n.name.clone(),
                    n.node_type.clone(),
                    n.file_path.clone(),
                    n.depth,
                    n.is_test,
                )
            })
            .collect()
    }

    #[test]
    fn traversal_matches_recursive_cte_oracle() {
        for seed in [3u32, 8_675_309] {
            let (db, _tmp) = test_db();
            let conn = db.conn();
            let ids = seeded_call_graph(conn, 14, 60, seed);
            assert!(!ids.is_empty());
            for depth in 1..=10 {
                for dir in [Direction::Callees, Direction::Callers] {
                    for i in 0..14 {
                        let name = format!("fn{i}");
                        let (bfs, bfs_limit) =
                            query_direction(conn, &name, depth, None, dir, 0).unwrap();
                        let (cte, cte_limit) =
                            legacy_query_direction_cte(conn, &name, depth, None, dir, 0).unwrap();
                        assert_eq!(
                            row_keys(&bfs),
                            row_keys(&cte),
                            "seed={seed} name={name} dir={dir:?} depth={depth}: \
                             BFS rows must equal the recursive-CTE oracle (order included)"
                        );
                        assert_eq!(bfs_limit, cte_limit, "limit_hit must agree");
                        assert_parents_are_shortest_path_edges(conn, &bfs, dir);
                    }
                }
            }
        }
    }

    #[test]
    fn traversal_matches_oracle_under_a_confidence_floor() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        let ids = seeded_call_graph(conn, 12, 50, 424_242);
        // Demote a third of the edges to `ambiguous` so the floor actually prunes.
        let mut rng = Lcg(99);
        for _ in 0..20 {
            let a = ids[rng.next(ids.len())];
            let b = ids[rng.next(ids.len())];
            set_edge_confidence(conn, a, b, "ambiguous");
        }
        for rank in [0u8, 1, 2] {
            for depth in 1..=10 {
                for dir in [Direction::Callees, Direction::Callers] {
                    for i in 0..12 {
                        let name = format!("fn{i}");
                        let (bfs, _) =
                            query_direction(conn, &name, depth, None, dir, rank).unwrap();
                        let (cte, _) =
                            legacy_query_direction_cte(conn, &name, depth, None, dir, rank)
                                .unwrap();
                        assert_eq!(
                            row_keys(&bfs),
                            row_keys(&cte),
                            "rank={rank} name={name} dir={dir:?} depth={depth}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn row_limit_truncation_matches_oracle() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "src/wide.rs".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();
        let root = insert_node(conn, &node("root", fid)).unwrap();
        // 250 direct callees > CALL_GRAPH_ROW_LIMIT, so the cap fires. Each
        // child gets a distinct number of inbound edges from the tail of the
        // list, exercising the `caller_count DESC` leg of the truncation sort.
        let kids: Vec<i64> = (0..250)
            .map(|i| insert_node(conn, &node(&format!("kid{i}"), fid)).unwrap())
            .collect();
        for (i, k) in kids.iter().enumerate() {
            insert_edge(conn, root, *k, REL_CALLS, None).unwrap();
            if i % 3 == 0 {
                insert_edge(conn, kids[(i + 1) % kids.len()], *k, REL_CALLS, None).unwrap();
            }
        }
        let (bfs, bfs_limit) =
            query_direction(conn, "root", 3, None, Direction::Callees, 0).unwrap();
        let (cte, cte_limit) =
            legacy_query_direction_cte(conn, "root", 3, None, Direction::Callees, 0).unwrap();
        assert_eq!(bfs.len(), CALL_GRAPH_ROW_LIMIT);
        assert!(bfs_limit && cte_limit, "both must report the cap was hit");
        assert_eq!(
            row_keys(&bfs),
            row_keys(&cte),
            "the truncated 200 rows and their order must be identical"
        );
    }

    #[test]
    fn dense_layered_graph_completes_at_max_depth() {
        // The audit's blowup shape: 11 layers of 6, fully connected between
        // adjacent layers (66 nodes / 360 edges). Simple paths from a layer-0
        // node to layer 10 number 6^10 ≈ 6.0e7, so the per-path-visited CTE did
        // not finish in two minutes; a global visited set walks 66 nodes.
        let (db, _tmp) = test_db();
        let conn = db.conn();
        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "src/layered.rs".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();
        const LAYERS: usize = 11;
        const WIDTH: usize = 6;
        let mut layers: Vec<Vec<i64>> = Vec::new();
        for l in 0..LAYERS {
            layers.push(
                (0..WIDTH)
                    .map(|w| insert_node(conn, &node(&format!("l{l}_w{w}"), fid)).unwrap())
                    .collect(),
            );
        }
        let mut edge_count = 0;
        for l in 0..LAYERS - 1 {
            for a in &layers[l] {
                for b in &layers[l + 1] {
                    insert_edge(conn, *a, *b, REL_CALLS, None).unwrap();
                    edge_count += 1;
                }
            }
        }
        assert_eq!((LAYERS * WIDTH, edge_count), (66, 360));

        let start = std::time::Instant::now();
        let (nodes, _) = query_direction(conn, "l0_w0", 10, None, Direction::Callees, 0).unwrap();
        let elapsed = start.elapsed();
        // Every node in layers 1..10 is reachable, plus the seed.
        assert_eq!(nodes.len(), 1 + WIDTH * (LAYERS - 1));
        assert_eq!(nodes.iter().filter(|n| n.depth == 10).count(), WIDTH);
        // Generous bound: the point is the difference between "milliseconds" and
        // "does not terminate", not a microbenchmark.
        assert!(
            elapsed.as_secs() < 10,
            "depth-10 traversal of a dense layered graph took {elapsed:?}"
        );
    }

    #[test]
    fn wide_frontier_spanning_chunks_matches_oracle() {
        // Frontier wider than FRONTIER_CHUNK → several `IN (...)` queries per
        // level, for expansion, node metadata and caller counts alike.
        //
        // Scope note: the 200-row cap means a node found by expanding the SECOND
        // chunk at depth 2 cannot appear in the output at all (the 450-node
        // depth-1 band fills the cap), so this test cannot discriminate broken
        // second-chunk EXPANSION — its uncapped sibling
        // `closure_spans_multiple_frontier_chunks_matching_oracle` in
        // `storage::queries::imports` does. What it does discriminate is the
        // metadata / caller-count stitching: the fixture gives the second
        // chunk's nodes the highest caller counts, so they sort to the front of
        // the band and a dropped chunk there is directly visible.
        let (db, _tmp) = test_db();
        let conn = db.conn();
        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "src/wide2.rs".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();
        let root = insert_node(conn, &node("root", fid)).unwrap();
        let wide = FRONTIER_CHUNK + 50;
        let kids: Vec<i64> = (0..wide)
            .map(|i| insert_node(conn, &node(&format!("kid{i}"), fid)).unwrap())
            .collect();
        for (i, k) in kids.iter().enumerate() {
            insert_edge(conn, root, *k, REL_CALLS, None).unwrap();
            // Second-chunk kids get extra inbound edges → caller_count 3 vs 1,
            // so they head the depth-1 band and survive truncation.
            if i >= FRONTIER_CHUNK {
                insert_edge(conn, kids[0], *k, REL_CALLS, None).unwrap();
                insert_edge(conn, kids[1], *k, REL_CALLS, None).unwrap();
            }
        }
        let (bfs, bfs_limit) =
            query_direction(conn, "root", 4, None, Direction::Callees, 0).unwrap();
        let (cte, cte_limit) =
            legacy_query_direction_cte(conn, "root", 4, None, Direction::Callees, 0).unwrap();
        assert_eq!(row_keys(&bfs), row_keys(&cte));
        assert_eq!((bfs_limit, cte_limit), (true, true));
        assert_parents_are_shortest_path_edges(conn, &bfs, Direction::Callees);

        // Skip the depth-0 seed; the depth-1 band starts right after it.
        let head: Vec<&str> = bfs
            .iter()
            .skip(1)
            .take(wide - FRONTIER_CHUNK)
            .map(|n| n.name.as_str())
            .collect();
        assert_eq!(
            head.first().copied(),
            Some("kid400"),
            "the highest-caller_count band comes first"
        );
        assert!(
            head.contains(&"kid449"),
            "nodes whose metadata lives in the second chunk must still be emitted"
        );
    }

    #[test]
    fn diamond_parent_is_the_first_discoverer_and_matches_the_cte() {
        // root→a, root→b, a→c, b→c: `c` has two shortest-path parents, so which
        // one lands in `parent_id` is a choice. It is a VISIBLE choice — the CLI
        // and MCP JSON both carry `parent_id` — so it is pinned here: the parent
        // discovered first (a, reached before b at depth 1). Asserting against
        // the CTE oracle in the same breath records that this reproduces what
        // SQLite's recursive-CTE queue used to emit, which is what kept the
        // rewrite byte-identical on real repositories.
        let (db, _tmp) = test_db();
        let conn = db.conn();
        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "src/dia.rs".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();
        let root = insert_node(conn, &node("root", fid)).unwrap();
        let a = insert_node(conn, &node("a", fid)).unwrap();
        let b = insert_node(conn, &node("b", fid)).unwrap();
        let c = insert_node(conn, &node("c", fid)).unwrap();
        for (s, t) in [(root, a), (root, b), (a, c), (b, c)] {
            insert_edge(conn, s, t, REL_CALLS, None).unwrap();
        }
        let (bfs, _) = query_direction(conn, "root", 5, None, Direction::Callees, 0).unwrap();
        let (cte, _) =
            legacy_query_direction_cte(conn, "root", 5, None, Direction::Callees, 0).unwrap();
        let parent_of = |rows: &[CallGraphNode], id: i64| {
            rows.iter().find(|n| n.node_id == id).unwrap().parent_id
        };
        assert_eq!(parent_of(&bfs, c), Some(a), "first discoverer wins");
        assert_eq!(
            parent_of(&cte, c),
            Some(a),
            "and that is what the CTE emitted"
        );
        assert_eq!(
            bfs.iter().find(|n| n.node_id == c).unwrap().depth,
            2,
            "c is reported once, at its shortest depth"
        );
    }

    #[test]
    fn traversal_is_deterministic_across_runs() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        seeded_call_graph(conn, 14, 60, 1_234_567);
        let first = get_call_graph(conn, "fn0", "both", 10, None).unwrap();
        let key = |r: &CallGraphResult| {
            r.nodes
                .iter()
                .map(|n| (n.node_id, n.depth, n.parent_id, n.direction.as_str()))
                .collect::<Vec<_>>()
        };
        let expected = key(&first);
        for _ in 0..8 {
            let again = get_call_graph(conn, "fn0", "both", 10, None).unwrap();
            assert_eq!(
                key(&again),
                expected,
                "repeated traversals must return identical rows in identical order"
            );
        }
    }

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

        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "test.ts".into(),
                blake3_hash: "h1".into(),
                last_modified: 1,
                language: Some("typescript".into()),
            },
        )
        .unwrap();

        let a = insert_node(conn, &node("A", fid)).unwrap();
        let b = insert_node(conn, &node("B", fid)).unwrap();
        let c = insert_node(conn, &node("C", fid)).unwrap();
        let d = insert_node(conn, &node("D", fid)).unwrap();

        insert_edge(conn, a, b, REL_CALLS, None).unwrap();
        insert_edge(conn, b, c, REL_CALLS, None).unwrap();
        insert_edge(conn, d, b, REL_CALLS, None).unwrap();

        let result = get_call_graph(conn, "A", "callees", 2, None).unwrap();

        // Should include A (depth 0), B (depth 1), C (depth 2)
        let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"A"), "should contain root node A");
        assert!(names.contains(&"B"), "should contain callee B");
        assert!(names.contains(&"C"), "should contain callee C");
        assert!(
            !names.contains(&"D"),
            "should NOT contain D (not a callee of A)"
        );

        // Verify depths
        let a_node = result.nodes.iter().find(|n| n.name == "A").unwrap();
        assert_eq!(a_node.depth, 0);
        let b_node = result.nodes.iter().find(|n| n.name == "B").unwrap();
        assert_eq!(b_node.depth, 1);
        let c_node = result.nodes.iter().find(|n| n.name == "C").unwrap();
        assert_eq!(c_node.depth, 2);
    }

    /// Query callers of B depth 2 → should contain A and D
    #[test]
    fn test_get_callers() {
        let (db, _tmp) = test_db();
        let conn = db.conn();

        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "test.ts".into(),
                blake3_hash: "h1".into(),
                last_modified: 1,
                language: Some("typescript".into()),
            },
        )
        .unwrap();

        let a = insert_node(conn, &node("A", fid)).unwrap();
        let b = insert_node(conn, &node("B", fid)).unwrap();
        let c = insert_node(conn, &node("C", fid)).unwrap();
        let d = insert_node(conn, &node("D", fid)).unwrap();

        insert_edge(conn, a, b, REL_CALLS, None).unwrap();
        insert_edge(conn, b, c, REL_CALLS, None).unwrap();
        insert_edge(conn, d, b, REL_CALLS, None).unwrap();

        let result = get_call_graph(conn, "B", "callers", 2, None).unwrap();

        let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"B"), "should contain root node B");
        assert!(names.contains(&"A"), "should contain caller A");
        assert!(names.contains(&"D"), "should contain caller D");
        assert!(
            !names.contains(&"C"),
            "should NOT contain C (C is a callee, not caller)"
        );

        // Verify depths
        let b_node = result.nodes.iter().find(|n| n.name == "B").unwrap();
        assert_eq!(b_node.depth, 0);
        let a_node = result.nodes.iter().find(|n| n.name == "A").unwrap();
        assert_eq!(a_node.depth, 1);
        let d_node = result.nodes.iter().find(|n| n.name == "D").unwrap();
        assert_eq!(d_node.depth, 1);
    }

    /// A→B→A mutual recursion. Query callees of A depth 10 → should terminate with <=3 results.
    #[test]
    fn test_cycle_detection() {
        let (db, _tmp) = test_db();
        let conn = db.conn();

        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "test.ts".into(),
                blake3_hash: "h1".into(),
                last_modified: 1,
                language: Some("typescript".into()),
            },
        )
        .unwrap();

        let a = insert_node(conn, &node("A", fid)).unwrap();
        let b = insert_node(conn, &node("B", fid)).unwrap();

        insert_edge(conn, a, b, REL_CALLS, None).unwrap();
        insert_edge(conn, b, a, REL_CALLS, None).unwrap();

        let result = get_call_graph(conn, "A", "callees", 10, None).unwrap();

        // Should terminate and contain at most A and B
        assert!(
            result.nodes.len() <= 2,
            "cycle detection should limit results to <=2, got {}",
            result.nodes.len()
        );

        let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"A"));
        assert!(names.contains(&"B"));
    }

    /// Query "both" on B → should contain A, D (callers) and C (callees)
    #[test]
    fn test_both_direction() {
        let (db, _tmp) = test_db();
        let conn = db.conn();

        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "test.ts".into(),
                blake3_hash: "h1".into(),
                last_modified: 1,
                language: Some("typescript".into()),
            },
        )
        .unwrap();

        let a = insert_node(conn, &node("A", fid)).unwrap();
        let b = insert_node(conn, &node("B", fid)).unwrap();
        let c = insert_node(conn, &node("C", fid)).unwrap();
        let d = insert_node(conn, &node("D", fid)).unwrap();

        insert_edge(conn, a, b, REL_CALLS, None).unwrap();
        insert_edge(conn, b, c, REL_CALLS, None).unwrap();
        insert_edge(conn, d, b, REL_CALLS, None).unwrap();

        let result = get_call_graph(conn, "B", "both", 2, None).unwrap();

        let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"B"), "should contain root node B");
        assert!(names.contains(&"A"), "should contain caller A");
        assert!(names.contains(&"D"), "should contain caller D");
        assert!(names.contains(&"C"), "should contain callee C");

        // B should be at depth 0
        let b_node = result.nodes.iter().find(|n| n.name == "B").unwrap();
        assert_eq!(b_node.depth, 0);
    }

    /// Verify parent_id is populated so the renderer can build a real tree.
    /// Setup: A→B→C, D→B. Query callers of C depth 2.
    /// Expected: B has parent_id=C; A and D have parent_id=B.
    #[test]
    fn test_parent_id_populated() {
        let (db, _tmp) = test_db();
        let conn = db.conn();

        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "test.ts".into(),
                blake3_hash: "h1".into(),
                last_modified: 1,
                language: Some("typescript".into()),
            },
        )
        .unwrap();

        let a = insert_node(conn, &node("A", fid)).unwrap();
        let b = insert_node(conn, &node("B", fid)).unwrap();
        let c = insert_node(conn, &node("C", fid)).unwrap();
        let d = insert_node(conn, &node("D", fid)).unwrap();

        insert_edge(conn, a, b, REL_CALLS, None).unwrap();
        insert_edge(conn, b, c, REL_CALLS, None).unwrap();
        insert_edge(conn, d, b, REL_CALLS, None).unwrap();

        let result = get_call_graph(conn, "C", "callers", 2, None).unwrap();

        let c_node = result.nodes.iter().find(|n| n.name == "C").unwrap();
        assert_eq!(c_node.parent_id, None, "root must have no parent");

        let b_node = result.nodes.iter().find(|n| n.name == "B").unwrap();
        assert_eq!(
            b_node.parent_id,
            Some(c),
            "depth-1 caller B's parent is the root C"
        );

        let a_node = result.nodes.iter().find(|n| n.name == "A").unwrap();
        assert_eq!(
            a_node.parent_id,
            Some(b),
            "depth-2 caller A's parent is depth-1 B (NOT C)"
        );
        let d_node = result.nodes.iter().find(|n| n.name == "D").unwrap();
        assert_eq!(
            d_node.parent_id,
            Some(b),
            "depth-2 caller D's parent is depth-1 B (NOT C)"
        );
    }

    /// Within a single depth, results are ordered by caller_count DESC so
    /// high-connectivity subtrees survive CALL_GRAPH_ROW_LIMIT truncation.
    /// Setup: R calls A1, A2, A3 (all depth 1).
    /// Additional callers boost A1 (5 extra) > A2 (1 extra) > A3 (0 extra).
    /// Query callees of R depth 1 → expect order [R, A1, A2, A3].
    #[test]
    fn test_callees_ordered_by_caller_count() {
        let (db, _tmp) = test_db();
        let conn = db.conn();

        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "test.ts".into(),
                blake3_hash: "h1".into(),
                last_modified: 1,
                language: Some("typescript".into()),
            },
        )
        .unwrap();

        let r = insert_node(conn, &node("R", fid)).unwrap();
        let a1 = insert_node(conn, &node("A1", fid)).unwrap();
        let a2 = insert_node(conn, &node("A2", fid)).unwrap();
        let a3 = insert_node(conn, &node("A3", fid)).unwrap();

        // R calls each A_i (gives every A_i one caller from R).
        insert_edge(conn, r, a1, REL_CALLS, None).unwrap();
        insert_edge(conn, r, a2, REL_CALLS, None).unwrap();
        insert_edge(conn, r, a3, REL_CALLS, None).unwrap();

        // External callers: 5 callers for A1, 1 caller for A2, 0 for A3.
        for i in 0..5 {
            let ext = insert_node(conn, &node(&format!("ext_a1_{}", i), fid)).unwrap();
            insert_edge(conn, ext, a1, REL_CALLS, None).unwrap();
        }
        let ext_a2 = insert_node(conn, &node("ext_a2", fid)).unwrap();
        insert_edge(conn, ext_a2, a2, REL_CALLS, None).unwrap();

        let result = get_call_graph(conn, "R", "callees", 1, None).unwrap();

        // Filter to depth=1 only (R itself is depth=0).
        let depth_1: Vec<&str> = result
            .nodes
            .iter()
            .filter(|n| n.depth == 1)
            .map(|n| n.name.as_str())
            .collect();
        assert_eq!(
            depth_1,
            vec!["A1", "A2", "A3"],
            "depth-1 callees must be ordered by caller_count DESC: A1(6) > A2(2) > A3(1)"
        );
    }

    /// requested depth > CALL_GRAPH_MAX_DEPTH must set depth_capped and clamp
    /// effective_max_depth without silently truncating.
    #[test]
    fn test_depth_capped_signal() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "test.ts".into(),
                blake3_hash: "h1".into(),
                last_modified: 1,
                language: Some("typescript".into()),
            },
        )
        .unwrap();
        let a = insert_node(conn, &node("A", fid)).unwrap();
        let b = insert_node(conn, &node("B", fid)).unwrap();
        insert_edge(conn, a, b, REL_CALLS, None).unwrap();

        let result = get_call_graph(conn, "A", "callees", 99, None).unwrap();
        assert!(result.depth_capped, "depth=99 must trip the cap");
        assert_eq!(result.requested_max_depth, 99);
        assert_eq!(result.effective_max_depth, CALL_GRAPH_MAX_DEPTH);
        assert!(
            !result.limit_hit,
            "this fixture has only 2 nodes, must not trigger row limit"
        );

        let small = get_call_graph(conn, "A", "callees", 5, None).unwrap();
        assert!(!small.depth_capped, "depth=5 must not trip the cap");
        assert_eq!(small.requested_max_depth, 5);
        assert_eq!(small.effective_max_depth, 5);
    }

    fn set_edge_confidence(conn: &Connection, src: i64, tgt: i64, conf: &str) {
        conn.execute(
            "UPDATE edges SET confidence = ?3 WHERE source_id = ?1 AND target_id = ?2",
            rusqlite::params![src, tgt, conf],
        )
        .unwrap();
    }

    /// The confidence filter prunes the recursive CTE traversal: at the default
    /// threshold (inferred, rank 1) an `ambiguous` edge — the bare-name fan-out
    /// class where a `.execute()` call resolves to every same-named def — is NOT
    /// followed, so it drops out of both directions, while `inferred`/`extracted`
    /// edges remain. Lowering the threshold to `ambiguous` (rank 0) restores the
    /// full graph. `suppressed_ambiguous` reports how many direct seed edges were
    /// hidden so the surface can point the user at `--min-confidence ambiguous`.
    #[test]
    fn test_min_confidence_filters_ambiguous_edges() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "test.ts".into(),
                blake3_hash: "h1".into(),
                last_modified: 1,
                language: Some("typescript".into()),
            },
        )
        .unwrap();

        let s = insert_node(conn, &node("S", fid)).unwrap();
        let b = insert_node(conn, &node("B", fid)).unwrap();
        let c = insert_node(conn, &node("C", fid)).unwrap();
        let p = insert_node(conn, &node("P", fid)).unwrap();

        // S calls B (inferred — kept) and C (ambiguous — fan-out noise, hidden).
        insert_edge(conn, s, b, REL_CALLS, None).unwrap();
        insert_edge(conn, s, c, REL_CALLS, None).unwrap();
        // P calls S, ambiguous — an ambiguous CALLER, hidden in the callers view.
        insert_edge(conn, p, s, REL_CALLS, None).unwrap();
        set_edge_confidence(conn, s, b, crate::domain::CONF_INFERRED);
        set_edge_confidence(conn, s, c, crate::domain::CONF_AMBIGUOUS);
        set_edge_confidence(conn, p, s, crate::domain::CONF_AMBIGUOUS);

        let inferred = crate::domain::confidence_rank(crate::domain::CONF_INFERRED);
        let show_all = crate::domain::confidence_rank(crate::domain::CONF_AMBIGUOUS);

        // Callees at default threshold: B kept, C (ambiguous) pruned.
        let callees = get_call_graph_filtered(conn, "S", "callees", 2, None, inferred).unwrap();
        let cn: Vec<&str> = callees.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            cn.contains(&"B"),
            "inferred callee kept at default threshold"
        );
        assert!(
            !cn.contains(&"C"),
            "ambiguous callee pruned at default threshold"
        );
        assert_eq!(
            callees.suppressed_ambiguous, 1,
            "one ambiguous direct callee hidden"
        );

        // Callers at default threshold: ambiguous caller P pruned.
        let callers = get_call_graph_filtered(conn, "S", "callers", 2, None, inferred).unwrap();
        let rn: Vec<&str> = callers.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            !rn.contains(&"P"),
            "ambiguous caller pruned at default threshold"
        );
        assert_eq!(
            callers.suppressed_ambiguous, 1,
            "one ambiguous direct caller hidden"
        );

        // Lowering the threshold to ambiguous restores everything; nothing suppressed.
        let all = get_call_graph_filtered(conn, "S", "both", 2, None, show_all).unwrap();
        let an: Vec<&str> = all.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            an.contains(&"C"),
            "ambiguous callee shown when threshold lowered to ambiguous"
        );
        assert!(
            an.contains(&"P"),
            "ambiguous caller shown when threshold lowered to ambiguous"
        );
        assert_eq!(
            all.suppressed_ambiguous, 0,
            "no edges below an ambiguous threshold"
        );

        // Back-compat: the bare get_call_graph wrapper shows all (rank 0).
        let compat = get_call_graph(conn, "S", "callees", 2, None).unwrap();
        let kn: Vec<&str> = compat.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            kn.contains(&"C"),
            "bare get_call_graph preserves show-all behavior"
        );
    }

    /// Pins the `extracted` tier of the SQL rank CASE (the leg
    /// test_min_confidence_filters_ambiguous_edges does not exercise) against
    /// `domain::confidence_rank`: an `extracted` edge survives the `inferred`
    /// floor, and the `extracted` floor drops an `inferred` edge. If a tier were
    /// re-mapped in Rust OR SQL alone, one of these assertions flips.
    #[test]
    fn test_min_confidence_extracted_tier_parity() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "test.ts".into(),
                blake3_hash: "h1".into(),
                last_modified: 1,
                language: Some("typescript".into()),
            },
        )
        .unwrap();
        let s = insert_node(conn, &node("S", fid)).unwrap();
        let x = insert_node(conn, &node("X", fid)).unwrap();
        let y = insert_node(conn, &node("Y", fid)).unwrap();
        insert_edge(conn, s, x, REL_CALLS, None).unwrap();
        insert_edge(conn, s, y, REL_CALLS, None).unwrap();
        set_edge_confidence(conn, s, x, crate::domain::CONF_EXTRACTED);
        set_edge_confidence(conn, s, y, crate::domain::CONF_INFERRED);

        let extracted = crate::domain::confidence_rank(crate::domain::CONF_EXTRACTED);
        let inferred = crate::domain::confidence_rank(crate::domain::CONF_INFERRED);

        // inferred floor (rank 1): extracted(2) and inferred(1) both survive.
        let at_inferred = get_call_graph_filtered(conn, "S", "callees", 2, None, inferred).unwrap();
        let n1: Vec<&str> = at_inferred.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            n1.contains(&"X") && n1.contains(&"Y"),
            "inferred floor keeps both extracted and inferred edges; got {n1:?}"
        );

        // extracted floor (rank 2): only extracted(2) survives; inferred(1) dropped + counted.
        let at_extracted =
            get_call_graph_filtered(conn, "S", "callees", 2, None, extracted).unwrap();
        let n2: Vec<&str> = at_extracted.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            n2.contains(&"X"),
            "extracted floor keeps the extracted edge; got {n2:?}"
        );
        assert!(
            !n2.contains(&"Y"),
            "extracted floor drops the inferred edge; got {n2:?}"
        );
        assert_eq!(
            at_extracted.suppressed_ambiguous, 1,
            "the inferred edge counts as suppressed at the extracted floor"
        );
    }

    /// Regression: `direction="both"` output MUST be deterministic and preserve
    /// each direction's relevance order. `merge_results` previously collected
    /// `HashMap::into_values()`, whose per-instance random seed reordered
    /// same-depth ties — so the DEFAULT `callgraph <symbol>` (direction=both, both
    /// CLI and MCP) printed the same caller/callee SET in a different order on
    /// every run, and the JSON `results[]` order was unstable, defeating
    /// diff/repro. Here S has depth-1 callers K5>K4>K3>K2>K1 by caller_count; the
    /// merged output must list them in that caller_count-DESC order (equivalently:
    /// exactly the deterministic order the SQL already produces per direction).
    #[test]
    fn test_both_direction_deterministic_and_relevance_ordered() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "test.ts".into(),
                blake3_hash: "h1".into(),
                last_modified: 1,
                language: Some("typescript".into()),
            },
        )
        .unwrap();

        let s = insert_node(conn, &node("S", fid)).unwrap();
        // Five callers of S, each given a distinct caller_count (K_i has i extra
        // callers) so the relevance order within depth 1 is unambiguous:
        // K5(5) > K4(4) > K3(3) > K2(2) > K1(1).
        let mut callers = Vec::new();
        for i in 1..=5 {
            let k = insert_node(conn, &node(&format!("K{i}"), fid)).unwrap();
            insert_edge(conn, k, s, REL_CALLS, None).unwrap(); // K_i calls S
            for j in 0..i {
                let ext = insert_node(conn, &node(&format!("ext_{i}_{j}"), fid)).unwrap();
                insert_edge(conn, ext, k, REL_CALLS, None).unwrap(); // boost caller_count(K_i)
            }
            callers.push(format!("K{i}"));
        }
        // S also calls two callees so the merge exercises both directions.
        let m1 = insert_node(conn, &node("M1", fid)).unwrap();
        let m2 = insert_node(conn, &node("M2", fid)).unwrap();
        insert_edge(conn, s, m1, REL_CALLS, None).unwrap();
        insert_edge(conn, s, m2, REL_CALLS, None).unwrap();

        let full_order = |r: &CallGraphResult| -> Vec<(String, &'static str, i32)> {
            r.nodes
                .iter()
                .map(|n| (n.name.clone(), n.direction.as_str(), n.depth))
                .collect()
        };

        let run1 = get_call_graph(conn, "S", "both", 1, None).unwrap();
        let run2 = get_call_graph(conn, "S", "both", 1, None).unwrap();

        // Determinism: two identical queries → identical node order.
        assert_eq!(
            full_order(&run1),
            full_order(&run2),
            "direction=both must be deterministic across calls"
        );

        // Relevance preserved through merge: depth-1 callers in caller_count-DESC order.
        let got: Vec<&str> = run1
            .nodes
            .iter()
            .filter(|n| n.depth == 1 && matches!(n.direction, Direction::Callers))
            .map(|n| n.name.as_str())
            .collect();
        let expected: Vec<&str> = vec!["K5", "K4", "K3", "K2", "K1"];
        assert_eq!(got, expected,
            "depth-1 callers must keep caller_count-DESC relevance order through merge; got {got:?}");
    }

    /// The final SQL sort carries a unique `node_id ASC` tiebreaker after
    /// `(depth ASC, caller_count DESC)`. A band of equal-caller_count siblings
    /// therefore returns in a specified, stable order — without it, the
    /// `LIMIT CALL_GRAPH_ROW_LIMIT` truncation on a wide fan-out could drop an
    /// arbitrary, query-plan-dependent subset (the same silent-truncation class
    /// the `caller_count DESC` key already guards for the primary sort). Here S
    /// calls four callees all with caller_count 1 (a full tie); they must come
    /// back in node_id-ascending order.
    #[test]
    fn test_tie_band_orders_by_node_id() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "test.ts".into(),
                blake3_hash: "h1".into(),
                last_modified: 1,
                language: Some("typescript".into()),
            },
        )
        .unwrap();
        let s = insert_node(conn, &node("S", fid)).unwrap();
        // Insert out of alphabetical order to prove node_id (insertion order), not
        // name, is the tiebreaker; each is called only by S → caller_count == 1.
        for name in ["Z", "A", "M", "B"] {
            let c = insert_node(conn, &node(name, fid)).unwrap();
            insert_edge(conn, s, c, REL_CALLS, None).unwrap();
        }

        let result = get_call_graph(conn, "S", "callees", 1, None).unwrap();
        let ids: Vec<i64> = result
            .nodes
            .iter()
            .filter(|n| n.depth == 1)
            .map(|n| n.node_id)
            .collect();
        let mut ascending = ids.clone();
        ascending.sort_unstable();
        assert_eq!(
            ids, ascending,
            "equal-caller_count callees must be ordered by node_id ASC; got {ids:?}"
        );
    }
}
