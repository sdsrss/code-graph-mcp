use super::*;

/// CLI arguments for the `trace` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp trace",
    about = "Trace HTTP route → handler → downstream calls"
)]
pub struct TraceArgs {
    /// Route to trace (e.g. "/api/login" or "POST /api/login")
    pub route: String,
    // The bound stays in the handler and is DERIVED from the traversal's own cap,
    // not typed here; clap parse-errors (exit 2) on non-numeric.
    /// Max traversal depth
    #[arg(long, default_value_t = 3)]
    pub depth: i32,
    // The old usage string advertised a phantom --include-middleware that the code
    // never read; --no-middleware is the real flag (middleware shown by default).
    // Migration drops the phantom and advertises --no-middleware (user-approved,
    // audit #4); --include-middleware now errors like any other stray flag.
    /// Hide downstream middleware/calls (shown by default)
    #[arg(long)]
    pub no_middleware: bool,
    /// Include test symbols in the call chain (hidden by default, matching the MCP trace tool)
    #[arg(long)]
    pub include_tests: bool,
    /// Minimum edge-resolution confidence to FOLLOW: extracted, inferred, or
    /// ambiguous. Default 'inferred' hides the ambiguous by-name fan-out (a method
    /// name shared by many defs resolving to all of them) from both the call chain
    /// and the downstream list; pass 'ambiguous' to show every edge.
    #[arg(long = "min-confidence")]
    pub min_confidence: Option<String>,
    /// JSON output
    #[arg(long)]
    pub json: bool,
}

