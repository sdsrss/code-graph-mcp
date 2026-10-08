use super::*;

/// CLI arguments for the `callgraph` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp callgraph",
    about = "Show call graph (callers/callees)"
)]
pub struct CallgraphArgs {
    /// Symbol name to analyze (required unless --node-id is given)
    pub symbol: Option<String>,
    /// Graph exactly this definition: a node_id from a same-file ambiguity
    /// answer or `show --json` (authoritative over --file)
    #[arg(long = "node-id")]
    pub node_id: Option<i64>,
    // --direction stays an in-handler String (NOT a clap ValueEnum) so the exact
    // "must be one of: callers, callees, both" exit-1 message is preserved.
    /// Direction: callers, callees, or both
    #[arg(long, default_value = "both")]
    pub direction: String,
    // .max(1) only (NOT clamp) stays in the handler: the engine caps depth and
    // reports requested vs effective separately, so the CLI must not pre-rewrite it.
    /// Max traversal depth (engine caps internally; default: 3)
    #[arg(long, default_value_t = 3)]
    pub depth: i32,
    /// JSON output
    #[arg(long)]
    pub json: bool,
    /// Compact output
    #[arg(long)]
    pub compact: bool,
    /// Show test callers/callees (hidden by default)
    #[arg(long)]
    pub include_tests: bool,
    /// Disambiguate same-name symbols by file path
    #[arg(long)]
    pub file: Option<String>,
    /// Minimum edge-resolution confidence to FOLLOW: extracted, inferred, or
    /// ambiguous. Default 'inferred' hides the ambiguous by-name fan-out (a
    /// method name shared by many defs resolving to all of them); pass
    /// 'ambiguous' to show every edge.
    #[arg(long = "min-confidence")]
    pub min_confidence: Option<String>,
    /// Token budget for the text answer (bytes/3, 100-100000): the deepest
    /// nodes with the fewest callers are shortened, then left out (a node goes
    /// before its parent); ends with a command that prints them
    #[arg(long, conflicts_with_all = ["json", "compact"])]
    pub budget: Option<u64>,
}

