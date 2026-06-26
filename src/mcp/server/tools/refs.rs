//! `find_references` — usage sites by relation kind, with type-definition warning
//! for struct/enum/trait/class targets where method-qualified calls aren't tracked.

use super::super::*;

impl McpServer {
    pub(in crate::mcp::server) fn tool_find_references(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        // node_id takes precedence — lets callers disambiguate multi-def same-file
        // collisions (e.g. two `fn new()` in one module) that file_path alone can't resolve.
        let node_id = args["node_id"].as_i64();
        // Treat empty/whitespace-only as absent — empty string used to fall
        // through to fuzzy-resolve and silently match a random unique candidate.
        let symbol_name_arg = args["symbol_name"].as_str().filter(|s| !s.trim().is_empty());
        // Normalize file_path: resolve absolute paths under project root → relative.
        // Empty file_path => no filter (treat like missing).
        let file_path_normalized = args["file_path"].as_str()
            .filter(|s| !s.is_empty())
            .map(|fp| super::super::helpers::normalize_tool_path(fp, self.project_root.as_deref()))
            .transpose()?;
        let file_path = file_path_normalized.as_deref();
        let relation = args["relation"].as_str().unwrap_or("all");
        let compact = args["compact"].as_bool().unwrap_or(false);
        // Default true preserves the "every usage site" contract for rename audits
        // (tests must be renamed too). Pass false for "production callers only".
        let include_tests = args["include_tests"].as_bool().unwrap_or(true);

        if node_id.is_none() && symbol_name_arg.is_none() {
            return Err(anyhow!("symbol_name or node_id is required"));
        }

        // Validate relation at tool entry — before index refresh AND before symbol
        // resolution. Otherwise a bogus relation surfaces only after a (possibly
        // failing) symbol lookup, so a typo'd relation on an absent symbol reports
        // "not found" and hides the real mistake. The exhaustive match below still
        // maps the validated value to a filter constant. feedback-enum-validate-at-entry.
        if !matches!(relation, "calls" | "imports" | "inherits" | "implements" | "references" | "all") {
            return Err(anyhow!(
                "Unknown relation filter: '{}'. Valid: calls, imports, inherits, implements, references, all",
                relation
            ));
        }

        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
            self.ensure_file_fresh_opt(file_path)?;
        }

