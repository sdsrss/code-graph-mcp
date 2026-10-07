//! `module_overview` — exports / hot paths / inactive symbols by file.
//! 60s TTL cache; partition into active (called by others) vs inactive to save tokens.

use super::super::*;

/// Active exports the default answer lists in full; the rest are counted
/// (`active_capped` / `total_active`) and named by `next`.
const MAX_ACTIVE: usize = 30;
/// Names per type the default answer lists for inactive symbols (`more` counts
/// the rest).
const MAX_INACTIVE_NAMES: usize = 8;

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
        // The bound originates one tool over: this value becomes
        // `dependency_graph`'s `depth`, which clamps to 1..=10. `arg_clamped`
        // applies the same bound here at the read so the clamp can be disclosed
        // against the tool the caller actually named.
        let deps_depth = arg_clamped(args, "deps_depth", "module_overview", 2)? as i64;
        let dead_min_lines = arg_u64(args, "dead_min_lines", 3)?;
        // P1 #2: absent = the unbudgeted answer.
        let max_tokens = match &args["max_tokens"] {
            serde_json::Value::Null => None,
            _ => Some(arg_clamped(args, "max_tokens", "module_overview", 0)? as usize),
        };

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

        // A budgeted call builds its own, uncapped base (the budget is the cap)
        // and neither reads nor fills the cache.
        let mut result = if max_tokens.is_some() {
            self.module_overview_base(path, raw_path, usize::MAX, usize::MAX)?
        } else if let Some(cached) = cached_base {
            cached
        } else {
            let result =
                self.module_overview_base(path, raw_path, MAX_ACTIVE, MAX_INACTIVE_NAMES)?;
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

        if let Some(tokens) = max_tokens {
            use crate::budget::NextCommand;
            let cli_path = if path.is_empty() { "." } else { path };
            let next = SectionNext {
                overview: NextCommand::new("overview").path(if raw_path.is_empty() {
                    "."
                } else {
                    raw_path
                }),
                dependencies: NextCommand::new("deps")
                    .path(cli_path)
                    .arg("--direction")
                    .arg(deps_direction.to_string())
                    .arg("--depth")
                    .arg(deps_depth.to_string()),
                dead_code: NextCommand::new("dead-code").path(cli_path).opt(
                    "--min-lines",
                    (dead_min_lines != 3).then(|| dead_min_lines.to_string()),
                ),
            };
            return Ok(module_overview_budgeted(&result, tokens, &next));
        }
        if compact {
            return self.compact_module_overview(&result);
        }
        Ok(result)
    }

    /// The `module_overview` envelope before `include_deps` / `include_dead` /
    /// `compact`. `max_active` / `max_inactive_names` are the default answer's
    /// caps ([`MAX_ACTIVE`], [`MAX_INACTIVE_NAMES`]); a budgeted call passes
    /// `usize::MAX` and lets the budget cut instead.
    fn module_overview_base(
        &self,
        path: &str,
        raw_path: &str,
        max_active: usize,
        max_inactive_names: usize,
    ) -> Result<serde_json::Value> {
        let exports = queries::get_module_exports(self.db.conn(), path)?;
        // Symbols the per-file export rule withheld. `summary` says "N active
        // + M inactive exports", which a caller reads as the file's whole
        // symbol census — for an ESM file it is only its public half, and
        // nothing in the response said so. Zero for Python/Rust/Go/CommonJS.
        //
        // The query filters on `is_test_node_sql`, the SQL mirror of the
        // `is_test_symbol` call the visible half uses below, so one rule
        // governs both halves. The Err is kept as an Err —
        // `not_exported_unavailable` below, matching the
        // `dependencies_unavailable` / `dead_code_unavailable` convention,
        // because "count failed" is not "nothing hidden".
        let not_exported = queries::count_export_filtered_out(self.db.conn(), path);

        // Filter out test functions — they add noise to module overviews
        let exports: Vec<_> = exports
            .into_iter()
            .filter(|e| !is_test_symbol(&e.name, &e.file_path))
            .collect();

        // Get import/dependency info at file level
        let files: std::collections::HashSet<&str> =
            exports.iter().map(|e| e.file_path.as_str()).collect();

        // Split exports into active (called by others) and inactive to save tokens.
        let (active, inactive): (Vec<_>, Vec<_>) = exports.iter().partition(|e| e.caller_count > 0);

        let mut hot_candidates: Vec<_> = exports.iter().filter(|e| e.caller_count > 0).collect();
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
        let active_capped = active.len() > max_active;
        let mut active_sorted = active.clone();
        active_sorted.sort_by_key(|e| std::cmp::Reverse(e.caller_count));
        let active_exports: Vec<serde_json::Value> = active_sorted
            .iter()
            .take(max_active)
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
                let display: Vec<&&str> = names.iter().take(max_inactive_names).collect();
                let mut obj = json!({
                    "type": typ,
                    "count": names.len(),
                    "names": display,
                });
                if names.len() > max_inactive_names {
                    obj["more"] = json!(names.len() - max_inactive_names);
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
        match &not_exported {
            Ok(0) => {}
            Ok(n) => {
                result["not_exported_hidden"] = json!(n);
                result["not_exported_note"] = json!(queries::export_filter_note(*n));
            }
            Err(e) => result["not_exported_unavailable"] = json!(e.to_string()),
        }
        if files.is_empty() {
            result["warning"] = json!(format!("No files found for path '{}'. Check that the path is relative to the project root.", raw_path));
        }
        if active_capped {
            result["active_capped"] = json!(true);
            result["showing"] = json!(max_active);
            result["total_active"] = json!(active.len());
            result["hint"] = json!("Active exports capped. Use a more specific path to see all.");
        }
        // Every cut above (the active cap, an inactive group's `more`) is
        // returned whole by the text listing (P1 #2).
        let names_cut = result["inactive_summary"]
            .as_array()
            .is_some_and(|a| a.iter().any(|g| g.get("more").is_some()));
        if active_capped || names_cut {
            result["next"] = json!(crate::budget::NextCommand::new("overview")
                .path(if raw_path.is_empty() { "." } else { raw_path })
                .to_string());
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
        // `not_exported_*` is forwarded because compact mode drops the inactive
        // NAMES — it is exactly where a partial symbol census is hardest to
        // notice, so the export-filter disclosure has to survive here or it
        // survives only where it is least needed.
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
            "not_exported_hidden",
            "not_exported_note",
            "not_exported_unavailable",
            "next",
        ] {
            if let Some(v) = full.get(key) {
                result[key] = v.clone();
            }
        }
        Ok(result)
    }
}