/// Call graph display.
///
/// Output format:
/// ```text
/// handle_tool_call (src/mcp/server.rs:350)
///   ← called by: process_message (src/mcp/server.rs:130)
///   → calls: tool_semantic_search (src/mcp/server.rs:1360)
/// ```
pub fn cmd_callgraph(project_root: &Path, args: CallgraphArgs) -> Result<()> {
    // clap accepts an empty-string positional; preserve the non-empty guard.
    let raw_symbol = args.symbol.as_deref().unwrap_or("");
    if raw_symbol.is_empty() && args.node_id.is_none() {
        anyhow::bail!("Usage: code-graph-mcp callgraph <symbol> [--node-id N] [--direction callers|callees|both] [--depth N] [--file <path>] [--json]");
    }

    let direction = crate::domain::normalize_call_direction(args.direction.as_str())
        .ok_or_else(|| anyhow::anyhow!("--direction must be one of: callers, callees, both"))?;
    // Floor only, and disclosed: the CEILING is the traversal's and it announces
    // itself ("⚠ depth capped to 10"), but `--depth 0` silently became 1 with
    // nothing said — the same half-disclosed shape `impact` and `trace` had.
    let depth: i32 = floor_arg("--depth", args.depth, 1);
    let json_mode = args.json;
    let compact = args.compact;
    let include_tests = args.include_tests;
    let explicit_file_owned: Option<String> = match args.file.as_deref() {
        Some(f) => Some(normalize_user_path(project_root, f)?),
        None => None,
    };
    // `--node-id` names one definition, so a file selector has nothing left to
    // choose (same rule as `refs --node-id`).
    if args.node_id.is_some() && explicit_file_owned.is_some() {
        eprintln!("[code-graph] Note: --file is ignored when --node-id is given (node_id is authoritative).");
    }
    // Read only on the by-name paths below; a node target never consults it.
    let explicit_file = explicit_file_owned.as_deref();

    // Confidence floor: default 'inferred' hides the ambiguous by-name fan-out
    // (the known false-positive class) from the traversal; --min-confidence
    // ambiguous restores every edge. Validated at entry, mirroring `refs`.
    let min_conf_tier: &'static str =
        crate::domain::parse_min_confidence(args.min_confidence.as_deref(), "--min-confidence")?
            .unwrap_or(crate::domain::DEFAULT_RISK_CONF_FLOOR);
    let min_conf_rank = crate::domain::confidence_rank(min_conf_tier);

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();
    // callgraph's in-band error envelope carries `results: []` (see the
    // not-found branch below).
    let not_found_extra = serde_json::json!({ "results": [] });
    let mut node_target = match args.node_id {
        Some(nid) => Some(CliNodeTarget::lookup(
            conn,
            nid,
            json_mode,
            &not_found_extra,
        )?),
        None => None,
    };
    // A symbol in a file added since the last index (D2).
    if node_target.is_none() {
        crate::cli::freshness::index_new_files_if_absent(&ctx.db, &ctx.project_root, raw_symbol);
    }
    let target_name: Option<String> = node_target.as_ref().map(|target| target.name.clone());
    let raw_symbol = target_name.as_deref().unwrap_or(raw_symbol);

    let selected = match &node_target {
        Some(target) => Ok(CliSymbolSelection {
            lookup_name: target.name.clone(),
            bare_name: target.name.clone(),
            file_filter: None,
            lookup: CliSymbolLookup::Bare,
        }),
        None => select_cli_symbol(conn, raw_symbol, explicit_file)?,
    };
    let selection = match selected {
        Ok(selection) => selection,
        Err(CliSymbolSelectionError::Ambiguous(candidates)) => {
            emit_exact_ambiguity(raw_symbol, &candidates, json_mode)
        }
        Err(CliSymbolSelectionError::QualifiedNotFound) => {
            // Only reached with --file (see `select_cli_symbol`).
            let fp = explicit_file.unwrap_or_default();
            let miss = crate::resolve::file_selector_miss(conn, raw_symbol, fp)?;
            emit_file_selector_miss(&miss, raw_symbol, fp, json_mode, MissEnvelope::Results)
        }
    };
    let is_exact_qualified = selection.lookup == CliSymbolLookup::ExactQualified;
    let symbol = selection.lookup_name.as_str();
    let file_filter = selection.file_filter.as_deref();
    // Refresh the selector's own file before the gate reads it: its node_ids
    // and start lines are what a caller picks a definition by (see `impact`).
    if let Some(fp) = file_filter {
        refresh_files_if_stale(&ctx.db, &ctx.project_root, &[fp.to_string()]).disclose();
    }

    // Exact-name ambiguity guard: a bare name with ≥2 non-test definitions
    // (cross-file OR same-file overloads) would silently merge call graphs.
    // Shared with MCP via crate::resolve so both surfaces agree (audit #6).
    // A file selector cannot split same-file definitions either; a node_id can.
    if !is_exact_qualified && node_target.is_none() {
        let cands = match file_filter {
            None => crate::resolve::detect_ambiguity(conn, symbol)?,
            Some(fp) => crate::resolve::detect_same_file_ambiguity(conn, symbol, fp)?,
        };
        if let Some(cands) = cands {
            emit_exact_ambiguity(symbol, &cands, json_mode);
        }
    }

    // Wrapped so a query-time freshness resync below can re-run it against the
    // refreshed index (parity with refs/show/… via refresh_files_if_stale).
    let run_query = |sym: &str, target: &Option<CliNodeTarget>| {
        let seed = match target {
            Some(target) => crate::graph::query::CallGraphSeed::Node(target.id),
            None => crate::graph::query::CallGraphSeed::Name {
                name: sym,
                file_path: file_filter,
            },
        };
        crate::graph::query::get_call_graph_seeded(conn, seed, direction, depth, min_conf_rank)
    };

    let mut result = run_query(symbol, &node_target)?;
    // Fuzzy auto-resolve: if exact-name lookup returned nothing (or only the seed
    // node with no edges) and no --file was specified, promote a unique fuzzy
    // match. Matches MCP get_call_graph behavior.
    let has_edges = result.nodes.iter().any(|n| n.depth > 0);
    let has_seed = result.nodes.iter().any(|n| n.depth == 0);
    // A --file that holds no definition is a miss (D#253), as in `impact`.
    // A path the index does not hold is refused whatever the name, and so is
    // a name defined only in other files. A name defined nowhere in an
    // indexed file still gets the fuzzy step below.
    if let (Some(fp), false, None) = (file_filter, has_seed, &node_target) {
        let miss = crate::resolve::file_selector_miss(conn, symbol, fp)?;
        if !miss.defined_nowhere() {
            emit_file_selector_miss(&miss, symbol, fp, json_mode, MissEnvelope::Results);
        }
    }
    let mut resolved_symbol: String = symbol.to_string();
    if !(is_exact_qualified
        || node_target.is_some()
        || has_edges
        || has_seed && file_filter.is_some())
    {
        match resolve_fuzzy_name_cli(conn, symbol)? {
            CliFuzzyResolution::Unique(resolved) => {
                if resolved != symbol {
                    result = run_query(&resolved, &node_target)?;
                    // Before any miss below, which names `resolved`.
                    eprintln!("[code-graph] Resolved '{}' → '{}'", symbol, resolved);
                    if let Some(fp) = file_filter {
                        if !result.nodes.iter().any(|n| n.depth == 0) {
                            let miss = crate::resolve::file_selector_miss(conn, &resolved, fp)?;
                            emit_file_selector_miss(
                                &miss,
                                &resolved,
                                fp,
                                json_mode,
                                MissEnvelope::Results,
                            );
                        }
                    }
                }
                resolved_symbol = resolved;
            }
            CliFuzzyResolution::Ambiguous(cands) => {
                // ARC-01: shared renderer, callgraph's published envelope
                // (`results` + `candidates`) and its own suffixes. The JSON error
                // string carries no trailing hint — refs is the one that does.
                crate::cli::symbols::emit_fuzzy_ambiguity(
                    symbol,
                    &cands,
                    json_mode,
                    crate::cli::symbols::FuzzyEnvelope::ResultsAndCandidates,
                    "",
                    ". Did you mean:",
                );
            }
            CliFuzzyResolution::NotFound => { /* fall through to empty-nodes branch */ }
        }
    }
    // Intentional shadow: if fuzzy promoted, `resolved_symbol` holds the resolved
    // name; otherwise it still equals the original input (initialized at
    // `symbol.to_string()` above). Either way, `symbol` below is the correct
    // identifier to print in the "No call graph results" eprintln.
    let symbol = resolved_symbol.as_str();
    let output_symbol = if is_exact_qualified {
        selection.bare_name.as_str()
    } else {
        symbol
    };

    // Query-time freshness (audit 2026-08-22 P2-11 — the one read command the
    // wiring had skipped on BOTH surfaces). What goes stale here is not a line
    // number, since this command prints paths: it is the caller/callee SET
    // itself. A call added or removed since the last index makes the tree wrong
    // in the direction that matters, and silently — so refresh the files the
    // answer names and re-run against the refreshed index, then disclose
    // whatever stayed stale.
    let files: Vec<String> = result.nodes.iter().map(|n| n.file_path.clone()).collect();
    let outcome = refresh_files_if_stale(&ctx.db, &ctx.project_root, &files);
    if outcome.any_changed {
        // The re-index reused ids: re-find the node before re-running anything
        // with its id (SURF-16).
        if let Some(target) = node_target.as_mut() {
            target.reresolve(conn, json_mode, &not_found_extra, || outcome.disclose())?;
        } else if is_exact_qualified {
            match select_cli_symbol(conn, raw_symbol, explicit_file)? {
                Ok(refreshed) if refreshed.lookup == CliSymbolLookup::ExactQualified => {}
                Err(CliSymbolSelectionError::Ambiguous(candidates)) => {
                    outcome.disclose();
                    emit_exact_ambiguity(raw_symbol, &candidates, json_mode);
                }
                Ok(_) | Err(CliSymbolSelectionError::QualifiedNotFound) => {
                    // Let the ordinary empty-result branch below render the
                    // command's established not-found envelope and hint.
                }
            }
        }
        result = run_query(symbol, &node_target)?;
    }
    outcome.disclose();

    if result.nodes.is_empty() {
        if json_mode {
            // In-band error (disclosure-gap class, roadmap 2026-07-18 §1.3):
            // a bare `{"results":[]}` under `2>/dev/null` is indistinguishable
            // from a legitimately edge-less symbol. Same shape as the ambiguous
            // branch above ({results, error, …}) and impact's error object.
            println!(
                "{}",
                serde_json::json!({
                    "results": [],
                    "error": format!("No call graph results for: {}", symbol),
                    "symbol": symbol,
                })
            );
        }
        eprintln!("[code-graph] No call graph results for: {}", symbol);
        // ISSUE-006's sibling surface (pre-tag review SF-1): callgraph is the
        // command the decision table points at first, so a just-added symbol
        // landing here needs the same stale-index hint as show/impact/similar/
        // refs. Gated on the symbol being genuinely ABSENT — a symbol that
        // exists with zero edges also reaches this branch, and hinting at
        // reindexing there would send the user chasing a non-problem.
        let absent = if is_exact_qualified {
            crate::resolve::selectable_qualified_definitions(conn, symbol, file_filter)
                .map(|nodes| nodes.is_empty())
                .unwrap_or(false)
        } else {
            queries::get_nodes_by_name(conn, symbol)
                .map(|nodes| nodes.is_empty())
                .unwrap_or(false)
        };
        if absent {
            hint_symbol_maybe_unindexed(symbol);
        }
        std::process::exit(1);
    }

    // Filter test nodes in BOTH directions unless --include-tests is set.
    //
    // This used to carry an extra `Direction::Callers` condition, so a
    // test-named CALLEE was rendered whether or not the flag was passed —
    // against the flag's own help ("Show test callers/callees") and against MCP
    // `get_call_graph`, which has never had the direction condition
    // (audit 2026-09-05 SURF-05).
    //
    // Counted per direction rather than in one bucket: `test_callers_hidden` is
    // a published JSON field, and folding callees into it would make the name
    // over-state what it counts — the failure mode this repo keeps fixing.
    // `test_callees_hidden` is additive alongside it.
    //
    // The seed (depth=0) is kept here because the human-readable renderer
    // below uses it as the tree root. The JSON path filters it separately
    // for parity with MCP `get_call_graph` (which excludes the seed).
    let (display_nodes, test_count, test_callee_count) = if include_tests {
        (result.nodes.iter().collect::<Vec<_>>(), 0usize, 0usize)
    } else {
        let mut display = Vec::new();
        let mut tests = 0usize;
        let mut test_callees = 0usize;
        for n in &result.nodes {
            if n.depth > 0 && crate::domain::is_test_node(n.is_test, &n.name, &n.file_path) {
                if matches!(n.direction, crate::graph::query::Direction::Callers) {
                    tests += 1;
                } else {
                    test_callees += 1;
                }
            } else {
                display.push(n);
            }
        }
        (display, tests, test_callees)
    };

    // An empty caller side cannot tell "nobody calls it" from "called through a
    // string key / reflection / function value". Disclose the dynamic-dispatch
    // sites that name it — never as edges, and only when the caller list shown
    // is empty, so a non-empty answer is byte-identical (P1 #4).
    let boundaries = if direction != "callees"
        && !display_nodes
            .iter()
            .any(|n| n.depth > 0 && matches!(n.direction, crate::graph::query::Direction::Callers))
    {
        let asked: Vec<i64> = display_nodes
            .iter()
            .filter(|n| n.depth == 0)
            .map(|n| n.node_id)
            .collect();
        crate::graph::boundaries::for_empty_result(conn, &ctx.project_root, output_symbol, &asked)?
    } else {
        None
    };

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        // Drop the seed (depth=0) — parity with MCP `get_call_graph`
        // (`format_call_graph_response` filters `n.depth > 0`). With
        // `direction=both` the seed appears twice (once per direction),
        // inflating result counts.
        let results: Vec<serde_json::Value> = display_nodes
            .iter()
            .filter(|n| n.depth > 0)
            .map(|n| {
                serde_json::json!({
                    "node_id": n.node_id,
                    "name": n.name,
                    "type": n.node_type,
                    "file_path": n.file_path,
                    "depth": n.depth,
                    "direction": n.direction.as_str(),
                    "parent_id": n.parent_id,
                })
            })
            .collect();
        let mut output = serde_json::json!({
            "symbol": output_symbol,
            "results": results,
        });
        if test_count > 0 {
            output["test_callers_hidden"] = serde_json::json!(test_count);
        }
        if test_callee_count > 0 {
            output["test_callees_hidden"] = serde_json::json!(test_callee_count);
        }
        if result.limit_hit {
            output["limit_hit"] = serde_json::json!(true);
        }
        if result.depth_capped {
            output["depth_capped"] = serde_json::json!(true);
            output["effective_max_depth"] = serde_json::json!(result.effective_max_depth);
            output["requested_max_depth"] = serde_json::json!(result.requested_max_depth);
        }
        if result.suppressed_ambiguous > 0 {
            output["ambiguous_edges_hidden"] = serde_json::json!(result.suppressed_ambiguous);
        }
        // The stderr note above is invisible under `--json 2>/dev/null`, and
        // this envelope is object-shaped, so it can carry the marker (parity
        // with ast-search/refs/trace/impact/report).
        if let Some(b) = &boundaries {
            output["boundaries"] = b.to_json();
        }
        outcome.attach_partial(&mut output);
        writeln!(stdout, "{}", serde_json::to_string(&output)?)?;
        return Ok(());
    }

    // Find root node (depth 0)
    let root = display_nodes.iter().find(|n| n.depth == 0);
    if let Some(root) = root {
        writeln!(stdout, "{} ({})", root.name, root.file_path)?;
    } else {
        return Ok(());
    }
    let root_id = root.unwrap().node_id;

    // Build parent_id → children map per direction, so depth-N nodes nest under
    // their *actual* depth-(N-1) parent rather than visually clumping under the
    // last sibling. Same direction filter so callers/callees subtrees stay
    // separate when --direction=both.
    use std::collections::HashMap;
    let mut children: HashMap<(i64, &'static str), Vec<&crate::graph::query::CallGraphNode>> =
        HashMap::new();
    let mut dedup = std::collections::HashSet::new();
    for n in &display_nodes {
        if n.depth == 0 {
            continue;
        }
        // Dedup cfg-gated duplicates (same name+file+direction+depth, different node_id).
        if !dedup.insert((&n.name, &n.file_path, n.direction.as_str(), n.depth)) {
            continue;
        }
        let parent = n.parent_id.unwrap_or(root_id);
        children
            .entry((parent, n.direction.as_str()))
            .or_default()
            .push(n);
    }

    fn render_subtree<W: std::io::Write>(
        out: &mut W,
        children: &HashMap<(i64, &'static str), Vec<&crate::graph::query::CallGraphNode>>,
        parent_id: i64,
        direction: &'static str,
        compact: bool,
    ) -> std::io::Result<()> {
        let arrow = match direction {
            "callers" => "←",
            _ => "→",
        };
        let arrow_text = match direction {
            "callers" => "← called by",
            _ => "→ calls",
        };
        if let Some(kids) = children.get(&(parent_id, direction)) {
            for n in kids {
                let indent = "  ".repeat(n.depth as usize);
                // `<module>` is our sentinel for "top level, no enclosing
                // function" and it reached the tree verbatim: `← called by:
                // <module> (users.test.ts) [module]` reads as a symbol the reader
                // cannot find in their file. --json keeps the raw name.
                let label = crate::domain::display_node_name(&n.name);
                if compact {
                    writeln!(out, "{}{} {} ({})", indent, arrow, label, n.file_path)?;
                } else {
                    writeln!(
                        out,
                        "{}{}: {} ({}) [{}]",
                        indent, arrow_text, label, n.file_path, n.node_type
                    )?;
                }
                render_subtree(out, children, n.node_id, direction, compact)?;
            }
        }
        Ok(())
    }

    // Footers are collected first so a budgeted answer can count them; the
    // unbudgeted answer writes the same bytes after the tree.
    let mut footer: Vec<u8> = Vec::new();
    write_callgraph_footer(
        &mut footer,
        test_count,
        test_callee_count,
        &result,
        boundaries.as_ref(),
    )?;

    if let Some(requested) = args.budget {
        let tokens = clamp_arg(
            "--budget",
            requested,
            crate::budget::MIN_BUDGET_TOKENS,
            crate::budget::MAX_BUDGET_TOKENS,
        ) as usize;
        let next = crate::budget::NextCommand::new("callgraph")
            .arg(raw_symbol)
            .opt_path("--file", args.file.clone())
            .opt(
                "--direction",
                (args.direction != "both").then(|| args.direction.clone()),
            )
            .opt("--depth", (args.depth != 3).then(|| args.depth.to_string()))
            .flag_if(include_tests, "--include-tests")
            .opt("--min-confidence", args.min_confidence.clone());
        let root = root.unwrap();
        let head = format!("{} ({})\n", root.name, root.file_path);
        let text = callgraph_budget_text(
            conn,
            &head,
            &children,
            root_id,
            &String::from_utf8_lossy(&footer),
            tokens,
            &next,
        )?;
        // The head line was already written above.
        write!(stdout, "{}", &text[head.len()..])?;
        return Ok(());
    }

    render_subtree(&mut stdout, &children, root_id, "callers", compact)?;
    render_subtree(&mut stdout, &children, root_id, "callees", compact)?;
    stdout.write_all(&footer)?;

    Ok(())
}

/// The lines after the tree: hidden-test counts, traversal limits, hidden
/// ambiguous edges, dynamic-dispatch boundaries.
fn write_callgraph_footer<W: std::io::Write>(
    stdout: &mut W,
    test_count: usize,
    test_callee_count: usize,
    result: &crate::graph::query::CallGraphResult,
    boundaries: Option<&crate::graph::boundaries::Boundaries>,
) -> Result<()> {
    if test_count > 0 || test_callee_count > 0 {
        let mut parts: Vec<String> = Vec::new();
        if test_count > 0 {
            parts.push(format!("{test_count} test callers"));
        }
        if test_callee_count > 0 {
            parts.push(format!("{test_callee_count} test callees"));
        }
        writeln!(
            stdout,
            "  ({} hidden, use --include-tests to show)",
            parts.join(" + ")
        )?;
    }
    if result.limit_hit {
        writeln!(
            stdout,
            "  ⚠ result truncated: hit row limit ({} rows) — more callers/callees may exist; pick a leaf and re-query",
            crate::graph::query::CALL_GRAPH_ROW_LIMIT,
        )?;
    }
    if result.depth_capped {
        writeln!(
            stdout,
            "  ⚠ depth capped to {} (requested {}) — deeper chains may exist",
            result.effective_max_depth, result.requested_max_depth,
        )?;
    }
    if result.suppressed_ambiguous > 0 {
        writeln!(
            stdout,
            "  ({} direct ambiguous by-name edge(s) hidden — use --min-confidence ambiguous to show)",
            result.suppressed_ambiguous,
        )?;
    }
    if let Some(b) = boundaries {
        b.render_text(stdout, "  ")?;
    }
    Ok(())
}

/// One rendered tree line of `callgraph` (depth, parent and whether it was
/// deduplicated are already settled by the unbudgeted renderer's rules).
struct TreeLine<'a> {
    node: &'a crate::graph::query::CallGraphNode,
    direction: &'static str,
    /// Index of the parent line, `None` under the root.
    parent: Option<usize>,
}

