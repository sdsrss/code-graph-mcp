//! `project_map` — modules / dependencies / entry points / hot functions.
//! 60s TTL cache; compact mode strips classes/languages, keeps key_symbols for discoverability.

use super::super::*;

impl McpServer {
    pub(in crate::mcp::server) fn tool_project_map(
        &self,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
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

        let mut result = if let Some(cached) = full_result {
            cached
        } else {
            let (modules, deps, entry_points, hot_functions) =
                queries::get_project_map(self.db.conn())?;

            let modules_json: Vec<serde_json::Value> = modules
                .iter()
                .map(|m| {
                    let mut obj = json!({
                        "path": m.path,
                        "files": m.files,
                        "functions": m.functions,
                        "classes": m.classes,
                    });
                    if m.interfaces_traits > 0 {
                        obj["interfaces_traits"] = json!(m.interfaces_traits);
                    }
                    if m.constants > 0 {
                        obj["constants"] = json!(m.constants);
                    }
                    // Symbols outside the four named buckets (TS `type` aliases,
                    // markdown headings). Without this a docs- or types-only
                    // module read as `functions: 0, classes: 0` — i.e. empty —
                    // while module_overview listed its symbols.
                    if m.other > 0 {
                        obj["other"] = json!(m.other);
                    }
                    if !m.languages.is_empty() {
                        obj["languages"] = json!(m.languages);
                    }
                    if !m.key_symbols.is_empty() {
                        obj["key_symbols"] = json!(m.key_symbols);
                    }
                    obj
                })
                .collect();

            let deps_json: Vec<serde_json::Value> = deps
                .iter()
                .map(|d| {
                    json!({
                        "from": d.from,
                        "to": d.to,
                        "imports": d.import_count,
                    })
                })
                .collect();

            let routes_json: Vec<serde_json::Value> = entry_points
                .iter()
                .map(|e| {
                    json!({
                        "route": e.route,
                        "handler": e.handler,
                        "file": e.file,
                        "kind": e.kind,
                    })
                })
                .collect();

            let hot_json: Vec<serde_json::Value> = hot_functions
                .iter()
                .map(|h| {
                    let mut obj = json!({
                        "name": h.name,
                        "type": h.node_type,
                        "file": h.file,
                        "caller_count": h.caller_count,
                    });
                    if h.test_caller_count > 0 {
                        obj["test_caller_count"] = json!(h.test_caller_count);
                    }
                    obj
                })
                .collect();

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

        // include_centrality (roadmap 2026-07-18 §2.4 — CLI `centrality` had no MCP
        // surface): architectural chokepoints by betweenness centrality. Computed
        // per call and attached OUTSIDE the 60s cache (the flag/limit vary per
        // call; the cached envelope stays flag-free). Test callers excluded, same
        // default as the CLI.
        if args["include_centrality"].as_bool().unwrap_or(false) {
            let limit = args["centrality_limit"].as_u64().unwrap_or(10).max(1) as usize;
            let ranked =
                crate::graph::centrality::betweenness_centrality(self.db.conn(), false, limit)?;
            let centrality_json: Vec<serde_json::Value> = ranked
                .iter()
                .map(|c| {
                    json!({
                        "name": c.name,
                        "type": c.node_type,
                        "file_path": c.file_path,
                        "betweenness": c.score,
                        "normalized": c.normalized,
                        "caller_count": c.caller_count,
                    })
                })
                .collect();
            result["centrality"] = json!(centrality_json);
        }

        if compact {
            // Compact mode: drop languages/classes/interfaces, keep key_symbols for discoverability
            let compact_modules: Vec<serde_json::Value> = result["modules"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|m| {
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
                        })
                        .collect()
                })
                .unwrap_or_default();

            let compact_deps: Vec<serde_json::Value> = result["module_dependencies"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|d| {
                            json!({
                                "from": d["from"],
                                "to": d["to"],
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();

            // Trim hot_functions: top 10, name+type+file+counts.
            // `type` retained so callers can distinguish function vs method
            // without a follow-up get_ast_node call (parity with non-compact
            // envelope and CLI `map --json`).
            let compact_hot: Vec<serde_json::Value> = result["hot_functions"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .take(10)
                        .map(|h| {
                            let mut obj = json!({
                                "name": h["name"],
                                "type": h["type"],
                                "file": h["file"],
                                "caller_count": h["caller_count"],
                            });
                            if h.get("test_caller_count")
                                .and_then(|v| v.as_i64())
                                .unwrap_or(0)
                                > 0
                            {
                                obj["test_caller_count"] = h["test_caller_count"].clone();
                            }
                            obj
                        })
                        .collect()
                })
                .unwrap_or_default();

            // Trim entry_points: file+handler+kind (kind lets LLM skip `main` when scanning HTTP surface)
            let compact_entries: Vec<serde_json::Value> = result["entry_points"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|e| {
                            json!({
                                "file": e["file"],
                                "handler": e["handler"],
                                "kind": e["kind"],
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();

            let mut compact_result = json!({
                "modules": compact_modules,
                "module_dependencies": compact_deps,
                "entry_points": compact_entries,
                "hot_functions": compact_hot,
            });
            // Compact is a WHITELIST rebuild (feedback_compact_field_allowlist —
            // v0.90/v0.97.1 both dropped new fields here): forward centrality
            // explicitly, trimmed to name+file+score.
            if let Some(cent) = result.get("centrality").and_then(|c| c.as_array()) {
                let compact_cent: Vec<serde_json::Value> = cent
                    .iter()
                    .map(|c| {
                        json!({
                            "name": c["name"],
                            "file_path": c["file_path"],
                            "betweenness": c["betweenness"],
                        })
                    })
                    .collect();
                compact_result["centrality"] = json!(compact_cent);
            }
            return Ok(compact_result);
        }

        Ok(result)
    }
}
