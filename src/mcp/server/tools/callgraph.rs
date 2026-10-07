//! `get_call_graph` — multi-hop callers/callees with rollup compression on dense fanouts.

use super::super::*;

/// Surface call_graph truncation provenance into a JSON response so agents
/// can tell when results are partial. Adds `limit_hit`, `depth_capped`, and
/// `effective_max_depth` only when the result was actually truncated, plus a
/// human-readable `truncation_warning` when either flag fires.
pub(super) fn attach_truncation_flags(
    target: &mut serde_json::Value,
    result: &crate::graph::query::CallGraphResult,
) {
    if !(result.limit_hit || result.depth_capped) {
        return;
    }
    if result.limit_hit {
        target["limit_hit"] = json!(true);
    }
    if result.depth_capped {
        target["depth_capped"] = json!(true);
        target["effective_max_depth"] = json!(result.effective_max_depth);
        target["requested_max_depth"] = json!(result.requested_max_depth);
    }
    let warning = match (result.limit_hit, result.depth_capped) {
        (true, true) => format!(
            "Result truncated: hit row limit ({} rows) AND depth was capped to {} (requested {}). Run with a more specific symbol or smaller depth, or call get_ast_node(node_id) on a leaf to expand further.",
            crate::graph::query::CALL_GRAPH_ROW_LIMIT,
            result.effective_max_depth,
            result.requested_max_depth,
        ),
        (true, false) => format!(
            "Result truncated: hit row limit ({} rows) — more callers/callees may exist. Use a more specific symbol_name+file_path or get_ast_node on a leaf node_id to drill down.",
            crate::graph::query::CALL_GRAPH_ROW_LIMIT,
        ),
        (false, true) => format!(
            "Depth was capped to {} (requested {}). Deeper chains may exist; pick a leaf node_id and re-query from there.",
            result.effective_max_depth,
            result.requested_max_depth,
        ),
        (false, false) => unreachable!(),
    };
    target["truncation_warning"] = json!(warning);
}

/// Disclose the by-name fan-out hidden by the confidence floor, instead of
/// silently dropping it: when the seed had `ambiguous` direct edges below the
/// requested `min_confidence`, add a count + how to reveal them. Silent when
/// none were suppressed (clean symbol, or `min_confidence:"ambiguous"`).
pub(super) fn attach_suppressed_ambiguous(
    target: &mut serde_json::Value,
    results: &crate::graph::query::CallGraphResult,
) {
    if results.suppressed_ambiguous == 0 {
        return;
    }
    target["ambiguous_edges_hidden"] = json!(results.suppressed_ambiguous);
    target["ambiguous_hint"] = json!(format!(
        "{} low-confidence (ambiguous, by-name-collision) direct edge(s) hidden — the bare-name fan-out class (a method/function name shared by many defs, resolved to all of them). Re-query with min_confidence:\"ambiguous\" to include them.",
        results.suppressed_ambiguous
    ));
}

/// The node ids of the definitions a call graph was asked about (its roots).
fn seed_ids(results: &crate::graph::query::CallGraphResult) -> Vec<i64> {
    results
        .nodes
        .iter()
        .filter(|n| n.depth == 0)
        .map(|n| n.node_id)
        .collect()
}

impl McpServer {
    /// `boundaries` for an empty caller result (P1 #4): the dynamic-dispatch
    /// sites naming `symbol`, and its calls with no resolved target (D#229)
    /// listed nearest the `near` definitions; shared by `get_call_graph`,
    /// `find_references` and `get_ast_node include_impact`. `None` when it
    /// does not apply (no project root, not a function, not an identifier).
    pub(in crate::mcp::server) fn empty_result_boundaries(
        &self,
        symbol: &str,
        near: &[i64],
    ) -> Result<Option<serde_json::Value>> {
        let Some(root) = self.project_root.as_deref() else {
            return Ok(None);
        };
        Ok(
            crate::graph::boundaries::for_empty_result(self.db.conn(), root, symbol, near)?
                .map(|b| b.to_json()),
        )
    }

