//! Advanced tools (folded into core 7 in next pass — kept here as an
//! intermediate stop so the split refactor is bisectable):
//!
//! - `trace_http_chain` → being absorbed by `get_call_graph` (`route_path` mode)
//! - `dependency_graph` → being absorbed by `module_overview` (`include_deps`)
//! - `find_similar_code` → being absorbed by `get_ast_node` (`include_similar`)
//! - `find_dead_code` → being absorbed by `ast_search` (`dead_code` filter) /
//!   `module_overview` (`include_dead`)
//!
//! Until that fold lands, these handlers stay reachable via raw JSON-RPC
//! `tools/call` and via CLI subcommands.

use super::super::*;
use super::callgraph::attach_truncation_flags;
use crate::domain::default_dead_code_ignores;

impl McpServer {
    pub(in crate::mcp::server) fn tool_trace_http_chain(
        &self,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        // Empty/whitespace-only acts like missing — otherwise the substring
        // route match treats "" as a wildcard and returns "no routes found"
        // when the project has no routes, which is indistinguishable from a
        // misspelled route.
        let route_path_raw = args["route_path"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow!("route_path is required (e.g. 'GET /api/users')"))?;
        let depth = args["depth"].as_i64().unwrap_or(3).clamp(1, 20) as i32;
        let include_middleware = args["include_middleware"].as_bool().unwrap_or(true);
        // Same default as `get_call_graph`'s symbol arm. This is the surviving
        // sibling of the `min_confidence` defect described just below: another
        // parameter the schema advertises that the route arm never read.
        let include_tests = args["include_tests"].as_bool().unwrap_or(false);
        // Confidence floor (default 'inferred'): hide the ambiguous by-name fan-out
        // from both the call chain and the downstream list, matching get_call_graph.
        // route_path mode of get_call_graph reaches here, where min_confidence was
        // advertised on the schema but previously dropped (this handler ran rank-0
        // show-all). Validated at entry (enum-validate-at-entry) so a bad value
        // errors before any index work.
        let min_conf_tier =
            crate::domain::parse_min_confidence(args["min_confidence"].as_str(), "min_confidence")?
                .unwrap_or(crate::domain::DEFAULT_RISK_CONF_FLOOR);
        let min_conf_rank = crate::domain::confidence_rank(min_conf_tier);

        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
        }

        let (method_filter, route_path) = parse_route_input(route_path_raw);

        use crate::domain::{REL_CALLS, REL_ROUTES_TO};
        let mut rows = queries::find_routes_by_path(self.db.conn(), route_path, REL_ROUTES_TO)?;
        filter_routes_by_method(&mut rows, &method_filter);

        // Batch-fetch downstream calls for all handlers in one query
        let downstream_map = if include_middleware {
            let node_ids: Vec<i64> = rows.iter().map(|rm| rm.node_id).collect();
            queries::get_edge_target_names_batch(
                self.db.conn(),
                &node_ids,
                REL_CALLS,
                min_conf_rank,
            )?
        } else {
            std::collections::HashMap::new()
        };

