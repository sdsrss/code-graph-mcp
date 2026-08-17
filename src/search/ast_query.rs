//! Shared core for the structural-search twins: MCP `ast_search`
//! (`src/mcp/server/tools/ast_search.rs`) and CLI `ast-search`
//! (`cmd_ast_search` in `src/cli.rs`).
//!
//! The two used to be ~230-line copies of the same pipeline, and they had
//! drifted: the MCP copy grew a name-substring fallback the CLI never got, so
//! the same query answered differently depending on the surface (audit
//! 2026-08-16 P1-8). Both now call [`run`]; only presentation (JSON envelope vs
//! stdout lines, hint wording) stays per-surface.
//!
//! The candidate pool is sized by [`crate::domain::search_fetch_count`] — the
//! same filter-aware widening `semantic_code_search` uses — because the filters
//! (`type`/`returns`/`params` plus the always-on module/external/test skip) are
//! applied in Rust AFTER the FTS fetch. The old fixed `limit * 4` let a
//! selective filter return nothing while matches sat just below the cut.

use anyhow::Result;
use rusqlite::Connection;

use crate::storage::queries::{self, NodeWithFile};

/// Inputs shared by both surfaces. `limit` is the caller's post-filter cap.
pub struct AstSearchParams<'a> {
    pub query: Option<&'a str>,
    pub type_filter: Option<&'a str>,
    pub returns_filter: Option<&'a str>,
    pub params_filter: Option<&'a str>,
    pub limit: usize,
}

/// Result of a structural search, with enough accounting for each surface to
/// tell the caller what actually happened instead of guessing a remedy.
pub struct AstSearchOutcome {
    /// Matches, already capped at `limit` and in FTS-rank order.
    pub results: Vec<NodeWithFile>,
    /// The FTS query itself returned nothing (distinct from "filtered to zero").
    pub fts_empty: bool,
    /// Candidates that matched the query but failed `type`/`returns`/`params`.
    /// The module/external/test skip is deliberately NOT counted: it is internal
    /// hygiene, not a filter the caller chose.
    pub dropped_by_filter: usize,
    /// Exact number of surviving matches inside the candidate pool, before the
    /// `limit` cut. `None` when the count is not knowable (the name-substring
    /// fallback and the filter-only SQL path are LIMIT-bounded in SQL).
    pub matched_total: Option<usize>,
    /// More matches existed than `limit` returned.
    pub truncated: bool,
    /// The FTS fetch came back full, so matches may exist below the cut and any
    /// "nothing matched" statement is bounded by the pool, not by the index.
    pub pool_saturated: bool,
    /// Size of the candidate pool that was fetched.
    pub fetch_count: i64,
    /// Results came from the name-substring fallback, not the FTS pool.
    pub fallback_used: bool,
}

/// Which surface the hints are worded for. Only the *spelling* of the remedy
/// differs (`--limit 20` on the CLI, `` `limit` `` for an MCP caller who passes
/// arguments, not flags) — the ORDER is shared, because the order was the bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintStyle {
    Cli,
    Mcp,
}

impl HintStyle {
    fn limit_remedy(self, limit: usize) -> String {
        match self {
            HintStyle::Cli => format!("--limit {limit} — raise --limit"),
            HintStyle::Mcp => format!("limit={limit} — raise `limit`"),
        }
    }
    fn broaden_remedy(self, rows: i64) -> String {
        match self {
            HintStyle::Cli => format!(
                "The candidate pool was full ({rows} rows), so matches may exist below it. Narrow the query, raise --limit, or drop the query and enumerate with the filters alone."
            ),
            HintStyle::Mcp => format!(
                "The candidate pool was full ({rows} rows), so matches may exist below it. Narrow the query, raise `limit`, or drop `query` and enumerate with the filters alone."
            ),
        }
    }
}