        // Resolve symbol to node_id(s)
        let (target_ids, symbol_name): (Vec<i64>, String) = if let Some(nid) = node_id {
            let node = queries::get_node_by_id(self.db.conn(), nid)?
                .ok_or_else(|| anyhow!("node_id {} not found in index", nid))?;
            (vec![nid], node.name)
        } else if let Some(fp) = file_path {
            let symbol_name = symbol_name_arg.unwrap();
            // Specific file: find the symbol in that file
            let nodes = queries::get_nodes_by_file_path(self.db.conn(), fp)?;
            let matching: Vec<i64> = nodes.iter()
                .filter(|n| n.name == symbol_name)
                .map(|n| n.id)
                .collect();
            if matching.is_empty() {
                return Err(anyhow!("Symbol '{}' not found in file '{}'.", symbol_name, fp));
            }
            // Multi-def in same file — report ambiguity with node_ids and start_lines
            // so callers can pick the specific definition.
            if matching.len() > 1 {
                let suggestions: Vec<_> = nodes.iter()
                    .filter(|n| n.name == symbol_name)
                    .map(|n| json!({
                        "name": n.name,
                        "file_path": fp,
                        "type": n.node_type,
                        "node_id": n.id,
                        "start_line": n.start_line,
                    }))
                    .collect();
                return Ok(json!({
                    "symbol": symbol_name,
                    "error": format!(
                        "Ambiguous symbol '{}' in '{}': {} definitions in the same file. Pass node_id to select one.",
                        symbol_name, fp, suggestions.len()
                    ),
                    "suggestions": suggestions,
                }));
            }
            (matching, symbol_name.to_string())
        } else {
            let symbol_name = symbol_name_arg.unwrap();
            // No file_path: fuzzy resolve
            match self.resolve_fuzzy_name(symbol_name)? {
                FuzzyResolution::Unique(resolved_name) => {
                    let all_nodes = queries::get_node_ids_by_name(self.db.conn(), &resolved_name)?;
                    let total = all_nodes.len();
                    let (prod, filtered): (Vec<_>, Vec<_>) = all_nodes.into_iter()
                        .partition(|(_, fp)| !is_test_symbol(&resolved_name, fp));
                    let ids: Vec<i64> = prod.into_iter().map(|(id, _)| id).collect();
                    if ids.is_empty() {
                        // Symbol exists but every match is in test/bench territory.
                        // Drop the misleading "not found" — surface the filter so the
                        // caller knows to bypass with file_path or node_id (covers the
                        // dead-code → find_references reverse-trace flow).
                        if total > 0 {
                            let example_paths: Vec<String> = filtered.iter()
                                .take(3)
                                .map(|(_, fp)| fp.clone())
                                .collect();
                            return Err(anyhow!(
                                "Symbol '{}' exists but all {} match(es) are in test/bench paths ({}). \
                                 Pass node_id (use ast_search or get_ast_node to obtain one) or \
                                 file_path explicitly to bypass the test filter.",
                                symbol_name, total, example_paths.join(", ")
                            ));
                        }
                        return Err(anyhow!("Symbol '{}' not found in index.", symbol_name));
                    }
                    (ids, resolved_name)
                }
                FuzzyResolution::Ambiguous(suggestions) => {
                    return Ok(json!({
                        "symbol": symbol_name,
                        "error": format!("Ambiguous symbol '{}': {} matches. Specify file_path or node_id to disambiguate.", symbol_name, suggestions.len()),
                        "suggestions": suggestions,
                    }));
                }
                FuzzyResolution::NotFound => {
                    // resolve_fuzzy_name filters test/bench candidates upstream.
                    // Distinguish "truly absent" from "found-but-filtered" by
                    // re-querying without that filter — the latter means the
                    // user is on a dead-code → find_references reverse-trace and
                    // needs to know they can bypass with node_id/file_path.
                    let unfiltered = queries::get_node_ids_by_name(self.db.conn(), symbol_name)?;
                    if !unfiltered.is_empty() {
                        let example_paths: Vec<String> = unfiltered.iter()
                            .take(3)
                            .map(|(_, fp)| fp.clone())
                            .collect();
                        return Err(anyhow!(
                            "Symbol '{}' exists but all {} match(es) are in test/bench paths ({}). \
                             Pass node_id (use ast_search or get_ast_node to obtain one) or \
                             file_path explicitly to bypass the test filter.",
                            symbol_name, unfiltered.len(), example_paths.join(", ")
                        ));
                    }
                    return Err(anyhow!("Symbol '{}' not found in index. Use semantic_code_search to find the correct symbol name.", symbol_name));
                }
            }
        };

        use crate::domain::{REL_CALLS, REL_IMPORTS, REL_INHERITS, REL_IMPLEMENTS, REL_REFERENCES};
        // Schema enum is ["calls", "imports", "inherits", "implements", "references", "all"].
        // Unknown values used to fall through to None (no filter), masking typos —
        // a caller passing relation:"call" silently got the same response as "all".
        let relation_filter = match relation {
            "calls" => Some(REL_CALLS),
            "imports" => Some(REL_IMPORTS),
            "inherits" => Some(REL_INHERITS),
            "implements" => Some(REL_IMPLEMENTS),
            "references" => Some(REL_REFERENCES),
            "all" => None,
            _ => return Err(anyhow!(
                "Unknown relation filter: '{}'. Valid: calls, imports, inherits, implements, references, all",
                relation
            )),
        };

