//! `module_overview` — exports / hot paths / inactive symbols by file.
//! 60s TTL cache; partition into active (called by others) vs inactive to save tokens.

use super::super::*;

impl McpServer {
    pub(in crate::mcp::server) fn tool_module_overview(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        // Validate deps_direction UNCONDITIONALLY at tool entry. It is only consumed
        // when `include_deps` folds in dependency_graph for a single-file path, but
        // validating it here — before ensure_indexed, regardless of include_deps or
        // path shape — stops a bogus value from being silently swallowed into the
        // `dependencies_unavailable` field (directory paths / include_deps:false
        // never reached the old gated check). feedback-enum-validate-at-entry.
        let deps_direction = args.get("deps_direction").and_then(|v| v.as_str()).unwrap_or("both");
        if !matches!(deps_direction, "outgoing" | "incoming" | "both") {
            return Err(anyhow!(
                "deps_direction must be one of: outgoing, incoming, both (got '{}')",
                deps_direction
            ));
        }

        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
        }

        let raw_path = args["path"].as_str()
            .ok_or_else(|| anyhow!("Missing path"))?;
        // Normalize: absolute paths under project root → relative, "./" stripped,
        // "." → "" (whole project). Rejects paths outside root and empty strings.
        let path = super::super::helpers::normalize_tool_path(
            raw_path,
            self.project_root.as_deref(),
        )?;
        let path = path.as_str();
        let compact = args["compact"].as_bool().unwrap_or(false);
        let include_deps = args["include_deps"].as_bool().unwrap_or(false);
        let include_dead = args["include_dead"].as_bool().unwrap_or(false);

        // Edit-aware refresh: when `path` names a single file (not a directory)
        // and the agent just edited it, sync-reindex before answering. Cache
        // invalidation inside `ensure_file_fresh_opt` evicts the stale overview
        // for this exact file path so the cached-result branch above doesn't
        // serve a pre-edit answer on the next call.
        if !should_skip_indexing(args) {
            self.ensure_file_fresh_opt(Some(path))?;
        }

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
        hot_candidates.sort_by_key(|e| std::cmp::Reverse(e.caller_count));
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
        active_sorted.sort_by_key(|e| std::cmp::Reverse(e.caller_count));
        let active_exports: Vec<serde_json::Value> = active_sorted.iter()
            .take(MAX_ACTIVE)
            .map(|e| json!({
                "node_id": e.node_id,
                "name": e.name,
                "type": e.node_type,
                "file": e.file_path,
                "caller_count": e.caller_count,
                "signature": e.signature,
                "start_line": e.start_line,
                "end_line": e.end_line,
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

        // include_deps: when path is a single file, fold in dependency_graph output.
        // Folds the former dependency_graph tool (v0.18.4).
        if include_deps {
            if path.contains('.') && !path.ends_with('/') {
                // deps_direction was validated at function entry (unconditionally).
                let dep_args = json!({
                    "file_path": path,
                    "direction": deps_direction,
                    "depth": args.get("deps_depth").and_then(|v| v.as_i64()).unwrap_or(2),
                    "compact": compact,
                    "skip_indexing": true,
                });
                match self.tool_dependency_graph(&dep_args) {
                    Ok(deps) => {
                        result["dependencies"] = json!({
                            "depends_on": deps.get("depends_on").cloned().unwrap_or(json!([])),
                            "depended_by": deps.get("depended_by").cloned().unwrap_or(json!([])),
                        });
                    }
                    Err(e) => {
                        result["dependencies_unavailable"] = json!(e.to_string());
                    }
                }
            } else {
                result["dependencies_unavailable"] = json!(
                    "include_deps requires path to be a single file (got a directory). \
                     Pass a file path like 'src/auth/login.ts'."
                );
            }
        }

        // include_dead: append unreferenced symbols under this path.
        // Folds the former find_dead_code tool (v0.18.4).
        if include_dead {
            let min_lines = args.get("dead_min_lines").and_then(|v| v.as_i64()).unwrap_or(3);
            let dead_args = json!({
                "path": path,
                "min_lines": min_lines,
                "compact": true,
                "skip_indexing": true,
            });
            match self.tool_find_dead_code(&dead_args) {
                Ok(dead) => {
                    result["dead_code"] = json!({
                        "results": dead.get("results").cloned().unwrap_or(json!([])),
                        "orphan_count": dead.get("orphan_count").cloned().unwrap_or(json!(0)),
                        "exported_unused_count": dead.get("exported_unused_count").cloned().unwrap_or(json!(0)),
                        "ignored_count": dead.get("ignored_count").cloned().unwrap_or(json!(0)),
                    });
                }
                Err(e) => {
                    result["dead_code_unavailable"] = json!(e.to_string());
                }
            }
        }

        if compact {
            return self.compact_module_overview(&result);
        }
        Ok(result)
    }

    pub(in crate::mcp::server) fn compact_module_overview(&self, full: &serde_json::Value) -> Result<serde_json::Value> {
        // Compact: keep node_id for chaining, drop signature.
        // Field name `caller_count` matches the non-compact envelope and the
        // CLI `overview --json` output (parity across surfaces).
        let active: Vec<serde_json::Value> = full["active_exports"].as_array()
            .map(|arr| arr.iter().map(|e| json!({
                "node_id": e["node_id"],
                "name": e["name"],
                "type": e["type"],
                "file": e["file"],
                "caller_count": e["caller_count"],
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
        // Forward truncation metadata so compact callers see the cap, not silent truncation.
        // `dead_code` is forwarded so `compact: true + include_dead: true` returns the
        // dead-code section instead of silently dropping it.
        for key in ["active_capped", "showing", "total_active", "hint", "dead_code"] {
            if let Some(v) = full.get(key) {
                result[key] = v.clone();
            }
        }
        Ok(result)
    }
}