/// Every hint an `ast_search` answer owes its caller, most-actionable first.
///
/// Both surfaces used to assign their `hint` field from several independent
/// `if` blocks, so the last statement executed won and the others vanished —
/// and the two surfaces disagreed about which that was. For a result set that
/// is BOTH truncated and answered by the name-substring fallback (reachable:
/// the fallback path carries its own `truncated`), the CLI kept only the
/// fallback note and MCP kept only the truncation notice, so the same query
/// got a different single hint depending on who asked, and one surface always
/// dropped the disclosure that stops "count: 20" being read as "20 matches
/// exist" (audit 2026-08-16 review Minor tail).
///
/// Returning the ordered set — rather than picking a winner — is what makes
/// "several things are true at once" expressible in a single string field.
/// Callers join with a space; the leading sentence is the one that matters most.
///
/// Order: why-it-is-empty first (it explains the answer), then the truncation
/// cut, then the provenance note.
pub fn hints(
    outcome: &AstSearchOutcome,
    query: Option<&str>,
    limit: usize,
    style: HintStyle,
) -> Vec<String> {
    let mut out = Vec::new();
    if outcome.results.is_empty() && outcome.dropped_by_filter > 0 {
        out.push(if outcome.pool_saturated {
            style.broaden_remedy(outcome.fetch_count)
        } else {
            "The index has no symbol matching both the query and the filter. Broaden or clear the filter.".to_string()
        });
    }
    if outcome.truncated {
        let remedy = style.limit_remedy(limit);
        out.push(match outcome.matched_total {
            Some(total) => format!("{total} symbols matched but {remedy} to see the rest."),
            None => format!("More symbols matched than {remedy} to see the rest."),
        });
    }
    if outcome.fallback_used {
        out.push(format!(
            "FTS rank had no '{}' under the active filter; falling back to name-substring match.",
            query.unwrap_or_default()
        ));
    }
    out
}

impl AstSearchOutcome {
    fn empty(fts_empty: bool, fetch_count: i64) -> Self {
        Self {
            results: Vec::new(),
            fts_empty,
            dropped_by_filter: 0,
            matched_total: Some(0),
            truncated: false,
            pool_saturated: false,
            fetch_count,
            fallback_used: false,
        }
    }
}

/// Run a structural search. Two paths, exactly as before: FTS + Rust-side
/// column filtering when a query is given, direct SQL when only filters are.
pub fn run(conn: &Connection, p: &AstSearchParams<'_>) -> Result<AstSearchOutcome> {
    let has_filters =
        p.type_filter.is_some() || p.returns_filter.is_some() || p.params_filter.is_some();

    let Some(query) = p.query else {
        return filter_only(conn, p);
    };

    // Filter-aware pool: the post-fetch filters can reject an arbitrary share of
    // the FTS hits, so the pool has to be sized for them (shared policy with
    // semantic_code_search via search_fetch_count).
    let fetch_count = crate::domain::search_fetch_count(p.limit as i64, has_filters);
    let fts_result = queries::fts5_search(conn, query, fetch_count)?;
    if fts_result.nodes.is_empty() {
        return Ok(AstSearchOutcome::empty(true, fetch_count));
    }
    let pool_saturated = fts_result.nodes.len() as i64 >= fetch_count;

    let node_ids: Vec<i64> = fts_result.nodes.iter().map(|n| n.id).collect();
    let all = queries::get_nodes_with_files_by_ids(conn, &node_ids)?;

    // Preserve FTS5 rank order (the id list is ranked; the batch fetch is not).
    let id_order: std::collections::HashMap<i64, usize> = node_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, i))
        .collect();
    let mut sorted = all;
    sorted.sort_by_key(|nwf| id_order.get(&nwf.node.id).copied().unwrap_or(usize::MAX));

    let mut dropped_by_filter = 0usize;
    let mut survivors: Vec<NodeWithFile> = Vec::new();
    for nwf in sorted {
        let n = &nwf.node;
        // Skip <module>/<external> placeholders and test symbols, consistent
        // with search/similar (domain::is_skippable_result).
        if crate::domain::is_skippable_result(n.is_test, &n.node_type, &n.name, &nwf.file_path) {
            continue;
        }
        if let Some(tf) = p.type_filter {
            let types = crate::domain::normalize_type_filter(tf);
            if !types.iter().any(|t| n.node_type == *t) {
                dropped_by_filter += 1;
                continue;
            }
        }
        if let Some(rf) = p.returns_filter {
            match &n.return_type {
                Some(rt) if rt.to_lowercase().contains(&rf.to_lowercase()) => {}
                _ => {
                    dropped_by_filter += 1;
                    continue;
                }
            }
        }
        if let Some(pf) = p.params_filter {
            match &n.param_types {
                Some(pt) if pt.to_lowercase().contains(&pf.to_lowercase()) => {}
                _ => {
                    dropped_by_filter += 1;
                    continue;
                }
            }
        }
        survivors.push(nwf);
    }

    let matched_total = survivors.len();
    let truncated = matched_total > p.limit;
    survivors.truncate(p.limit);

    // FTS-rank fallback: when query+type returns zero (FTS rank can drown
    // structs/enums under function-name hits — e.g. query="Result" type=struct
    // bottoms out because the top FTS hits for "Result" are functions like
    // `compress_results`), retry as SQL `name LIKE '%query%'` + filters.
    // Single-identifier queries only — a multi-word/operator query is not a
    // useful LIKE pattern.
    if survivors.is_empty() && p.type_filter.is_some() && is_identifier_like(query) {
        let retry = name_substring_lookup(conn, p, query)?;
        if !retry.0.is_empty() {
            return Ok(AstSearchOutcome {
                results: retry.0,
                fts_empty: false,
                dropped_by_filter,
                matched_total: None,
                truncated: retry.1,
                pool_saturated,
                fetch_count,
                fallback_used: true,
            });
        }
    }

    Ok(AstSearchOutcome {
        results: survivors,
        fts_empty: false,
        dropped_by_filter,
        matched_total: Some(matched_total),
        truncated,
        pool_saturated,
        fetch_count,
        fallback_used: false,
    })
}