        let mut handlers: Vec<serde_json::Value> = Vec::new();
        let mut ambiguous_hidden: usize = 0;
        for rm in &rows {
            let mut handler = json!({
                "node_id": rm.node_id,
                "metadata": rm.metadata,
                "handler_name": rm.handler_name,
                "handler_type": rm.handler_type,
                "file_path": rm.file_path,
                "start_line": rm.start_line,
                "end_line": rm.end_line,
            });

            apply_inline_handler_metadata(&mut handler, rm.metadata.as_deref());

            if include_middleware {
                let downstream = downstream_map.get(&rm.node_id).cloned().unwrap_or_default();
                handler["downstream_calls"] = json!(downstream);
            }

            // Recursive call chain via call graph
            let chain = crate::graph::query::get_call_graph_filtered(
                self.db.conn(),
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
                .filter(|n| n.depth > 0) // exclude root (the handler itself)
                // `is_test_node`, the canonical predicate — the AST `is_test` flag
                // OR the name/path heuristic. This read the heuristic ALONE, so an
                // inline `#[cfg(test)]` helper with a descriptive name (the exact
                // case `CallGraphNode::is_test` was added to catch, per its own
                // field doc) appeared in a route's PRODUCTION call chain. CLI
                // `trace` has used `is_test_node` since v0.91.0; this was the
                // unmigrated sibling (2026-08-16 audit §四).
                //
                // `include_tests` likewise: the flag is already declared on
                // `get_call_graph`'s schema and already parsed by its symbol arm,
                // and the whole `args` object reaches here — the route arm simply
                // never read it, so the escape hatch the schema advertises did
                // nothing in route mode.
                .filter(|n| {
                    include_tests || !crate::domain::is_test_node(n.is_test, &n.name, &n.file_path)
                })
                .map(|n| {
                    json!({
                        "node_id": n.node_id,
                        "name": n.name,
                        "type": n.node_type,
                        "file_path": n.file_path,
                        "depth": n.depth,
                    })
                })
                .collect();
            handler["call_chain"] = json!(chain_nodes);
            if chain.limit_hit || chain.depth_capped {
                handler["call_chain_truncated"] = json!(true);
            }

            handlers.push(handler);
        }

        let mut result = json!({
            "route": route_path,
            "handlers": handlers,
        });
        if handlers.is_empty() {
            result["message"] = json!("No matching routes found. This may mean: (1) the project has no HTTP routes, (2) the route pattern didn't match, or (3) routes use a framework not yet supported. Try a broader pattern or use semantic_code_search to find route handlers.");
        }
        // Disclose the ambiguous by-name fan-out folded out of the chain/downstream
        // so the agent knows the trace may be wider and can re-query with
        // min_confidence:"ambiguous". Explicitly attached (not a struct field).
        if ambiguous_hidden > 0 {
            result["ambiguous_edges_hidden"] = json!(ambiguous_hidden);
        }

        // Compress if result exceeds token threshold
        let tokens = crate::sandbox::compressor::estimate_json_tokens(&result);
        if tokens > COMPRESSION_TOKEN_THRESHOLD {
            let compressed_handlers: Vec<serde_json::Value> = handlers
                .iter()
                .map(|h| {
                    json!({
                        "node_id": h["node_id"],
                        "handler_name": h["handler_name"],
                        "file_path": h["file_path"],
                        "start_line": h["start_line"],
                        "end_line": h["end_line"],
                        "chain_count": h["call_chain"].as_array().map_or(0, |a| a.len()),
                    })
                })
                .collect();
            let mut compressed = json!({
                "mode": "compressed_http_chain",
                "message": "HTTP chain exceeded token limit. Use get_ast_node(node_id) or get_call_graph(symbol_name) to expand.",
                "route": route_path,
                "results": compressed_handlers,
            });
            if ambiguous_hidden > 0 {
                compressed["ambiguous_edges_hidden"] = json!(ambiguous_hidden);
            }
            return Ok(compressed);
        }

        // Discourage attach_truncation_flags compile-warn for unused import in
        // case future edits drop the call_graph fanout above.
        let _ = attach_truncation_flags;

        Ok(result)
    }

