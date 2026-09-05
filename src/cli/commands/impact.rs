use super::*;

/// CLI arguments for the `impact` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp impact",
    about = "Impact analysis (callers, routes, risk level)"
)]
pub struct ImpactArgs {
    /// Symbol name to analyze
    pub symbol: String,
    // The bound stays in the handler and is DERIVED from the traversal's own cap,
    // not typed here; clap parse-errors (exit 2) on non-numeric.
    /// Max traversal depth (default: 3)
    #[arg(long, default_value_t = 3)]
    pub depth: i32,
    /// JSON output
    #[arg(long)]
    pub json: bool,
    /// Disambiguate same-name symbols by file path
    #[arg(long)]
    pub file: Option<String>,
    // --change-type stays an in-handler String (NOT a clap ValueEnum) so the exact
    // "must be one of: signature, behavior, remove" exit-1 message is preserved.
    /// Change type: signature, behavior, or remove
    #[arg(long = "change-type", default_value = "behavior")]
    pub change_type: String,
    /// Minimum caller-edge confidence to count toward risk: extracted, inferred,
    /// or ambiguous. Default 'inferred' folds the ambiguous by-name fan-out out
    /// of the blast radius (the excluded count is still reported); pass
    /// 'ambiguous' to count every resolved caller.
    #[arg(long = "min-confidence")]
    pub min_confidence: Option<String>,
}