/// `name LIKE '%query%'` + the structural filters. Returns the capped rows and
/// whether more existed (detected by asking for one row past `limit`).
fn name_substring_lookup(
    conn: &Connection,
    p: &AstSearchParams<'_>,
    query: &str,
) -> Result<(Vec<NodeWithFile>, bool)> {
    let normalized = p.type_filter.map(crate::domain::normalize_type_filter);
    let type_refs: Option<Vec<&str>> = normalized.as_ref().map(|v| v.to_vec());
    let mut rows = queries::get_nodes_with_files_by_filters(
        conn,
        type_refs.as_deref(),
        p.returns_filter,
        p.params_filter,
        Some(query),
        p.limit.saturating_add(1),
    )?;
    let more = rows.len() > p.limit;
    rows.truncate(p.limit);
    Ok((rows, more))
}

/// Filter-only path: no query, so the filters go straight into SQL.
fn filter_only(conn: &Connection, p: &AstSearchParams<'_>) -> Result<AstSearchOutcome> {
    let normalized = p.type_filter.map(crate::domain::normalize_type_filter);
    let type_refs: Option<Vec<&str>> = normalized.as_ref().map(|v| v.to_vec());
    // One past `limit` so "there are more" is a measurement, not a guess.
    let mut rows = queries::get_nodes_with_files_by_filters(
        conn,
        type_refs.as_deref(),
        p.returns_filter,
        p.params_filter,
        None,
        p.limit.saturating_add(1),
    )?;
    let truncated = rows.len() > p.limit;
    rows.truncate(p.limit);
    Ok(AstSearchOutcome {
        results: rows,
        fts_empty: false,
        dropped_by_filter: 0,
        matched_total: None,
        truncated,
        pool_saturated: false,
        fetch_count: 0,
        fallback_used: false,
    })
}