    pub(in crate::mcp::server) fn tool_dependency_graph(
        &self,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        // Validate required + enum args at tool entry, before any index/freshness
        // work, so a missing file_path or bogus direction errors cleanly instead of
        // after ensure_indexed ran. feedback-enum-validate-at-entry.
        // Separator-normalized at entry (see `super::normalize_path_arg`) so the
        // freshness target and the `get_nodes_by_file_path` key below match.
        let file_path_owned = args["file_path"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .map(super::normalize_path_arg)
            .ok_or_else(|| anyhow!("file_path is required (relative to project root)"))?;
        let file_path = file_path_owned.as_str();
        let direction_raw = args
            .get("direction")
            .and_then(|v| v.as_str())
            .unwrap_or("both");
        let direction = crate::domain::normalize_dep_direction(direction_raw).ok_or_else(|| {
            anyhow!(
                "direction must be one of: outgoing, incoming, both (got '{}')",
                direction_raw
            )
        })?;

        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
            // Edit-aware: file_path is required for this tool — post-edit
            // staleness is the canonical failure mode here.
            self.ensure_file_fresh_opt(Some(file_path))?;
        }
        let depth = args
            .get("depth")
            .and_then(|v| v.as_i64())
            .unwrap_or(2)
            .clamp(1, 10) as i32;
        let compact = args["compact"].as_bool().unwrap_or(false);

        // Check if file exists in index
        let file_nodes = queries::get_nodes_by_file_path(self.db.conn(), file_path)?;
        if file_nodes.is_empty() {
            let hint = if file_path.ends_with('/') || !file_path.contains('.') {
                // Looks like a directory — suggest using module_overview instead
                let dir = if file_path.ends_with('/') {
                    file_path.to_string()
                } else {
                    format!("{}/", file_path)
                };
                format!(
                    "Path '{}' looks like a directory. Use module_overview(path=\"{}\") for directory-level analysis, or specify an exact file (e.g., '{}mod.rs')",
                    file_path, file_path, dir
                )
            } else {
                format!(
                    "File '{}' not found in index. Check path is relative to project root.",
                    file_path
                )
            };
            return Ok(json!({
                "file": file_path,
                "depends_on": [],
                "depended_by": [],
                "warning": hint,
                "summary": format!("File '{}' not found in index", file_path)
            }));
        }

        let deps = queries::get_import_tree(self.db.conn(), file_path, direction, depth)?;

        // Filter out cross-language false edges (e.g. Rust file "calling" a JS function
        // due to name-based resolution matching common names like `update`, `read`, etc.)
        // Also drop the synthetic `<external>` bucket — it's a container for unresolved
        // imports, not a real file dependency.
        let is_compatible_lang =
            |dep_path: &str| crate::utils::config::is_compatible_lang(file_path, dep_path);

        let outgoing: Vec<serde_json::Value> = deps
            .iter()
            .filter(|d| d.direction == "outgoing")
            .filter(|d| is_compatible_lang(&d.file_path))
            .map(|d| {
                let mut obj = json!({
                    "file": d.file_path,
                    "depth": d.depth,
                });
                // Only show symbols for direct dependencies (depth 1);
                // deeper entries have 0 direct edges from root which is misleading
                // Skip symbols in compact mode to save tokens
                if !compact && d.depth == 1 {
                    obj["symbols"] = json!(d.symbol_count);
                }
                obj
            })
            .collect();

        let incoming: Vec<serde_json::Value> = deps
            .iter()
            .filter(|d| d.direction == "incoming")
            .filter(|d| is_compatible_lang(&d.file_path))
            .map(|d| {
                let mut obj = json!({
                    "file": d.file_path,
                    "depth": d.depth,
                });
                if !compact && d.depth == 1 {
                    obj["symbols"] = json!(d.symbol_count);
                }
                obj
            })
            .collect();

        Ok(json!({
            "file": file_path,
            "depends_on": outgoing,
            "depended_by": incoming,
            "summary": format!("{} depends on {} file{}, {} file{} depend{} on it",
                file_path,
                outgoing.len(), if outgoing.len() == 1 { "" } else { "s" },
                incoming.len(), if incoming.len() == 1 { "" } else { "s" },
                if incoming.len() == 1 { "s" } else { "" })
        }))
    }

    pub(in crate::mcp::server) fn tool_find_similar_code(
        &self,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.try_lazy_load_model();
        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
        }

