//! `module_overview` — exports / hot paths / inactive symbols by file.
//! 60s TTL cache; partition into active (called by others) vs inactive to save tokens.

use super::super::*;

impl McpServer {
    pub(in crate::mcp::server) fn tool_module_overview(
        &self,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        // Validate deps_direction UNCONDITIONALLY at tool entry. It is only consumed
        // when `include_deps` folds in dependency_graph for a single-file path, but
        // validating it here — before ensure_indexed, regardless of include_deps or
        // path shape — stops a bogus value from being silently swallowed into the
        // `dependencies_unavailable` field (directory paths / include_deps:false
        // never reached the old gated check). feedback-enum-validate-at-entry.
        let deps_direction_raw = args
            .get("deps_direction")
            .and_then(|v| v.as_str())
            .unwrap_or("both");
        let deps_direction = crate::domain::normalize_dep_direction(deps_direction_raw)
            .ok_or_else(|| {
                anyhow!(
                    "deps_direction must be one of: outgoing, incoming, both (got '{}')",
                    deps_direction_raw
                )
            })?;
        // Same argument, applied to the numeric half (CON-15): both of these are
        // consumed only inside their `include_*` block, so a wrong-typed value sent
        // without the companion flag would be swallowed exactly the way a bogus
        // `deps_direction` used to be. Bind them here so the check is unconditional.
        let deps_depth = arg_i64(args, "deps_depth", 2)?;
        let dead_min_lines = arg_u64(args, "dead_min_lines", 3)?;

        if !should_skip_indexing(args)? {
            self.ensure_indexed()?;
        }

        // Separator-normalize BEFORE the escape checks and the prefix match: the
        // index stores `/`, so a Windows caller's `src\parser` must become
        // `src/parser` to match `get_module_exports`'s prefix (and to be the same
        // string `ensure_file_fresh_opt` refreshes). Doing it before validation
        // also makes `..\foo` reach the `../` guard instead of slipping past it.
        let raw_path = args["path"]
            .as_str()
            .map(super::normalize_path_arg)
            .ok_or_else(|| anyhow!("Missing path"))?;
        let raw_path = raw_path.as_str();
        // Reject empty-string path explicitly: it normalizes to the "match all"
        // prefix the same way "." does, but is almost always a variable-substitution
        // bug at the call site (env var unset, optional chain returned ""). Surface
        // it instead of silently dumping the whole project as if path:"." was passed.
        if raw_path.is_empty() {
            return Err(anyhow!(
                "path must not be empty — use '.' to scan the whole project root"
            ));
        }
        // Reject paths that obviously aim outside the project root. The index
        // stores file paths relative to project_root, so '/etc', '../foo', or
        // 'C:\Windows' will never match anything — but currently they silently
        // return `0 files` with a generic warning. An upfront error is clearer
        // and matches the lesson from #259 (validate at parse time).
        //
        // The drive form REQUIRES a separator after the colon (`C:\x`, `C:/x`)
        // or the bare root (`C:`). Keying on "a colon at byte 1" alone — which
        // this did, and `src/cli.rs` copied before fixing it there — refuses
        // `a:b.rs`, a perfectly legal POSIX filename sitting in the project
        // root, with "must be relative to the project root". `src/cli.rs:9441`
        // now asserts that exact name must survive; this surface disagreed.
        let pb = raw_path.as_bytes();
        let drive_shaped = pb.len() >= 2
            && pb[0].is_ascii_alphabetic()
            && pb[1] == b':'
            && (pb.len() == 2 || pb[2] == b'/' || pb[2] == b'\\');
        if raw_path.starts_with('/')
            || raw_path.starts_with("../")
            || raw_path.contains("/../")
            || drive_shaped
            || raw_path.starts_with(r"\\")
        {
            return Err(anyhow!(
                "path '{}' must be relative to the project root (no leading '/' or '../', no absolute paths)",
                raw_path
            ));
        }
        let compact = arg_bool(args, "compact", false)?;
        let include_deps = arg_bool(args, "include_deps", false)?;
        let include_dead = arg_bool(args, "include_dead", false)?;
        // Normalize: strip leading "./" and treat "." as empty prefix (match all)
        let path = raw_path.strip_prefix("./").unwrap_or(raw_path);
        let path = if path == "." { "" } else { path };

        // Edit-aware refresh: when `path` names a single file (not a directory)
        // and the agent just edited it, sync-reindex before answering. Cache
        // invalidation inside `ensure_file_fresh_opt` evicts the stale overview
        // for this exact file path so the cached-result branch above doesn't
        // serve a pre-edit answer on the next call.
        if !should_skip_indexing(args)? {
            self.ensure_file_fresh_opt(Some(path))?;
        }

        // Return cached result if fresh (< 60s), evict if expired.
        //
        // The cache holds the BASE overview only, and the `include_deps` /
        // `include_dead` folding below runs on cached and freshly-built results
        // alike. The flags are not part of the cache key, so an early return here
        // silently dropped them: once any caller warmed `path` (SessionStart
        // injection does), every `include_dead:true` call for the next 60s came
        // back byte-identical to a plain one — no `dead_code`, and no
        // `dead_code_unavailable` either, so the absence was indistinguishable
        // from "nothing dead here". `project_map` keeps `centrality` outside its
        // cache for exactly this reason; this one was missed.
        let cached_base = {
            let mut cache = lock_or_recover(&self.cache.cached_module_overviews, "cached_movw");
            match cache.get(path) {
                Some((ts, val)) if ts.elapsed().as_secs() < 60 => Some(val.clone()),
                Some(_) => {
                    cache.remove(path);
                    None
                }
                None => None,
            }
        };

        let mut result = if let Some(cached) = cached_base {
            cached
        } else {
            let exports = queries::get_module_exports(self.db.conn(), path)?;

            // Filter out test functions — they add noise to module overviews
            let exports: Vec<_> = exports
                .into_iter()
                .filter(|e| !is_test_symbol(&e.name, &e.file_path))
                .collect();

            // Get import/dependency info at file level
            let files: std::collections::HashSet<&str> =
                exports.iter().map(|e| e.file_path.as_str()).collect();

            // Split exports into active (called by others) and inactive to save tokens.
            let (active, inactive): (Vec<_>, Vec<_>) =
                exports.iter().partition(|e| e.caller_count > 0);

            let mut hot_candidates: Vec<_> =
                exports.iter().filter(|e| e.caller_count > 0).collect();
            hot_candidates.sort_by_key(|e| std::cmp::Reverse(e.caller_count));
            let hot_paths: Vec<serde_json::Value> = hot_candidates
                .iter()
                .take(5)
                .map(|e| {
                    let mut obj = json!({
                        "name": e.name,
                        "type": e.node_type,
                        "file": e.file_path,
                        "caller_count": e.caller_count,
                    });
                    if e.qualified_name != e.name {
                        obj["qualified_name"] = json!(e.qualified_name);
                    }
                    obj
                })
                .collect();

            // Active exports get full detail; inactive ones are summarized by type.
            const MAX_ACTIVE: usize = 30;
            let active_capped = active.len() > MAX_ACTIVE;
            let mut active_sorted = active.clone();
            active_sorted.sort_by_key(|e| std::cmp::Reverse(e.caller_count));
            let active_exports: Vec<serde_json::Value> = active_sorted
                .iter()
                .take(MAX_ACTIVE)
                .map(|e| {
                    let mut obj = json!({
                        "node_id": e.node_id,
                        "name": e.name,
                        "type": e.node_type,
                        "file": e.file_path,
                        "caller_count": e.caller_count,
                        "signature": e.signature,
                        "start_line": e.start_line,
                        "end_line": e.end_line,
                    });
                    // Disambiguate same-named methods of different classes (parity with
                    // CLI `overview --json`). Present only when it adds info.
                    if e.qualified_name != e.name {
                        obj["qualified_name"] = json!(e.qualified_name);
                    }
                    obj
                })
                .collect();

            // Compact summary for inactive symbols — just counts by type.
            //
            // BTreeMap, not HashMap: this array goes straight into an
            // LLM-visible tool response, and `HashMap`'s iteration order is
            // seeded per instance — the same binary over the same index emitted
            // a different group order on every run. That makes a response
            // irreproducible and taints any run-to-run diff. Ordering by type is
            // structural here rather than a sort applied afterwards, so the
            // property cannot be lost by an edit that forgets the sort.
            let mut inactive_by_type: std::collections::BTreeMap<&str, Vec<&str>> =
                std::collections::BTreeMap::new();
            for e in &inactive {
                // Show `Class.method` for members so two same-named methods of different
                // classes don't both surface as a bare, indistinguishable `render`.
                inactive_by_type
                    .entry(e.node_type.as_str())
                    .or_default()
                    .push(e.display_name());
            }
            let inactive_summary: Vec<serde_json::Value> = inactive_by_type
                .iter()
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
                result["hint"] =
                    json!("Active exports capped. Use a more specific path to see all.");
            }

            // Cache the full result (max 10 entries to bound memory)
            {
                let mut cache = lock_or_recover(&self.cache.cached_module_overviews, "cached_movw");
                if cache.len() >= 10 {
                    // Evict oldest entry
                    if let Some(oldest_key) = cache
                        .iter()
                        .min_by_key(|(_, (ts, _))| *ts)
                        .map(|(k, _)| k.to_string())
                    {
                        cache.remove(&oldest_key);
                    }
                }
                cache.insert(
                    path.to_string(),
                    (std::time::Instant::now(), result.clone()),
                );
            }
            result
        };

