//! `get_ast_node` — single-symbol introspection by node_id or symbol_name+file_path.
//!
//! Also hosts `append_impact_summary` (powers `include_impact=true` on get_ast_node)
//! and the path-traversal-safe `read_source_context` helper.

use super::super::*;

impl McpServer {
    pub(in crate::mcp::server) fn tool_get_ast_node(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        // Normalize file_path early: resolve absolute paths under project root → relative.
        // Needed before ensure_file_fresh_opt, which uses the normalized path.
        let file_path_normalized = args["file_path"].as_str()
            .map(|fp| super::super::helpers::normalize_tool_path(fp, self.project_root.as_deref()))
            .transpose()?;
        let file_path = file_path_normalized.as_deref();

        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
            // Edit-aware refresh fires only on the file_path branch — node_id
            // lookups have no path to refresh against, and node_id stability
            // across reindex isn't guaranteed.
            self.ensure_file_fresh_opt(file_path)?;
        }

        let include_refs = args["include_references"].as_bool().unwrap_or(false);
        let include_tests = args["include_tests"].as_bool().unwrap_or(false);
        let include_impact = args["include_impact"].as_bool().unwrap_or(false);
        let include_similar = args["include_similar"].as_bool().unwrap_or(false);
        let similar_top_k = args["similar_top_k"].as_i64().unwrap_or(5);
        let compact = args["compact"].as_bool().unwrap_or(false);

        // Support lookup by node_id or file_path+symbol_name
        if let Some(nid) = args["node_id"].as_i64() {
            // When called with node_id, default context_lines=3
            let ctx = args["context_lines"].as_i64().unwrap_or(3).clamp(0, 100) as usize;
            let mut out = self.ast_node_by_id(nid, include_refs, include_tests, include_impact, ctx, compact)?;
            if include_similar {
                self.attach_similar(&mut out, nid, similar_top_k)?;
            }
            return Ok(out);
        }

        let context_lines = args["context_lines"].as_i64().unwrap_or(0).clamp(0, 100) as usize;

        // Empty/whitespace-only symbol_name behaves like absent — prevents
        // "Symbol '' not found" and accidental fuzzy hits on the only candidate.
        let symbol_name = args["symbol_name"].as_str().filter(|s| !s.trim().is_empty());

