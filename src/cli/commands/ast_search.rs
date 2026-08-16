use super::*;
use crate::search::ast_query::HintStyle;

/// CLI arguments for the `ast-search` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp ast-search",
    about = "Structured search with --type/--returns/--params filters"
)]
pub struct AstSearchArgs {
    /// Search query (optional if a --type/--returns/--params filter is given)
    pub query: Option<String>,
    #[arg(long = "type", help = crate::domain::TYPE_FILTER_HELP_ARG)]
    pub type_filter: Option<String>,
    /// Filter by return type
    #[arg(long)]
    pub returns: Option<String>,
    /// Filter by parameter text
    #[arg(long)]
    pub params: Option<String>,
    /// JSON output
    #[arg(long)]
    pub json: bool,
    /// Limit results (default: 20, max: 100)
    #[arg(long)]
    pub limit: Option<usize>,
}

/// Structured AST search: FTS5 + column filtering.
///
/// Flags: --type <type>, --returns <type>, --params <text>
pub fn cmd_ast_search(project_root: &Path, args: AstSearchArgs) -> Result<()> {
    // clap accepts an empty-string positional; treat "" as "no query" (the old
    // .filter(|q| !q.is_empty())) so the query-or-filter requirement still fires.
    let query = args.query.as_deref().filter(|q| !q.is_empty());

    let type_filter = args.type_filter.as_deref();
    let returns_filter = args.returns.as_deref();
    let params_filter = args.params.as_deref();
    let json_mode = args.json;
    let limit: usize = args.limit.unwrap_or(20).clamp(1, 100);

    // Require either a query or at least one structural filter
    let has_filters = type_filter.is_some() || returns_filter.is_some() || params_filter.is_some();
    if query.is_none() && !has_filters {
        anyhow::bail!(
            "Usage: code-graph-mcp ast-search <query> [--type fn|class|...] [--returns type] [--params text] [--json]\n\
             Either a query or at least one filter (--type, --returns, --params) is required."
        );
    }

    // Validate --type up-front: an unknown alias normalizes to an empty Vec,
    // which silently filters every node away. Surface as an error so the user
    // doesn't read "No results matching filters" and assume the index is empty.
    if let Some(tf) = type_filter {
        if crate::domain::normalize_type_filter(tf).is_empty() {
            anyhow::bail!(
                "Unknown type filter: '{}'. Valid: {}",
                crate::domain::TYPE_FILTER_HELP,
                tf
            );
        }
    }

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    // Both paths (FTS5+filter, and filter-only SQL) live in the shared core the
    // MCP `ast_search` tool also calls — the two used to be copies and had
    // drifted (audit 2026-08-16 P1-8). Wrapped in a closure so a query-time
    // freshness resync can re-run it against the refreshed index.
    let run_query =
        |conn: &rusqlite::Connection| -> Result<crate::search::ast_query::AstSearchOutcome> {
            crate::search::ast_query::run(
                conn,
                &crate::search::ast_query::AstSearchParams {
                    query,
                    type_filter,
                    returns_filter,
                    params_filter,
                    limit,
                },
            )
        };

    let mut search = run_query(conn)?;
    // Re-index any displayed file edited since indexing so start_line/end_line are
    // post-edit, then re-run once (shared resync with show/refs/…).
    let files: Vec<String> = search
        .results
        .iter()
        .map(|nwf| nwf.file_path.clone())
        .collect();
    let outcome = refresh_files_if_stale(&ctx.db, &ctx.project_root, &files);
    if outcome.any_changed {
        search = run_query(conn)?;
    }
    outcome.disclose();

    let results_with_files = &search.results;
    let dropped_by_filter = search.dropped_by_filter;

    if search.fts_empty {
        if json_mode {
            println!("{}", serde_json::json!({"results": [], "count": 0}));
        }
        eprintln!("[code-graph] No results for: {}", query.unwrap_or_default());
        return Ok(());
    }

    if results_with_files.is_empty() {
        if dropped_by_filter > 0 {
            // The query HAD hits; the structural filters removed every one. Say so
            // in-band — a bare empty envelope under `2>/dev/null` reads as "no such
            // code" (disclosure-gap class, roadmap 2026-07-18 §1.1). Mirrors the
            // cmd_search filter-emptied object.
            let filter_desc = [
                type_filter.map(|t| format!("type: {t}")),
                returns_filter.map(|r| format!("returns: {r}")),
                params_filter.map(|p| format!("params: {p}")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ");
            // The remedy depends on WHY it is empty. When the candidate pool
            // came back full, matches may exist below the cut and "broaden the
            // filter" is the wrong advice — the query is what needs narrowing
            // (audit 2026-08-16 P1-8 measured that exact misdirection). Built by
            // the shared ordered builder so this surface and MCP cannot disagree
            // about which hint survives when several apply.
            let remedy =
                crate::search::ast_query::hints(&search, query, limit, HintStyle::Cli).join(" ");
            if json_mode {
                println!(
                    "{}",
                    serde_json::json!({
                        "results": [],
                        "count": 0,
                        "filtered_out": dropped_by_filter,
                        "filter": filter_desc,
                        "pool_saturated": search.pool_saturated,
                        "hint": remedy,
                    })
                );
            } else {
                println!(
                    "[code-graph] No results — {} candidate(s) matched the query but were removed by the active filter ({}). {}",
                    dropped_by_filter, filter_desc, remedy
                );
            }
            eprintln!(
                "[code-graph] No results matching filters — {} candidate(s) removed by ({}). {}",
                dropped_by_filter, filter_desc, remedy
            );
        } else {
            if json_mode {
                println!("{}", serde_json::json!({"results": [], "count": 0}));
            }
            eprintln!("[code-graph] No results matching filters.");
        }
        return Ok(());
    }

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        let results: Vec<serde_json::Value> = results_with_files
            .iter()
            .map(|nwf| {
                let n = &nwf.node;
                serde_json::json!({
                    "node_id": n.id,
                    "type": n.node_type,
                    "name": n.qualified_name.as_deref().unwrap_or(&n.name),
                    "file_path": &nwf.file_path,
                    "start_line": n.start_line,
                    "end_line": n.end_line,
                    "return_type": n.return_type,
                    "param_types": n.param_types,
                })
            })
            .collect();
        // Envelope matches MCP ast_search: {results, count, matched_total, truncated}
        let mut envelope = serde_json::json!({
            "results": results,
            "count": results_with_files.len(),
        });
        if let Some(total) = search.matched_total {
            envelope["matched_total"] = serde_json::json!(total);
        }
        if search.truncated {
            envelope["truncated"] = serde_json::json!(true);
        }
        // ONE assignment site. The truncation notice and the fallback note used
        // to be two independent `if` blocks writing the same key, so a result
        // set that was both truncated and fallback-answered lost the truncation
        // disclosure — the one that stops a cut answer being read as complete.
        let hints = crate::search::ast_query::hints(&search, query, limit, HintStyle::Cli);
        if !hints.is_empty() {
            envelope["hint"] = serde_json::json!(hints.join(" "));
        }
        outcome.attach_partial(&mut envelope);
        writeln!(stdout, "{}", serde_json::to_string(&envelope)?)?;
        return Ok(());
    }

    for nwf in results_with_files {
        writeln!(stdout, "{}", format_node_compact(&nwf.node, &nwf.file_path))?;
    }
    // "20 results" must not read as "20 matches exist" — name the cut and the
    // remedy (raise --limit), which is the opposite of the "broaden the filter"
    // advice the under-fetching version gave (audit 2026-08-16 P1-8). Same
    // ordered set the JSON envelope carries; the human path always printed both
    // and it was the JSON path that dropped one.
    for hint in crate::search::ast_query::hints(&search, query, limit, HintStyle::Cli) {
        eprintln!("[code-graph] {hint}");
    }
    Ok(())
}

/// Normalize type filter shorthand: fn → function/method, class → class/struct, etc.
pub(crate) fn normalize_type_filter(input: &str) -> Vec<&'static str> {
    let result = crate::domain::normalize_type_filter(input);
    if result.is_empty() {
        eprintln!(
            "[code-graph] Unknown type filter: '{}'. Valid: {}",
            input,
            crate::domain::TYPE_FILTER_HELP
        );
    }
    result
}

// --- callgraph subcommand ---