/// The commands that return what a budgeted `module_overview` left out, one
/// per section: the overview itself (exports, names, hot paths) and the two
/// folded tools.
struct SectionNext {
    overview: crate::budget::NextCommand,
    dependencies: crate::budget::NextCommand,
    dead_code: crate::budget::NextCommand,
}

/// `module_overview` with `max_tokens`: the uncapped envelope fitted to the
/// budget.
///
/// Units: each active export (ranked by caller count; shorter form drops
/// `signature` / `end_line`), each inactive name (all rank below every
/// active export; each type group loses names from its tail, the groups in
/// proportion), each folded `dependencies` entry (nearest first) and
/// `dead_code` result (in listing order) — these two lose entries in
/// proportion with the inactive names — and each `hot_paths` entry, which
/// goes last (it repeats the most-called exports). One file's active exports
/// are first held to [`crate::budget::FILE_SHARE_PERCENT`] of the budget.
/// `budget` counts what each section lost and names the command per section
/// that returns it; `over_budget` says the fixed part alone did not fit.
fn module_overview_budgeted(
    full: &serde_json::Value,
    tokens: usize,
    next: &SectionNext,
) -> serde_json::Value {
    use crate::budget::{self, Level};
    use std::cmp::Reverse;
    let list = |v: &serde_json::Value| v.as_array().cloned().unwrap_or_default();
    let active = list(&full["active_exports"]);
    let groups = list(&full["inactive_summary"]);
    let names: Vec<Vec<serde_json::Value>> = groups.iter().map(|g| list(&g["names"])).collect();
    let hot = list(&full["hot_paths"]);
    let deps_out = list(&full["dependencies"]["depends_on"]);
    let deps_in = list(&full["dependencies"]["depended_by"]);
    let deps: Vec<serde_json::Value> = deps_out.iter().chain(deps_in.iter()).cloned().collect();
    let dead = list(&full["dead_code"]["results"]);
    let na = active.len();
    // Inactive names: unit index na + offset[g] + j.
    let mut offset = Vec::with_capacity(names.len());
    let mut n = na;
    for g in &names {
        offset.push(n);
        n += g.len();
    }
    // Then the hot paths, the dependencies (outgoing first), the dead code.
    let (oh, od, ox) = (n, n + hot.len(), n + hot.len() + deps.len());
    let n = ox + dead.len();
    let skeleton = |e: &serde_json::Value| {
        let mut s = e.clone();
        if let Some(o) = s.as_object_mut() {
            o.remove("signature");
            o.remove("end_line");
        }
        s
    };
    let size = |v: &serde_json::Value| serde_json::to_string(v).map(|s| s.len()).unwrap_or(0) + 1;
    let callers = |i: usize| active[i]["caller_count"].as_i64().unwrap_or(0);
    let active_order = budget::order_by_importance(na, |i| (callers(i), Reverse(i)));
    let shift = |v: Vec<usize>, by: usize| v.into_iter().map(|x| x + by).collect::<Vec<_>>();
    let mut lower: Vec<Vec<usize>> = names
        .iter()
        .enumerate()
        .map(|(g, list)| shift(budget::order_by_importance(list.len(), Reverse), offset[g]))
        .collect();
    let u = |v: &serde_json::Value, k: &str| v[k].as_u64().unwrap_or(0);
    lower.push(shift(
        budget::order_by_importance(deps.len(), |i| {
            (
                Reverse(u(&deps[i], "depth")),
                u(&deps[i], "symbols"),
                Reverse(i),
            )
        }),
        od,
    ));
    lower.push(shift(budget::order_by_importance(dead.len(), Reverse), ox));
    let mut order = budget::interleave(&lower);
    order.extend(active_order.iter().copied());
    order.extend(shift(
        budget::order_by_importance(hot.len(), |i| (u(&hot[i], "caller_count"), Reverse(i))),
        oh,
    ));
    let has_skeleton = |u: usize| u < na;
    let steps = budget::standard_steps(&order, has_skeleton);

    // One file's share first.
    let mut initial = vec![Level::Full; n];
    let share = budget::budget_bytes(tokens) * budget::FILE_SHARE_PERCENT / 100;
    let mut by_file: std::collections::BTreeMap<&str, Vec<usize>> =
        std::collections::BTreeMap::new();
    for &i in &active_order {
        by_file
            .entry(active[i]["file"].as_str().unwrap_or(""))
            .or_default()
            .push(i);
    }
    for units in by_file.values() {
        budget::cap_group(
            &mut initial,
            units,
            has_skeleton,
            |u, l| match l {
                Level::Full => size(&active[u]),
                Level::Skeleton => size(&skeleton(&active[u])),
                Level::Dropped => 0,
            },
            share,
        );
    }
    let past_share = initial.iter().filter(|l| **l != Level::Full).count();

    let render = |levels: &[Level], over_budget: bool| -> serde_json::Value {
        let keep = |items: &[serde_json::Value], off: usize| -> Vec<serde_json::Value> {
            items
                .iter()
                .enumerate()
                .filter(|(i, _)| levels[off + i] != Level::Dropped)
                .map(|(_, v)| v.clone())
                .collect()
        };
        let dropped =
            |r: std::ops::Range<usize>| r.filter(|&x| levels[x] == Level::Dropped).count();
        let mut out = full.clone();
        out["active_exports"] = json!(active
            .iter()
            .enumerate()
            .filter(|(i, _)| levels[*i] != Level::Dropped)
            .map(|(i, e)| if levels[i] == Level::Skeleton {
                skeleton(e)
            } else {
                e.clone()
            })
            .collect::<Vec<_>>());
        let mut names_omitted = 0usize;
        let new_groups: Vec<serde_json::Value> = groups
            .iter()
            .enumerate()
            .map(|(g, grp)| {
                let kept = keep(&names[g], offset[g]);
                let mut o = grp.clone();
                let total = grp["count"].as_u64().unwrap_or(names[g].len() as u64) as usize;
                names_omitted += names[g].len() - kept.len();
                o["names"] = json!(kept);
                if let Some(obj) = o.as_object_mut() {
                    obj.remove("more");
                }
                if total > kept.len() {
                    o["more"] = json!(total - kept.len());
                }
                o
            })
            .collect();
        out["inactive_summary"] = json!(new_groups);
        if full.get("hot_paths").is_some() {
            out["hot_paths"] = json!(keep(&hot, oh));
        }
        if full["dependencies"].is_object() {
            out["dependencies"]["depends_on"] = json!(keep(&deps_out, od));
            out["dependencies"]["depended_by"] = json!(keep(&deps_in, od + deps_out.len()));
        }
        if full["dead_code"].is_object() {
            out["dead_code"]["results"] = json!(keep(&dead, ox));
        }
        let active_omitted = dropped(0..na);
        let short = (0..na).filter(|&i| levels[i] == Level::Skeleton).count();
        let hot_omitted = dropped(oh..od);
        let deps_omitted = dropped(od..ox);
        let dead_omitted = dropped(ox..n);
        let overview_cut = active_omitted + names_omitted + short + hot_omitted > 0;
        if overview_cut || deps_omitted + dead_omitted > 0 || over_budget {
            let mut b = json!({ "max_tokens": tokens });
            let mut omitted = serde_json::Map::new();
            for (k, c) in [
                ("active_exports", active_omitted),
                ("inactive_names", names_omitted),
                ("hot_paths", hot_omitted),
                ("dependencies", deps_omitted),
                ("dead_code", dead_omitted),
            ] {
                if c > 0 {
                    omitted.insert(k.into(), json!(c));
                }
            }
            if !omitted.is_empty() {
                b["omitted"] = serde_json::Value::Object(omitted);
            }
            if short > 0 {
                b["active_exports_without_signature"] = json!(short);
            }
            if past_share > 0 {
                b["cut_for_file_share"] = json!(past_share);
            }
            if over_budget {
                b["over_budget"] = json!(true);
            }
            let mut cmds = Vec::new();
            if overview_cut || deps_omitted + dead_omitted == 0 {
                cmds.push(next.overview.to_string());
            }
            if deps_omitted > 0 {
                cmds.push(next.dependencies.to_string());
            }
            if dead_omitted > 0 {
                cmds.push(next.dead_code.to_string());
            }
            b["next"] = json!(cmds.join("; "));
            out["budget"] = b;
        }
        out
    };
    let fitted = budget::fit(&initial, &steps, budget::budget_bytes(tokens), |levels| {
        let v = render(levels, false);
        let len = serde_json::to_string(&v).map(|s| s.len()).unwrap_or(0);
        (v, len)
    });
    if fitted.over_budget {
        // Even with every unit left out the fixed part is larger than asked:
        // say so, rather than pass as sized.
        return render(&fitted.levels, true);
    }
    fitted.output
}