/// Trace HTTP route → handler → downstream calls.
/// CLI equivalent of MCP `trace_http_chain`.
pub fn cmd_trace(project_root: &Path, args: TraceArgs) -> Result<()> {
    // clap requires the positional (missing → exit 2) but accepts ""; keep the
    // non-empty guard with a Usage string (now advertising --no-middleware).
    let route_path = args.route.as_str();
    if route_path.is_empty() {
        anyhow::bail!("Usage: code-graph-mcp trace <route> [--depth N] [--no-middleware] [--json]");
    }

    // Same derivation as `impact`: both feed `get_call_graph_filtered`, which
    // caps at `CALL_GRAPH_MAX_DEPTH`. See the note there.
    let depth: i32 = clamp_arg("--depth", args.depth, 1, CALL_GRAPH_MAX_DEPTH);
    let json_mode = args.json;
    let include_middleware = !args.no_middleware;
    // Hide test symbols from the recursive call chain by default, matching the MCP
    // trace_http_chain tool (server/tools/advanced.rs). The one-hop downstream list
    // stays unfiltered FOR TEST SYMBOLS on both surfaces (it still honors the
    // confidence floor below). --include-tests opts the chain back in.
    let include_tests = args.include_tests;

    // Confidence floor (default 'inferred'): hide the ambiguous by-name fan-out from
    // both the recursive chain and the one-hop downstream list, matching callgraph /
    // impact / get_call_graph (v0.77 — trace was previously rank-0 show-all).
    // --min-confidence ambiguous restores every edge. Validated at entry, mirroring
    // cmd_callgraph.
    let min_conf_tier: &'static str = match args.min_confidence.as_deref() {
        None | Some("") => crate::domain::CONF_INFERRED,
        Some(c) => crate::domain::normalize_confidence(c).ok_or_else(|| {
            anyhow::anyhow!(
                "--min-confidence must be one of: extracted, inferred, ambiguous (got '{}')",
                c
            )
        })?,
    };
    let min_conf_rank = crate::domain::confidence_rank(min_conf_tier);

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    // Parse method filter (e.g., "POST /api/login" → method=POST, path=/api/login)
    let (method_filter, path) = if let Some(idx) = route_path.find(' ') {
        (
            Some(route_path[..idx].to_uppercase()),
            &route_path[idx + 1..],
        )
    } else {
        (None, route_path)
    };

    use crate::domain::REL_ROUTES_TO;
    // Fetch + method-filter the route handlers. Wrapped so a query-time freshness
    // resync can re-run it against the refreshed index (shared with show/refs/…) —
    // the printed handler start_line then reflects a post-edit route file.
    let run_query = |conn: &rusqlite::Connection| -> Result<Vec<queries::RouteMatch>> {
        let mut rows = queries::find_routes_by_path(conn, path, REL_ROUTES_TO)?;
        // Filter by HTTP method if specified (parse metadata JSON for accurate matching)
        if let Some(ref method) = method_filter {
            rows.retain(|r| {
                r.metadata.as_ref().is_some_and(|m| {
                    serde_json::from_str::<serde_json::Value>(m)
                        .ok()
                        .and_then(|v| {
                            v.get("method")
                                .and_then(|m| m.as_str())
                                .map(|s| s.to_string())
                        })
                        .is_some_and(|rm| crate::domain::route_method_matches(&rm, method))
                })
            });
        }
        Ok(rows)
    };
    let mut rows = run_query(conn)?;
    let files: Vec<String> = rows.iter().map(|rm| rm.file_path.clone()).collect();
    let outcome = refresh_files_if_stale(&ctx.db, &ctx.project_root, &files);
    if outcome.any_changed {
        rows = run_query(conn)?;
    }
    outcome.disclose();

    if rows.is_empty() {
        // Disclose the framework-coverage limit (mirrors the MCP trace path's
        // richer message): route extraction is implemented for Express/Connect
        // (JS/TS/TSX), Go net/http, Flask/FastAPI (Python), and axum (Rust,
        // v51) — an actix (Rust) or Java (Spring) project has real routes the
        // extractor never sees, so a bare "no match" reads as "no such route"
        // and misleads.
        let hint = "route extraction covers Express/Connect (JS/TS), Go net/http, Flask/FastAPI (Python), and axum (Rust); \
                    actix and Java web frameworks are not yet extracted";
        if json_mode {
            // Tier 3 of the CLI JSON-empty contract (v0.99.1,
            // feedback_cli_json_empty_contract): a miss keys on `error` beside a
            // success-shaped empty array, exactly like `show --json`
            // ({candidates, error, symbol}) and `callgraph --json`. This envelope
            // keyed on `message` — a spelling no other CLI JSON surface uses — so
            // a consumer switching on `error` read the miss as a clean success
            // with zero handlers. `route` echoes the identifier under the SAME key
            // the success envelope uses, so one shape reads both legs, and the
            // framework-coverage limit moves out of the error text into `hint`
            // (the established key for a remedy note, cf. ast_search.rs): it is a
            // disclosure about the extractor, not a description of this miss.
            println!(
                "{}",
                serde_json::json!({
                    "route": path,
                    "handlers": [],
                    "error": format!("No routes matching: {}", route_path),
                    "hint": hint,
                })
            );
        }
        // Match the refs/impact/show not-found pattern (clean `[code-graph] …` on
        // stderr + exit 1) instead of `anyhow::bail!`, which main renders as the
        // double-prefixed `Error: [code-graph] No routes matching`.
        eprintln!(
            "[code-graph] No routes matching: {}\n  Note: {}.",
            route_path, hint
        );
        std::process::exit(1);
    }

    let mut stdout = std::io::stdout().lock();

    // Batch-fetch downstream calls if middleware included
    use crate::domain::REL_CALLS;
    let downstream_map = if include_middleware {
        let node_ids: Vec<i64> = rows.iter().map(|rm| rm.node_id).collect();
        queries::get_edge_target_names_batch(conn, &node_ids, REL_CALLS, min_conf_rank)?
    } else {
        std::collections::HashMap::new()
    };

    if json_mode {
        // Single JSON object envelope matching MCP trace_http_chain shape
        let mut handlers = Vec::with_capacity(rows.len());
        let mut ambiguous_hidden: usize = 0;
        for rm in &rows {
            let chain = crate::graph::query::get_call_graph_filtered(
                conn,
                &rm.handler_name,
                "callees",
                depth,
                Some(&rm.file_path),
                min_conf_rank,
            )?;
            ambiguous_hidden += chain.suppressed_ambiguous;
            let chain_nodes: Vec<serde_json::Value> = chain
                .nodes
                .iter()
                .filter(|n| n.depth > 0)
                .filter(|n| {
                    include_tests || !crate::domain::is_test_node(n.is_test, &n.name, &n.file_path)
                })
                .map(|n| {
                    serde_json::json!({
                        "name": n.name, "file_path": n.file_path, "depth": n.depth,
                    })
                })
                .collect();
            let mut entry = serde_json::json!({
                "handler_name": rm.handler_name,
                "file_path": rm.file_path,
                "start_line": rm.start_line,
                "end_line": rm.end_line,
                "metadata": rm.metadata,
                "call_chain": chain_nodes,
            });
            if chain.limit_hit || chain.depth_capped {
                entry["call_chain_truncated"] = serde_json::json!(true);
            }
            if include_middleware {
                let downstream = downstream_map.get(&rm.node_id).cloned().unwrap_or_default();
                entry["downstream_calls"] = serde_json::json!(downstream);
            }
            handlers.push(entry);
        }
        let mut envelope = serde_json::json!({
            "route": path,
            "handlers": handlers,
        });
        if ambiguous_hidden > 0 {
            envelope["ambiguous_edges_hidden"] = serde_json::json!(ambiguous_hidden);
        }
        outcome.attach_partial(&mut envelope);
        writeln!(stdout, "{}", serde_json::to_string(&envelope)?)?;
        return Ok(());
    }

    let mut ambiguous_hidden: usize = 0;
    for rm in &rows {
        // Render the route label as "METHOD path" from the routes_to metadata
        // (matching the map's Entry Points) instead of dumping the raw JSON blob.
        let route_label = rm
            .metadata
            .as_deref()
            .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
            .map(|v| {
                format!(
                    "{} {}",
                    v["method"].as_str().unwrap_or("ALL"),
                    v["path"].as_str().unwrap_or(path)
                )
            })
            .unwrap_or_else(|| path.to_string());
        writeln!(
            stdout,
            "{} → {} ({}:{})",
            route_label, rm.handler_name, rm.file_path, rm.start_line
        )?;

        if include_middleware {
            if let Some(downstream) = downstream_map.get(&rm.node_id) {
                if !downstream.is_empty() {
                    writeln!(stdout, "  downstream: {}", downstream.join(", "))?;
                }
            }
        }

        // Show call chain
        let chain = crate::graph::query::get_call_graph_filtered(
            conn,
            &rm.handler_name,
            "callees",
            depth,
            Some(&rm.file_path),
            min_conf_rank,
        )?;
        ambiguous_hidden += chain.suppressed_ambiguous;
        for n in &chain.nodes {
            if n.depth == 0 {
                continue;
            }
            if !include_tests && crate::domain::is_test_node(n.is_test, &n.name, &n.file_path) {
                continue;
            }
            let indent = "  ".repeat(n.depth as usize);
            writeln!(stdout, "{}→ {} ({})", indent, n.name, n.file_path)?;
        }
        if chain.limit_hit || chain.depth_capped {
            writeln!(stdout, "  ⚠ chain truncated for {}", rm.handler_name)?;
        }
    }
    if ambiguous_hidden > 0 {
        writeln!(
            stdout,
            "  ({} direct ambiguous by-name edge(s) hidden — use --min-confidence ambiguous to show)",
            ambiguous_hidden,
        )?;
    }

    Ok(())
}