        // Accept node_id directly, or resolve from symbol_name. Treat empty
        // symbol_name as absent — without this, the error message echoes
        // "Symbol '' not found" which looks like a real lookup miss.
        let node_id = if let Some(id) = args["node_id"].as_i64() {
            id
        } else if let Some(name) = args["symbol_name"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
        {
            // Ambiguity FIRST, through the shared resolver. Taking the first row
            // meant `find_similar_code symbol_name:"new"` silently answered about
            // ONE arbitrary definition out of five while `get_call_graph` on the
            // same word reported the ambiguity — the CLI half of this pair had
            // the identical defect (2026-08-16 audit §四). `node_id` above is the
            // escape hatch the response points at.
            if let Some(cands) = crate::resolve::detect_ambiguity(self.db.conn(), name)? {
                return Ok(crate::resolve::ambiguity_response(name, &cands));
            }
            match queries::get_first_node_id_by_name(self.db.conn(), name)? {
                Some(id) => id,
                None => return Err(anyhow!("Symbol '{}' not found in index. Use semantic_code_search to find the correct symbol name, or check spelling.", name)),
            }
        } else {
            return Err(anyhow!("Either node_id or symbol_name is required. Provide symbol_name (e.g. \"my_function\") or node_id (from other tool results)."));
        };
        let top_k = args
            .get("top_k")
            .and_then(|v| v.as_i64())
            .unwrap_or(5)
            .clamp(1, 100);
        let max_distance = args
            .get("max_distance")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.8);

        // Check if embeddings are available
        if !self.db.vec_enabled() {
            return Err(anyhow!(
                "Embedding not available. Build with --features embed-model."
            ));
        }

        // Check if any embeddings exist at all
        let (embedded_count, total_nodes) = queries::count_nodes_with_vectors(self.db.conn())?;
        if embedded_count == 0 {
            return Err(anyhow!("No embeddings found ({} nodes indexed, 0 embedded). The embedding model may not be loaded — restart the MCP server with the embed-model feature enabled. Alternative: use semantic_code_search with a descriptive query to find similar code by text matching.", total_nodes));
        }

        // Get the node's embedding
        let embedding: Vec<f32> = {
            let bytes = queries::get_node_embedding(self.db.conn(), node_id)
                .map_err(|_| anyhow!("No embedding found for node_id {}. Node may not have been embedded yet ({}/{} nodes embedded).", node_id, embedded_count, total_nodes))?;
            bytemuck::cast_slice(&bytes).to_vec()
        };

        // Search for similar vectors. Fetch extra so max_distance filtering
        // doesn't silently starve `top_k` — we need enough candidates to know
        // whether the cutoff actually dropped results. Shared over-fetch policy with
        // the CLI `similar` twin (single source of truth, avoids CLI↔MCP drift).
        let fetch_count = crate::domain::similar_fetch_count(top_k);
        let results = queries::vector_search(self.db.conn(), &embedding, fetch_count)?;

        // Split raw (self excluded) from cutoff-filtered candidates so we can
        // report whether max_distance is hiding matches.
        let raw_non_self: Vec<(i64, f64)> = results
            .iter()
            .filter(|(id, _)| *id != node_id)
            .map(|(id, dist)| (*id, *dist))
            .collect();
        let candidates: Vec<(i64, f64)> = raw_non_self
            .iter()
            .filter(|(_, dist)| *dist <= max_distance)
            .copied()
            .collect();
        let cutoff_dropped = raw_non_self.len() - candidates.len();
        let candidate_ids: Vec<i64> = candidates.iter().map(|(id, _)| *id).collect();
        let nodes_with_files =
            queries::get_nodes_with_files_by_ids(self.db.conn(), &candidate_ids)?;
        let node_map: std::collections::HashMap<i64, &queries::NodeWithFile> =
            nodes_with_files.iter().map(|nf| (nf.node.id, nf)).collect();

