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

impl McpServer {
    pub(in crate::mcp::server) fn tool_get_call_graph(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
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
        let direction = args["direction"].as_str().unwrap_or("both");
        // Validate enum at tool entry. Without this, a bogus direction first hit
        // the ambiguity check (which echoes the bad value back) — only after the
        // user disambiguated with file_path would the underlying graph layer
        // reject it. Two errors for one mistake.
        if !matches!(direction, "callers" | "callees" | "both") {
            return Err(anyhow!(
                "direction must be one of: callers, callees, both (got '{}')",
                direction
            ));
        }
        let depth = args["depth"].as_i64().unwrap_or(3).clamp(1, 20) as i32;
        // Normalize file_path: resolve absolute paths under project root → relative.
        // Empty file_path is identical to absent.
        let file_path_normalized = args["file_path"].as_str()
            .filter(|s| !s.is_empty())
            .map(|fp| super::super::helpers::normalize_tool_path(fp, self.project_root.as_deref()))
            .transpose()?;
        let file_path = file_path_normalized.as_deref();
        let compact = args["compact"].as_bool().unwrap_or(false);
        let include_tests = args["include_tests"].as_bool().unwrap_or(false);

        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
            // Edit-aware: if the agent named a specific file, sync-refresh it
            // before answering so post-Edit queries don't see stale call edges.
            self.ensure_file_fresh_opt(file_path)?;
        }

        // Disambiguate: if no file_path provided, check if symbol matches multiple
        // distinct nodes (cross-file OR same-file overloads). Message + suggestion
        // shape are shared with the CLI via crate::resolve (audit #6).
        if file_path.is_none() {
            if let Some(cands) = self.disambiguate_symbol(function_name)? {
                return Ok(json!({
                    "function": function_name,
                    "direction": direction,
                    "error": crate::resolve::ambiguity_message(function_name, &cands, crate::resolve::Surface::Mcp),
                    "suggestions": crate::resolve::candidates_to_json(&cands),
                }));
            }
        }

        let results = crate::graph::query::get_call_graph(
            self.db.conn(), function_name, direction, depth, file_path,
        )?;

        // If exact match returns empty (only seed node, no edges), try fuzzy name resolution
        let has_edges = results.nodes.iter().any(|n| n.depth > 0);
        let has_seed = results.nodes.iter().any(|n| n.depth == 0);
        if !(has_edges || (has_seed && file_path.is_some())) {
            match self.resolve_fuzzy_name(function_name)? {
                FuzzyResolution::Unique(resolved) => {
                    let results2 = crate::graph::query::get_call_graph(
                        self.db.conn(), &resolved, direction, depth, file_path,
                    )?;
                    return self.format_call_graph_response(&resolved, direction, &results2, compact, include_tests);
                }
                FuzzyResolution::Ambiguous(suggestions) => {
                    return Ok(json!({
                        "function": function_name,
                        "direction": direction,
                        "callees": [],
                        "callers": [],
                        "suggestion": format!("No exact match for '{}'. Did you mean one of these?", function_name),
                        "candidates": suggestions,
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

        self.format_call_graph_response(function_name, direction, &results, compact, include_tests)
    }

    pub(in crate::mcp::server) fn format_call_graph_response(
        &self,
        function_name: &str,
        direction: &str,
        results: &crate::graph::query::CallGraphResult,
        compact: bool,
        include_tests: bool,
    ) -> Result<serde_json::Value> {
        let is_test = |n: &&crate::graph::query::CallGraphNode| {
            is_test_symbol(&n.name, &n.file_path)
        };
        let mut seen_nodes = std::collections::HashSet::new();
        let all_nodes: Vec<serde_json::Value> = results.nodes.iter()
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
        let test_callers_count = if include_tests {
            0
        } else {
            results.nodes.iter()
                .filter(|n| n.depth > 0 && is_test(n))
                .count()
        };

        let est_tokens = crate::sandbox::compressor::estimate_json_tokens(&json!(all_nodes));
        if est_tokens > COMPRESSION_TOKEN_THRESHOLD {
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
                    names: Vec::new(), node_ids: Vec::new(),
                    min_depth: depth, max_depth: depth,
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
                let node_ids: Vec<i64> = rollup.node_ids.iter().take(SAMPLE_LIMIT).copied().collect();
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
            let caller_rollups: Vec<serde_json::Value> = caller_entries.into_iter().map(|(_, v)| v).collect();
            let callee_rollups: Vec<serde_json::Value> = callee_entries.into_iter().map(|(_, v)| v).collect();

            let mut rollup = json!({
                "mode": "rollup_call_graph",
                "message": "Call graph is dense; returned as file-level rollup. Pick any node_id and call get_ast_node(node_id) to expand a specific symbol.",
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
            attach_truncation_flags(&mut rollup, results);
            return Ok(rollup);
        }

        let callee_nodes: Vec<&serde_json::Value> = all_nodes.iter()
            .filter(|n| n["direction"] == "callees")
            .collect();
        let caller_nodes: Vec<&serde_json::Value> = all_nodes.iter()
            .filter(|n| n["direction"] == "callers")
            .collect();

        let mut result = json!({
            "function": function_name,
            "direction": direction,
            "callees": callee_nodes,
            "callers": caller_nodes,
        });
        if test_callers_count > 0 {
            result["test_callers_filtered"] = json!(test_callers_count);
        }
        attach_truncation_flags(&mut result, results);
        Ok(result)
    }
}
