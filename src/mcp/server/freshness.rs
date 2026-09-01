//! Query-time freshness for the MCP surface.
//!
//! Two mechanisms, and the distinction is the whole design: a tool CALLED with
//! an explicit `file_path` refreshes that file before answering
//! ([`McpServer::ensure_file_fresh_opt`]), while a tool whose answer is a set
//! of other files' symbols can only know which files matter after the query has
//! run, so it refreshes the RESULT SET and re-runs
//! ([`McpServer::refresh_result_set`]).
//!
//! Split out of `server/mod.rs` (audit 2026-08-22 P2-8), which had grown to
//! ~2,850 lines of production code across startup indexing, cache invalidation,
//! lock recovery, backfill, dispatch and this. The predicate and the budget
//! themselves live one layer down in `crate::indexer::resync`, shared with the
//! CLI; what is here is what only the server has — the promoted write handle,
//! the busy-timeout guard on a long-lived connection, and the cache
//! invalidation that must follow a refresh.

use super::{lock_or_recover, McpServer};
use anyhow::Result;
use serde_json::json;
use std::path::Path;

/// Tools that return file/line data but take no `file_path` argument, so
/// [`McpServer::ensure_file_fresh_opt`] cannot cover them (FRS-2).
/// `find_http_route` is the alias of `trace_http_chain`.
///
/// `get_call_graph` and `find_references` were the two gaps (audit 2026-08-22
/// P2-11): both DO accept a `file_path`, so they looked covered, but that
/// argument is optional and disambiguates same-name symbols — the ordinary
/// call passes a bare symbol name and reached
/// [`McpServer::ensure_file_fresh_opt`]'s `None` early return. The answer they
/// give is a set of OTHER files' symbols, so what goes stale after an edit is
/// not the named file but the callers and references the query found. Listing
/// them here refreshes by result set instead, which is what the result set
/// needs; the two mechanisms overlap harmlessly when a `file_path` IS passed
/// (the second pass finds that file already fresh).
pub(super) const RESULT_REFRESH_TOOLS: &[&str] = &[
    "semantic_code_search",
    "ast_search",
    "project_map",
    "find_similar_code",
    "trace_http_chain",
    "find_http_route",
    "get_call_graph",
    "find_references",
    // These two DO take a `path`, and both call `ensure_file_fresh_opt` with it —
    // but that is a FILE refresher and the path they are called with is a
    // directory (or nothing). A directory is classified fresh, so the call was a
    // no-op: `did_reindex` stayed false, the 60s overview cache was never
    // evicted, and the answer carried pre-edit line numbers with no disclosure.
    // They were the only two MCP read surfaces that could silently answer from a
    // pre-edit index, and `freshness_parity.rs` counted that no-op call as
    // coverage (audit 2026-08-29 CON-02). The file-path leg stays: its overlap
    // with result-set refresh is documented as harmless.
    "module_overview",
    "find_dead_code",
];

/// Max files hashed for one result set before the rest is reported unchecked.
const RESULT_REFRESH_SCAN_CAP: usize = 32;
/// Short busy_timeout for the refresh, so a concurrent writer cannot stall a
/// tool call. Restored to the connection default afterwards.
const RESULT_REFRESH_BUSY_TIMEOUT_MS: u32 = crate::indexer::resync::RESYNC_BUSY_TIMEOUT_MS;
/// Connection default set by `Database::open` — what the guard restores.
const DEFAULT_BUSY_TIMEOUT_MS: u32 = 5000;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct ResultRefreshOutcome {
    pub(super) refreshed: usize,
    pub(super) failed: usize,
    pub(super) skipped_over_budget: usize,
}

/// Restores the connection's default `busy_timeout` when dropped.
struct BusyTimeoutGuard<'a>(&'a rusqlite::Connection);

impl<'a> BusyTimeoutGuard<'a> {
    fn apply(conn: &'a rusqlite::Connection, ms: u32) -> Self {
        let _ = conn.execute_batch(&format!("PRAGMA busy_timeout = {ms};"));
        Self(conn)
    }
}

impl Drop for BusyTimeoutGuard<'_> {
    fn drop(&mut self) {
        let _ = self
            .0
            .execute_batch(&format!("PRAGMA busy_timeout = {DEFAULT_BUSY_TIMEOUT_MS};"));
    }
}

