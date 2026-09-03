//! `get_ast_node` — single-symbol introspection by node_id or symbol_name+file_path.
//!
//! Also hosts `append_impact_summary` (powers `include_impact=true` on get_ast_node)
//! and the path-traversal-safe `read_source_context` helper.

use super::super::*;

/// Outcome of refreshing the file a `node_id` lives in (CON-10).
pub(in crate::mcp::server) enum NodeIdRefresh {
    /// Nothing was re-indexed, so the id still names what it named.
    Unchanged,
    /// The file was re-indexed; this is the same symbol's new id.
    Renumbered(i64),
    /// The file was re-indexed and the symbol is no longer in it.
    Gone { name: String, file_path: String },
}

impl McpServer {
    /// Re-index the file a `node_id` points into, then re-resolve the node BY
    /// IDENTITY rather than by id.
    ///
    /// Re-resolving by id would be the obvious shortcut and is wrong: `nodes.id`
    /// is a bare `INTEGER PRIMARY KEY`, i.e. a rowid alias with no AUTOINCREMENT,
    /// and an incremental re-index deletes and re-inserts the file's rows. SQLite
    /// then hands out `max(rowid)+1`, so ids freed by the delete get REUSED —
    /// the caller's stale id can come back attached to a different symbol in the
    /// same file. Identity here is (file_path, qualified_name or name, type),
    /// which is what the tool's own "re-resolve by symbol_name + file_path" error
    /// message already tells callers to do by hand.
    fn refresh_node_file_and_reresolve(&self, node_id: i64) -> Result<NodeIdRefresh> {
        let Some(nf) = queries::get_node_with_file_by_id(self.db.conn(), node_id)? else {
            // Unknown id: leave the miss to `ast_node_by_id`, whose error already
            // explains rebuild-scoped ids and how to re-resolve.
            return Ok(NodeIdRefresh::Unchanged);
        };
        let file_path = nf.file_path;
        let name = nf.node.name;
        let qualified = nf.node.qualified_name;
        let node_type = nf.node.node_type;

        if !self.ensure_file_fresh_reported(Some(&file_path))? {
            return Ok(NodeIdRefresh::Unchanged);
        }

        let hit = crate::resolve::reresolve_node_by_identity(
            self.db.conn(),
            &file_path,
            &name,
            qualified.as_deref(),
            &node_type,
        )?;
        Ok(match hit {
            Some(c) => NodeIdRefresh::Renumbered(c.node.id),
            None => NodeIdRefresh::Gone { name, file_path },
        })
    }
    pub(in crate::mcp::server) fn tool_get_ast_node(
        &self,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        // Validate min_confidence at entry, BEFORE any index/freshness work, so a
        // bad value errors cleanly instead of after a possible reindex (and isn't
        // preempted by a freshness error) — enum-validate-at-entry. Used by the
        // include_impact caller traversal; min_confidence:"ambiguous" includes
        // every caller, default 'inferred' folds the ambiguous by-name fan-out.
        let impact_conf_rank = crate::domain::confidence_rank(
            crate::domain::parse_min_confidence(args["min_confidence"].as_str(), "min_confidence")?
                .unwrap_or(crate::domain::DEFAULT_RISK_CONF_FLOOR),
        );

        // Normalize the caller's separator spelling ONCE, at entry, so the
        // freshness target and the index lookup key below are the same string
        // (see `super::normalize_path_arg`). Must precede the freshness call.
        //
        // Empty/whitespace-only `file_path` behaves like absent, matching the
        // `symbol_name` treatment 40 lines below and the `.filter(|s|
        // !s.trim().is_empty())` every sibling tool applies (trace, deps,
        // similar, ast_search, callgraph). This was the one of the five without
        // it, so `{symbol_name: "foo", file_path: ""}` took the file branch and
        // answered "File '' not found" instead of resolving by name — an LLM
        // client that fills every declared field with a placeholder gets a hard
        // error for a request that names a real symbol.
        let file_path_arg = args["file_path"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .map(super::normalize_path_arg);

        // Bound BEFORE `ensure_indexed` / `ensure_file_fresh_opt`, which can run a
        // full index pass and re-index a file — i.e. seconds of work and a write.
        // Reading them afterwards meant `{"file_path": "x.ts", "compact": "true"}`
        // paid for all of that and then failed on the type; validation this cheap
        // belongs in front of the side effect, which is the rule `overview.rs`
        // already states for its own numeric args (pre-tag review P3-2).
        let include_refs = arg_bool(args, "include_references", false)?;
        let include_tests = arg_bool(args, "include_tests", false)?;
        let include_impact = arg_bool(args, "include_impact", false)?;
        let include_similar = arg_bool(args, "include_similar", false)?;
        let similar_top_k = arg_u64(args, "similar_top_k", 5)? as i64;
        let compact = arg_bool(args, "compact", false)?;

        if !should_skip_indexing(args)? {
            self.ensure_indexed()?;
            self.ensure_file_fresh_opt(file_path_arg.as_deref())?;
        }

        // Support lookup by node_id or file_path+symbol_name
        if let Some(nid) = arg_opt_i64(args, "node_id")? {
            // When called with node_id, default context_lines=3
            let ctx = arg_u64(args, "context_lines", 3)?.clamp(0, 100) as usize;
            // CON-10: this branch used to skip the refresh entirely, reasoning
            // that a node_id lookup "has no path to refresh against" — but the
            // row it is about to read carries the path. With context_lines
            // defaulting to 3 HERE and nowhere else, the branch that skipped the
            // refresh is the one that opens the CURRENT file and cuts a window at
            // the index's line numbers, so an insertion above the symbol returned
            // unrelated code under that symbol's name and signature.
            let (nid, renumbered) = if should_skip_indexing(args)? {
                (nid, false)
            } else {
                match self.refresh_node_file_and_reresolve(nid)? {
                    NodeIdRefresh::Unchanged => (nid, false),
                    NodeIdRefresh::Renumbered(new_id) => (new_id, true),
                    NodeIdRefresh::Gone { name, file_path } => {
                        return Ok(json!({
                            "error": "Symbol no longer present after refresh",
                            "node_id": nid,
                            "name": name,
                            "file_path": file_path,
                            "note": "The file changed on disk and was re-indexed before this \
                                     answer; the symbol this node_id named is not in the new \
                                     index. Re-resolve with get_ast_node(symbol_name, file_path) \
                                     or ast_search.",
                        }));
                    }
                }
            };
            let mut out = self.ast_node_by_id(
                nid,
                include_refs,
                include_tests,
                include_impact,
                ctx,
                compact,
                impact_conf_rank,
            )?;
            if include_similar {
                self.attach_similar(&mut out, nid, similar_top_k)?;
            }
            if renumbered {
                // The id the caller passed is dead — it may already name a
                // different symbol. Say so rather than letting them reuse it.
                out["node_id_renumbered"] = json!(true);
                out["note"] = json!(
                    "This file was re-indexed to answer your call, which renumbered its nodes. \
                     The node_id you passed is no longer valid; use the node_id in this response."
                );
            }
            return Ok(out);
        }

        let context_lines = arg_u64(args, "context_lines", 0)?.clamp(0, 100) as usize;

        // Empty/whitespace-only symbol_name behaves like absent — prevents
        // "Symbol '' not found" and accidental fuzzy hits on the only candidate.
        let symbol_name = args["symbol_name"]
            .as_str()
            .filter(|s| !s.trim().is_empty());
        let file_path = file_path_arg.as_deref();

        // If only symbol_name provided (no file_path), resolve by name lookup
        if let (Some(sym), None) = (symbol_name, file_path) {
            // Ambiguity verdict + response from the shared resolver (2026-08-16
            // audit §四/§六). This site had its own copy of both, and the copy was
            // not equivalent: it re-rendered the candidate JSON by hand (one of
            // four shapes for one verdict, and the only one missing the `symbol`
            // key), and — the part with teeth — it never filtered the
            // `<external>` sentinel. That filter exists because IDX v53 started
            // binding Rust `use std::…` to the sentinel, so in any repo doing
            // `use std::mem::take` the project's own `fn take` read as ambiguous
            // with `<external>` offered as the file to disambiguate BY. `callgraph`
            // and `impact` were fixed then; `get_ast_node` was still exposed.
            if let Some(cands) = crate::resolve::detect_ambiguity(self.db.conn(), sym)? {
                return Ok(crate::resolve::ambiguity_response(sym, &cands));
            }
            let candidates = queries::get_nodes_with_files_by_name(self.db.conn(), sym)?;
            let non_test: Vec<_> = candidates
                .iter()
                .filter(|nf| crate::resolve::is_selectable_definition(&nf.file_path))
                .filter(|nf| !is_test_symbol(&nf.node.name, &nf.file_path))
                .collect();
            // `detect_ambiguity` already returned above for >1, so this is 0 or 1.
            return match non_test.first() {
                None => Err(anyhow!("Symbol '{}' not found in index. Use semantic_code_search to find the correct symbol name, or check spelling.", sym)),
                Some(nf) => {
                    let nid = nf.node.id;
                    let mut out = self.ast_node_by_id(nid, include_refs, include_tests, include_impact, context_lines, compact, impact_conf_rank)?;
                    if include_similar {
                        self.attach_similar(&mut out, nid, similar_top_k)?;
                    }
                    Ok(out)
                }
            };
        }

        let file_path = file_path.ok_or_else(|| {
            anyhow!("Either node_id, symbol_name, or file_path+symbol_name is required")
        })?;
        let symbol_name =
            symbol_name.ok_or_else(|| anyhow!("symbol_name is required when using file_path"))?;

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
                    if let Some(code) =
                        self.read_source_context(file_path, n.start_line, n.end_line, context_lines)
                    {
                        result["code_content"] = json!(code);
                    } else {
                        result["code_content"] = json!(n.code_content);
                    }
                } else {
                    result["code_content"] = json!(n.code_content);
                }

                if include_refs {
                    use crate::domain::REL_CALLS as CALLS;
                    let callees =
                        queries::get_edge_targets_with_files(self.db.conn(), n.id, CALLS)?;
                    let callers =
                        queries::get_edge_sources_with_files(self.db.conn(), n.id, CALLS)?;
                    result["calls"] = json!(callees
                        .into_iter()
                        .map(|(name, file)| json!({"name": name, "file": file}))
                        .collect::<Vec<_>>());
                    let (filtered, test_count) = if include_tests {
                        // Stable sort prod-first: downstream truncation in centralized_compress
                        // keeps first 10 + last 5; without this, test-heavy SQL row order can
                        // crowd all production callers out of the kept window.
                        let mut all = callers;
                        all.sort_by_key(|(n, f, t)| crate::domain::is_test_node(*t, n, f));
                        (all, 0)
                    } else {
                        let total = callers.len();
                        let prod: Vec<_> = callers
                            .into_iter()
                            .filter(|(n, f, t)| !crate::domain::is_test_node(*t, n, f))
                            .collect();
                        let tc = total - prod.len();
                        (prod, tc)
                    };
                    result["called_by"] = json!(filtered
                        .into_iter()
                        .map(|(name, file, _)| json!({"name": name, "file": file}))
                        .collect::<Vec<_>>());
                    if test_count > 0 {
                        result["test_callers_hidden"] = json!(test_count);
                    }
                }

                if include_impact {
                    self.append_impact_summary(
                        &mut result,
                        &n.name,
                        file_path,
                        &n.node_type,
                        impact_conf_rank,
                    )?;
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
                    result["summary"] = json!(format!(
                        "{} {} in {} (lines {}-{}){}",
                        n.node_type,
                        n.name,
                        file_path,
                        n.start_line,
                        n.end_line,
                        n.signature
                            .as_ref()
                            .map(|s| format!(" {}", s))
                            .unwrap_or_default()
                    ));
                    return Ok(result);
                }

                Ok(result)
            }
            None => {
                // List available symbols to help the user
                let available: Vec<String> = nodes
                    .iter()
                    .filter(|n| n.name != "<module>")
                    .take(10)
                    .map(|n| format!("{} ({})", n.name, n.node_type))
                    .collect();
                let hint = if available.is_empty() {
                    String::new()
                } else {
                    format!(". Available symbols: {}", available.join(", "))
                };
                Err(anyhow!(
                    "Symbol '{}' not found in '{}'{}",
                    symbol_name,
                    file_path,
                    hint
                ))
            }
        }
    }

    /// Lookup AST node by node_id.
    #[allow(clippy::too_many_arguments)] // flag-driven introspection: 7 independent display toggles + the impact-summary confidence floor; a struct would just relocate the same fields
    pub(in crate::mcp::server) fn ast_node_by_id(
        &self,
        node_id: i64,
        include_refs: bool,
        include_tests: bool,
        include_impact: bool,
        context_lines: usize,
        compact: bool,
        min_confidence_rank: u8,
    ) -> Result<serde_json::Value> {
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
                if let Some(code) = self.read_source_context(
                    &file_path,
                    node.start_line,
                    node.end_line,
                    context_lines,
                ) {
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
            result["calls"] = json!(callees
                .into_iter()
                .map(|(name, file)| json!({"name": name, "file": file}))
                .collect::<Vec<_>>());
            let (filtered, test_count) = if include_tests {
                // Stable sort prod-first: downstream truncation in centralized_compress
                // keeps first 10 + last 5; without this, test-heavy SQL row order can
                // crowd all production callers out of the kept window.
                let mut all = callers;
                all.sort_by_key(|(n, f, t)| crate::domain::is_test_node(*t, n, f));
                (all, 0)
            } else {
                let total = callers.len();
                let prod: Vec<_> = callers
                    .into_iter()
                    .filter(|(n, f, t)| !crate::domain::is_test_node(*t, n, f))
                    .collect();
                let tc = total - prod.len();
                (prod, tc)
            };
            result["called_by"] = json!(filtered
                .into_iter()
                .map(|(name, file, _)| json!({"name": name, "file": file}))
                .collect::<Vec<_>>());
            if test_count > 0 {
                result["test_callers_hidden"] = json!(test_count);
            }
        }

        if include_impact {
            self.append_impact_summary(
                &mut result,
                &node.name,
                &file_path,
                &node.node_type,
                min_confidence_rank,
            )?;
        }

        Ok(result)
    }

    /// Append a lightweight impact summary to an existing result JSON.
    /// Reuses the shared impact query logic (graph::impact) but returns a compact summary object.
    /// `node_type` is required so that impact on non-function symbols (constant /
    /// struct / enum / trait / ...) with zero callers reports `risk_level: UNKNOWN`
    /// plus a warning, rather than a misleading LOW.
    pub(in crate::mcp::server) fn append_impact_summary(
        &self,
        result: &mut serde_json::Value,
        symbol_name: &str,
        file_path: &str,
        node_type: &str,
        min_confidence_rank: u8,
    ) -> Result<()> {
        let callers = crate::graph::routes::get_callers_with_route_info(
            self.db.conn(),
            symbol_name,
            Some(file_path),
            3,
            min_confidence_rank,
        )?;
        let callers: Vec<_> = callers.into_iter().filter(|c| c.depth > 0).collect();
        // Direct ambiguous callers folded out of the risk count by the floor —
        // disclosed (not silently dropped) so a hidden real caller never
        // under-states risk; min_confidence:"ambiguous" includes them.
        // Frontier-wide: seed direct + every kept caller's pruned callers, so a
        // transitive ambiguous caller is disclosed too (not just seed-direct).
        let caller_ids: Vec<i64> = callers.iter().map(|c| c.node_id).collect();
        let ambiguous_callers_excluded = crate::graph::query::count_suppressed_seed_edges(
            self.db.conn(),
            symbol_name,
            Some(file_path),
            crate::graph::query::Direction::Callers,
            min_confidence_rank,
        )? + crate::graph::query::count_suppressed_into(
            self.db.conn(),
            &caller_ids,
            min_confidence_rank,
        )?;
        // Shared prod/test partition + route + risk classification (graph::impact) —
        // the single source that also drives `cmd_impact`. Trusts the AST `is_test`
        // flag (catches inline `#[cfg(test)]` unit tests whose descriptive names the
        // name heuristic misses), excludes routes reachable only through test callers,
        // and dedups callers by (name, file, depth). Previously this summary
        // reimplemented the partition with the weaker `is_test_symbol` heuristic and
        // counted unparseable/test-only routes — the v0.79.1 audit sibling-hole.
        let is_function_like = crate::domain::is_function_node_type(node_type);
        let cls = crate::graph::impact::classify_impact(&callers, "behavior", is_function_like);

        let mut impact = json!({
            "risk_level": cls.risk_level,
            "direct_callers": cls.prod_callers.iter().filter(|c| c.depth == 1).count(),
            "transitive_callers": cls.prod_callers.iter().filter(|c| c.depth > 1).count(),
            "affected_files": cls.affected_files,
            "affected_routes": cls.route_callers.len(),
        });
        if cls.test_count > 0 {
            impact["test_callers_filtered"] = json!(cls.test_count);
        }
        if let Some(warning) = cls.type_warning {
            impact["warning"] = json!(warning);
        }
        if ambiguous_callers_excluded > 0 {
            impact["ambiguous_callers_excluded"] = json!(ambiguous_callers_excluded);
            // Note parity with cmd_impact — the count alone
            // doesn't tell the agent the risk may be larger or how to see them.
            impact["ambiguous_note"] = json!(format!(
                "{} caller(s) resolved only by ambiguous name-match were excluded from this risk assessment; actual blast radius may be larger. Re-query with min_confidence:\"ambiguous\" to include them.",
                ambiguous_callers_excluded
            ));
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
    pub(in crate::mcp::server) fn read_source_context(
        &self,
        file_path: &str,
        start_line: i64,
        end_line: i64,
        context_lines: usize,
    ) -> Option<String> {
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
    use crate::domain::is_test_node;

    #[test]
    fn called_by_prod_first_sort_survives_truncation() {
        // SQL row order without ORDER BY can interleave or cluster test callers.
        // Worst case observed: tests/integration.rs hits at array tail and
        // src/foo/bar.rs unit tests at head, leaving zero prod callers in
        // a `first 10 + last 5` truncation window. The sort must also push inline
        // `#[cfg(test)]` unit tests (is_test=1, name with no `test` substring in a
        // src/ file) to the back — only the AST flag catches those.
        let mut callers: Vec<(String, String, bool)> = vec![
            (
                "test_v1_to_v2_migration".into(),
                "src/storage/db.rs".into(),
                false,
            ),
            (
                "test_init_creates_db_and_tables".into(),
                "src/storage/db.rs".into(),
                false,
            ),
            ("cmd_health_check".into(), "src/cli.rs".into(), false),
            (
                "run_full_index".into(),
                "src/indexer/pipeline/mod.rs".into(),
                false,
            ),
            (
                "tool_module_overview".into(),
                "src/mcp/server/tools/overview.rs".into(),
                false,
            ),
            (
                "test_camelcase_search_finds_split_tokens".into(),
                "tests/integration.rs".into(),
                false,
            ),
            // inline unit test: heuristic-invisible name + src/ path, only is_test=1 classifies it
            (
                "two_node_cycle_is_detected".into(),
                "src/graph/cycles.rs".into(),
                true,
            ),
        ];
        callers.sort_by_key(|(n, f, t)| is_test_node(*t, n, f));

        let prod_count = callers
            .iter()
            .take_while(|(n, f, t)| !is_test_node(*t, n, f))
            .count();
        assert_eq!(prod_count, 3, "prod callers must occupy contiguous prefix");
        let prod_names: std::collections::HashSet<&str> = callers[..prod_count]
            .iter()
            .map(|(n, _, _)| n.as_str())
            .collect();
        assert!(prod_names.contains("cmd_health_check"));
        assert!(prod_names.contains("run_full_index"));
        assert!(prod_names.contains("tool_module_overview"));
        // The inline unit test must NOT sit in the prod prefix — the flag drives it back.
        assert!(
            !prod_names.contains("two_node_cycle_is_detected"),
            "inline unit test (is_test=1) leaked into the prod prefix"
        );
    }
}
