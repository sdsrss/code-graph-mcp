use super::*;

/// Caller-traversal confidence floor for `show --impact`.
///
/// Must equal the default `impact`/MCP `get_ast_node` use, and it did not: both
/// call sites here passed a literal `0` (= keep ambiguous by-name callers), so
/// the SAME symbol got one risk level from `show --impact` and a lower one from
/// `impact`, with no field in either output explaining the difference
/// (2026-08-16 audit §四). `inferred` is the documented default floor — folding
/// the ambiguous fan-out out of a RISK number is the whole point of having one.
/// `show` has no `--min-confidence` flag of its own, so this is a constant rather
/// than a parsed tier; `impact_and_show_agree_on_the_default_confidence_floor`
/// pins it to `cmd_impact`'s default.
pub(crate) const SHOW_IMPACT_MIN_CONF_RANK: u8 = 1; // confidence_rank(CONF_INFERRED)

/// CLI arguments for the `show` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp show",
    about = "Show symbol details (code, type, signature)"
)]
pub struct ShowArgs {
    /// Symbol name (required unless --node-id is given)
    pub symbol: Option<String>,
    /// Look up by node ID instead of name
    #[arg(long = "node-id")]
    pub node_id: Option<i64>,
    /// Disambiguate same-name symbols by file path
    #[arg(long)]
    pub file: Option<String>,
    /// Show callers/callees (hidden aliases: --include-refs, --include-references)
    #[arg(long = "refs", aliases = ["include-refs", "include-references"])]
    pub refs: bool,
    /// Show impact summary (hidden alias: --include-impact)
    #[arg(long = "impact", alias = "include-impact")]
    pub impact: bool,
    /// Show test callers/callees in the --refs section (hidden by default)
    #[arg(long)]
    pub include_tests: bool,
    /// Surrounding source lines (default: 3 with --node-id, else 0)
    #[arg(long = "context-lines")]
    pub context_lines: Option<usize>,
    /// Compact output
    #[arg(long)]
    pub compact: bool,
    /// JSON output
    #[arg(long)]
    pub json: bool,
}

/// Show symbol details (code, type, signature).
/// CLI equivalent of MCP `get_ast_node`.
/// Resolve a `show` positional symbol to its node(s), applying the shared
/// `Class.method` base-name fallback. Factored out of `cmd_show` so it can be
/// re-run after a query-time freshness resync without duplicating the fallback.
pub(crate) fn resolve_show_nodes(
    conn: &rusqlite::Connection,
    symbol: &str,
    file_filter: Option<&str>,
) -> Result<Vec<queries::NodeResult>> {
    let nodes = if let Some(fp) = file_filter {
        let mut found: Vec<_> = queries::get_nodes_by_file_path(conn, fp)?
            .into_iter()
            .filter(|n| n.name == symbol || n.qualified_name.as_deref() == Some(symbol))
            .collect();
        // Same `Class.method` fallback as the name path: if exact match fails
        // but the symbol has a dot, fall back to the base name within the file.
        // Why: parsers populate qualified_name inconsistently across languages
        // (Rust `impl` blocks: yes; free functions: no), so the literal-match
        // filter above used to silently miss legitimate symbols.
        if found.is_empty() && symbol.contains('.') {
            if let Some(base_name) = symbol.rsplit('.').next() {
                found = queries::get_nodes_by_file_path(conn, fp)?
                    .into_iter()
                    .filter(|n| n.name == base_name)
                    .collect();
            }
        }
        found
    } else {
        let mut found = queries::get_nodes_by_name(conn, symbol)?;
        // `Class.method` fallback: when no node has the exact qualified name
        // stored in DB, prefer nodes whose qualified_name matches; otherwise
        // fall back to all nodes with the base name. Without this fallback,
        // `show McpServer.lock_or_recover` was reporting "Symbol not found"
        // even though `callgraph` resolves the same input via prefix-strip.
        if found.is_empty() && symbol.contains('.') {
            if let Some(base_name) = symbol.rsplit('.').next() {
                let by_name = queries::get_nodes_by_name(conn, base_name)?;
                let any_qualified = by_name
                    .iter()
                    .any(|n| n.qualified_name.as_deref() == Some(symbol));
                if any_qualified {
                    found = by_name
                        .into_iter()
                        .filter(|n| n.qualified_name.as_deref() == Some(symbol))
                        .collect();
                } else {
                    found = by_name;
                }
            }
        }
        found
    };
    Ok(nodes)
}