/// Collect every file-ish path string in a tool result.
///
/// Keys mirror what the handlers emit: `file_path` (search / ast_search /
/// similar / trace), `file` (project_map hotspots + entrypoints) and `path`
/// (project_map modules — a directory, filtered out downstream by the
/// "must already be an indexed file" rule).
pub(super) fn collect_result_paths(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                if matches!(k.as_str(), "file_path" | "file" | "path") {
                    if let Some(s) = v.as_str() {
                        if !s.is_empty() && !s.starts_with('<') {
                            out.push(s.to_string());
                        }
                    }
                }
                collect_result_paths(v, out);
            }
        }
        serde_json::Value::Array(items) => {
            for v in items {
                collect_result_paths(v, out);
            }
        }
        _ => {}
    }
}

impl McpServer {
    /// Sync-reindex a single file when its on-disk hash differs from the stored
    /// hash. Closes the post-Edit→pre-incremental-index staleness window for
    /// MCP tools that take an explicit `file_path`.
    ///
    /// No-op when:
    ///
    /// - this instance is a read-only secondary (only the primary holds the write capability),
    /// - `path` is `None` / empty / a directory-shaped path (caller doesn't know which file to refresh),
    /// - the on-disk hash already matches the stored hash.
    ///
    /// Reindex caches are invalidated only when the call actually re-indexed.
    pub(super) fn ensure_file_fresh_opt(&self, path: Option<&str>) -> Result<()> {
        self.ensure_file_fresh_reported(path).map(|_| ())
    }

