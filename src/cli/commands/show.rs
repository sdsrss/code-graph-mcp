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
    /// Token budget for the text answer (bytes/3, 100-100000): definitions
    /// with the fewest callers lose their body first (signature + file:line
    /// stay), then reference lines, then whole definitions are left out; a
    /// body is never cut part-way; ends with a command that prints the rest
    #[arg(long, conflicts_with_all = ["json", "compact"])]
    pub budget: Option<u64>,
}

/// The text `show` prints for one definition, split into the parts a budget
/// can keep or leave out. Concatenated in field order it is exactly the
/// unbudgeted answer.
struct ShowNodeText {
    header: String,
    body: String,
    calls: Vec<String>,
    callers: Vec<String>,
    /// `Called by:` is printed when the node has any caller, test callers
    /// included, so the section can exist with no visible line.
    callers_section: bool,
    callers_hidden: String,
    impact: String,
}

impl ShowNodeText {
    fn render(&self, body: bool, calls: &[bool], callers: &[bool]) -> String {
        let mut out = String::new();
        out.push_str(&self.header);
        if body {
            out.push_str(&self.body);
        }
        if calls.iter().any(|k| *k) {
            out.push_str("  Calls:\n");
            for (l, k) in self.calls.iter().zip(calls) {
                if *k {
                    out.push_str(l);
                }
            }
        }
        let any_caller = callers.iter().any(|k| *k);
        if self.callers_section && (any_caller || self.callers.is_empty()) {
            out.push_str("  Called by:\n");
            for (l, k) in self.callers.iter().zip(callers) {
                if *k {
                    out.push_str(l);
                }
            }
            out.push_str(&self.callers_hidden);
        }
        out.push_str(&self.impact);
        out
    }

    fn render_full(&self) -> String {
        self.render(
            true,
            &vec![true; self.calls.len()],
            &vec![true; self.callers.len()],
        )
    }
}