/// File-level dependency graph.
/// CLI equivalent of MCP `dependency_graph`.
/// Scan a file for language-appropriate barrel / re-export / import patterns.
/// Used by `cmd_deps` as a fallback when the graph has no tracked edges for
/// a file (e.g. Rust `mod.rs` barrels that only contain `pub mod X;`).
pub(crate) fn scan_barrel_patterns(
    project_root: &Path,
    file_path: &str,
) -> Option<Vec<(usize, String)>> {
    // Resolve symlinks and confine to the project root before reading. This
    // function echoes import/export lines from a caller-supplied path, so an
    // in-repo symlink pointing outside the root would turn `deps` into a
    // restricted file-read oracle. Mirrors read_source_context's guard (M2).
    let canonical = project_root.join(file_path).canonicalize().ok()?;
    let root_canonical = project_root.canonicalize().ok()?;
    if !canonical.starts_with(&root_canonical) {
        return None;
    }
    let content = std::fs::read_to_string(&canonical).ok()?;
    let lang = crate::utils::config::detect_language(file_path);
    let mut hits = Vec::new();
    for (idx, line) in content.lines().enumerate().take(1000) {
        let t = line.trim_start();
        let matched = match lang {
            Some("rust") => {
                t.starts_with("pub mod ")
                    || t.starts_with("mod ")
                    || t.starts_with("pub use ")
                    || t.starts_with("use ")
            }
            Some("typescript") | Some("tsx") | Some("javascript") => {
                t.starts_with("import ") || (t.starts_with("export ") && t.contains(" from "))
            }
            Some("python") => {
                (t.starts_with("from ") && t.contains(" import ")) || t.starts_with("import ")
            }
            Some("go") | Some("java") | Some("csharp") | Some("kotlin") => t.starts_with("import "),
            Some("ruby") => t.starts_with("require ") || t.starts_with("require_relative "),
            Some("php") => {
                t.starts_with("use ") || t.starts_with("require ") || t.starts_with("include ")
            }
            _ => false,
        };
        if matched {
            hits.push((idx + 1, line.to_string()));
        }
    }
    if hits.is_empty() {
        None
    } else {
        Some(hits)
    }
}

// --- deps subcommand ---