    pub(in crate::mcp::server) fn tool_get_call_graph(
        &self,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        // Route mode: when route_path is set, dispatch to HTTP-chain tracer.
        // Folds the former trace_http_chain tool into get_call_graph (v0.18.4).
        // Schema marks symbol_name and route_path mutually exclusive — enforce it
        // so a caller passing both doesn't silently get route-only behavior with
        // symbol_name dropped on the floor.
        // Treat empty/whitespace-only strings as absent — without this, empty
        // symbol_name falls through to fuzzy-resolve and silently matches a
        // random "Unique" candidate from a 1-symbol DB (saw it match `x` when
        // the only function was named x).
        fn nonblank(v: Option<&str>) -> Option<&str> {
            v.filter(|s| !s.trim().is_empty())
        }
        let has_route = nonblank(args.get("route_path").and_then(|v| v.as_str())).is_some();
        let has_symbol = nonblank(args.get("symbol_name").and_then(|v| v.as_str())).is_some()
            || nonblank(args.get("function_name").and_then(|v| v.as_str())).is_some();
        if has_route && has_symbol {
            return Err(anyhow!(
                "symbol_name and route_path are mutually exclusive — pass exactly one"
            ));
        }
        if has_route {
            return self.tool_trace_http_chain(args);
        }

        // Accept both "symbol_name" (canonical) and "function_name" (legacy alias)
        let function_name = nonblank(args["symbol_name"].as_str())
            .or_else(|| nonblank(args["function_name"].as_str()))
            .ok_or_else(|| anyhow!("symbol_name or route_path is required"))?;
        let direction_raw = args["direction"].as_str().unwrap_or("both");
        // Validate + case-normalize enum at tool entry. Without this, a bogus
        // direction first hit the ambiguity check (which echoes the bad value
        // back) — only after the user disambiguated with file_path would the
        // underlying graph layer reject it. Two errors for one mistake.
        // normalize_call_direction canonicalizes case so `direction:"Both"` is
        // accepted like the other enum filters.
        let direction =
            crate::domain::normalize_call_direction(direction_raw).ok_or_else(|| {
                anyhow!(
                    "direction must be one of: callers, callees, both (got '{}')",
                    direction_raw
                )
            })?;
        let depth = arg_clamped(args, "depth", "get_call_graph", 3)? as i32;
        // P1 #2: absent = the unbudgeted answer (flat list, or the rollup above
        // the threshold).
        let max_tokens = match &args["max_tokens"] {
            serde_json::Value::Null => None,
            _ => Some(arg_clamped(args, "max_tokens", "get_call_graph", 0)? as usize),
        };
        // Empty file_path is identical to absent — without this the
        // disambiguation/fuzzy path treats Some("") as "filter by this exact
        // path" and silently returns no edges. Separator-normalized at entry
        // (see `super::normalize_path_arg`): this value is both the freshness
        // target and the `get_call_graph_filtered` path filter, and a raw
        // `src\foo.rs` filter matches no indexed row.
        let file_path_arg = args["file_path"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|p| super::normalize_path_arg(p, self.project_root.as_deref()));
        let file_path = file_path_arg.as_deref();
        let compact = arg_bool(args, "compact", false)?;
        let include_tests = arg_bool(args, "include_tests", false)?;
        // Confidence floor (default 'inferred'): hide the ambiguous by-name
        // fan-out from the default response so Claude Code isn't fed phantom
        // call edges; min_confidence:"ambiguous" includes every edge. Validated
        // here so a bad value errors loudly rather than silently passing all.
        let min_conf_tier =
            crate::domain::parse_min_confidence(args["min_confidence"].as_str(), "min_confidence")?
                .unwrap_or(crate::domain::DEFAULT_RISK_CONF_FLOOR);
        let min_conf_rank = crate::domain::confidence_rank(min_conf_tier);

        if !should_skip_indexing(args)? {
            self.ensure_indexed()?;
            // Edit-aware: if the agent named a specific file, sync-refresh it
            // before answering so post-Edit queries don't see stale call edges.
            self.ensure_file_fresh_opt(file_path)?;
        }

        // Disambiguate: the symbol may match several distinct nodes (cross-file
        // OR same-file overloads), and a file_path cannot split same-file ones
        // either. Message + suggestion shape are shared with the CLI via
        // crate::resolve (audit #6).
        let ambiguous = match file_path {
            None => self.disambiguate_symbol(function_name)?,
            Some(fp) => {
                crate::resolve::detect_same_file_ambiguity(self.db.conn(), function_name, fp)?
            }
        };
        if let Some(cands) = ambiguous {
            return Ok(json!({
                "function": function_name,
                "direction": direction,
                "error": crate::resolve::ambiguity_message(function_name, &cands, crate::resolve::Surface::Mcp),
                // MUST stay capped in step with `ambiguity_message`, which
                // now says "Showing the first 5 of N" whenever it hides
                // something. This site renders the message but built its own
                // uncapped list, so SURF-34 turned a truthful envelope into
                // one that announced a cap it had not applied — 7 suggestions
                // under a note claiming 5. `ambiguity_response` (the other
                // four MCP sites) caps internally; this arm is hand-rolled
                // because it also carries `function`/`direction`.
                "suggestions": crate::resolve::candidates_to_json(&cands)
                    .into_iter()
                    .take(crate::resolve::SUGGESTION_CAP)
                    .collect::<Vec<_>>(),
            }));
        }

        let results = crate::graph::query::get_call_graph_filtered(
            self.db.conn(),
            function_name,
            direction,
            depth,
            file_path,
            min_conf_rank,
        )?;

        // If exact match returns empty (only seed node, no edges), try fuzzy name resolution
        let has_edges = results.nodes.iter().any(|n| n.depth > 0);
        let has_seed = results.nodes.iter().any(|n| n.depth == 0);
        // A file_path that holds no definition is a miss, not a filter that
        // legitimately matches nothing (D#253). A path the index does not
        // hold is refused here whatever the name, since no lookup can find
        // anything in it, and so is a name defined only in other files. A
        // name defined nowhere in an indexed file still gets the fuzzy step,
        // and its pick is checked below.
        if let (Some(fp), false) = (file_path, has_seed) {
            let miss = crate::resolve::file_selector_miss(self.db.conn(), function_name, fp)?;
            if !miss.defined_nowhere() {
                return Err(anyhow!(miss.message(function_name, fp)));
            }
        }
        if !(has_edges || (has_seed && file_path.is_some())) {
            match self.resolve_fuzzy_name(function_name)? {
                FuzzyResolution::Unique(resolved) => {
                    let results2 = crate::graph::query::get_call_graph_filtered(
                        self.db.conn(),
                        &resolved,
                        direction,
                        depth,
                        file_path,
                        min_conf_rank,
                    )?;
                    if let Some(fp) = file_path {
                        if !results2.nodes.iter().any(|n| n.depth == 0) {
                            let miss =
                                crate::resolve::file_selector_miss(self.db.conn(), &resolved, fp)?;
                            // Say which name was looked for: the caller typed
                            // another one (the CLI prints "Resolved 'X' → 'Y'").
                            return Err(anyhow!(
                                "No exact match for '{}'; it resolves to '{}'. {}",
                                function_name,
                                resolved,
                                miss.message(&resolved, fp)
                            ));
                        }
                    }
                    return self.format_call_graph_response(
                        &resolved,
                        direction,
                        &results2,
                        compact,
                        include_tests,
                        max_tokens,
                        args,
                    );
                }
                FuzzyResolution::Ambiguous(cands) => {
                    // Deliberately NOT `resolve::ambiguity_response`: this arm is
                    // the "no exact match, here are near misses" case, not "one
                    // exact name with several definitions", and it answers in the
                    // tool's own empty-result envelope so a caller's result parser
                    // still works. Only the candidate rendering is shared (audit
                    // §四/§六 — the other three sites now use `ambiguity_response`).
                    return Ok(json!({
                        "function": function_name,
                        "direction": direction,
                        "callees": [],
                        "callers": [],
                        // Capped and disclosed like the CLI's fuzzy arm
                        // (`cli::symbols::emit_fuzzy_ambiguity`). This list was
                        // unbounded while the CLI's was `take(5)`, so the same
                        // query answered with a different number of candidates
                        // depending on which surface asked — the "same input,
                        // two verdicts" shape SURF-17 and audit #6 both were.
                        "suggestion": format!(
                            "No exact match for '{}'. Did you mean one of these?{}",
                            function_name,
                            crate::resolve::suggestion_cap_note(cands.len()),
                        ),
                        "candidates": crate::resolve::candidates_to_json(&cands)
                            .into_iter()
                            .take(crate::resolve::SUGGESTION_CAP)
                            .collect::<Vec<_>>(),
                    }));
                }
                FuzzyResolution::NotFound => {
                    if !has_seed {
                        return Err(anyhow!("Symbol '{}' not found in the index. Use semantic_code_search to find the correct symbol name, or check spelling.", function_name));
                    }
                    // Function exists but has no callers/callees — fall through
                }
            }
        }

        self.format_call_graph_response(
            function_name,
            direction,
            &results,
            compact,
            include_tests,
            max_tokens,
            args,
        )
    }