/// Impact analysis.
///
/// Shows callers with route info and risk level.
pub fn cmd_impact(project_root: &Path, args: ImpactArgs) -> Result<()> {
    // clap accepts an empty-string positional; preserve the non-empty guard.
    let raw_symbol = args.symbol.as_str();
    if raw_symbol.is_empty() {
        anyhow::bail!("Usage: code-graph-mcp impact <symbol> [--depth N] [--file <path>] [--change-type signature|behavior|remove] [--json]");
    }

    // 1..=CALL_GRAPH_MAX_DEPTH, taken from the constant the traversal enforces
    // rather than restated. The literal here was 20 while
    // `get_call_graph_filtered` has always stopped at 10, so `--depth 15` ran at
    // 10 with nothing said, and `--depth 99` would have disclosed a ceiling of
    // 20 that no query ever used. That exact pair — an advertised 20 against a
    // traversal that stops at 10 — is what `0.132.0` fixed on the MCP side by
    // deriving `COUNT_RANGES` from this same constant; a disclosure naming a
    // number the code never uses is worse than no disclosure.
    let depth: i32 = clamp_arg("--depth", args.depth, 1, CALL_GRAPH_MAX_DEPTH);
    let json_mode = args.json;
    let explicit_file_owned: Option<String> = match args.file.as_deref() {
        Some(f) => Some(normalize_user_path(project_root, f)?),
        None => None,
    };
    let explicit_file = explicit_file_owned.as_deref();
    let change_type = args.change_type.as_str();
    if !matches!(change_type, "signature" | "behavior" | "remove") {
        anyhow::bail!("--change-type must be one of: signature, behavior, remove");
    }
    // Confidence floor for caller traversal: default 'inferred' folds the
    // ambiguous by-name fan-out out of the risk count; --min-confidence ambiguous
    // counts every caller. The excluded count is disclosed below so a folded
    // ambiguous caller never silently under-states risk.
    let min_conf_tier: &'static str =
        crate::domain::parse_min_confidence(args.min_confidence.as_deref(), "--min-confidence")?
            .unwrap_or(crate::domain::DEFAULT_RISK_CONF_FLOOR);
    let min_conf_rank = crate::domain::confidence_rank(min_conf_tier);

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    let (symbol, resolved_file) = resolve_qualified_symbol(conn, raw_symbol, explicit_file);
    let file_filter = explicit_file.or(resolved_file.as_deref());

    // Verify symbol exists before running impact analysis
    let mut symbol_nodes = queries::get_nodes_by_name(conn, symbol)?;
    if symbol_nodes.is_empty() {
        if json_mode {
            println!(
                "{}",
                serde_json::json!({"error": "Symbol not found", "symbol": symbol})
            );
        }
        eprintln!("[code-graph] Symbol not found: {}", symbol);
        let candidates = queries::find_functions_by_fuzzy_name(conn, symbol)?;
        if !candidates.is_empty() {
            eprintln!("[code-graph] Did you mean:");
            for c in candidates.iter().take(5) {
                eprintln!("  {} ({}) in {}", c.name, c.node_type, c.file_path);
            }
        } else {
            hint_symbol_maybe_unindexed(symbol);
        }
        std::process::exit(1);
    }

    // An explicit `--file` that holds no such definition is a MISS, not a
    // filter that legitimately matches nothing. The existence check above uses
    // `get_nodes_by_name`, which ignores the filter, so it passed on a
    // definition in ANOTHER file; the ambiguity guard below is skipped whenever
    // a filter is present; and the caller query then ran with a filter no
    // definition satisfies — zero callers, `"risk":"LOW"`, exit 0. That is a
    // safety endorsement handed to a typo'd path on the command the decision
    // table puts BEFORE an edit. `refs` (`print_refs_notfound_json` +
    // exit 1), `show` and `callgraph` already exit 1 on this exact input;
    // impact was the fourth `--file` taker and the only one that answered
    // (audit 2026-08-16 P1-9).
    if let Some(fp) = explicit_file {
        let in_file = queries::get_nodes_by_file_path(conn, fp)?;
        // `--file` NARROWS, so a qualifier the user typed must survive it.
        // `resolve_qualified_symbol` returns early when `--file` is present and
        // hands back the bare name, so this check used to accept any same-named
        // node in the file: `impact Gamma.run --file two.ts` matched `Alpha.run`
        // and answered `"risk":"LOW"` exit 0 for a class that does not exist —
        // the same safety-endorsement-for-a-typo shape P1-9 fixed for paths,
        // still reachable through the qualifier (audit 2026-08-16 Minor tail).
        let qualified_input = raw_symbol != symbol;
        let present = if qualified_input {
            in_file
                .iter()
                .any(|n| n.qualified_name.as_deref() == Some(raw_symbol))
        } else {
            in_file
                .iter()
                .any(|n| n.name == symbol || n.qualified_name.as_deref() == Some(symbol))
        };
        if !present {
            if json_mode {
                // Same in-band miss contract as `show`: {error, symbol, …} +
                // exit 1, with the files that DO define the symbol so the
                // caller can correct the path instead of re-querying.
                let candidates: Vec<serde_json::Value> = symbol_nodes
                    .iter()
                    .take(5)
                    .map(|n| {
                        serde_json::json!({
                            "name": n.name,
                            "type": n.node_type,
                            "file_path": queries::get_file_path(conn, n.file_id)
                                .ok()
                                .flatten()
                                .unwrap_or_default(),
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::json!({
                        // Echo what the user TYPED. Reporting the stripped
                        // `symbol` for a qualified miss reads as if the bare
                        // name were absent, when the file may well define it
                        // under a different qualifier.
                        "error": "Symbol not found in file",
                        "symbol": raw_symbol,
                        "file": fp,
                        "candidates": candidates,
                    })
                );
            }
            eprintln!(
                "[code-graph] Symbol '{}' not found in file '{}'.",
                raw_symbol, fp
            );
            let defined_in: Vec<String> = symbol_nodes
                .iter()
                .filter_map(|n| queries::get_file_path(conn, n.file_id).ok().flatten())
                .take(5)
                .collect();
            if !defined_in.is_empty() {
                eprintln!("[code-graph] Defined in: {}", defined_in.join(", "));
            }
            std::process::exit(1);
        }
        // The qualifier gates ENTRY but not the traversal: `get_callers_with_route_info`
        // is name+file based, so when the file defines the bare name more than
        // once the blast radius still covers every one of them. Say so rather
        // than reporting a number that silently means "…and its namesakes".
        if qualified_input {
            let same_name = in_file.iter().filter(|n| n.name == symbol).count();
            if same_name > 1 {
                eprintln!(
                    "[code-graph] Note: '{}' defines {} symbols named '{}'; the caller set below \
                     covers all of them (the qualifier narrows the lookup, not the traversal).",
                    fp, same_name, symbol
                );
            }
        }
    }

    // Exact-name ambiguity guard: a bare name with ≥2 non-test definitions
    // (cross-file OR same-file overloads) would silently merge callers across
    // both, misreporting risk/blast radius. Shared with MCP via crate::resolve.
    if file_filter.is_none() {
        if let Some(cands) = crate::resolve::detect_ambiguity(conn, symbol)? {
            emit_exact_ambiguity(symbol, &cands, json_mode);
        }
    }

    let mut callers = crate::graph::routes::get_callers_with_route_info(
        conn,
        symbol,
        file_filter,
        depth,
        min_conf_rank,
    )?;
    // Query-time freshness (shared resync with show/refs/… via refresh_files_if_stale):
    // re-index the symbol's own file(s) and its caller files so the blast radius
    // reflects disk (a caller added/removed since indexing). impact prints no line
    // numbers, so this refreshes the caller SET; re-run the caller query and re-fetch
    // symbol_nodes when anything changed.
    let fresh_outcome = {
        let mut files: Vec<String> = symbol_nodes
            .iter()
            .filter_map(|n| queries::get_file_path(conn, n.file_id).ok().flatten())
            .collect();
        for c in &callers {
            files.push(c.file_path.clone());
        }
        let outcome = refresh_files_if_stale(&ctx.db, &ctx.project_root, &files);
        if outcome.any_changed {
            callers = crate::graph::routes::get_callers_with_route_info(
                conn,
                symbol,
                file_filter,
                depth,
                min_conf_rank,
            )?;
            symbol_nodes = queries::get_nodes_by_name(conn, symbol)?;
        }
        outcome.disclose();
        outcome
    };
    // Ambiguous callers folded out of the blast radius by the confidence floor,
    // counted across the whole returned frontier (seed direct + every kept
    // caller's pruned callers) so a TRANSITIVE ambiguous caller of a
    // uniquely-named symbol is disclosed too. Surfaced (not silently dropped) so a
    // folded real caller never under-states risk; --min-confidence ambiguous counts them.
    let caller_ids: Vec<i64> = callers
        .iter()
        .filter(|c| c.depth > 0)
        .map(|c| c.node_id)
        .collect();
    let ambiguous_callers_excluded =
        crate::graph::query::count_suppressed_seed_edges(
            conn,
            symbol,
            file_filter,
            crate::graph::query::Direction::Callers,
            min_conf_rank,
        )? + crate::graph::query::count_suppressed_into(conn, &caller_ids, min_conf_rank)?;

    // Partition prod/test callers (deduped by name,file,depth), count routes/files,
    // and assess risk via the surface-shared classifier — the MCP impact tool runs
    // the identical rule. crate::graph::impact owns the prod-only route policy (a
    // test-only endpoint is not a production blast radius) and the dedup.
    let is_function_like = symbol_nodes
        .iter()
        .any(|n| crate::domain::is_function_node_type(n.node_type.as_str()));
    let impact = crate::graph::impact::classify_impact(&callers, change_type, is_function_like);
    let prod_callers = &impact.prod_callers;
    let routes = &impact.route_callers;
    let direct_callers = prod_callers.iter().filter(|c| c.depth == 1).count();
    let risk = impact.risk_level;

    // Value references (REL_REFERENCES): callbacks / fn-pointers / type-position
    // couplings the call graph misses. Prod sources, deduped by referencing symbol.
    // Mirrors the MCP impact tool (server/tools/advanced.rs) so both surfaces report
    // the same signal — CLI/MCP parity. NEVER folded into the caller counts above.
    let value_references = {
        use std::collections::HashSet;
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for n in &symbol_nodes {
            for r in
                queries::get_incoming_references(conn, n.id, Some(crate::domain::REL_REFERENCES))?
            {
                // `is_test_node`, not the name/path heuristic alone: this count
                // feeds a PRODUCTION coupling signal, and an inline `#[cfg(test)]`
                // reference used to land in it (2026-08-16 audit §四).
                if !crate::domain::is_test_node(r.is_test, &r.name, &r.file_path) {
                    seen.insert((r.name, r.file_path));
                }
            }
        }
        seen.len()
    };

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        let mut result = serde_json::json!({
            "symbol": symbol,
            "risk": risk,
            "direct_callers": direct_callers,
            "total_callers": prod_callers.len(),
            "tests_affected": impact.test_count,
            "affected_files": impact.affected_files,
            "affected_routes": routes.len(),
            "value_references": value_references,
            "callers": prod_callers.iter().map(|c| serde_json::json!({
                "name": c.name,
                "type": c.node_type,
                "file": c.file_path,
                "depth": c.depth,
                "route": c.route_info,
            })).collect::<Vec<_>>(),
            // Covering tests behind `tests_affected` — name + file is enough for a
            // hook to build a runnable test command (e.g. `cargo test`/`pytest`).
            // Full list (not capped here); display-side capping is the surface's job.
            "test_callers": impact.test_callers.iter().map(|c| serde_json::json!({
                "name": c.name,
                "file": c.file_path,
            })).collect::<Vec<_>>(),
        });
        if let Some(warning) = impact.type_warning {
            result["warning"] = serde_json::json!(warning);
        }
        if ambiguous_callers_excluded > 0 {
            result["ambiguous_callers_excluded"] = serde_json::json!(ambiguous_callers_excluded);
            result["ambiguous_note"] = serde_json::json!(format!(
                "{} direct caller(s) resolved only by ambiguous name-match were excluded from this risk assessment; actual blast radius may be larger. Re-run with --min-confidence ambiguous to include them.",
                ambiguous_callers_excluded
            ));
        }
        fresh_outcome.attach_partial(&mut result);
        writeln!(stdout, "{}", serde_json::to_string(&result)?)?;
        return Ok(());
    }

    writeln!(stdout, "Impact: {} — Risk: {}", symbol, risk)?;
    if let Some(warning) = impact.type_warning {
        writeln!(stdout, "  (warning: {})", warning)?;
    }
    writeln!(
        stdout,
        "  {} direct, {} total, {}, {} ({} affected)",
        direct_callers,
        plural(prod_callers.len() as i64, "caller"),
        plural(impact.affected_files as i64, "file"),
        plural(routes.len() as i64, "route"),
        plural(impact.test_count as i64, "test")
    )?;
    if ambiguous_callers_excluded > 0 {
        writeln!(
            stdout,
            "  ⚠ {} ambiguous by-name caller(s) excluded from risk — actual blast radius may be larger; use --min-confidence ambiguous to include",
            ambiguous_callers_excluded
        )?;
    }
    if value_references > 0 {
        writeln!(
            stdout,
            "  {} value reference(s) — callbacks / fn-pointers / type positions (not call-graph callers)",
            value_references
        )?;
    }

    if !routes.is_empty() {
        writeln!(stdout, "Routes:")?;
        for r in routes {
            let route_str = r.route_info.as_deref().unwrap_or("?");
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(route_str) {
                let method = v["method"].as_str().unwrap_or("?");
                let path = v["path"].as_str().unwrap_or("?");
                writeln!(
                    stdout,
                    "  {} {} → {} ({})",
                    method, path, r.name, r.file_path
                )?;
            } else {
                writeln!(stdout, "  {} → {} ({})", route_str, r.name, r.file_path)?;
            }
        }
    }

    if !prod_callers.is_empty() {
        writeln!(stdout, "Callers:")?;
        for c in prod_callers {
            let indent = "  ".repeat(c.depth as usize);
            writeln!(
                stdout,
                "{}{}  ({}) {}",
                indent, c.name, c.node_type, c.file_path
            )?;
        }
    }

    Ok(())
}

// --- affected subcommand ---