/// True when `s` looks like a single identifier (alphanumeric + underscore, no
/// whitespace). Gates the name-substring fallback — multi-word queries like
/// "function returning result" must not silently turn into LIKE patterns.
pub fn is_identifier_like(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db::Database;

    #[test]
    fn test_is_identifier_like() {
        assert!(is_identifier_like("Result"));
        assert!(is_identifier_like("FtsResult"));
        assert!(is_identifier_like("snake_case"));
        assert!(is_identifier_like("with42numbers"));
        assert!(is_identifier_like("中文标识符"));
        assert!(!is_identifier_like(""));
        assert!(!is_identifier_like("two words"));
        assert!(!is_identifier_like("Result<T>"));
        assert!(!is_identifier_like("a:b"));
        assert!(!is_identifier_like("path/to/file"));
    }

    /// Build an index where `n_fns` functions repeat the term "node" and
    /// `struct_names` structs carry it in their names, so the structs rank below
    /// the functions in BM25 order.
    fn drown_index(n_fns: usize, struct_names: &[&str]) -> (Database, tempfile::TempDir) {
        let project = tempfile::TempDir::new().unwrap();
        let src = project.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let mut walk = String::new();
        for i in 0..n_fns {
            walk.push_str(&format!(
                "pub fn node_walk_{i:03}(node: &NodeRef) -> u32 {{\n    let node_id = node.node_id();\n    let node_depth = node.node_depth();\n    node_id + node_depth + node_id\n}}\n"
            ));
        }
        std::fs::write(src.join("walk.rs"), walk).unwrap();
        let mut types = String::new();
        for name in struct_names {
            types.push_str(&format!("pub struct {name} {{\n    pub id: u32,\n}}\n"));
        }
        std::fs::write(src.join("types.rs"), types).unwrap();
        std::fs::write(src.join("lib.rs"), "pub mod walk;\npub mod types;\n").unwrap();
        std::fs::write(
            project.path().join("Cargo.toml"),
            "[package]\nname = \"fixture_lib\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        let db_dir = project.path().join(crate::domain::CODE_GRAPH_DIR);
        std::fs::create_dir_all(&db_dir).unwrap();
        let db = Database::open(&db_dir.join("index.db")).unwrap();
        crate::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
        (db, project)
    }

    /// The candidate pool must be sized for the filter. At `limit = 5` the old
    /// `limit * 4` fetched 20 rows, all functions, and reported zero structs.
    #[test]
    fn type_filtered_query_sees_past_the_old_limit_times_four_cut() {
        let names = [
            "NodeAlpha",
            "NodeBravo",
            "NodeCharlie",
            "NodeDelta",
            "NodeEcho",
            "NodeFoxtrot",
            "NodeGolf",
            "NodeHotel",
        ];
        let (db, _tmp) = drown_index(40, &names);
        let out = run(
            db.conn(),
            &AstSearchParams {
                query: Some("node"),
                type_filter: Some("struct"),
                returns_filter: None,
                params_filter: None,
                limit: 5,
            },
        )
        .unwrap();
        assert_eq!(
            out.results.len(),
            5,
            "5 of 8 matching structs must come back at limit=5"
        );
        assert_eq!(
            out.matched_total,
            Some(8),
            "all 8 survivors must be counted before the limit cut"
        );
        assert!(out.truncated, "3 matches were cut — that must be visible");
        assert!(!out.fallback_used, "the FTS pool itself must find these");
    }

    /// The fallback is not decoration: when the pool saturates on a large index
    /// the type-filtered survivors can still be zero, and `name LIKE` is the
    /// only thing that answers. Asserted through `fallback_used` so the test
    /// pins the MECHANISM, not just a non-empty result.
    #[test]
    fn name_substring_fallback_answers_a_saturated_pool() {
        // 400 `node_*` functions vs a pool of search_fetch_count(1, true) = 100:
        // the structs cannot be in it.
        let (db, _tmp) = drown_index(400, &["NodeAlpha", "NodeBravo"]);
        let out = run(
            db.conn(),
            &AstSearchParams {
                query: Some("node"),
                type_filter: Some("struct"),
                returns_filter: None,
                params_filter: None,
                limit: 1,
            },
        )
        .unwrap();
        assert!(
            out.pool_saturated,
            "fixture must saturate the pool for this test to mean anything"
        );
        assert!(
            out.fallback_used,
            "with zero type-survivors in a saturated pool the name-substring fallback must fire"
        );
        assert_eq!(out.results.len(), 1);
        assert!(
            out.results[0].node.name.contains("Node"),
            "got {}",
            out.results[0].node.name
        );
        assert!(out.truncated, "a second Node* struct exists past limit=1");
    }

    /// Filter-only (no query) still reports truncation, so "20 results" is not
    /// mistaken for "20 matches exist".
    #[test]
    fn filter_only_path_reports_truncation() {
        let (db, _tmp) = drown_index(10, &["NodeAlpha", "NodeBravo", "NodeCharlie"]);
        let out = run(
            db.conn(),
            &AstSearchParams {
                query: None,
                type_filter: Some("struct"),
                returns_filter: None,
                params_filter: None,
                limit: 2,
            },
        )
        .unwrap();
        assert_eq!(out.results.len(), 2);
        assert!(out.truncated, "3 structs exist, 2 asked for");
    }

    /// Build an outcome with exactly the flags a case needs; `results` is only
    /// read for emptiness, so a single placeholder row stands in for "non-empty".
    fn outcome_with(truncated: bool, fallback_used: bool, dropped: usize) -> AstSearchOutcome {
        let (db, _tmp) = drown_index(1, &["NodeAlpha"]);
        let results = if dropped > 0 {
            Vec::new()
        } else {
            crate::storage::queries::get_nodes_with_files_by_filters(
                db.conn(),
                Some(&["struct"]),
                None,
                None,
                None,
                1,
            )
            .unwrap()
        };
        assert!(
            dropped > 0 || !results.is_empty(),
            "fixture must produce a row for the non-empty cases"
        );
        AstSearchOutcome {
            results,
            fts_empty: false,
            dropped_by_filter: dropped,
            matched_total: Some(9),
            truncated,
            pool_saturated: false,
            fetch_count: 80,
            fallback_used,
        }
    }

    /// The regression: `truncated` AND `fallback_used` are both reachable (the
    /// fallback path carries its own `truncated`), and each surface used to
    /// assign `hint` from two independent `if` blocks — so one sentence was
    /// silently dropped, and the CLI and MCP dropped DIFFERENT ones.
    #[test]
    fn truncation_and_fallback_hints_both_survive_in_order() {
        for style in [HintStyle::Cli, HintStyle::Mcp] {
            let out = outcome_with(true, true, 0);
            let h = hints(&out, Some("node"), 5, style);
            assert_eq!(h.len(), 2, "{style:?}: both hints are owed, got {h:?}");
            assert!(
                h[0].contains("9 symbols matched"),
                "{style:?}: the truncation cut leads — it is what stops a cut answer \
                 being read as complete; got {h:?}"
            );
            assert!(
                h[1].contains("name-substring"),
                "{style:?}: the provenance note follows; got {h:?}"
            );
        }
    }

    /// When the answer is empty because the filter rejected everything, that
    /// explanation outranks the truncation notice (which would otherwise read as
    /// "9 matched" on a zero-result answer).
    #[test]
    fn filter_emptied_explanation_outranks_the_truncation_notice() {
        let out = outcome_with(true, false, 7);
        let h = hints(&out, Some("node"), 5, HintStyle::Cli);
        assert_eq!(h.len(), 2, "got {h:?}");
        assert!(h[0].contains("Broaden or clear the filter"), "got {h:?}");
        assert!(h[1].contains("9 symbols matched"), "got {h:?}");
    }

    /// Nothing to say when nothing happened — an unconditional hint would train
    /// callers to ignore the field.
    #[test]
    fn a_complete_untruncated_answer_owes_no_hint() {
        let out = outcome_with(false, false, 0);
        assert!(hints(&out, Some("node"), 5, HintStyle::Cli).is_empty());
        assert!(hints(&out, Some("node"), 5, HintStyle::Mcp).is_empty());
    }

    /// The two surfaces order hints identically but spell the remedy for their
    /// own caller: a CLI user raises `--limit`, an MCP caller passes `limit`.
    #[test]
    fn hint_wording_is_surface_specific_but_order_is_not() {
        let out = outcome_with(true, false, 0);
        let cli = hints(&out, Some("node"), 5, HintStyle::Cli);
        let mcp = hints(&out, Some("node"), 5, HintStyle::Mcp);
        assert!(cli[0].contains("--limit 5"), "got {cli:?}");
        assert!(mcp[0].contains("limit=5"), "got {mcp:?}");
        assert!(!mcp[0].contains("--limit"), "got {mcp:?}");
        assert_eq!(cli.len(), mcp.len());
    }
}