    /// [`Self::ensure_file_fresh_opt`], reporting whether it actually re-indexed.
    ///
    /// CON-10: `get_ast_node(node_id)` needs the answer, not just the effect.
    /// `nodes.id` is a bare `INTEGER PRIMARY KEY` — a rowid alias with no
    /// AUTOINCREMENT — and an incremental re-index deletes and re-inserts the
    /// file's rows, so a re-index can hand the caller's stale id to a DIFFERENT
    /// symbol. A caller holding a node_id across a refresh must therefore
    /// re-resolve by identity, and it can only know to do that if the refresh
    /// says whether it fired.
    pub(super) fn ensure_file_fresh_reported(&self, path: Option<&str>) -> Result<bool> {
        if !self.is_primary() {
            return Ok(false);
        }
        let Some(rel_path) = path else {
            return Ok(false);
        };
        // Belt-and-braces: every `tool_*` entry point now normalizes its path
        // argument via `tools::normalize_path_arg` before calling here, so the
        // freshness target and the subsequent index lookup key are the same
        // string. This repeat is idempotent and keeps the leaf correct for any
        // future caller that forgets. Normalizing BEFORE the trailing check also
        // makes `src\` register as the directory it is.
        let rel_path = crate::indexer::merkle::normalize_rel_str(rel_path);
        if rel_path.is_empty() || rel_path.ends_with('/') {
            return Ok(false);
        }
        let rel_path = rel_path.as_str();
        let Some(root) = self.project_root.as_deref() else {
            return Ok(false);
        };

        let did_reindex = {
            let model_guard = lock_or_recover(&self.embedding_model, "embedding_model");
            let write_db = self.write_db();
            crate::indexer::pipeline::ensure_file_indexed(
                &write_db,
                root,
                rel_path,
                model_guard.as_ref(),
            )?
        };
        if did_reindex {
            *lock_or_recover(&self.cache.cached_project_map, "cached_pmap") = None;
            lock_or_recover(&self.cache.cached_module_overviews, "cached_movw").clear();
            tracing::debug!(
                "[fresh] sync-reindexed {} on query-time freshness",
                rel_path
            );
        }
        Ok(did_reindex)
    }
    /// FRS-2: re-index the files a result set points at, and re-run the tool if
    /// any of them actually changed.
    ///
    /// Tools that take a `file_path` argument close the post-Edit window with
    /// `ensure_file_fresh_opt`. The tools in [`RESULT_REFRESH_TOOLS`] have no
    /// such argument: which files matter is only known once the query has run.
    /// Watcher delivery is asynchronous, so a query issued immediately after an
    /// Edit legitimately sees `drain_watcher_events() == false` and answers with
    /// pre-edit line numbers — previously with no disclosure at all.
    ///
    /// Budget and failure policy deliberately mirror the CLI's
    /// `refresh_files_if_stale`: at most [`crate::indexer::resync::RESYNC_BUDGET`] files per
    /// call, a short `busy_timeout` so a concurrent writer can't stall the tool,
    /// and stale data KEPT (never dropped) with a disclosure whenever the budget
    /// or an error prevents the refresh.
    pub(super) fn refresh_result_set(
        &self,
        name: &str,
        args: &serde_json::Value,
        value: serde_json::Value,
    ) -> serde_json::Value {
        // The caller said not to index. `HONORED_UNDECLARED_ARGS` states that
        // `skip_indexing` is read by every tool through `should_skip_indexing`,
        // and every tool's own dispatch arm does read it — but FRS-2 arrived
        // later and wraps those arms from OUTSIDE, so the flag bought nothing:
        // eight tools still took a write handle, ran a resync and re-dispatched
        // (audit 2026-08-29 CON-01).
        //
        // The gate lives HERE rather than at the `handle_tool` call site the
        // report suggested: a guard that lives in the caller is a guard the next
        // caller does not inherit, and this function's whole job is the work the
        // flag forbids.
        if super::helpers::should_skip_indexing(args) {
            return value;
        }
        // Secondaries hold a read-only DB; nothing to refresh with.
        if !self.is_primary() {
            return value;
        }
        let Some(root) = self.project_root.clone() else {
            return value;
        };
        let mut paths = Vec::new();
        collect_result_paths(&value, &mut paths);
        paths.sort_unstable();
        paths.dedup();
        // Bound the hashing cost on large result sets. Anything past the cap is
        // left unchecked and said so — silently checking a prefix would be the
        // same "false clean" the disclosure exists to avoid.
        let unchecked = paths.len().saturating_sub(RESULT_REFRESH_SCAN_CAP);
        paths.truncate(RESULT_REFRESH_SCAN_CAP);
        if paths.is_empty() {
            return value;
        }

        let outcome = self.reindex_stale_result_files(&root, &paths);
        let mut value = if outcome.refreshed > 0 {
            // Line numbers/snippets in the old result are now wrong; re-run once.
            // A failing re-run keeps the first (stale) answer rather than turning
            // a working query into an error — disclosed below.
            match self.dispatch_tool(name, args) {
                Ok(fresh) => fresh,
                Err(e) => {
                    tracing::warn!("[fresh] re-run of {} after refresh failed: {}", name, e);
                    value
                }
            }
        } else {
            value
        };

        let stale_kept = outcome.failed + outcome.skipped_over_budget + unchecked;
        if stale_kept > 0 {
            if let Some(obj) = value.as_object_mut() {
                obj.insert(
                    "freshness".to_string(),
                    json!({
                        "refreshed": outcome.refreshed,
                        "stale_kept": stale_kept,
                        "note": "Some files in this result changed on disk and were not re-indexed \
                                 (per-call budget or a busy database). Their line numbers and \
                                 snippets may predate your last edit — re-run the query, or pass \
                                 an explicit file_path tool for those files.",
                    }),
                );
            }
        }
        value
    }
    /// Re-index result-set files whose on-disk hash no longer matches the index.
    ///
    /// The predicate, the budget policy and the batching all live in
    /// [`crate::indexer::resync`] — this surface adds only the promoted write
    /// handle, the busy-timeout guard, and the cache invalidation.
    fn reindex_stale_result_files(&self, root: &Path, paths: &[String]) -> ResultRefreshOutcome {
        let refreshed;
        let resync;
        {
            // Scoped so the promoted-DB handle (lock level 7) is released before the
            // cache invalidation below takes the two cache mutexes (level 5) — the
            // sibling of the same ordering inversion fixed in
            // `run_incremental_with_cache_restore` (2026-08-16 audit §四).
            let write_db = self.write_db();
            // Never let a concurrent writer (background embedding, another index run)
            // stall a tool call for the default 5s busy_timeout — fail fast and keep
            // the stale row. Restored on drop: this connection is long-lived, unlike
            // the CLI's short process where the same PRAGMA is set and forgotten.
            let _busy = BusyTimeoutGuard::apply(write_db.conn(), RESULT_REFRESH_BUSY_TIMEOUT_MS);
            resync = crate::indexer::resync::resync_stale_files(
                &write_db,
                root,
                paths,
                self.result_refresh_budget,
                crate::indexer::pipeline::RefreshScope::IndexedOnly,
            );
            refreshed = resync.refreshed;
        }
        if refreshed > 0 {
            *lock_or_recover(&self.cache.cached_project_map, "cached_pmap") = None;
            lock_or_recover(&self.cache.cached_module_overviews, "cached_movw").clear();
        }
        ResultRefreshOutcome {
            refreshed: resync.refreshed,
            failed: resync.failed,
            skipped_over_budget: resync.skipped_over_budget,
        }
    }
}