    /// `max_tokens` and `args` (P1 #2): with a budget, the flat answer is fitted
    /// to it instead of switching to the rollup; `args` rebuilds the CLI
    /// command that returns what either leaves out.
    #[allow(clippy::too_many_arguments)] // the six query facts plus the budget and the raw args the next-step command is built from
    pub(in crate::mcp::server) fn format_call_graph_response(
        &self,
        function_name: &str,
        direction: &str,
        results: &crate::graph::query::CallGraphResult,
        compact: bool,
        include_tests: bool,
        max_tokens: Option<usize>,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let next = cli_next_command(
            "get_call_graph",
            args,
            &json!({ "function": function_name }),
        )
        .map(|c| c.to_string())
        .unwrap_or_default();
        // `compact` is inert beside a budget (reported in `ignored_arguments`).
        let compact = compact && max_tokens.is_none();
        // Authoritative AST flag first, name heuristic as fallback — so inline
        // `#[cfg(test)]` unit tests (descriptive names the heuristic misses) don't
        // leak into the default caller/callee view. Shared with impact/trace/show.
        let is_test = |n: &&crate::graph::query::CallGraphNode| {
            crate::domain::is_test_node(n.is_test, &n.name, &n.file_path)
        };
        let mut seen_nodes = std::collections::HashSet::new();
        let all_nodes: Vec<serde_json::Value> = results
            .nodes
            .iter()
            .filter(|n| n.depth > 0 && (include_tests || !is_test(n)))
            // Deduplicate cfg-gated functions (same name+file+depth+direction, different node_id)
            .filter(|n| seen_nodes.insert((&n.name, &n.file_path, n.depth, n.direction.as_str())))
            .map(|n| {
                if compact {
                    // Compact: keep node_id for chaining to get_ast_node, drop type (usually "function")
                    json!({
                        "node_id": n.node_id,
                        "name": n.name,
                        "file_path": n.file_path,
                        "depth": n.depth,
                        "direction": n.direction.as_str(),
                    })
                } else {
                    json!({
                        "node_id": n.node_id,
                        "name": n.name,
                        "type": n.node_type,
                        "file_path": n.file_path,
                        "depth": n.depth,
                        "direction": n.direction.as_str(),
                    })
                }
            })
            .collect();
        // Counted PER DIRECTION. One bucket named `test_callers_filtered` was
        // reporting hidden callees too, so an agent reading it concluded a test
        // caller existed. The CLI twin was split in 0.136.0 for the same reason;
        // this half was missed (pre-ship review 2026-09-06). `test_callers_filtered`
        // keeps its exact meaning and `test_callees_filtered` is additive beside
        // it, rather than a rename that would break readers of the old field.
        let (test_callers_count, test_callees_count) = if include_tests {
            (0usize, 0usize)
        } else {
            let mut callers = 0usize;
            let mut callees = 0usize;
            for n in results.nodes.iter().filter(|n| n.depth > 0 && is_test(n)) {
                if matches!(n.direction, crate::graph::query::Direction::Callers) {
                    callers += 1;
                } else {
                    callees += 1;
                }
            }
            (callers, callees)
        };

        // BOTH response shapes owe this disclosure. The rollup branch below builds
        // its payload from scratch, so attaching the counts only on the flat path
        // meant that above the compression threshold — a dense graph, exactly where
        // one hidden test caller is hardest to notice — the agent was told nothing
        // had been hidden (pre-ship review 2026-09-06, filed for this release).
        let attach_test_filtered = |v: &mut serde_json::Value| {
            if test_callers_count > 0 {
                v["test_callers_filtered"] = json!(test_callers_count);
            }
            if test_callees_count > 0 {
                v["test_callees_filtered"] = json!(test_callees_count);
            }
        };

        let est_tokens = crate::sandbox::compressor::estimate_json_tokens(&json!(all_nodes));
        if max_tokens.is_none() && est_tokens > COMPRESSION_TOKEN_THRESHOLD {
            // File-level rollup: group by (file_path, direction), emit counts + a small
            // sample of names/node_ids + depth range. Previously this path returned
            // `mode: compressed_call_graph` with the raw flat list (which still ate
            // tokens). The rollup collapses dense fanouts (e.g. 12 handlers in one
            // tools.rs file → one line with count=12 + first-10 node_ids), while
            // preserving the node_ids needed for `get_ast_node` drill-down.
            use std::collections::BTreeMap;
            const SAMPLE_LIMIT: usize = 10;

            struct Rollup {
                names: Vec<String>,
                node_ids: Vec<i64>,
                min_depth: i64,
                max_depth: i64,
            }

            let mut groups: BTreeMap<(String, String), Rollup> = BTreeMap::new();
            for node in &all_nodes {
                let file = node["file_path"].as_str().unwrap_or("").to_string();
                let dir = node["direction"].as_str().unwrap_or("").to_string();
                let name = node["name"].as_str().unwrap_or("").to_string();
                let node_id = node["node_id"].as_i64().unwrap_or(0);
                let depth = node["depth"].as_i64().unwrap_or(0);
                let entry = groups.entry((file, dir)).or_insert(Rollup {
                    names: Vec::new(),
                    node_ids: Vec::new(),
                    min_depth: depth,
                    max_depth: depth,
                });
                entry.names.push(name);
                entry.node_ids.push(node_id);
                entry.min_depth = entry.min_depth.min(depth);
                entry.max_depth = entry.max_depth.max(depth);
            }

            let mut caller_entries: Vec<(usize, serde_json::Value)> = Vec::new();
            let mut callee_entries: Vec<(usize, serde_json::Value)> = Vec::new();
            let mut caller_total = 0usize;
            let mut callee_total = 0usize;

            for ((file, direction), rollup) in groups {
                let count = rollup.names.len();
                let truncated = count > SAMPLE_LIMIT;
                let names: Vec<String> = rollup.names.iter().take(SAMPLE_LIMIT).cloned().collect();
                let node_ids: Vec<i64> =
                    rollup.node_ids.iter().take(SAMPLE_LIMIT).copied().collect();
                let entry = json!({
                    "file": file,
                    "count": count,
                    "names": names,
                    "node_ids": node_ids,
                    "min_depth": rollup.min_depth,
                    "max_depth": rollup.max_depth,
                    "sample_truncated": truncated,
                });
                if direction == "callers" {
                    caller_total += count;
                    caller_entries.push((count, entry));
                } else {
                    callee_total += count;
                    callee_entries.push((count, entry));
                }
            }

            // Sort by count desc so the densest files appear first.
            caller_entries.sort_by_key(|e| std::cmp::Reverse(e.0));
            callee_entries.sort_by_key(|e| std::cmp::Reverse(e.0));
            let caller_rollups: Vec<serde_json::Value> =
                caller_entries.into_iter().map(|(_, v)| v).collect();
            let callee_rollups: Vec<serde_json::Value> =
                callee_entries.into_iter().map(|(_, v)| v).collect();

            let mut rollup = json!({
                "mode": "rollup_call_graph",
                "message": "Call graph is dense; returned as file-level rollup. Pick any node_id and call get_ast_node(node_id) to expand a specific symbol.",
                // Every node, by name and file, as the CLI tree (P1 #2).
                "next": next,
                "function": function_name,
                "direction": direction,
                "total_nodes": all_nodes.len(),
                "callers": {
                    "rollups": caller_rollups,
                    "total_count": caller_total,
                },
                "callees": {
                    "rollups": callee_rollups,
                    "total_count": callee_total,
                },
            });
            attach_test_filtered(&mut rollup);
            attach_truncation_flags(&mut rollup, results);
            attach_suppressed_ambiguous(&mut rollup, results);
            if direction != "callees" && caller_total == 0 {
                if let Some(b) = self.empty_result_boundaries(function_name, &seed_ids(results))? {
                    rollup["boundaries"] = b;
                }
            }
            return Ok(rollup);
        }

        let callee_nodes: Vec<&serde_json::Value> = all_nodes
            .iter()
            .filter(|n| n["direction"] == "callees")
            .collect();
        let caller_nodes: Vec<&serde_json::Value> = all_nodes
            .iter()
            .filter(|n| n["direction"] == "callers")
            .collect();

        let mut result = json!({
            "function": function_name,
            "direction": direction,
            "callees": callee_nodes,
            "callers": caller_nodes,
        });
        attach_test_filtered(&mut result);
        attach_truncation_flags(&mut result, results);
        attach_suppressed_ambiguous(&mut result, results);
        // Empty caller side: disclose dynamic-dispatch sites, never as edges.
        // Both response shapes owe it (the rollup arm above attaches the same).
        if direction != "callees" && caller_nodes.is_empty() {
            if let Some(b) = self.empty_result_boundaries(function_name, &seed_ids(results))? {
                result["boundaries"] = b;
            }
        }
        if let Some(tokens) = max_tokens {
            return self.call_graph_budgeted(result, &all_nodes, tokens, &next);
        }
        Ok(result)
    }

