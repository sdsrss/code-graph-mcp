use super::*;

impl McpServer {
    pub(super) fn tool_semantic_search(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let query = required_str(args, "query")?;
        let top_k = args["top_k"].as_u64()
            .or_else(|| args["limit"].as_u64())
            .unwrap_or(20).clamp(1, 100) as i64;
        let language_filter = args["language"].as_str();
        let node_type_filter = args["node_type"].as_str();
        let compact = args["compact"].as_bool().unwrap_or(false);

        // Query quality factor: penalize vague/short queries so relevance scores
        // reflect actual match quality, not just relative rank position.
        let meaningful_tokens: Vec<&str> = query.split_whitespace()
            .filter(|w| {
                let has_alnum = w.chars().any(|c| c.is_alphanumeric());
                let char_count = w.chars().count();
                has_alnum && (char_count > 1 || w.chars().all(|c| c.is_uppercase()))
            })
            .collect();
        let query_quality = match meaningful_tokens.len() {
            0 => 0.3,
            1 if meaningful_tokens[0].len() <= 2 => 0.4,
            1 => 0.7,
            2 => 0.85,
            _ => 1.0,
        };

        // Lazy model loading: pick up model if downloaded in background
        self.try_lazy_load_model();

        // Ensure index is up to date (unless caller requested read-only mode)
        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
        }

        // FTS5 search (fetch extra to allow for filtering)
        // Use a floor of 20 so small top_k values still have enough candidates after filtering
        let fetch_count = (top_k * 4).max(20);
        let fts_result = queries::fts5_search(self.db.conn(), query, fetch_count)?;
        let fts_or_fallback = fts_result.or_fallback;

        // Convert to SearchResult for RRF, carrying raw BM25 scores for score blending
        let fts_search: Vec<crate::search::fusion::SearchResult> = fts_result.nodes.iter()
            .enumerate()
            .map(|(i, r)| crate::search::fusion::SearchResult {
                node_id: r.id,
                score: fts_result.bm25_scores.get(i).copied().unwrap_or(0.0),
            })
            .collect();

        // Vector search (if embedding model available and vec enabled)
        let model_guard = lock_or_recover(&self.embedding_model, "embedding_model");
        let vec_search: Vec<crate::search::fusion::SearchResult> =
            if let Some(ref model) = *model_guard {
                if self.db.vec_enabled() {
                    match model.embed(query) {
                        Ok(query_embedding) => {
                            queries::vector_search(self.db.conn(), &query_embedding, fetch_count)?
                                .iter()
                                .map(|(node_id, distance)| {
                                    // Convert distance to similarity: 1.0 - distance (L2-normalized vectors)
                                    crate::search::fusion::SearchResult { node_id: *node_id, score: 1.0 - distance }
                                })
                                .collect()
                        }
                        Err(_) => vec![],
                    }
                } else {
                    vec![]
                }
            } else {
                vec![]
            };
        drop(model_guard);

        // Track search source IDs for confidence scoring
        let fts_node_ids: std::collections::HashSet<i64> = fts_search.iter().map(|r| r.node_id).collect();
        let vec_node_ids: std::collections::HashSet<i64> = vec_search.iter().map(|r| r.node_id).collect();

        // RRF fusion (FTS + Vec when available, FTS-only otherwise)
        // k=30: sharper rank sensitivity than default 60 (top results matter more)
        // fts=1.0, vec=1.2: slightly favor vector similarity since FTS is now stronger
        // with name_tokens and type columns in v2 schema
        let fused = weighted_rrf_fusion(&fts_search, &vec_search, 30, fetch_count as usize, 1.0, 1.2);

        // Match confidence: penalize when search signals are weak
        let match_confidence = {
            let mut c = 1.0_f64;
            // FTS-empty penalty: no text match → results are purely vector similarity (often noise)
            if fts_search.is_empty() && !vec_search.is_empty() {
                c *= 0.35;
            } else if !fts_search.is_empty() {
                // OR-fallback penalty: AND mode failed → query terms don't co-occur (weaker match)
                if fts_or_fallback { c *= 0.6; }
                // FTS sparsity: fewer results relative to fetch_count → weaker text match
                let fts_ratio = fts_search.len() as f64 / fetch_count as f64;
                if fts_ratio < 0.1 { c *= 0.5; }
                else if fts_ratio < 0.25 { c *= 0.65; }
                else if fts_ratio < 0.5 { c *= 0.8; }
            }
            // Source intersection: when both sources available, low overlap → less confidence
            if !fts_search.is_empty() && !vec_search.is_empty() {
                let top_ids: Vec<i64> = fused.iter().take(top_k as usize).map(|r| r.node_id).collect();
                let in_both = top_ids.iter()
                    .filter(|id| fts_node_ids.contains(id) && vec_node_ids.contains(id))
                    .count();
                let ratio = in_both as f64 / top_ids.len().max(1) as f64;
                if ratio < 0.2 { c *= 0.75; }
            }
            c
        };

        // Batch-fetch all candidate nodes with file info (single query instead of N+1)
        let candidate_ids: Vec<i64> = fused.iter().map(|r| r.node_id).collect();
        let nodes_with_files = queries::get_nodes_with_files_by_ids(self.db.conn(), &candidate_ids)?;

        // Build a lookup by node_id preserving the fused ranking order
        let mut nwf_map: std::collections::HashMap<i64, &queries::NodeWithFile> =
            nodes_with_files.iter().map(|nwf| (nwf.node.id, nwf)).collect();