        // include_deps: when path is a single file, fold in dependency_graph output.
        // Folds the former dependency_graph tool (v0.18.4).
        if include_deps {
            if path.contains('.') && !path.ends_with('/') {
                // deps_direction was validated at function entry (unconditionally).
                let dep_args = json!({
                    "file_path": path,
                    "direction": deps_direction,
                    "depth": deps_depth,
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
            // Bound at entry (above) as u64: this is forwarded to find_dead_code's
            // `min_lines`, whose own `as_u64` used to turn a negative into the
            // default a SECOND time — CON-15's double downgrade. Rejecting means
            // the caller hears about it once, at the surface they actually called.
            let min_lines = dead_min_lines;
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

    pub(in crate::mcp::server) fn compact_module_overview(
        &self,
        full: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        // Compact: keep node_id for chaining, drop signature.
        // Field name `caller_count` matches the non-compact envelope and the
        // CLI `overview --json` output (parity across surfaces).
        let active: Vec<serde_json::Value> = full["active_exports"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|e| {
                        let mut obj = json!({
                            "node_id": e["node_id"],
                            "name": e["name"],
                            "type": e["type"],
                            "file": e["file"],
                            "caller_count": e["caller_count"],
                        });
                        // Forward the method disambiguator when the full envelope carries it.
                        if let Some(qn) = e.get("qualified_name") {
                            obj["qualified_name"] = qn.clone();
                        }
                        obj
                    })
                    .collect()
            })
            .unwrap_or_default();

        let inactive_count: usize = full["inactive_summary"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|s| s["count"].as_u64()).sum::<u64>() as usize)
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
        // dead-code section instead of silently dropping it. `dependencies` +
        // the two `*_unavailable` error variants are forwarded so `include_deps`/
        // `include_dead` payloads (and their failure disclosures) survive compact mode.
        // Any new top-level key assigned onto `result` in tool_module_overview MUST be
        // added here (or to DELIBERATELY_COMPACTED in tests/freshness_parity.rs) —
        // the `compact_allowlist_covers_all_result_keys` drift-guard enforces this.
        for key in [
            "active_capped",
            "showing",
            "total_active",
            "hint",
            "dead_code",
            "dependencies",
            "dependencies_unavailable",
            "dead_code_unavailable",
        ] {
            if let Some(v) = full.get(key) {
                result[key] = v.clone();
            }
        }
        Ok(result)
    }
}