    /// `get_call_graph` with `max_tokens`: the flat answer fitted to the budget.
    ///
    /// One unit per node. Least important first: deepest, then fewest callers
    /// (call-edge in-degree), then latest listed. The shorter form drops
    /// `type`. `budget` counts what was shortened or left out and names the
    /// CLI tree that lists every node.
    fn call_graph_budgeted(
        &self,
        full: serde_json::Value,
        all_nodes: &[serde_json::Value],
        tokens: usize,
        next: &str,
    ) -> Result<serde_json::Value> {
        use crate::budget::{self, Level};
        use std::cmp::Reverse;
        let ids: Vec<i64> = all_nodes
            .iter()
            .filter_map(|n| n["node_id"].as_i64())
            .collect();
        let in_degree = budget::caller_counts(self.db.conn(), &ids)?;
        let n = all_nodes.len();
        let depth = |i: usize| all_nodes[i]["depth"].as_i64().unwrap_or(0);
        let order = budget::order_by_importance(n, |i| {
            let id = all_nodes[i]["node_id"].as_i64().unwrap_or(0);
            (
                Reverse(depth(i)),
                in_degree.get(&id).copied().unwrap_or(0),
                Reverse(i),
            )
        });
        let steps = budget::standard_steps(&order, |_| true);
        let render = |levels: &[Level]| -> serde_json::Value {
            let pick = |dir: &str| -> Vec<serde_json::Value> {
                all_nodes
                    .iter()
                    .enumerate()
                    .filter(|(i, v)| v["direction"] == dir && levels[*i] != Level::Dropped)
                    .map(|(i, v)| {
                        let mut v = v.clone();
                        if levels[i] == Level::Skeleton {
                            if let Some(o) = v.as_object_mut() {
                                o.remove("type");
                            }
                        }
                        v
                    })
                    .collect()
            };
            let mut out = full.clone();
            out["callers"] = json!(pick("callers"));
            out["callees"] = json!(pick("callees"));
            let dropped = |dir: &str| {
                (0..n)
                    .filter(|&i| all_nodes[i]["direction"] == dir && levels[i] == Level::Dropped)
                    .count()
            };
            let (dc, de) = (dropped("callers"), dropped("callees"));
            let short = levels.iter().filter(|l| **l == Level::Skeleton).count();
            if dc + de + short > 0 {
                let mut b = json!({ "max_tokens": tokens });
                let mut omitted = serde_json::Map::new();
                if dc > 0 {
                    omitted.insert("callers".into(), json!(dc));
                }
                if de > 0 {
                    omitted.insert("callees".into(), json!(de));
                }
                if !omitted.is_empty() {
                    b["omitted"] = serde_json::Value::Object(omitted);
                }
                if short > 0 {
                    b["nodes_without_type"] = json!(short);
                }
                b["next"] = json!(next);
                out["budget"] = b;
            }
            out
        };
        Ok(budget::fit(
            &vec![Level::Full; n],
            &steps,
            budget::budget_bytes(tokens),
            |levels| {
                let v = render(levels);
                let len = serde_json::to_string(&v).map(|s| s.len()).unwrap_or(0);
                (v, len)
            },
        )
        .output)
    }
}