        // Phase 1: Collect all valid candidates with adjusted scores
        // Name match boost + size dampening counter BM25/vector bias toward large nodes
        struct Candidate<'a> {
            node: &'a queries::NodeResult,
            file_path: &'a str,
            adjusted_score: f64,
        }
        let max_rrf = fused.first().map(|f| f.score).unwrap_or(0.0);
        let query_terms_lower: Vec<String> = meaningful_tokens.iter()
            .map(|t| t.to_lowercase())
            .collect();
        let mut candidates: Vec<Candidate> = Vec::new();
        for r in &fused {
            if let Some(nwf) = nwf_map.remove(&r.node_id) {
                let node = &nwf.node;
                if node.node_type == "module" && node.name == "<module>" { continue; }
                if is_test_symbol(&node.name, &nwf.file_path) { continue; }
                if let Some(nt) = node_type_filter {
                    let normalized = normalize_type_filter_mcp(nt);
                    if !normalized.iter().any(|t| t == &node.node_type) { continue; }
                }
                if let Some(lang) = language_filter { if nwf.language.as_deref() != Some(lang) { continue; } }

                let base_score = if max_rrf > 0.0 {
                    (r.score / max_rrf * query_quality * match_confidence * 100.0).round() / 100.0
                } else { 0.0 };

                // Name match boost: symbols whose name contains query terms are more likely relevant
                let name_lower = node.name.to_lowercase();
                let name_match_count = query_terms_lower.iter()
                    .filter(|t| name_lower.contains(t.as_str()))
                    .count();
                let name_boost = (1.0 + name_match_count as f64 * 0.3).min(2.0);

                // Size dampening: counter BM25/vector bias toward very large nodes (>100 lines)
                let node_lines = (node.end_line.saturating_sub(node.start_line) + 1) as f64;
                let size_factor = if node_lines > 100.0 {
                    1.0 / (1.0 + (node_lines / 100.0).ln() * 0.4)
                } else {
                    1.0
                };

                let adjusted = (base_score * name_boost * size_factor * 100.0).round() / 100.0;
                candidates.push(Candidate { node, file_path: &nwf.file_path, adjusted_score: adjusted });
            }
        }

        // Phase 2: Re-rank by adjusted score (name relevance + size normalization)
        candidates.sort_by(|a, b| b.adjusted_score.total_cmp(&a.adjusted_score));
        candidates.truncate(top_k as usize);

        // Phase 3: Build results
        struct MatchedNode<'a> {
            node: &'a queries::NodeResult,
            file_path: &'a str,
        }
        let mut matched: Vec<MatchedNode> = Vec::new();
        let mut results = Vec::new();
        for c in &candidates {
            let node = c.node;
            let score = c.adjusted_score;

            if compact {
                results.push(json!({
                    "node_id": node.id,
                    "name": node.name,
                    "type": node.node_type,
                    "file_path": c.file_path,
                    "line": format!("{}-{}", node.start_line, node.end_line),
                    "signature": node.signature,
                    "relevance": score,
                }));
            } else {
                const MAX_SEARCH_CODE_LEN: usize = 500;
                let code = if node.code_content.len() > MAX_SEARCH_CODE_LEN {
                    let truncated = &node.code_content[..node.code_content[..MAX_SEARCH_CODE_LEN]
                        .rfind('\n').unwrap_or(MAX_SEARCH_CODE_LEN)];
                    format!("{}\n// ... truncated ({} lines total, use get_ast_node for full code)",
                        truncated, node.end_line - node.start_line + 1)
                } else {
                    node.code_content.clone()
                };
                results.push(json!({
                    "node_id": node.id,
                    "name": node.name,
                    "type": node.node_type,
                    "file_path": c.file_path,
                    "start_line": node.start_line,
                    "end_line": node.end_line,
                    "code_content": code,
                    "signature": node.signature,
                    "relevance": score,
                }));
            }
            matched.push(MatchedNode { node, file_path: c.file_path });
        }

        // Record search metrics (before potential compression return)
        lock_or_recover(&self.metrics, "metrics")
            .record_search(results.len(), query_quality, vec_search.is_empty());

        // Context Sandbox: compress only if results likely exceed token threshold
        // Skip compression when compact=true — compact results are already token-efficient
        // (~85% smaller than full results) and contain fields (relevance, signature)
        // that would be lost by compression.
        use crate::sandbox::compressor::CompressedOutput;
        let estimated_tokens: usize = if compact { 0 } else {
            matched.iter()
                .map(|m| {
                    let node = m.node;
                    node.context_string.as_ref().map_or_else(
                        || node.code_content.len() + node.name.len() + node.signature.as_ref().map_or(0, |s| s.len()),
                        |ctx| ctx.len(),
                    ) / crate::domain::CHARS_PER_TOKEN
                })
                .sum()
        };
        if estimated_tokens > COMPRESSION_TOKEN_THRESHOLD {
            // Build node_results and file_paths only when compression is needed
            let node_results: Vec<queries::NodeResult> = matched.iter().map(|m| {
                let node = m.node;
                queries::NodeResult {
                    id: node.id,
                    file_id: node.file_id,
                    node_type: node.node_type.clone(),
                    name: node.name.clone(),
                    qualified_name: node.qualified_name.clone(),
                    start_line: node.start_line,
                    end_line: node.end_line,
                    code_content: node.code_content.clone(),
                    signature: node.signature.clone(),
                    doc_comment: node.doc_comment.clone(),
                    context_string: node.context_string.clone(),
                    name_tokens: node.name_tokens.clone(),
                    return_type: node.return_type.clone(),
                    param_types: node.param_types.clone(),
                    is_test: node.is_test,
                }
            }).collect();
            let file_paths: Vec<String> = matched.iter().map(|m| m.file_path.to_string()).collect();
        if let Some(compressed) = crate::sandbox::compressor::compress_if_needed(&node_results, &file_paths, COMPRESSION_TOKEN_THRESHOLD)? {
            let (mode, compact) = match compressed {
                CompressedOutput::Nodes(nodes) => {
                    let items: Vec<serde_json::Value> = nodes.iter().map(|c| json!({
                        "node_id": c.node_id,
                        "file_path": c.file_path,
                        "summary": c.summary,
                    })).collect();
                    ("compressed_nodes", items)
                }
                CompressedOutput::Files(groups) => {
                    let items: Vec<serde_json::Value> = groups.iter().map(|g| json!({
                        "file_path": g.file_path,
                        "summary": g.summary,
                        "node_ids": g.node_ids,
                    })).collect();
                    ("compressed_files", items)
                }
                CompressedOutput::Directories(groups) => {
                    let items: Vec<serde_json::Value> = groups.iter().map(|g| json!({
                        "file_path": g.file_path,
                        "summary": g.summary,
                        "node_ids": g.node_ids,
                    })).collect();
                    ("compressed_directories", items)
                }
            };
            return Ok(json!({
                "mode": mode,
                "message": "Results exceeded token limit. Use get_ast_node(node_id) to expand individual symbols.",
                "results": compact
            }));
        }
        } // end estimated_tokens check

        if results.is_empty() {
            let has_non_ascii = !query.is_ascii();
            let hint = if has_non_ascii {
                "Try using English keywords — the search index is English-optimized. Also try broader terms or check spelling."
            } else {
                "Try broader terms, check spelling, or use different keywords. The index may need rebuilding if the codebase changed significantly."
            };
            return Ok(json!({
                "results": [],
                "message": "No matching symbols found.",
                "hint": hint
            }));
        }

        Ok(json!(results))
    }

    pub(super) fn tool_get_call_graph(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        // Accept both "symbol_name" (canonical) and "function_name" (legacy alias)
        let function_name = args["symbol_name"].as_str()
            .or_else(|| args["function_name"].as_str())
            .ok_or_else(|| anyhow!("symbol_name is required"))?;
        let direction = args["direction"].as_str().unwrap_or("both");
        let depth = args["depth"].as_i64().unwrap_or(3).clamp(1, 20) as i32;
        let file_path = args["file_path"].as_str();
        let compact = args["compact"].as_bool().unwrap_or(false);
        let include_tests = args["include_tests"].as_bool().unwrap_or(false);

        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
        }

        // Disambiguate: if no file_path provided, check if symbol matches multiple distinct nodes
        if file_path.is_none() {
            if let Some(suggestions) = self.disambiguate_symbol(function_name)? {
                return Ok(json!({
                    "function": function_name,
                    "direction": direction,
                    "error": format!("Ambiguous symbol '{}': {} matches in different files. Specify file_path to disambiguate.", function_name, suggestions.len()),
                    "suggestions": suggestions,
                }));
            }
        }

        let results = crate::graph::query::get_call_graph(
            self.db.conn(), function_name, direction, depth, file_path,
        )?;

        // If exact match returns empty (only seed node, no edges), try fuzzy name resolution
        let has_edges = results.iter().any(|n| n.depth > 0);
        let has_seed = results.iter().any(|n| n.depth == 0);
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

    pub(super) fn format_call_graph_response(
        &self,
        function_name: &str,
        direction: &str,
        results: &[crate::graph::query::CallGraphNode],
        compact: bool,
        include_tests: bool,
    ) -> Result<serde_json::Value> {
        let is_test = |n: &&crate::graph::query::CallGraphNode| {
            is_test_symbol(&n.name, &n.file_path)
        };
        let mut seen_nodes = std::collections::HashSet::new();
        let all_nodes: Vec<serde_json::Value> = results.iter()
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
            results.iter()
                .filter(|n| n.depth > 0 && is_test(n))
                .count()
        };

        let est_tokens = crate::sandbox::compressor::estimate_json_tokens(&json!(all_nodes));
        if est_tokens > COMPRESSION_TOKEN_THRESHOLD {
            return Ok(json!({
                "mode": "compressed_call_graph",
                "message": "Call graph exceeded token limit. Use get_ast_node(node_id) to expand individual nodes.",
                "function": function_name,
                "results": all_nodes,
            }));
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
        Ok(result)
    }

    // find_http_route merged into trace_http_chain — old name kept as alias in handle_tool()

    pub(super) fn tool_trace_http_chain(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let route_path_raw = args["route_path"].as_str()
            .ok_or_else(|| anyhow!("route_path is required"))?;
        let depth = args["depth"].as_i64().unwrap_or(3).clamp(1, 20) as i32;
        let include_middleware = args["include_middleware"].as_bool().unwrap_or(true);

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
            queries::get_edge_target_names_batch(self.db.conn(), &node_ids, REL_CALLS)?
        } else {
            std::collections::HashMap::new()
        };

        let mut handlers: Vec<serde_json::Value> = Vec::new();
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
                let downstream = downstream_map.get(&rm.node_id)
                    .cloned()
                    .unwrap_or_default();
                handler["downstream_calls"] = json!(downstream);
            }

            // Recursive call chain via call graph
            let chain = crate::graph::query::get_call_graph(
                self.db.conn(), &rm.handler_name, "callees", depth, Some(&rm.file_path),
            )?;
            let chain_nodes: Vec<serde_json::Value> = chain.iter()
                .filter(|n| n.depth > 0) // exclude root (the handler itself)
                .filter(|n| !is_test_symbol(&n.name, &n.file_path))
                .map(|n| json!({
                    "node_id": n.node_id,
                    "name": n.name,
                    "type": n.node_type,
                    "file_path": n.file_path,
                    "depth": n.depth,
                }))
                .collect();
            handler["call_chain"] = json!(chain_nodes);

            handlers.push(handler);
        }

        let mut result = json!({
            "route": route_path,
            "handlers": handlers,
        });
        if handlers.is_empty() {
            result["message"] = json!("No matching routes found. This may mean: (1) the project has no HTTP routes, (2) the route pattern didn't match, or (3) routes use a framework not yet supported. Try a broader pattern or use semantic_code_search to find route handlers.");
        }

        // Compress if result exceeds token threshold
        let tokens = crate::sandbox::compressor::estimate_json_tokens(&result);
        if tokens > COMPRESSION_TOKEN_THRESHOLD {
            let compressed_handlers: Vec<serde_json::Value> = handlers.iter().map(|h| {
                json!({
                    "node_id": h["node_id"],
                    "handler_name": h["handler_name"],
                    "file_path": h["file_path"],
                    "start_line": h["start_line"],
                    "end_line": h["end_line"],
                    "chain_count": h["call_chain"].as_array().map_or(0, |a| a.len()),
                })
            }).collect();
            return Ok(json!({
                "mode": "compressed_http_chain",
                "message": "HTTP chain exceeded token limit. Use get_ast_node(node_id) or get_call_graph(symbol_name) to expand.",
                "route": route_path,
                "results": compressed_handlers,
            }));
        }

        Ok(result)
    }

    pub(super) fn tool_get_ast_node(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
        }

        let include_refs = args["include_references"].as_bool().unwrap_or(false);
        let include_impact = args["include_impact"].as_bool().unwrap_or(false);
        let compact = args["compact"].as_bool().unwrap_or(false);

        // Support lookup by node_id or file_path+symbol_name
        if let Some(nid) = args["node_id"].as_i64() {
            // When called with node_id, default context_lines=3
            let ctx = args["context_lines"].as_i64().unwrap_or(3).clamp(0, 100) as usize;
            return self.ast_node_by_id(nid, include_refs, include_impact, ctx, compact);
        }

        let context_lines = args["context_lines"].as_i64().unwrap_or(0).clamp(0, 100) as usize;

        let symbol_name = args["symbol_name"].as_str();
        let file_path = args["file_path"].as_str();

        // If only symbol_name provided (no file_path), resolve by name lookup
        if let (Some(sym), None) = (symbol_name, file_path) {
            let candidates = queries::get_nodes_with_files_by_name(self.db.conn(), sym)?;
            let non_test: Vec<_> = candidates.iter()
                .filter(|nf| !is_test_symbol(&nf.node.name, &nf.file_path))
                .collect();
            return match non_test.len() {
                0 => Err(anyhow!("Symbol '{}' not found in index. Use semantic_code_search to find the correct symbol name, or check spelling.", sym)),
                1 => self.ast_node_by_id(non_test[0].node.id, include_refs, include_impact, context_lines, compact),
                _ => {
                    let suggestions: Vec<_> = non_test.iter().map(|nf| {
                        json!({
                            "name": nf.node.name,
                            "file_path": &nf.file_path,
                            "type": nf.node.node_type,
                            "node_id": nf.node.id,
                        })
                    }).collect();
                    Ok(json!({
                        "error": format!("Ambiguous symbol '{}': {} matches found. Specify file_path or use node_id.", sym, suggestions.len()),
                        "suggestions": suggestions,
                    }))
                }
            };
        }

        let file_path = file_path
            .ok_or_else(|| anyhow!("Either node_id, symbol_name, or file_path+symbol_name is required"))?;
        let symbol_name = symbol_name
            .ok_or_else(|| anyhow!("symbol_name is required when using file_path"))?;

        let nodes = queries::get_nodes_by_file_path(self.db.conn(), file_path)?;
        if nodes.is_empty() {
            return Err(anyhow!("File '{}' not found in index. Check that the path is relative to the project root and the file has been indexed.", file_path));
        }
        let node = nodes.iter().find(|n| n.name == symbol_name);

        match node {
            Some(n) => {
                let mut result = json!({
                    "node_id": n.id,
                    "name": n.name,
                    "type": n.node_type,
                    "file_path": file_path,
                    "start_line": n.start_line,
                    "end_line": n.end_line,
                    "signature": n.signature,
                    "qualified_name": n.qualified_name,
                });

                // Include source code: prefer context view, fall back to stored code_content
                if context_lines > 0 {
                    if let Some(code) = self.read_source_context(file_path, n.start_line, n.end_line, context_lines) {
                        result["code_content"] = json!(code);
                    } else {
                        result["code_content"] = json!(n.code_content);
                    }
                } else {
                    result["code_content"] = json!(n.code_content);
                }

                if include_refs {
                    use crate::domain::REL_CALLS as CALLS;
                    let callees = queries::get_edge_targets_with_files(self.db.conn(), n.id, CALLS)?;
                    let callers = queries::get_edge_sources_with_files(self.db.conn(), n.id, CALLS)?;
                    result["calls"] = json!(callees.into_iter().map(|(name, file)| json!({"name": name, "file": file})).collect::<Vec<_>>());
                    result["called_by"] = json!(callers.into_iter().map(|(name, file)| json!({"name": name, "file": file})).collect::<Vec<_>>());
                }

                if include_impact {
                    self.append_impact_summary(&mut result, &n.name, file_path)?;
                }

                // Compact mode: strip code_content and context_string to save tokens
                if compact {
                    if let Some(obj) = result.as_object_mut() {
                        obj.remove("code_content");
                        obj.remove("context_string");
                    }
                    return Ok(result);
                }

                // Compress if result exceeds token threshold: drop code_content but keep references/impact
                let tokens = crate::sandbox::compressor::estimate_json_tokens(&result);
                if tokens > COMPRESSION_TOKEN_THRESHOLD {
                    result.as_object_mut().map(|obj| obj.remove("code_content"));
                    result["mode"] = json!("compressed_node");
                    result["message"] = json!(format!(
                        "Code content omitted ({} lines, ~{} tokens). Use Read tool on {}:{}-{} to view source.",
                        n.end_line.saturating_sub(n.start_line) + 1, tokens, file_path, n.start_line, n.end_line
                    ));
                    result["summary"] = json!(format!("{} {} in {} (lines {}-{}){}",
                        n.node_type, n.name, file_path, n.start_line, n.end_line,
                        n.signature.as_ref().map(|s| format!(" {}", s)).unwrap_or_default()));
                    return Ok(result);
                }

                Ok(result)
            }
            None => {
                // List available symbols to help the user
                let available: Vec<String> = nodes.iter()
                    .filter(|n| n.name != "<module>")
                    .take(10)
                    .map(|n| format!("{} ({})", n.name, n.node_type))
                    .collect();
                let hint = if available.is_empty() {
                    String::new()
                } else {
                    format!(". Available symbols: {}", available.join(", "))
                };
                Err(anyhow!("Symbol '{}' not found in '{}'{}", symbol_name, file_path, hint))
            }
        }
    }

    /// Lookup AST node by node_id.
    pub(super) fn ast_node_by_id(&self, node_id: i64, include_refs: bool, include_impact: bool, context_lines: usize, compact: bool) -> Result<serde_json::Value> {
        let nf = queries::get_node_with_file_by_id(self.db.conn(), node_id)?
            .ok_or_else(|| anyhow!("Node {} not found", node_id))?;
        let node = nf.node;
        let file_path = nf.file_path;

        let mut result = json!({
            "node_id": node.id,
            "name": node.name,
            "type": node.node_type,
            "file_path": file_path,
            "start_line": node.start_line,
            "end_line": node.end_line,
            "signature": node.signature,
            "qualified_name": node.qualified_name,
        });

        // Skip code loading in compact mode — saves tokens
        if !compact {
            // Include source code: prefer context view when requested, fall back to stored code_content
            if context_lines > 0 {
                if let Some(code) = self.read_source_context(&file_path, node.start_line, node.end_line, context_lines) {
                    result["code_content"] = json!(code);
                } else {
                    result["code_content"] = json!(node.code_content);
                }
            } else {
                result["code_content"] = json!(node.code_content);
            }
        }

        if include_refs {
            use crate::domain::REL_CALLS as CALLS;
            let callees = queries::get_edge_targets_with_files(self.db.conn(), node.id, CALLS)?;
            let callers = queries::get_edge_sources_with_files(self.db.conn(), node.id, CALLS)?;
            result["calls"] = json!(callees.into_iter().map(|(name, file)| json!({"name": name, "file": file})).collect::<Vec<_>>());
            result["called_by"] = json!(callers.into_iter().map(|(name, file)| json!({"name": name, "file": file})).collect::<Vec<_>>());
        }

        if include_impact {
            self.append_impact_summary(&mut result, &node.name, &file_path)?;
        }

        Ok(result)
    }

    /// Append a lightweight impact summary to an existing result JSON.
    /// Reuses the impact_analysis query logic but returns a compact summary object.
    pub(super) fn append_impact_summary(&self, result: &mut serde_json::Value, symbol_name: &str, file_path: &str) -> Result<()> {
        let callers = queries::get_callers_with_route_info(
            self.db.conn(), symbol_name, Some(file_path), 3
        )?;
        let callers: Vec<_> = callers.into_iter().filter(|c| c.depth > 0).collect();
        let prod_callers: Vec<_> = callers.iter()
            .filter(|c| !is_test_symbol(&c.name, &c.file_path))
            .collect();
        let affected_files: std::collections::HashSet<&str> = prod_callers.iter()
            .map(|c| c.file_path.as_str()).collect();
        let affected_routes: usize = callers.iter()
            .filter(|c| c.route_info.is_some())
            .count();

        let test_callers_count = callers.len() - prod_callers.len();
        let risk = crate::domain::compute_risk_level(prod_callers.len(), affected_routes, false);

        let mut impact = json!({
            "risk_level": risk,
            "direct_callers": prod_callers.iter().filter(|c| c.depth == 1).count(),
            "transitive_callers": prod_callers.iter().filter(|c| c.depth > 1).count(),
            "affected_files": affected_files.len(),
            "affected_routes": affected_routes,
        });
        if test_callers_count > 0 {
            impact["test_callers_filtered"] = json!(test_callers_count);
        }
        result["impact"] = impact;
        Ok(())
    }

    /// Read source code with context lines from the project file system.
    /// Uses BufReader to avoid loading entire file into memory.
    pub(super) fn read_source_context(&self, file_path: &str, start_line: i64, end_line: i64, context_lines: usize) -> Option<String> {
        use std::io::BufRead;
        let root = self.project_root.as_ref()?;
        let abs_path = root.join(file_path);
        let canonical = abs_path.canonicalize().ok()?;
        let root_canonical = root.canonicalize().ok()?;
        if !canonical.starts_with(&root_canonical) {
            return None; // path traversal
        }
        let file = std::fs::File::open(&canonical).ok()?;
        let reader = std::io::BufReader::new(file);
        let start = (start_line as usize).saturating_sub(1 + context_lines);
        let end = (end_line as usize) + context_lines; // 0-indexed end line to collect through
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
        Some(collected.join("\n"))
    }

    // read_snippet is a legacy alias for get_ast_node in handle_tool()

    pub(super) fn tool_start_watch(&self) -> Result<serde_json::Value> {
        if !self.is_primary {
            return Ok(json!({
                "status": "secondary",
                "message": "This instance is in secondary (read-only) mode. File watching is handled by the primary instance."
            }));
        }
        let project_root = self.project_root.as_ref()
            .ok_or_else(|| anyhow!("No project root configured"))?;

        let mut watcher_guard = lock_or_recover(&self.watcher, "watcher");
        if watcher_guard.is_some() {
            return Ok(json!({
                "status": "already_watching",
                "message": "File watcher is already running"
            }));
        }

        let (tx, rx) = mpsc::channel();
        let fw = FileWatcher::start(project_root, tx)?;
        *watcher_guard = Some(WatcherState {
            _watcher: fw,
            receiver: rx,
        });

        Ok(json!({
            "status": "watching",
            "message": "File watcher started. Changes will be detected and indexed on next tool call."
        }))
    }

    pub(super) fn tool_stop_watch(&self) -> Result<serde_json::Value> {
        if !self.is_primary {
            return Ok(json!({
                "status": "secondary",
                "message": "This instance is in secondary (read-only) mode. File watching is handled by the primary instance."
            }));
        }
        let mut watcher_guard = lock_or_recover(&self.watcher, "watcher");
        if watcher_guard.is_none() {
            return Ok(json!({
                "status": "not_watching",
                "message": "File watcher was not running"
            }));
        }
        *watcher_guard = None; // Drops the FileWatcher, stopping it
        Ok(json!({
            "status": "stopped",
            "message": "File watcher stopped"
        }))
    }

    pub(super) fn tool_get_index_status(&self) -> Result<serde_json::Value> {
        let mut status = serde_json::to_value(
            queries::get_index_status(self.db.conn(), self.is_watching())?
        )?;

        // Add embedding status fields
        let model_available = lock_or_recover(&self.embedding_model, "embedding_model").is_some();
        let (vectors_done, vectors_total) = if self.db.vec_enabled() {
            queries::count_nodes_with_vectors(self.db.conn()).unwrap_or((0, 0))
        } else {
            (0, 0)
        };

        let embedding_status = if !model_available {
            "unavailable"
        } else if self.indexing.embedding_in_progress.load(Ordering::Acquire) {
            "in_progress"
        } else if vectors_done >= vectors_total && vectors_total > 0 {
            "complete"
        } else if vectors_done > 0 {
            "partial"
        } else {
            "pending"
        };

        if let Some(obj) = status.as_object_mut() {
            obj.insert("embedding_status".into(), json!(embedding_status));
            obj.insert("embedding_progress".into(), json!(format!("{}/{}", vectors_done, vectors_total)));
            obj.insert("model_available".into(), json!(model_available));
            let coverage_pct = if vectors_total > 0 {
                (vectors_done as f64 / vectors_total as f64 * 100.0).round() as i64
            } else {
                0
            };
            obj.insert("embedding_coverage_pct".into(), json!(coverage_pct));
            obj.insert("search_mode".into(), json!(if model_available && vectors_done > 0 {
                "hybrid"
            } else {
                "fts_only"
            }));

            // Add indexing observability stats (skipped files, truncations)
            let stats = lock_or_recover(&self.last_index_stats, "last_index_stats").clone();
            let skipped_total = stats.files_skipped_size + stats.files_skipped_parse
                + stats.files_skipped_read + stats.files_skipped_hash;
            if skipped_total > 0 {
                obj.insert("skipped_files".into(), json!({
                    "total": skipped_total,
                    "too_large": stats.files_skipped_size,
                    "parse_error": stats.files_skipped_parse,
                    "read_error": stats.files_skipped_read,
                    "hash_error": stats.files_skipped_hash,
                }));
            }
            if stats.files_skipped_language > 0 {
                obj.insert("files_skipped_unsupported_language".into(), json!(stats.files_skipped_language));
            }
            obj.insert("instance_mode".into(), json!(if self.is_primary { "primary" } else { "secondary" }));

            // Health and age fields (consistent with CLI health-check)
            let expected_schema = crate::storage::schema::SCHEMA_VERSION;
            let schema_ok = obj.get("schema_version")
                .and_then(|v| v.as_i64())
                .map(|v| v == expected_schema as i64)
                .unwrap_or(false);
            let has_data = obj.get("nodes_count")
                .and_then(|v| v.as_i64())
                .map(|v| v > 0)
                .unwrap_or(false);
            obj.insert("healthy".into(), json!(schema_ok && has_data));
            if let Some(ts) = obj.get("last_indexed_at").and_then(|v| v.as_i64()) {
                let elapsed = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64 - ts)
                    .unwrap_or(0);
                let age = if elapsed < 60 { format!("{}s ago", elapsed) }
                    else if elapsed < 3600 { format!("{}m ago", elapsed / 60) }
                    else if elapsed < 86400 { format!("{}h ago", elapsed / 3600) }
                    else { format!("{}d ago", elapsed / 86400) };
                obj.insert("index_age".into(), json!(age));
            }
        }

        Ok(status)
    }

    pub(super) fn tool_rebuild_index(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        if !self.is_primary {
            return Ok(json!({
                "status": "secondary",
                "message": "This instance is in secondary (read-only) mode. Rebuild must be done from the primary instance."
            }));
        }
        let confirm = args["confirm"].as_bool().unwrap_or(false);
        if !confirm {
            return Err(anyhow!("Must pass confirm: true to rebuild index"));
        }

        let project_root = self.project_root.as_ref()
            .ok_or_else(|| anyhow!("No project root configured"))?;

        // Wait for background embedding to finish before clearing data
        // to avoid race where embedding thread writes vectors for deleted nodes
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while self.indexing.embedding_in_progress.load(Ordering::Acquire) {
            if std::time::Instant::now() > deadline {
                return Err(anyhow!("Background embedding still in progress. Try again shortly."));
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        // Clear all data in a single transaction (CASCADE handles nodes→edges)
        {
            let tx = self.db.conn().unchecked_transaction()?;
            tx.execute("DELETE FROM files", [])?;
            tx.commit()?;
        }

        self.send_log("info", "Rebuilding index...");
        let progress_cb = |current: usize, total: usize| {
            self.send_progress("rebuild-index", current, total);
        };
        // Skip inline embedding, background thread handles it
        let result = run_full_index(&self.db, project_root, None, Some(&progress_cb))?;

        // Save indexing stats for observability
        *lock_or_recover(&self.last_index_stats, "last_index_stats") = result.stats.clone();

        // Reset indexed flag and invalidate caches
        *lock_or_recover(&self.indexed, "indexed") = true;
        *lock_or_recover(&self.cache.cached_project_map, "cached_pmap") = None;
        lock_or_recover(&self.cache.cached_module_overviews, "cached_movw").clear();

        self.spawn_background_embedding();

        Ok(json!({
            "status": "rebuilt",
            "files_indexed": result.files_indexed,
            "nodes_created": result.nodes_created,
            "edges_created": result.edges_created,
        }))
    }

    pub(super) fn tool_impact_analysis(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
        }

        let symbol_name = required_str(args, "symbol_name")?;
        let change_type = args.get("change_type")
            .and_then(|v| v.as_str())
            .unwrap_or("behavior");
        if !matches!(change_type, "signature" | "behavior" | "remove") {
            return Err(anyhow!("change_type must be one of: signature, behavior, remove"));
        }
        let depth = args.get("depth")
            .and_then(|v| v.as_i64())
            .unwrap_or(3)
            .clamp(1, 20) as i32;
        let file_path = args.get("file_path").and_then(|v| v.as_str());

        // Disambiguate: check if symbol matches multiple distinct nodes in different files
        if file_path.is_none() {
            if let Some(suggestions) = self.disambiguate_symbol(symbol_name)? {
                return Ok(json!({
                    "symbol": symbol_name,
                    "change_type": change_type,
                    "error": format!("Ambiguous symbol '{}': {} matches in different files. Cannot assess impact without disambiguation.", symbol_name, suggestions.len()),
                    "suggestions": suggestions,
                }));
            }
        }

        let mut resolved_name = symbol_name.to_string();
        let mut callers = queries::get_callers_with_route_info(
            self.db.conn(), symbol_name, file_path, depth
        )?;

        // Fuzzy fallback: if no callers found, try fuzzy name resolution
        if callers.is_empty() {
            match self.resolve_fuzzy_name(symbol_name)? {
                FuzzyResolution::Unique(resolved) => {
                    resolved_name = resolved;
                    callers = queries::get_callers_with_route_info(
                        self.db.conn(), &resolved_name, file_path, depth
                    )?;
                }
                FuzzyResolution::Ambiguous(suggestions) => {
                    return Ok(json!({
                        "symbol": symbol_name,
                        "change_type": change_type,
                        "direct_callers": [],
                        "transitive_callers": [],
                        "affected_routes": [],
                        "affected_files": 0,
                        "risk_level": "LOW",
                        "summary": format!("No exact match for '{}'. Did you mean one of these?", symbol_name),
                        "candidates": suggestions,
                    }));
                }
                FuzzyResolution::NotFound => {
                    return Err(anyhow!("Symbol '{}' not found in index. Cannot assess impact. Use semantic_code_search to find the correct symbol name.", symbol_name));
                }
            }
        }

        // Exclude root node (depth 0) — it's the queried symbol itself, not a caller
        let callers: Vec<_> = callers.into_iter().filter(|c| c.depth > 0).collect();

        // Separate production callers from test callers
        let is_test = |c: &&queries::CallerWithRouteInfo| {
            is_test_symbol(&c.name, &c.file_path)
        };
        let prod_callers: Vec<_> = callers.iter().filter(|c| !is_test(c)).collect();
        let test_callers: Vec<_> = callers.iter().filter(|c| is_test(c)).collect();

        let affected_files: std::collections::HashSet<&str> = prod_callers.iter()
            .map(|c| c.file_path.as_str()).collect();
        let affected_routes: Vec<serde_json::Value> = callers.iter()
            .filter_map(|c| {
                c.route_info.as_ref().and_then(|meta| serde_json::from_str(meta).ok())
            }).collect();

        // Risk based on production callers, not test callers
        let risk_level = crate::domain::compute_risk_level(
            prod_callers.len(), affected_routes.len(), change_type == "remove"
        );

        let direct: Vec<_> = prod_callers.iter().filter(|c| c.depth == 1).collect();
        let transitive: Vec<_> = prod_callers.iter().filter(|c| c.depth > 1).collect();

        // For non-function types (struct/class/enum), call graph may miss type-usage references
        let type_warning = if prod_callers.is_empty() {
            let nodes = queries::get_nodes_by_name(self.db.conn(), &resolved_name)?;
            let is_type = nodes.iter().any(|n| matches!(n.node_type.as_str(), "struct" | "class" | "enum" | "interface" | "type_alias"));
            if is_type {
                Some("Impact analysis tracks function call chains. This is a type definition — actual usage (field access, type annotations, instantiation) may be broader than shown. Use semantic_code_search to find all references.")
            } else {
                None
            }
        } else {
            None
        };

        let mut result = json!({
            "symbol": &resolved_name,
            "change_type": change_type,
            "direct_callers": direct.iter().map(|c| json!({
                "name": c.name, "file": c.file_path, "depth": c.depth
            })).collect::<Vec<_>>(),
            "transitive_callers": transitive.iter().map(|c| json!({
                "name": c.name, "file": c.file_path, "depth": c.depth
            })).collect::<Vec<_>>(),
            "affected_routes": affected_routes,
            "affected_files": affected_files.len(),
            "risk_level": risk_level,
            "tests_affected": test_callers.len(),
            "summary": format!("Changing {} affects {} routes, {} functions across {} files [{}] ({} tests affected)",
                &resolved_name, affected_routes.len(), prod_callers.len(), affected_files.len(), risk_level, test_callers.len())
        });
        if let Some(warning) = type_warning {
            result["warning"] = json!(warning);
        }
        Ok(result)
    }

    pub(super) fn tool_module_overview(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
        }

        let raw_path = args["path"].as_str()
            .ok_or_else(|| anyhow!("Missing path"))?;
        let compact = args["compact"].as_bool().unwrap_or(false);
        // Normalize: strip leading "./" and treat "." as empty prefix (match all)
        let path = raw_path.strip_prefix("./").unwrap_or(raw_path);
        let path = if path == "." { "" } else { path };

        // Return cached result if fresh (< 60s), evict if expired
        {
            let mut cache = lock_or_recover(&self.cache.cached_module_overviews, "cached_movw");
            if let Some((ts, _)) = cache.get(path) {
                if ts.elapsed().as_secs() < 60 {
                    let val = cache.get(path).unwrap().1.clone();
                    if compact {
                        return self.compact_module_overview(&val);
                    }
                    return Ok(val);
                } else {
                    cache.remove(path);
                }
            }
        }

        let exports = queries::get_module_exports(self.db.conn(), path)?;

        // Filter out test functions — they add noise to module overviews
        let exports: Vec<_> = exports.into_iter()
            .filter(|e| !is_test_symbol(&e.name, &e.file_path))
            .collect();

        // Get import/dependency info at file level
        let files: std::collections::HashSet<&str> = exports.iter()
            .map(|e| e.file_path.as_str()).collect();

        // Split exports into active (called by others) and inactive to save tokens.
        let (active, inactive): (Vec<_>, Vec<_>) = exports.iter()
            .partition(|e| e.caller_count > 0);

        let mut hot_candidates: Vec<_> = exports.iter()
            .filter(|e| e.caller_count > 0)
            .collect();
        hot_candidates.sort_by(|a, b| b.caller_count.cmp(&a.caller_count));
        let hot_paths: Vec<serde_json::Value> = hot_candidates.iter()
            .take(5)
            .map(|e| json!({
                "name": e.name,
                "type": e.node_type,
                "file": e.file_path,
                "caller_count": e.caller_count,
            }))
            .collect();

        // Active exports get full detail; inactive ones are summarized by type.
        const MAX_ACTIVE: usize = 30;
        let active_capped = active.len() > MAX_ACTIVE;
        let mut active_sorted = active.clone();
        active_sorted.sort_by(|a, b| b.caller_count.cmp(&a.caller_count));
        let active_exports: Vec<serde_json::Value> = active_sorted.iter()
            .take(MAX_ACTIVE)
            .map(|e| json!({
                "node_id": e.node_id,
                "name": e.name,
                "type": e.node_type,
                "file": e.file_path,
                "caller_count": e.caller_count,
                "signature": e.signature,
            }))
            .collect();

        // Compact summary for inactive symbols — just counts by type
        let mut inactive_by_type: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();
        for e in &inactive {
            inactive_by_type.entry(e.node_type.as_str()).or_default().push(e.name.as_str());
        }
        let inactive_summary: Vec<serde_json::Value> = inactive_by_type.iter()
            .map(|(typ, names)| {
                let display: Vec<&&str> = names.iter().take(8).collect();
                let mut obj = json!({
                    "type": typ,
                    "count": names.len(),
                    "names": display,
                });
                if names.len() > 8 {
                    obj["more"] = json!(names.len() - 8);
                }
                obj
            })
            .collect();

        let mut result = json!({
            "path": raw_path,
            "files_count": files.len(),
            "active_exports": active_exports,
            "inactive_summary": inactive_summary,
            "hot_paths": hot_paths,
            "summary": format!("Module '{}': {} active + {} inactive exports across {} files",
                raw_path, active.len(), inactive.len(), files.len())
        });
        if files.is_empty() {
            result["warning"] = json!(format!("No files found for path '{}'. Check that the path is relative to the project root.", raw_path));
        }
        if active_capped {
            result["active_capped"] = json!(true);
            result["showing"] = json!(MAX_ACTIVE);
            result["total_active"] = json!(active.len());
            result["hint"] = json!("Active exports capped. Use a more specific path to see all.");
        }

        // Cache the full result (max 10 entries to bound memory)
        {
            let mut cache = lock_or_recover(&self.cache.cached_module_overviews, "cached_movw");
            if cache.len() >= 10 {
                // Evict oldest entry
                if let Some(oldest_key) = cache.iter()
                    .min_by_key(|(_, (ts, _))| *ts)
                    .map(|(k, _)| k.to_string())
                {
                    cache.remove(&oldest_key);
                }
            }
            cache.insert(path.to_string(), (std::time::Instant::now(), result.clone()));
        }

        if compact {
            return self.compact_module_overview(&result);
        }
        Ok(result)
    }

    pub(super) fn compact_module_overview(&self, full: &serde_json::Value) -> Result<serde_json::Value> {
        // Compact: keep node_id for chaining, drop signature
        let active: Vec<serde_json::Value> = full["active_exports"].as_array()
            .map(|arr| arr.iter().map(|e| json!({
                "node_id": e["node_id"],
                "name": e["name"],
                "type": e["type"],
                "file": e["file"],
                "callers": e["caller_count"],
            })).collect())
            .unwrap_or_default();

        let inactive_count: usize = full["inactive_summary"].as_array()
            .map(|arr| arr.iter()
                .filter_map(|s| s["count"].as_u64())
                .sum::<u64>() as usize)
            .unwrap_or(0);

        let mut result = json!({
            "path": full["path"],
            "files": full["files_count"],
            "active": active,
            "inactive_count": inactive_count,
            "hot_paths": full["hot_paths"],
            "summary": full["summary"],
        });
        if full.get("warning").is_some() {
            result["warning"] = full["warning"].clone();
        }
        Ok(result)
    }

    pub(super) fn tool_dependency_graph(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
        }

        let file_path = args["file_path"].as_str()
            .ok_or_else(|| anyhow!("Missing file_path"))?;
        let direction = args.get("direction")
            .and_then(|v| v.as_str())
            .unwrap_or("both");
        let depth = args.get("depth")
            .and_then(|v| v.as_i64())
            .unwrap_or(2)
            .clamp(1, 10) as i32;
        let compact = args["compact"].as_bool().unwrap_or(false);

        // Check if file exists in index
        let file_nodes = queries::get_nodes_by_file_path(self.db.conn(), file_path)?;
        if file_nodes.is_empty() {
            let hint = if file_path.ends_with('/') || !file_path.contains('.') {
                // Looks like a directory — suggest using module_overview instead
                let dir = if file_path.ends_with('/') { file_path.to_string() } else { format!("{}/", file_path) };
                format!(
                    "Path '{}' looks like a directory. Use module_overview(path=\"{}\") for directory-level analysis, or specify an exact file (e.g., '{}mod.rs')",
                    file_path, file_path, dir
                )
            } else {
                format!("File '{}' not found in index. Check path is relative to project root.", file_path)
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
        let root_lang = crate::utils::config::detect_language(file_path);
        let is_compatible_lang = |dep_path: &str| -> bool {
            let dep_lang = crate::utils::config::detect_language(dep_path);
            match (root_lang, dep_lang) {
                (None, _) | (_, None) => true, // unknown language → keep
                (Some(a), Some(b)) if a == b => true,
                // JS/TS family can cross-reference
                (Some(a), Some(b)) if matches!((a, b),
                    ("javascript" | "typescript" | "tsx", "javascript" | "typescript" | "tsx")
                ) => true,
                // C/C++ family can cross-reference
                (Some(a), Some(b)) if matches!((a, b),
                    ("c" | "cpp", "c" | "cpp")
                ) => true,
                _ => false,
            }
        };

        let outgoing: Vec<serde_json::Value> = deps.iter()
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

        let incoming: Vec<serde_json::Value> = deps.iter()
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

    pub(super) fn tool_find_similar_code(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        self.try_lazy_load_model();
        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
        }

        // Accept node_id directly, or resolve from symbol_name
        let node_id = if let Some(id) = args["node_id"].as_i64() {
            id
        } else if let Some(name) = args["symbol_name"].as_str() {
            match queries::get_first_node_id_by_name(self.db.conn(), name)? {
                Some(id) => id,
                None => return Err(anyhow!("Symbol '{}' not found in index. Use semantic_code_search to find the correct symbol name, or check spelling.", name)),
            }
        } else {
            return Err(anyhow!("Either node_id or symbol_name is required. Provide symbol_name (e.g. \"my_function\") or node_id (from other tool results)."));
        };
        let top_k = args.get("top_k")
            .and_then(|v| v.as_i64())
            .unwrap_or(5)
            .clamp(1, 100);
        let max_distance = args.get("max_distance")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.8);

        // Check if embeddings are available
        if !self.db.vec_enabled() {
            return Err(anyhow!("Embedding not available. Build with --features embed-model."));
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

        // Search for similar vectors
        let results = queries::vector_search(self.db.conn(), &embedding, top_k + 1)?; // +1 to exclude self

        // Pre-filter by distance and self, then batch-fetch nodes with file paths
        let candidates: Vec<(i64, f64)> = results.iter()
            .filter(|(id, dist)| *id != node_id && *dist <= max_distance)
            .map(|(id, dist)| (*id, *dist))
            .collect();
        let candidate_ids: Vec<i64> = candidates.iter().map(|(id, _)| *id).collect();
        let nodes_with_files = queries::get_nodes_with_files_by_ids(self.db.conn(), &candidate_ids)?;
        let node_map: std::collections::HashMap<i64, &queries::NodeWithFile> =
            nodes_with_files.iter().map(|nf| (nf.node.id, nf)).collect();

        let similar: Vec<serde_json::Value> = candidates.iter()
            .filter_map(|(id, distance)| {
                let nf = node_map.get(id)?;
                if nf.node.node_type == "module" && nf.node.name == "<module>" {
                    return None;
                }
                if is_test_symbol(&nf.node.name, &nf.file_path) {
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

        Ok(json!({
            "query_node_id": node_id,
            "results": similar,
            "count": similar.len(),
        }))
    }

    pub(super) fn tool_project_map(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
        }
        let compact = args["compact"].as_bool().unwrap_or(false);

        // Return cached result if fresh (< 60s) — project_map is expensive and rarely changes mid-session
        // Note: cache stores full result; compact is derived from it on the fly
        let full_result = {
            let cache = lock_or_recover(&self.cache.cached_project_map, "cached_pmap");
            if let Some((ts, ref val)) = *cache {
                if ts.elapsed().as_secs() < 60 {
                    Some(val.clone())
                } else {
                    None
                }
            } else {
                None
            }
        };

        let result = if let Some(cached) = full_result {
            cached
        } else {
            let (modules, deps, entry_points, hot_functions) = queries::get_project_map(self.db.conn())?;

            let modules_json: Vec<serde_json::Value> = modules.iter().map(|m| {
                let mut obj = json!({
                    "path": m.path,
                    "files": m.files,
                    "functions": m.functions,
                    "classes": m.classes,
                });
                if m.interfaces_traits > 0 {
                    obj["interfaces_traits"] = json!(m.interfaces_traits);
                }
                if !m.languages.is_empty() {
                    obj["languages"] = json!(m.languages);
                }
                if !m.key_symbols.is_empty() {
                    obj["key_symbols"] = json!(m.key_symbols);
                }
                obj
            }).collect();

            let deps_json: Vec<serde_json::Value> = deps.iter().map(|d| {
                json!({
                    "from": d.from,
                    "to": d.to,
                    "imports": d.import_count,
                })
            }).collect();

            let routes_json: Vec<serde_json::Value> = entry_points.iter().map(|e| {
                json!({
                    "route": e.route,
                    "handler": e.handler,
                    "file": e.file,
                })
            }).collect();

            let hot_json: Vec<serde_json::Value> = hot_functions.iter().map(|h| {
                json!({
                    "name": h.name,
                    "type": h.node_type,
                    "file": h.file,
                    "caller_count": h.caller_count,
                })
            }).collect();

            let r = json!({
                "modules": modules_json,
                "module_dependencies": deps_json,
                "entry_points": routes_json,
                "hot_functions": hot_json,
            });

            // Cache the full result
            *lock_or_recover(&self.cache.cached_project_map, "cached_pmap") =
                Some((std::time::Instant::now(), r.clone()));

            r
        };

        if compact {
            // Compact mode: drop languages/classes/interfaces, keep key_symbols for discoverability
            let compact_modules: Vec<serde_json::Value> = result["modules"].as_array()
                .map(|arr| arr.iter().map(|m| {
                    let mut obj = json!({
                        "path": m["path"],
                        "files": m["files"],
                        "functions": m["functions"],
                    });
                    // Preserve key_symbols — essential for deciding what to explore next
                    if let Some(ks) = m.get("key_symbols") {
                        if ks.is_array() && !ks.as_array().unwrap().is_empty() {
                            obj["key_symbols"] = ks.clone();
                        }
                    }
                    obj
                }).collect())
                .unwrap_or_default();

            let compact_deps: Vec<serde_json::Value> = result["module_dependencies"].as_array()
                .map(|arr| arr.iter().map(|d| json!({
                    "from": d["from"],
                    "to": d["to"],
                })).collect())
                .unwrap_or_default();

            // Trim hot_functions: top 10, name+file only
            let compact_hot: Vec<serde_json::Value> = result["hot_functions"].as_array()
                .map(|arr| arr.iter().take(10).map(|h| json!({
                    "name": h["name"],
                    "file": h["file"],
                    "caller_count": h["caller_count"],
                })).collect())
                .unwrap_or_default();

            // Trim entry_points: file+handler only
            let compact_entries: Vec<serde_json::Value> = result["entry_points"].as_array()
                .map(|arr| arr.iter().map(|e| json!({
                    "file": e["file"],
                    "handler": e["handler"],
                })).collect())
                .unwrap_or_default();

            return Ok(json!({
                "modules": compact_modules,
                "module_dependencies": compact_deps,
                "entry_points": compact_entries,
                "hot_functions": compact_hot,
            }));
        }

        Ok(result)
    }

    pub(super) fn tool_ast_search(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
        }

        let query = args["query"].as_str().map(|s| s.trim()).filter(|s| !s.is_empty());
        let type_filter = args["type"].as_str();
        let returns_filter = args["returns"].as_str();
        let params_filter = args["params"].as_str();
        let limit = args["limit"].as_u64().unwrap_or(20).clamp(1, 100) as usize;

        let has_filters = type_filter.is_some() || returns_filter.is_some() || params_filter.is_some();
        if query.is_none() && !has_filters {
            return Err(anyhow!("Either query or at least one filter (type, returns, params) is required."));
        }

        let results: Vec<queries::NodeWithFile> = if let Some(q) = query {
            // FTS5 search + filter in Rust
            let fts_result = queries::fts5_search(self.db.conn(), q, (limit * 4) as i64)?;
            if fts_result.nodes.is_empty() {
                return Ok(json!({ "results": [], "message": "No results found." }));
            }
            let node_ids: Vec<i64> = fts_result.nodes.iter().map(|n| n.id).collect();
            let all = queries::get_nodes_with_files_by_ids(self.db.conn(), &node_ids)?;

            // Preserve FTS5 rank order
            let id_order: std::collections::HashMap<i64, usize> = node_ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
            let mut sorted = all;
            sorted.sort_by_key(|nwf| id_order.get(&nwf.node.id).copied().unwrap_or(usize::MAX));

            sorted.into_iter()
                .filter(|nwf| {
                    let n = &nwf.node;
                    if let Some(tf) = type_filter {
                        let types = normalize_type_filter_mcp(tf);
                        if !types.contains(&n.node_type) {
                            return false;
                        }
                    }
                    if let Some(rf) = returns_filter {
                        match &n.return_type {
                            Some(rt) => if !rt.to_lowercase().contains(&rf.to_lowercase()) { return false; },
                            None => return false,
                        }
                    }
                    if let Some(pf) = params_filter {
                        match &n.param_types {
                            Some(pt) => if !pt.to_lowercase().contains(&pf.to_lowercase()) { return false; },
                            None => return false,
                        }
                    }
                    true
                })
                .take(limit)
                .collect()
        } else {
            // Filter-only: direct SQL
            let normalized = type_filter.map(normalize_type_filter_mcp);
            let type_refs: Option<Vec<&str>> = normalized.as_ref()
                .map(|v| v.iter().map(|s| s.as_str()).collect());
            queries::get_nodes_with_files_by_filters(
                self.db.conn(),
                type_refs.as_deref(),
                returns_filter, params_filter, limit,
            )?
        };

        let items: Vec<serde_json::Value> = results.iter().map(|nwf| {
            let n = &nwf.node;
            json!({
                "node_id": n.id,
                "name": n.qualified_name.as_deref().unwrap_or(&n.name),
                "type": n.node_type,
                "file_path": nwf.file_path,
                "start_line": n.start_line,
                "end_line": n.end_line,
                "signature": n.signature,
                "return_type": n.return_type,
                "param_types": n.param_types,
            })
        }).collect();

        Ok(json!({
            "results": items,
            "count": items.len(),
        }))
    }

    pub(super) fn tool_find_dead_code(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let path = args["path"].as_str();
        let node_type = args["node_type"].as_str();
        let include_tests = args["include_tests"].as_bool().unwrap_or(false);
        let min_lines = args["min_lines"].as_u64().unwrap_or(3) as u32;
        let compact = args["compact"].as_bool().unwrap_or(true);

        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
        }

        let results = queries::find_dead_code(
            self.db.conn(),
            path,
            node_type,
            include_tests,
            min_lines,
            200,
        )?;

        if results.is_empty() {
            return Ok(json!({
                "results": [],
                "orphan_count": 0,
                "exported_unused_count": 0,
                "summary": "No dead code found with the given filters."
            }));
        }

        // Classify into orphans and exported-unused
        let mut orphan_items: Vec<serde_json::Value> = Vec::new();
        let mut exported_items: Vec<serde_json::Value> = Vec::new();

        for r in &results {
            let is_exported = r.has_export_edge
                || r.code_content.starts_with("pub ")
                || r.code_content.starts_with("pub(")
                || (r.file_path.ends_with(".go")
                    && r.name.chars().next().is_some_and(|c| c.is_uppercase()));
            let lines = r.end_line - r.start_line + 1;
            let mut item = json!({
                "name": r.name,
                "type": r.node_type,
                "file_path": r.file_path,
                "start_line": r.start_line,
                "end_line": r.end_line,
                "lines": lines,
                "category": if is_exported { "exported_unused" } else { "orphan" },
            });
            if !compact {
                item["code"] = json!(r.code_content);
            }
            if is_exported {
                exported_items.push(item);
            } else {
                orphan_items.push(item);
            }
        }

        let mut all_items = orphan_items.clone();
        all_items.extend(exported_items.iter().cloned());

        Ok(json!({
            "results": all_items,
            "orphan_count": orphan_items.len(),
            "exported_unused_count": exported_items.len(),
            "summary": format!("Dead code: {} results ({} orphan, {} exported-unused)",
                all_items.len(), orphan_items.len(), exported_items.len())
        }))
    }

    pub(super) fn tool_find_references(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let symbol_name = required_str(args, "symbol_name")?;
        let file_path = args["file_path"].as_str();
        let relation = args["relation"].as_str().unwrap_or("all");
        let compact = args["compact"].as_bool().unwrap_or(false);

        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
        }

        // Resolve symbol to node_id(s)
        let target_ids: Vec<i64> = if let Some(fp) = file_path {
            // Specific file: find the symbol in that file
            let nodes = queries::get_nodes_by_file_path(self.db.conn(), fp)?;
            let matching: Vec<i64> = nodes.iter()
                .filter(|n| n.name == symbol_name)
                .map(|n| n.id)
                .collect();
            if matching.is_empty() {
                return Err(anyhow!("Symbol '{}' not found in file '{}'.", symbol_name, fp));
            }
            matching
        } else {
            // No file_path: fuzzy resolve
            match self.resolve_fuzzy_name(symbol_name)? {
                FuzzyResolution::Unique(resolved_name) => {
                    let nodes = queries::get_node_ids_by_name(self.db.conn(), &resolved_name)?;
                    let ids: Vec<i64> = nodes.into_iter()
                        .filter(|(_, fp)| !is_test_symbol(&resolved_name, fp))
                        .map(|(id, _)| id)
                        .collect();
                    if ids.is_empty() {
                        return Err(anyhow!("Symbol '{}' not found in index.", symbol_name));
                    }
                    ids
                }
                FuzzyResolution::Ambiguous(suggestions) => {
                    return Ok(json!({
                        "symbol": symbol_name,
                        "error": format!("Ambiguous symbol '{}': {} matches. Specify file_path to disambiguate.", symbol_name, suggestions.len()),
                        "suggestions": suggestions,
                    }));
                }
                FuzzyResolution::NotFound => {
                    return Err(anyhow!("Symbol '{}' not found in index. Use semantic_code_search to find the correct symbol name.", symbol_name));
                }
            }
        };

        use crate::domain::{REL_CALLS, REL_IMPORTS, REL_INHERITS, REL_IMPLEMENTS};
        let relation_filter = match relation {
            "calls" => Some(REL_CALLS),
            "imports" => Some(REL_IMPORTS),
            "inherits" => Some(REL_INHERITS),
            "implements" => Some(REL_IMPLEMENTS),
            _ => None,
        };

        // Collect references for all matching node IDs
        let mut all_refs: Vec<serde_json::Value> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for target_id in &target_ids {
            let refs = queries::get_incoming_references(self.db.conn(), *target_id, relation_filter)?;
            for r in refs {
                // Deduplicate by (name, file_path, relation)
                let key = (r.name.clone(), r.file_path.clone(), r.relation.clone());
                if seen.insert(key) {
                    if compact {
                        all_refs.push(json!({
                            "name": r.name,
                            "file_path": r.file_path,
                            "start_line": r.start_line,
                            "relation": r.relation,
                            "node_id": r.node_id,
                        }));
                    } else {
                        all_refs.push(json!({
                            "name": r.name,
                            "type": r.node_type,
                            "file_path": r.file_path,
                            "start_line": r.start_line,
                            "relation": r.relation,
                            "node_id": r.node_id,
                        }));
                    }
                }
            }
        }

        // Group by relation for readability
        let mut by_relation: std::collections::HashMap<String, Vec<&serde_json::Value>> = std::collections::HashMap::new();
        for r in &all_refs {
            let rel = r["relation"].as_str().unwrap_or("unknown").to_string();
            by_relation.entry(rel).or_default().push(r);
        }

        let summary: serde_json::Value = by_relation.iter().map(|(rel, refs)| {
            (rel.clone(), json!(refs.len()))
        }).collect::<serde_json::Map<String, serde_json::Value>>().into();

        Ok(json!({
            "symbol": symbol_name,
            "total_references": all_refs.len(),
            "by_relation": summary,
            "references": all_refs,
        }))
    }
}