        // Collect references for all matching node IDs
        let mut all_refs: Vec<serde_json::Value> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut test_refs_filtered: usize = 0;
        for target_id in &target_ids {
            let refs = queries::get_incoming_references(self.db.conn(), *target_id, relation_filter)?;
            for r in refs {
                // Test filter: caller (r.name + r.file_path) looks like a test node → skip
                // unless caller opted in. Count separately so response shows the hidden total.
                if !include_tests && is_test_symbol(&r.name, &r.file_path) {
                    test_refs_filtered += 1;
                    continue;
                }
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

        // Stable sort prod-first: include_tests defaults to true and
        // centralized_compress truncates large arrays to first 10 + last 5.
        // Without prod-first ordering, alphabetic file paths sandwich prod
        // callers (benches/, src/, tests/) and the kept window can be all-test.
        if include_tests {
            all_refs.sort_by_key(|r| {
                let name = r["name"].as_str().unwrap_or("");
                let file = r["file_path"].as_str().unwrap_or("");
                is_test_symbol(name, file)
            });
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

        // Type-definition warning: for structs/enums/traits/types/interfaces/classes,
        // the edge index only captures explicit call/import/inherit/implement edges.
        // Field access, type annotations, and method-qualified calls (e.g. `Type::method()`
        // which binds to the method node, not the type) will not appear here. Tell the
        // caller so rename audits get broader coverage via a second query.
        let type_kinds = ["struct", "enum", "trait", "type", "interface", "class"];
        let target_types: Vec<String> = target_ids.iter()
            .filter_map(|id| queries::get_node_by_id(self.db.conn(), *id).ok().flatten())
            .map(|n| n.node_type)
            .collect();
        let is_type_def = target_types.iter().any(|t| type_kinds.contains(&t.as_str()));

        let mut out = json!({
            "symbol": symbol_name,
            "total_references": all_refs.len(),
            "by_relation": summary,
            "references": all_refs,
        });
        if !include_tests && test_refs_filtered > 0 {
            if let Some(obj) = out.as_object_mut() {
                obj.insert("test_references_filtered".to_string(), json!(test_refs_filtered));
            }
        }
        if is_type_def {
            if let Some(obj) = out.as_object_mut() {
                obj.insert("type_definition_note".to_string(), json!(
                    "Symbol is a type definition (struct/enum/trait/class). References list \
                     captures explicit imports/inherits/implements and struct-literal instantiation, \
                     but NOT method-qualified calls (e.g. `Type::method()`), field access, or type \
                     annotations. For a rename audit, also query find_references on each method of \
                     this type (see module_overview for method list) and grep for the bare name."
                ));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::is_test_symbol;
    use serde_json::json;

    #[test]
    fn references_prod_first_sort_survives_truncation() {
        // get_incoming_references returns ORDER BY f.path, n.start_line.
        // Alphabetic file paths place benches/ first, tests/ last, sandwiching
        // production callers in src/. Truncation to first 10 + last 5 then
        // drops them when caller count is high.
        let mut refs = [
            json!({"name": "bench_call_graph", "file_path": "benches/indexing.rs"}),
            json!({"name": "cmd_health_check", "file_path": "src/cli.rs"}),
            json!({"name": "run_full_index", "file_path": "src/indexer/pipeline/mod.rs"}),
            json!({"name": "tool_module_overview", "file_path": "src/mcp/server/tools/overview.rs"}),
            json!({"name": "test_camelcase_search", "file_path": "tests/integration.rs"}),
        ];
        refs.sort_by_key(|r| {
            let name = r["name"].as_str().unwrap_or("");
            let file = r["file_path"].as_str().unwrap_or("");
            is_test_symbol(name, file)
        });

        let prod_count = refs.iter().take_while(|r| {
            let name = r["name"].as_str().unwrap_or("");
            let file = r["file_path"].as_str().unwrap_or("");
            !is_test_symbol(name, file)
        }).count();
        assert_eq!(prod_count, 3, "prod callers must occupy contiguous prefix");
        let prod_files: std::collections::HashSet<&str> = refs[..prod_count]
            .iter()
            .map(|r| r["file_path"].as_str().unwrap_or(""))
            .collect();
        assert!(prod_files.contains("src/cli.rs"));
        assert!(prod_files.contains("src/indexer/pipeline/mod.rs"));
        assert!(prod_files.contains("src/mcp/server/tools/overview.rs"));
    }
}