/// `callgraph --budget`: the tree fitted to `tokens`.
///
/// One unit per tree line. Least important first: deepest, then fewest
/// callers (call-edge in-degree), then latest in the listing — so a node is
/// always shortened or left out before its parent. The shorter form is the
/// `--compact` line (`← name (file)`).
fn callgraph_budget_text(
    conn: &rusqlite::Connection,
    head: &str,
    children: &std::collections::HashMap<
        (i64, &'static str),
        Vec<&crate::graph::query::CallGraphNode>,
    >,
    root_id: i64,
    footer: &str,
    tokens: usize,
    next: &crate::budget::NextCommand,
) -> Result<String> {
    use crate::budget::{self, Level};
    use std::cmp::Reverse;
    fn collect<'a>(
        children: &std::collections::HashMap<
            (i64, &'static str),
            Vec<&'a crate::graph::query::CallGraphNode>,
        >,
        parent_id: i64,
        parent: Option<usize>,
        direction: &'static str,
        out: &mut Vec<TreeLine<'a>>,
    ) {
        if let Some(kids) = children.get(&(parent_id, direction)) {
            for n in kids {
                out.push(TreeLine {
                    node: n,
                    direction,
                    parent,
                });
                let me = out.len() - 1;
                collect(children, n.node_id, Some(me), direction, out);
            }
        }
    }
    let mut lines: Vec<TreeLine> = Vec::new();
    collect(children, root_id, None, "callers", &mut lines);
    collect(children, root_id, None, "callees", &mut lines);
    let ids: Vec<i64> = lines.iter().map(|l| l.node.node_id).collect();
    let in_degree = budget::caller_counts(conn, &ids)?;
    let n = lines.len();
    let order = budget::order_by_importance(n, |i| {
        (
            Reverse(lines[i].node.depth),
            in_degree.get(&lines[i].node.node_id).copied().unwrap_or(0),
            Reverse(i),
        )
    });
    let steps = budget::standard_steps(&order, |_| true);
    let line_text = |l: &TreeLine, level: Level| -> String {
        let indent = "  ".repeat(l.node.depth as usize);
        let label = crate::domain::display_node_name(&l.node.name);
        let (arrow, arrow_text) = match l.direction {
            "callers" => ("←", "← called by"),
            _ => ("→", "→ calls"),
        };
        if level == Level::Skeleton {
            format!("{indent}{arrow} {label} ({})\n", l.node.file_path)
        } else {
            format!(
                "{indent}{arrow_text}: {label} ({}) [{}]\n",
                l.node.file_path, l.node.node_type
            )
        }
    };
    let render = |levels: &[Level]| -> String {
        let mut out = String::from(head);
        let mut shown = vec![false; n];
        for (i, l) in lines.iter().enumerate() {
            // A line whose parent is not shown is not shown either: the order
            // above already guarantees it, this keeps the tree well-formed if
            // the fill pass brings a child back without its parent.
            let parent_shown = l.parent.is_none_or(|p| shown[p]);
            if levels[i] == Level::Dropped || !parent_shown {
                continue;
            }
            shown[i] = true;
            out.push_str(&line_text(l, levels[i]));
        }
        out.push_str(footer);
        let omitted = shown.iter().filter(|s| !**s).count();
        let short = (0..n)
            .filter(|&i| shown[i] && levels[i] == Level::Skeleton)
            .count();
        if let Some(line) = budget::notice(
            "  ",
            tokens,
            &[
                (omitted, "node omitted", "nodes omitted"),
                (short, "node without its type", "nodes without their type"),
            ],
        ) {
            out.push_str(&format!("{line}\n  next: {next}\n"));
        }
        out
    };
    Ok(budget::fit(
        &vec![Level::Full; n],
        &steps,
        budget::budget_bytes(tokens),
        |l| {
            let s = render(l);
            let len = s.len();
            (s, len)
        },
    )
    .output)
}

// --- impact subcommand ---