        // If only symbol_name provided (no file_path), resolve by name lookup
        if let (Some(sym), None) = (symbol_name, file_path) {
            let candidates = queries::get_nodes_with_files_by_name(self.db.conn(), sym)?;
            let non_test: Vec<_> = candidates.iter()
                .filter(|nf| !is_test_symbol(&nf.node.name, &nf.file_path))
                .collect();
            return match non_test.len() {
                0 => Err(anyhow!("Symbol '{}' not found in index. Use semantic_code_search to find the correct symbol name, or check spelling.", sym)),
                1 => {
                    let nid = non_test[0].node.id;
                    let mut out = self.ast_node_by_id(nid, include_refs, include_tests, include_impact, context_lines, compact)?;
                    if include_similar {
                        self.attach_similar(&mut out, nid, similar_top_k)?;
                    }
                    Ok(out)
                }
                _ => {
                    let suggestions: Vec<_> = non_test.iter().map(|nf| {
                        json!({
                            "name": nf.node.name,
                            "file_path": &nf.file_path,
                            "type": nf.node.node_type,
                            "node_id": nf.node.id,
                            "start_line": nf.node.start_line,
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
                    let (filtered, test_count) = if include_tests {
                        // Stable sort prod-first: downstream truncation in centralized_compress
                        // keeps first 10 + last 5; without this, test-heavy SQL row order can
                        // crowd all production callers out of the kept window.
                        let mut all = callers;
                        all.sort_by_key(|(n, f)| is_test_symbol(n, f));
                        (all, 0)
                    } else {
                        let total = callers.len();
                        let prod: Vec<_> = callers.into_iter()
                            .filter(|(n, f)| !is_test_symbol(n, f))
                            .collect();
                        let tc = total - prod.len();
                        (prod, tc)
                    };
                    result["called_by"] = json!(filtered.into_iter().map(|(name, file)| json!({"name": name, "file": file})).collect::<Vec<_>>());
                    if test_count > 0 {
                        result["test_callers_hidden"] = json!(test_count);
                    }
                }

                if include_impact {
                    self.append_impact_summary(&mut result, &n.name, file_path, &n.node_type)?;
                }

                if include_similar {
                    self.attach_similar(&mut result, n.id, similar_top_k)?;
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
    pub(in crate::mcp::server) fn ast_node_by_id(&self, node_id: i64, include_refs: bool, include_tests: bool, include_impact: bool, context_lines: usize, compact: bool) -> Result<serde_json::Value> {
        let nf = queries::get_node_with_file_by_id(self.db.conn(), node_id)?
            .ok_or_else(|| anyhow!(
                "Node {} not found in index. node_ids are rebuild-scoped — a reindex (file change, incremental update, or rebuild_index) may have renumbered nodes. Re-resolve by calling get_ast_node(symbol_name, file_path) or semantic_code_search to obtain a current node_id.",
                node_id
            ))?;
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
            let (filtered, test_count) = if include_tests {
                // Stable sort prod-first: downstream truncation in centralized_compress
                // keeps first 10 + last 5; without this, test-heavy SQL row order can
                // crowd all production callers out of the kept window.
                let mut all = callers;
                all.sort_by_key(|(n, f)| is_test_symbol(n, f));
                (all, 0)
            } else {
                let total = callers.len();
                let prod: Vec<_> = callers.into_iter()
                    .filter(|(n, f)| !is_test_symbol(n, f))
                    .collect();
                let tc = total - prod.len();
                (prod, tc)
            };
            result["called_by"] = json!(filtered.into_iter().map(|(name, file)| json!({"name": name, "file": file})).collect::<Vec<_>>());
            if test_count > 0 {
                result["test_callers_hidden"] = json!(test_count);
            }
        }

        if include_impact {
            self.append_impact_summary(&mut result, &node.name, &file_path, &node.node_type)?;
        }

        Ok(result)
    }

    /// Append a lightweight impact summary to an existing result JSON.
    /// Reuses the impact_analysis query logic but returns a compact summary object.
    /// `node_type` is required so that impact on non-function symbols (constant /
    /// struct / enum / trait / ...) with zero callers reports `risk_level: UNKNOWN`
    /// plus a warning, rather than a misleading LOW.
    pub(in crate::mcp::server) fn append_impact_summary(&self, result: &mut serde_json::Value, symbol_name: &str, file_path: &str, node_type: &str) -> Result<()> {
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

        let is_function_like = crate::domain::is_function_node_type(node_type);
        let warn_non_function = prod_callers.is_empty() && !is_function_like;
        let risk: &'static str = if warn_non_function {
            "UNKNOWN"
        } else {
            crate::domain::compute_risk_level(prod_callers.len(), affected_routes, false)
        };

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
        if warn_non_function {
            impact["warning"] = json!(crate::domain::NON_FUNCTION_IMPACT_WARNING);
        }
        result["impact"] = impact;
        Ok(())
    }

    /// Attach an embedding-similar list under `result["similar"]`. Best-effort:
    /// silently sets `result["similar_unavailable"]` with a reason on failure
    /// (no embed-model, no embeddings yet, or node has no vector).
    pub(in crate::mcp::server) fn attach_similar(
        &self,
        result: &mut serde_json::Value,
        node_id: i64,
        top_k: i64,
    ) -> Result<()> {
        let args = json!({
            "node_id": node_id,
            "top_k": top_k.clamp(1, 50),
            "skip_indexing": true,
        });
        match self.tool_find_similar_code(&args) {
            Ok(v) => {
                if let Some(arr) = v.get("results") {
                    result["similar"] = arr.clone();
                    if let Some(hint) = v.get("hint") {
                        result["similar_hint"] = hint.clone();
                    }
                }
                Ok(())
            }
            Err(e) => {
                result["similar_unavailable"] = json!(e.to_string());
                Ok(())
            }
        }
    }

    /// Read source code with context lines from the project file system.
    /// Uses BufReader to avoid loading entire file into memory.
    pub(in crate::mcp::server) fn read_source_context(&self, file_path: &str, start_line: i64, end_line: i64, context_lines: usize) -> Option<String> {
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
}

#[cfg(test)]
mod tests {
    use super::super::super::is_test_symbol;

    #[test]
    fn called_by_prod_first_sort_survives_truncation() {
        // SQL row order without ORDER BY can interleave or cluster test callers.
        // Worst case observed: tests/integration.rs hits at array tail and
        // src/foo/bar.rs unit tests at head, leaving zero prod callers in
        // a `first 10 + last 5` truncation window.
        let mut callers: Vec<(String, String)> = vec![
            ("test_v1_to_v2_migration".into(), "src/storage/db.rs".into()),
            ("test_init_creates_db_and_tables".into(), "src/storage/db.rs".into()),
            ("cmd_health_check".into(), "src/cli.rs".into()),
            ("run_full_index".into(), "src/indexer/pipeline/mod.rs".into()),
            ("tool_module_overview".into(), "src/mcp/server/tools/overview.rs".into()),
            ("test_camelcase_search_finds_split_tokens".into(), "tests/integration.rs".into()),
            ("test_type_based_search".into(), "tests/integration.rs".into()),
        ];
        callers.sort_by_key(|(n, f)| is_test_symbol(n, f));

        let prod_count = callers.iter().take_while(|(n, f)| !is_test_symbol(n, f)).count();
        assert_eq!(prod_count, 3, "prod callers must occupy contiguous prefix");
        let prod_names: std::collections::HashSet<&str> =
            callers[..prod_count].iter().map(|(n, _)| n.as_str()).collect();
        assert!(prod_names.contains("cmd_health_check"));
        assert!(prod_names.contains("run_full_index"));
        assert!(prod_names.contains("tool_module_overview"));
    }
}
