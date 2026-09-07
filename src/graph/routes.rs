//! Route-aware caller traversal. Composes a call-graph traversal (this layer)
//! with route-edge metadata (storage layer). Lives in `graph`, not `storage`,
//! so storage never has to import graph — the dependency runs one way
//! (graph → storage). See tests/hardening.rs::no_storage_module_imports_graph.

use anyhow::Result;
use rusqlite::Connection;

use crate::graph::query::get_call_graph_filtered;
use crate::storage::queries::routes::fetch_route_metadata_map;
use crate::storage::queries::CallerWithRouteInfo;

/// Route-annotated callers, WITH the traversal's truncation provenance.
///
/// The provenance is not decoration: every impact surface derives
/// `total_callers` / `affected_files` / `risk` from this list, so a traversal
/// that stopped at `CALL_GRAPH_ROW_LIMIT` produces a risk verdict computed from
/// a partial set. This used to be dropped here — the function returned
/// `Vec<CallerWithRouteInfo>` and threw the rest of `CallGraphResult` away — so
/// `callgraph conn --direction callers` answered `limit_hit: true` while
/// `impact conn` answered "78 callers" for the same traversal, with no key
/// saying the number was a floor (CORE-11, audit 2026-09-07). It is the same
/// class of hole `CallGraphResult::suppressed_ambiguous` was added to close:
/// risk must never be silently under-stated.
#[derive(Default)]
pub struct RouteCallers {
    pub callers: Vec<CallerWithRouteInfo>,
    /// The recursive CTE hit `CALL_GRAPH_ROW_LIMIT` — more callers exist.
    pub limit_hit: bool,
    /// The requested depth exceeded `CALL_GRAPH_MAX_DEPTH` — deeper callers exist.
    pub depth_capped: bool,
}

impl RouteCallers {
    /// True when the caller set is a prefix of the real one.
    pub fn truncated(&self) -> bool {
        self.limit_hit || self.depth_capped
    }

    /// One sentence naming why the set is partial and what it makes the numbers
    /// mean. Single-sourced so every impact surface says the same thing —
    /// `attach_truncation_flags` is the equivalent for the call-graph surfaces.
    pub fn truncation_note(&self) -> Option<String> {
        if !self.truncated() {
            return None;
        }
        let cause = match (self.limit_hit, self.depth_capped) {
            (true, true) => format!(
                "hit the {}-row traversal limit and the depth cap",
                crate::graph::query::CALL_GRAPH_ROW_LIMIT
            ),
            (true, false) => format!(
                "hit the {}-row traversal limit",
                crate::graph::query::CALL_GRAPH_ROW_LIMIT
            ),
            (false, true) => "hit the depth cap".to_string(),
            (false, false) => unreachable!("guarded by truncated()"),
        };
        Some(format!(
            "Caller traversal {cause}, so the caller counts, affected files and risk level \
             below are computed from a PARTIAL caller set and are a FLOOR, not a total. \
             Narrow the query (file_path / a more specific symbol) or read the full \
             traversal with get_call_graph / `callgraph --direction callers`."
        ))
    }
}