        let similar: Vec<serde_json::Value> = candidates
            .iter()
            .filter_map(|(id, distance)| {
                let nf = node_map.get(id)?;
                if crate::domain::is_skippable_result(
                    &nf.node.node_type,
                    &nf.node.name,
                    &nf.file_path,
                ) {
                    return None;
                }
                let similarity = 1.0 / (1.0 + distance);
                Some(json!({
                    "node_id": nf.node.id,
                    "name": nf.node.name,
                    "type": nf.node.node_type,
                    "file_path": nf.file_path,
                    "start_line": nf.node.start_line,
                    "similarity": (similarity * 10000.0).round() / 10000.0,
                    "distance": (distance * 10000.0).round() / 10000.0,
                }))
            })
            .take(top_k as usize)
            .collect();

        let mut out = json!({
            "query_node_id": node_id,
            "results": similar,
            "count": similar.len(),
            "top_k": top_k,
            "max_distance": max_distance,
        });
        if (similar.len() as i64) < top_k && cutoff_dropped > 0 {
            out["cutoff_applied"] = json!(true);
            out["cutoff_dropped"] = json!(cutoff_dropped);
            out["hint"] = json!(format!(
                "Fewer results than top_k ({}): {} candidate(s) exceeded max_distance={}. Raise max_distance to widen the search.",
                top_k, cutoff_dropped, max_distance
            ));
        }
        Ok(out)
    }

    pub(in crate::mcp::server) fn tool_find_dead_code(
        &self,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        // Separator-normalized at entry (see `super::normalize_path_arg`): this
        // value becomes a LIKE prefix against `files.path`, which stores `/`. A
        // Windows client passing `src\parser` matched nothing and the tool
        // answered "No dead code found" — a false clean, the quietest possible
        // failure for a tool whose whole job is reporting absence.
        let path = args["path"].as_str().map(super::normalize_path_arg);
        let path = path.as_deref();
        let node_type = args["node_type"].as_str();
        // Validate node_type up-front: an unknown alias normalizes to an empty Vec
        // and find_dead_code falls through to a literal `n.type = :x` match that
        // returns zero rows — a false-clean empty result. Mirror tool_ast_search.
        crate::storage::queries::validate_dead_code_type_filter(node_type)?;
        let include_tests = args["include_tests"].as_bool().unwrap_or(false);
        let min_lines = args["min_lines"].as_u64().unwrap_or(3) as u32;
        let compact = args["compact"].as_bool().unwrap_or(true);

        // ignore_paths: prefix-match exclusions. When omitted, apply defaults for
        // shell-invoked entry points (plugin hooks / lifecycle scripts) that the
        // static AST call graph can't track. Pass an empty array to disable.
        // Separator-normalized for the same reason `path` above is, and it is the
        // sibling that was missed when `path` was fixed FIVE LINES UP: these are
        // matched with `starts_with` against `/`-stored file paths, so a Windows
        // client's `ignore_paths: ["src\\generated"]` excludes nothing and the
        // tool over-reports dead code. The entry-normalization drift guard reads
        // the literal `args["…"]` bracket form and only the keys `path` /
        // `file_path`, so it cannot see this site — `assert_ignore_paths_
        // normalized_in_source` in tests/hardening.rs covers it instead.
        let (ignore_prefixes, ignore_was_defaulted) = match args.get("ignore_paths") {
            Some(serde_json::Value::Array(arr)) => (
                arr.iter()
                    .filter_map(|v| v.as_str().map(super::normalize_path_arg))
                    .collect::<Vec<_>>(),
                false,
            ),
            _ => (default_dead_code_ignores(), true),
        };

        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
            // Edit-aware: this tool emits start_line/end_line, and it was the
            // ONE path-taking tool without the query-time refresh — post-Edit
            // dead-code line numbers went stale where every sibling resynced
            // (audit 2026-08-02 MED-6; its CLI twin already refreshes).
            self.ensure_file_fresh_opt(path)?;
        }

        let report = crate::storage::queries::dead_code_report(
            self.db.conn(),
            path,
            node_type,
            include_tests,
            min_lines,
            &ignore_prefixes,
        )?;

        if report.is_empty() {
            // A `path` that matches no indexed file is zero coverage, not a
            // clean bill of health — and this tool's entire output is an
            // assertion of ABSENCE, on the surface an LLM reads. The CLI twin
            // has probed and exited 1 since v0.91.0; the MCP half answered the
            // same input with `{"results": [], "summary": "No dead code found
            // …"}` (audit 2026-08-16 P1-22). Same callee both sides now, so the
            // two cannot drift again. Reported as a tool error (the shape
            // `find_references` / `get_ast_node` already use for "your filter
            // names nothing in the index"), because a `warning` key inside an
            // otherwise-clean report is exactly what gets skimmed past.
            if let Some(prefix) =
                crate::storage::queries::unindexed_path_prefix(self.db.conn(), path)
            {
                return Err(anyhow!(
                    "No indexed files under path '{}' — this is zero coverage, not a clean result. \
                     Check the path is relative to the project root (use module_overview or \
                     project_map to see indexed paths), or index it first.",
                    prefix
                ));
            }
            let mut summary = "No dead code found with the given filters.".to_string();
            if report.ignored_count > 0 {
                summary.push_str(&format!(
                    " ({} result(s) suppressed by ignore_paths; pass ignore_paths:[] to see them.)",
                    report.ignored_count
                ));
            } else if report.hidden_below_threshold > 0 {
                summary.push_str(&format!(
                    " ({} shorter symbol(s) are below the min_lines={min_lines} threshold; pass min_lines:1 to include them.)",
                    report.hidden_below_threshold
                ));
            }
            return Ok(json!({
                "results": [],
                "orphan_count": 0,
                "exported_unused_count": 0,
                "ignored_count": report.ignored_count,
                "ignore_paths_applied": ignore_prefixes,
                "ignore_paths_defaulted": ignore_was_defaulted,
                "summary": summary,
            }));
        }

        let mut orphan_items: Vec<serde_json::Value> = Vec::new();
        let mut exported_items: Vec<serde_json::Value> = Vec::new();
        for it in &report.items {
            let lines = it.end_line - it.start_line + 1;
            let mut item = json!({
                "name": it.name,
                "type": it.node_type,
                "file_path": it.file_path,
                "start_line": it.start_line,
                "end_line": it.end_line,
                "lines": lines,
                "category": if it.is_exported { "exported_unused" } else { "orphan" },
            });
            if !compact {
                item["code"] = json!(it.code_content);
            }
            if it.is_exported {
                exported_items.push(item);
            } else {
                orphan_items.push(item);
            }
        }
        let mut all_items = orphan_items.clone();
        all_items.extend(exported_items.iter().cloned());

        Ok(json!({
            "results": all_items,
            "orphan_count": report.orphan_count,
            "exported_unused_count": report.exported_count,
            "ignored_count": report.ignored_count,
            "ignore_paths_applied": ignore_prefixes,
            "ignore_paths_defaulted": ignore_was_defaulted,
            // "candidates" not "results": receiver-method calls and cross-file
            // const/type uses are not edge-tracked, so a flagged symbol may still
            // be used — the caller should verify before treating it as dead.
            "summary": if report.ignored_count > 0 {
                format!("Dead code: {} candidates ({} orphan, {} exported-unused); {} suppressed by ignore_paths (pass ignore_paths:[] to see them). Verify — receiver-method/cross-file uses aren't edge-tracked.",
                    all_items.len(), report.orphan_count, report.exported_count, report.ignored_count)
            } else {
                format!("Dead code: {} candidates ({} orphan, {} exported-unused). Verify — receiver-method/cross-file uses aren't edge-tracked.",
                    all_items.len(), report.orphan_count, report.exported_count)
            },
        }))
    }
}