/// `show --budget`: the definitions fitted to `tokens`.
///
/// Units: one per definition (shorter form = the signature line without the
/// body; ranked by call-edge in-degree) and one per reference line. Every body
/// goes first (least-called definition first), then reference lines (from the
/// end of each list), then whole definitions.
fn show_budget_text(
    conn: &rusqlite::Connection,
    nodes: &[(i64, ShowNodeText)],
    tokens: usize,
    next: &crate::budget::NextCommand,
) -> Result<String> {
    use crate::budget::{self, Level};
    use std::cmp::Reverse;
    let ids: Vec<i64> = nodes.iter().map(|(id, _)| *id).collect();
    let in_degree = budget::caller_counts(conn, &ids)?;
    let nn = nodes.len();
    // Unit layout: 0..nn definitions, then every reference line.
    let mut refs: Vec<(usize, bool, usize)> = Vec::new(); // (node, is_caller, index)
    for (ni, (_, t)) in nodes.iter().enumerate() {
        refs.extend((0..t.calls.len()).map(|i| (ni, false, i)));
        refs.extend((0..t.callers.len()).map(|i| (ni, true, i)));
    }
    let n = nn + refs.len();
    let node_order = budget::order_by_importance(nn, |i| {
        (in_degree.get(&nodes[i].0).copied().unwrap_or(0), Reverse(i))
    });
    let mut rank_of = vec![0usize; nn];
    for (r, &i) in node_order.iter().enumerate() {
        rank_of[i] = r;
    }
    let ref_order: Vec<usize> = budget::order_by_importance(refs.len(), |r| {
        let (ni, _, idx) = refs[r];
        (rank_of[ni], Reverse(idx))
    })
    .into_iter()
    .map(|r| nn + r)
    .collect();
    let mut order = ref_order;
    order.extend(node_order.iter().copied());
    let steps = budget::standard_steps(&order, |u| u < nn && !nodes[u].1.body.is_empty());
    let render = |levels: &[Level]| -> String {
        let mut out = String::new();
        let mut refs_cut = 0usize;
        for (ni, (_, t)) in nodes.iter().enumerate() {
            if levels[ni] == Level::Dropped {
                continue;
            }
            let keep = |caller: bool, len: usize| -> Vec<bool> {
                (0..len)
                    .map(|i| {
                        let r = refs
                            .iter()
                            .position(|&(a, b, c)| a == ni && b == caller && c == i)
                            .unwrap();
                        levels[nn + r] != Level::Dropped
                    })
                    .collect()
            };
            let calls = keep(false, t.calls.len());
            let callers = keep(true, t.callers.len());
            refs_cut += calls.iter().chain(&callers).filter(|k| !**k).count();
            out.push_str(&t.render(levels[ni] == Level::Full, &calls, &callers));
        }
        let dropped = (0..nn).filter(|&i| levels[i] == Level::Dropped).count();
        let no_body = (0..nn).filter(|&i| levels[i] == Level::Skeleton).count();
        if let Some(line) = budget::notice(
            "",
            tokens,
            &[
                (dropped, "definition omitted", "definitions omitted"),
                (
                    no_body,
                    "definition without its body",
                    "definitions without their body",
                ),
                (
                    refs_cut,
                    "reference line omitted",
                    "reference lines omitted",
                ),
            ],
        ) {
            out.push_str(&format!("{line}\nnext: {next}\n"));
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
        // Exact-qualified precedence for dotted input. `show` used to invert it:
        // it queried the literal `name` column first and consulted
        // `qualified_name` only when THAT came back empty. Several extractors put
        // a dotted string in the `name` column; the ones that collide with a real
        // `Class.method` spelling are markdown headings, gtest `TEST(Suite, Case)`
        // and bash `function Foo.bar()`. For those the first query was never empty
        // and the fallback below never ran, so `show Widget.run` answered with the
        // heading or the test case and dropped the method entirely. The class is
        // not closed — see the note on `get_nodes_with_files_by_qualified_name`.
        //
        // The two guards over this collision only exercised refs/callgraph/impact,
        // which refuse rather than render, so neither covered `show`.
        //
        // Deliberately the RAW shared query, not `resolve::selectable_qualified_definitions`
        // (what refs/callgraph/impact select through). That wrapper adds a
        // production-over-test partition on top of this query, which would ALSO
        // drop the gtest case from the rendered set — a second, unrelated
        // narrowing. `show` rendered the gtest case before this change too, so
        // taking the partition here would go past restoring the method. What the
        // shared query does buy is the vetted `module` / `h1`..`h6` / `<external>`
        // exclusions, in one place rather than restated.
        let qualified: Vec<queries::NodeResult> = if symbol.contains('.') {
            queries::get_nodes_with_files_by_qualified_name(conn, symbol)?
                .into_iter()
                .map(|n| n.node)
                .collect()
        } else {
            Vec::new()
        };
        if !qualified.is_empty() {
            return Ok(qualified);
        }
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
    let args = ShowArgs {
        symbol: args
            .symbol
            .map(|s| crate::resolve::canonical_symbol_spelling(&s)),
        ..args
    };
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
        // A symbol in a file added since the last index (D2).
        crate::cli::freshness::index_new_files_if_absent(&ctx.db, &ctx.project_root, symbol);

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
                    .take(crate::resolve::SUGGESTION_CAP)
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
                for c in candidates.iter().take(crate::resolve::SUGGESTION_CAP) {
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
        // SURF-18 (audit 2026-09-07): the closure returns a Result and the
        // collect propagates it. It used to swallow every edge-query failure
        // with `unwrap_or_default()`, which turned "the query failed" into
        // "this symbol has no callers" and then into `Impact: LOW` — a safety
        // endorsement manufactured out of a database error. `cmd_impact` runs
        // the identical query with `?`, and the LOW-on-a-typo version of this
        // same hazard is what `impact.rs:102-112` was fixed for.
        let results: Vec<serde_json::Value> = nodes_with_paths.iter().map(|(node, fp)| -> Result<serde_json::Value> {
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
                let callees = queries::get_edge_targets_with_files(conn, node.id, REL_CALLS)?;
                let callers = queries::get_edge_sources_with_files(conn, node.id, REL_CALLS)?;
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
                let caller_set = crate::graph::routes::get_callers_with_route_info(conn, &node.name, Some(fp.as_str()), 3, SHOW_IMPACT_MIN_CONF_RANK)?;
                let is_function_like = crate::domain::is_function_node_type(&node.node_type);
                let cls = crate::graph::impact::classify_impact(&caller_set.callers, "behavior", is_function_like);
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
                // CORE-11: same disclosure as `cmd_impact` and MCP get_ast_node.
                // This is the third consumer of the same truncatable traversal;
                // leaving one silent recreates the "two surfaces, one traversal,
                // different stories" split the flag exists to close.
                if let Some(note) = caller_set.truncation_note() {
                    obj["impact"]["callers_truncated"] = serde_json::json!(true);
                    obj["impact"]["callers_truncated_note"] = serde_json::json!(note);
                }
            }
            Ok(obj)
        }).collect::<Result<Vec<_>>>()?;
        writeln!(stdout, "{}", serde_json::to_string(&results)?)?;
        return Ok(());
    }

    let mut texts: Vec<(i64, ShowNodeText)> = Vec::with_capacity(nodes_with_paths.len());
    for (node, fp) in &nodes_with_paths {
        let header = format!("{}\n", format_node_compact(node, fp));
        let mut body = String::new();
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
                        body.push_str(&format!(
                            "  [lines {}-{}, ±{} context]\n",
                            first, last, context_lines
                        ));
                    }
                    for line in code.lines() {
                        body.push_str(&format!("  {}\n", line));
                    }
                } else if !node.code_content.is_empty() {
                    for line in node.code_content.lines() {
                        body.push_str(&format!("  {}\n", line));
                    }
                }
            } else if !node.code_content.is_empty() {
                for line in node.code_content.lines() {
                    body.push_str(&format!("  {}\n", line));
                }
            }
        }
        let mut calls: Vec<String> = Vec::new();
        let mut callers_lines: Vec<String> = Vec::new();
        let mut callers_section = false;
        let mut callers_hidden = String::new();
        if include_refs {
            use crate::domain::REL_CALLS;
            let include_tests = args.include_tests;
            // SURF-18: `?`, not `unwrap_or_default()` — see the JSON arm above.
            let callees = queries::get_edge_targets_with_files(conn, node.id, REL_CALLS)?;
            let callers = queries::get_edge_sources_with_files(conn, node.id, REL_CALLS)?;
            for (name, file) in &callees {
                calls.push(format!("    → {} ({})\n", name, file));
            }
            if !callers.is_empty() {
                callers_section = true;
                let mut test_count = 0usize;
                for (name, file, is_test) in &callers {
                    if !include_tests && crate::domain::is_test_node(*is_test, name, file) {
                        test_count += 1;
                    } else {
                        callers_lines.push(format!("    ← {} ({})\n", name, file));
                    }
                }
                if test_count > 0 {
                    callers_hidden = format!(
                        "    ({} test callers hidden, use --include-tests to show)\n",
                        test_count
                    );
                }
            }
        }
        let mut impact = String::new();
        if include_impact {
            let caller_set = crate::graph::routes::get_callers_with_route_info(
                conn,
                &node.name,
                Some(fp.as_str()),
                3,
                SHOW_IMPACT_MIN_CONF_RANK,
            )?;
            let is_function_like = crate::domain::is_function_node_type(&node.node_type);
            let cls = crate::graph::impact::classify_impact(
                &caller_set.callers,
                "behavior",
                is_function_like,
            );
            impact.push_str(&format!(
                "  Impact: {} — {} direct, {} transitive, {} files, {} routes\n",
                cls.risk_level,
                cls.prod_callers.iter().filter(|c| c.depth == 1).count(),
                cls.prod_callers.iter().filter(|c| c.depth > 1).count(),
                cls.affected_files,
                cls.route_callers.len()
            ));
            if cls.test_count > 0 {
                impact.push_str(&format!(
                    "  ({} test callers excluded from the risk count)\n",
                    cls.test_count
                ));
            }
            if let Some(note) = caller_set.truncation_note() {
                impact.push_str(&format!("  ⚠ {}\n", note));
            }
        }
        texts.push((
            node.id,
            ShowNodeText {
                header,
                body,
                calls,
                callers: callers_lines,
                callers_section,
                callers_hidden,
                impact,
            },
        ));
    }

    if let Some(requested) = args.budget {
        let tokens = clamp_arg(
            "--budget",
            requested,
            crate::budget::MIN_BUDGET_TOKENS,
            crate::budget::MAX_BUDGET_TOKENS,
        ) as usize;
        let mut next = match node_id_arg {
            Some(id) => crate::budget::NextCommand::new("show")
                .arg("--node-id")
                .arg(id.to_string()),
            None => {
                crate::budget::NextCommand::new("show").arg(args.symbol.clone().unwrap_or_default())
            }
        };
        next = next
            .opt_path("--file", args.file.clone())
            .flag_if(include_refs, "--refs")
            .flag_if(include_impact, "--impact")
            .flag_if(args.include_tests, "--include-tests")
            .opt(
                "--context-lines",
                context_lines_explicit.map(|c| c.to_string()),
            );
        write!(stdout, "{}", show_budget_text(conn, &texts, tokens, &next)?)?;
        return Ok(());
    }

    for (_, t) in &texts {
        write!(stdout, "{}", t.render_full())?;
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