/// Callers of `symbol_name`, each annotated with its `routes_to` metadata if the
/// caller is itself a route handler. `min_confidence_rank` filters caller edges
/// (see domain::confidence_rank).
pub fn get_callers_with_route_info(
    conn: &Connection,
    symbol_name: &str,
    file_path: Option<&str>,
    max_depth: i32,
    min_confidence_rank: u8,
) -> Result<RouteCallers> {
    let callers = get_call_graph_filtered(
        conn,
        symbol_name,
        "callers",
        max_depth,
        file_path,
        min_confidence_rank,
    )?;
    // The flags are read off the traversal even when it returned nothing: a
    // depth request above the cap is still a capped answer, and reporting
    // "0 callers, complete" for it would be the same false-total this struct
    // exists to prevent.
    let limit_hit = callers.limit_hit;
    let depth_capped = callers.depth_capped;
    if callers.nodes.is_empty() {
        return Ok(RouteCallers {
            callers: vec![],
            limit_hit,
            depth_capped,
        });
    }
    let caller_ids: Vec<i64> = callers.nodes.iter().map(|c| c.node_id).collect();
    let route_map = fetch_route_metadata_map(conn, &caller_ids)?;
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
            is_test: caller.is_test,
        })
        .collect();
    Ok(RouteCallers {
        callers: results,
        limit_hit,
        depth_capped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::queries::helpers::test_db;

    #[test]
    fn test_callers_with_routes() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('test.ts', 'h1', 0, 'typescript', 0)", []).unwrap();
        conn.execute("INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) VALUES (1, 'function', 'handler', 'handler', 1, 10, 'fn handler()')", []).unwrap();
        conn.execute("INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) VALUES (1, 'function', 'caller', 'caller', 11, 20, 'fn caller()')", []).unwrap();
        conn.execute("INSERT INTO edges (source_id, target_id, relation, metadata) VALUES (2, 1, 'calls', NULL)", []).unwrap();
        conn.execute("INSERT INTO edges (source_id, target_id, relation, metadata) VALUES (2, 2, 'routes_to', '{\"method\":\"GET\",\"path\":\"/api/test\"}')", []).unwrap();
        let results = get_callers_with_route_info(conn, "handler", None, 3, 0).unwrap();
        assert!(!results.callers.is_empty());
        assert!(results.callers.iter().any(|r| r.route_info.is_some()));
        assert!(
            !results.truncated(),
            "a two-node fixture cannot saturate the traversal — if this flips, the \
             provenance is being read from the wrong place"
        );
        assert_eq!(results.truncation_note(), None);
    }

    /// CORE-11. The traversal's row limit is what makes an impact verdict a
    /// floor rather than a total, and this function used to drop the flag on
    /// the floor. Built by direct insert rather than by parsing a generated
    /// source file: the assertion is about provenance surviving this function,
    /// and a parser fixture would put an extractor between the flag and the
    /// test.
    #[test]
    fn saturating_the_row_limit_is_reported_not_dropped() {
        use crate::graph::query::CALL_GRAPH_ROW_LIMIT;
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute("INSERT INTO files (path, blake3_hash, last_modified, language, indexed_at) VALUES ('big.rs', 'h1', 0, 'rust', 0)", []).unwrap();
        conn.execute("INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) VALUES (1, 'function', 'hot', 'hot', 1, 1, '')", []).unwrap();
        // One more caller than the traversal will return.
        let callers = CALL_GRAPH_ROW_LIMIT + 5;
        for i in 0..callers {
            conn.execute(
                "INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content) \
                 VALUES (1, 'function', ?1, ?1, ?2, ?2, '')",
                rusqlite::params![format!("caller_{i}"), (i + 2) as i64],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO edges (source_id, target_id, relation, metadata) VALUES (?1, 1, 'calls', NULL)",
                [(i + 2) as i64],
            )
            .unwrap();
        }

        let result = get_callers_with_route_info(conn, "hot", None, 3, 0).unwrap();
        assert!(
            result.limit_hit,
            "precondition: {callers} callers must saturate the {CALL_GRAPH_ROW_LIMIT}-row limit"
        );
        assert!(result.truncated());
        let note = result
            .truncation_note()
            .expect("a truncated traversal must carry a note");
        assert!(
            note.contains("FLOOR") && note.contains(&CALL_GRAPH_ROW_LIMIT.to_string()),
            "the note must say the numbers are a floor and name the limit: {note}"
        );
        // Negative control: the same query below the limit must stay silent, so
        // the assertions above are about saturation and not about a flag that
        // is always on.
        conn.execute("DELETE FROM edges WHERE source_id > 20", [])
            .unwrap();
        let small = get_callers_with_route_info(conn, "hot", None, 3, 0).unwrap();
        assert!(
            !small.truncated(),
            "19 callers is not a truncated traversal"
        );
        assert_eq!(small.truncation_note(), None);
    }
}