pub fn cmd_show(project_root: &Path, args: ShowArgs) -> Result<()> {
    let json_mode = args.json;
    let compact = args.compact;
    let include_refs = args.refs;
    let include_impact = args.impact;
    let file_filter_owned: Option<String> = match args.file.as_deref() {
        Some(f) => Some(normalize_user_path(project_root, f)?),
        None => None,
    };
    let file_filter = file_filter_owned.as_deref();
    let context_lines_explicit: Option<usize> = args.context_lines;
    let node_id_arg: Option<i64> = args.node_id;
    // Default context_lines=3 when using --node-id (align with MCP behavior), 0 otherwise
    let context_lines: usize =
        context_lines_explicit.unwrap_or(if node_id_arg.is_some() { 3 } else { 0 });

    // If positional arg points at a real file on disk (has a recognized code
    // extension), nudge the user toward `overview` — `show` takes symbol names.
    //
    // The probe resolves the argument the same way `--file` above and every
    // other path in this command do: against the caller's cwd (audit 2026-08-29
    // CON-12). It used to be `project_root.join(arg)`, which made the hint dead
    // from every subdirectory — `show auth.ts` from `src/` fell through to
    // symbol resolution and answered "Symbol not found: auth.ts", for the one
    // input where the tool knows the right next command. A normalization error
    // (a `..` escape, a drive letter) is not a file path worth hinting about,
    // so it falls through to the ordinary symbol path.
    if node_id_arg.is_none() {
        if let Some(arg) = args.symbol.as_deref() {
            let as_path = normalize_user_path(project_root, arg)
                .ok()
                .filter(|rel| !rel.is_empty())
                .map(|rel| project_root.join(rel));
            if !arg.is_empty()
                && crate::utils::config::detect_language(arg).is_some()
                && as_path.is_some_and(|p| p.is_file())
            {
                eprintln!(
                    "[code-graph] `{}` looks like a file path. `show` takes a symbol name (function/struct/const).",
                    arg
                );
                eprintln!(
                    "            File-level symbols: code-graph-mcp overview {}",
                    arg
                );
                eprintln!("            Full file content:  Read the file directly.");
                std::process::exit(1);
            }
        }
    }

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    // Resolve node(s): by --node-id, or by positional symbol name
    let nodes_with_paths: Vec<(queries::NodeResult, String)> = if let Some(nid) = node_id_arg {
        match queries::get_node_with_file_by_id(conn, nid)? {
            // CON-10: the symbol branch below resyncs; this one used to return
            // straight from the index. `show` prints start_line/end_line AND
            // slices the live file at those offsets via `read_source_context`
            // (with context_lines defaulting to 3 on exactly this branch), so the
            // branch that skipped the refresh is the one that could print a
            // window of unrelated code under the symbol's name.
            //
            // Re-resolve by identity, never by id: ids are rowid-scoped and a
            // re-index reuses freed ones — see `resolve::reresolve_node_by_identity`.
            Some(nwf) => {
                let outcome = refresh_files_if_stale(
                    &ctx.db,
                    &ctx.project_root,
                    std::slice::from_ref(&nwf.file_path),
                );
                let resolved = if outcome.any_changed {
                    crate::resolve::reresolve_node_by_identity(
                        conn,
                        &nwf.file_path,
                        &nwf.node.name,
                        nwf.node.qualified_name.as_deref(),
                        &nwf.node.node_type,
                    )?
                } else {
                    Some(nwf)
                };
                outcome.disclose();
                match resolved {
                    Some(nwf) => vec![(nwf.node, nwf.file_path)],
                    None => {
                        if json_mode {
                            println!(
                                "{}",
                                serde_json::json!({
                                    "error": "Symbol no longer present after refresh",
                                    "node_id": nid,
                                })
                            );
                        }
                        eprintln!(
                            "[code-graph] Node ID {} named a symbol that is gone from the \
                             re-indexed file. Re-resolve by name: code-graph-mcp show <symbol>",
                            nid
                        );
                        std::process::exit(1);
                    }
                }
            }
            None => {
                if json_mode {
                    // In-band error object (roadmap 2026-07-18 §1.3), matching
                    // impact's `{"error", "symbol"}` miss contract.
                    println!(
                        "{}",
                        serde_json::json!({
                            "error": "Node ID not found", "node_id": nid,
                        })
                    );
                }
                eprintln!("[code-graph] Node ID {} not found.", nid);
                std::process::exit(1);
            }
        }
    } else {
        let symbol = args.symbol.as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!(
                "Usage: code-graph-mcp show <symbol> [--node-id N] [--file <path>] [--refs] [--impact] [--context-lines N] [--compact] [--json]"
            ))?;

        let mut nodes = resolve_show_nodes(conn, symbol, file_filter)?;

        // Lazy query-time freshness (parity with `cmd_grep`'s resync and the MCP
        // tools' `ensure_file_fresh_opt`): `show` prints start_line/end_line +
        // code_content straight from the index, so a file edited after the last
        // index would report pre-edit line numbers — the "sed to a `show` line and
        // land off by the inserted-line count" bug. Hash-compare each file the
        // symbol resolves into, re-index the dirty ones, then re-resolve. Bounded
        // so a common name spanning many dirty files can't stall an interactive
        // show; on write contention / parse failure we keep the (stale-but-present)
        // node — exactly the pre-fix behavior, never worse.
        let mut files: Vec<String> = nodes
            .iter()
            .filter_map(|n| queries::get_file_path(conn, n.file_id).ok().flatten())
            .collect();
        // With --file, also refresh the named file when the symbol didn't resolve
        // yet — an edit that ADDED the symbol post-index is then picked up too.
        if let Some(fp) = file_filter {
            files.push(fp.to_string());
        }
        let outcome = refresh_files_if_stale(&ctx.db, &ctx.project_root, &files);
        if outcome.any_changed {
            nodes = resolve_show_nodes(conn, symbol, file_filter)?;
        }
        outcome.disclose();

        if nodes.is_empty() {
            let candidates = queries::find_functions_by_fuzzy_name(conn, symbol)?;
            if json_mode {
                // In-band error + fuzzy candidates (roadmap 2026-07-18 §1.3):
                // the stderr-only "Did you mean" list was invisible under
                // `--json 2>/dev/null`, so the miss read as "symbol absent".
                // Shape matches impact's `{"error", "symbol"}` miss contract.
                let sugg: Vec<serde_json::Value> = candidates
                    .iter()
                    .take(5)
                    .map(|c| {
                        serde_json::json!({
                            "name": c.name, "type": c.node_type, "file_path": c.file_path,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::json!({
                        "error": "Symbol not found", "symbol": symbol, "candidates": sugg,
                    })
                );
            }
            eprintln!("[code-graph] Symbol not found: {}", symbol);
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

        nodes
            .into_iter()
            .map(|n| {
                let fp = queries::get_file_path(conn, n.file_id)
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "?".to_string());
                (n, fp)
            })
            .collect()
    };

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        let results: Vec<serde_json::Value> = nodes_with_paths.iter().map(|(node, fp)| {
            let mut obj = serde_json::json!({
                "node_id": node.id,
                "type": node.node_type,
                "name": node.qualified_name.as_deref().unwrap_or(&node.name),
                "file_path": fp,
                "start_line": node.start_line,
                "end_line": node.end_line,
                "signature": node.signature,
                "return_type": node.return_type,
                "param_types": node.param_types,
            });
            if !compact {
                if context_lines > 0 {
                    // ctx.project_root, NOT the raw one: from a linked worktree
                    // with no own index, CliContext reads the MAIN checkout's
                    // index (effective_read_root), so start_line/end_line below
                    // are the main checkout's. Slicing the WORKTREE's bytes at
                    // those offsets prints whatever happens to sit on those lines
                    // on the other branch (audit 2026-08-02 FRS-4).
                    if let Some((code, first, last)) = read_source_context(&ctx.project_root, fp, node.start_line, node.end_line, context_lines) {
                        obj["code_content"] = serde_json::json!(code);
                        // `code_content` is wider than start_line..end_line here.
                        // Publish the range it really covers (parity with MCP
                        // get_ast_node); omitted when the two agree so the common
                        // context_lines=0 envelope is byte-identical to before.
                        if first != node.start_line || last != node.end_line {
                            obj["content_start_line"] = serde_json::json!(first);
                            obj["content_end_line"] = serde_json::json!(last);
                        }
                    } else {
                        obj["code_content"] = serde_json::json!(node.code_content);
                    }
                } else {
                    obj["code_content"] = serde_json::json!(node.code_content);
                }
            }
            if include_refs {
                use crate::domain::REL_CALLS;
                let include_tests = args.include_tests;
                let callees = queries::get_edge_targets_with_files(conn, node.id, REL_CALLS).unwrap_or_default();
                let callers = queries::get_edge_sources_with_files(conn, node.id, REL_CALLS).unwrap_or_default();
                obj["calls"] = serde_json::json!(callees.iter().map(|(n, f)| serde_json::json!({"name": n, "file": f})).collect::<Vec<_>>());
                let filtered_callers: Vec<_> = if include_tests {
                    callers.iter().collect()
                } else {
                    callers.iter().filter(|(n, f, t)| !crate::domain::is_test_node(*t, n, f)).collect()
                };
                obj["called_by"] = serde_json::json!(filtered_callers.iter().map(|(n, f, _)| serde_json::json!({"name": n, "file": f})).collect::<Vec<_>>());
                if !include_tests {
                    let test_count = callers.len() - filtered_callers.len();
                    if test_count > 0 {
                        obj["test_callers_hidden"] = serde_json::json!(test_count);
                    }
                }
            }
            if include_impact {
                // Shared prod/test partition + risk (graph::impact) — same source as
                // `cmd_impact`/MCP get_ast_node. Trusts the AST `is_test` flag so inline
                // `#[cfg(test)]` unit tests don't inflate the prod count / risk level.
                let callers = crate::graph::routes::get_callers_with_route_info(conn, &node.name, Some(fp.as_str()), 3, SHOW_IMPACT_MIN_CONF_RANK).unwrap_or_default();
                let is_function_like = crate::domain::is_function_node_type(&node.node_type);
                let cls = crate::graph::impact::classify_impact(&callers, "behavior", is_function_like);
                obj["impact"] = serde_json::json!({
                    "risk_level": cls.risk_level,
                    "direct_callers": cls.prod_callers.iter().filter(|c| c.depth == 1).count(),
                    "transitive_callers": cls.prod_callers.iter().filter(|c| c.depth > 1).count(),
                    "affected_files": cls.affected_files,
                    "affected_routes": cls.route_callers.len(),
                });
                // Disclose how many test callers were excluded from the prod risk count
                // (parity with MCP get_ast_node's impact.test_callers_filtered, and with
                // callgraph's test_callers_hidden / project_map's test_caller_count).
                if cls.test_count > 0 {
                    obj["impact"]["test_callers_filtered"] = serde_json::json!(cls.test_count);
                }
            }
            obj
        }).collect();
        writeln!(stdout, "{}", serde_json::to_string(&results)?)?;
        return Ok(());
    }

    for (node, fp) in &nodes_with_paths {
        writeln!(stdout, "{}", format_node_compact(node, fp))?;
        if !compact {
            if context_lines > 0 {
                // Same worktree-aware root as the JSON arm above (FRS-4).
                if let Some((code, first, last)) = read_source_context(
                    &ctx.project_root,
                    fp,
                    node.start_line,
                    node.end_line,
                    context_lines,
                ) {
                    // The header line above says `path:start-end` (the SYMBOL);
                    // the block below is wider. Name the range actually printed,
                    // or a reader counting down from `start` is off by the amount
                    // of leading context.
                    if first != node.start_line || last != node.end_line {
                        writeln!(
                            stdout,
                            "  [lines {}-{}, ±{} context]",
                            first, last, context_lines
                        )?;
                    }
                    for line in code.lines() {
                        writeln!(stdout, "  {}", line)?;
                    }
                } else if !node.code_content.is_empty() {
                    for line in node.code_content.lines() {
                        writeln!(stdout, "  {}", line)?;
                    }
                }
            } else if !node.code_content.is_empty() {
                for line in node.code_content.lines() {
                    writeln!(stdout, "  {}", line)?;
                }
            }
        }
        if include_refs {
            use crate::domain::REL_CALLS;
            let include_tests = args.include_tests;
            let callees =
                queries::get_edge_targets_with_files(conn, node.id, REL_CALLS).unwrap_or_default();
            let callers =
                queries::get_edge_sources_with_files(conn, node.id, REL_CALLS).unwrap_or_default();
            if !callees.is_empty() {
                writeln!(stdout, "  Calls:")?;
                for (name, file) in &callees {
                    writeln!(stdout, "    → {} ({})", name, file)?;
                }
            }
            if !callers.is_empty() {
                let mut test_count = 0usize;
                writeln!(stdout, "  Called by:")?;
                for (name, file, is_test) in &callers {
                    if !include_tests && crate::domain::is_test_node(*is_test, name, file) {
                        test_count += 1;
                    } else {
                        writeln!(stdout, "    ← {} ({})", name, file)?;
                    }
                }
                if test_count > 0 {
                    writeln!(
                        stdout,
                        "    ({} test callers hidden, use --include-tests to show)",
                        test_count
                    )?;
                }
            }
        }
        if include_impact {
            let callers = crate::graph::routes::get_callers_with_route_info(
                conn,
                &node.name,
                Some(fp.as_str()),
                3,
                SHOW_IMPACT_MIN_CONF_RANK,
            )
            .unwrap_or_default();
            let is_function_like = crate::domain::is_function_node_type(&node.node_type);
            let cls = crate::graph::impact::classify_impact(&callers, "behavior", is_function_like);
            writeln!(
                stdout,
                "  Impact: {} — {} direct, {} transitive, {} files, {} routes",
                cls.risk_level,
                cls.prod_callers.iter().filter(|c| c.depth == 1).count(),
                cls.prod_callers.iter().filter(|c| c.depth > 1).count(),
                cls.affected_files,
                cls.route_callers.len()
            )?;
            if cls.test_count > 0 {
                writeln!(
                    stdout,
                    "  ({} test callers excluded from the risk count)",
                    cls.test_count
                )?;
            }
        }
    }

    Ok(())
}

/// Read source code with context lines from the project file system.
///
/// Returns the slice AND the 1-based inclusive line range it actually covers.
/// The range is not decoration: with `context_lines > 0` the returned text spans
/// more lines than the symbol's own `start_line..end_line`, and every consumer
/// (the `--json` envelope, the text arm, MCP `get_ast_node`/`read_snippet`) used
/// to publish the symbol's range next to the widened text — so anything counting
/// lines from `start_line` landed on the wrong one. Clamped at both ends: the
/// leading context stops at line 1 and the trailing context at EOF, so the
/// caller cannot derive the true range from `context_lines` alone either.
pub(crate) fn read_source_context(
    project_root: &Path,
    file_path: &str,
    start_line: i64,
    end_line: i64,
    context_lines: usize,
) -> Option<(String, i64, i64)> {
    use std::io::BufRead;
    let abs_path = project_root.join(file_path);
    let canonical = abs_path.canonicalize().ok()?;
    let root_canonical = project_root.canonicalize().ok()?;
    if !canonical.starts_with(&root_canonical) {
        return None;
    }
    let file = std::fs::File::open(&canonical).ok()?;
    let reader = std::io::BufReader::new(file);
    let start = (start_line as usize).saturating_sub(1 + context_lines);
    let end = (end_line as usize) + context_lines;
    let mut collected = Vec::new();
    for (i, line) in reader.lines().enumerate() {
        if i >= end {
            break;
        }
        if i >= start {
            collected.push(line.ok()?);
        }
    }
    if collected.is_empty() {
        return None;
    }
    let first = start as i64 + 1;
    let last = start as i64 + collected.len() as i64;
    Some((collected.join("\n"), first, last))
}

// --- trace subcommand ---
