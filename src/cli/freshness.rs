use super::*;

/// Outcome of a query-time freshness resync (`refresh_files_if_stale`) over the
/// files a read command is about to print. Callers re-run their query when
/// `any_changed` and call `disclose()` to honestly surface a partial refresh.
#[derive(Default, Debug)]
pub(crate) struct FreshOutcome {
    /// At least one displayed file was dirty and successfully re-indexed → the
    /// query must re-run so line numbers reflect the post-edit index.
    pub(crate) any_changed: bool,
    /// Dirty files re-indexed within budget.
    refreshed: usize,
    /// Dirty files left stale because the reindex budget was exhausted.
    skipped_over_budget: usize,
    /// Dirty files whose reindex failed (write contention / parse error) — kept
    /// stale, never worse than before.
    failed: usize,
}

impl FreshOutcome {
    /// Some displayed files stayed stale (budget exhausted or reindex failed),
    /// so the printed line numbers for those files may be pre-edit.
    fn is_partial(&self) -> bool {
        self.skipped_over_budget > 0 || self.failed > 0
    }

    /// One-line honest disclosure when the refresh was only partial. stderr only
    /// — stdout carries the JSON/text contract and must not be polluted — and
    /// dual-written to `tracing` per the project's user-facing-warning rule
    /// (`feedback_tracing_invisible_in_cli`). No-op when everything was fresh or
    /// fully refreshed.
    pub(crate) fn disclose(&self) {
        if !self.is_partial() {
            return;
        }
        let stale = self.skipped_over_budget + self.failed;
        let msg = format!(
            "{} file(s) changed since indexing; refreshed {}, line numbers for the rest may be stale (rerun after 'code-graph-mcp incremental-index')",
            stale, self.refreshed
        );
        eprintln!("[code-graph] note: {msg}");
        tracing::warn!("cli freshness partial: {msg}");
    }

    /// In-band partial-freshness marker for OBJECT-shaped `--json` outputs
    /// (roadmap 2026-07-18 §1.4): the stderr note above is invisible under
    /// `--json 2>/dev/null`, so envelope emitters attach `freshness_partial:
    /// true` when some displayed files stayed stale. Array-shaped outputs
    /// (search/show/overview/similar/dead-code) cannot carry a top-level field
    /// without breaking their success shape — for those the stderr note remains
    /// the only channel (documented boundary). No-op when fully fresh.
    pub(crate) fn attach_partial(&self, obj: &mut serde_json::Value) {
        if self.is_partial() {
            obj["freshness_partial"] = serde_json::json!(true);
        }
    }
}

/// Query-time freshness resync shared by the read commands that print
/// `start_line`/`end_line` straight from the index (`show`, `refs`, `overview`,
/// `search`, `ast-search`, `trace`, `similar`, `impact`, `dead-code`). Semantics
/// lifted from `cmd_show`'s original inline loop and the MCP tools'
/// `ensure_file_fresh_opt`: for each DISPLAYED file (dedup + sorted), hash-compare
/// against the index and re-index the dirty ones through `ensure_file_indexed` so
/// their line numbers reflect the post-edit source.
///
/// Bounded (8-file reindex budget — overridable via `CODE_GRAPH_RESYNC_BUDGET`
/// for tests — plus a 250ms busy_timeout) so a common name spanning many dirty
/// files can't stall an interactive command; on write contention / parse failure
/// the stale node is kept, exactly the pre-resync behavior, never worse. `paths`
/// must be the POST-limit result set (what the command will print), not the whole
/// index. Callers re-run their query when the outcome reports `any_changed`.
pub(crate) fn refresh_files_if_stale(db: &Database, root: &Path, paths: &[String]) -> FreshOutcome {
    let mut outcome = FreshOutcome::default();
    let conn = db.conn();
    // Never let a concurrent writer (MCP watcher, another index run) stall an
    // interactive command for the default 5s busy_timeout — fail fast, keep stale.
    let _ = conn.execute_batch("PRAGMA busy_timeout = 250;");

    let mut files: Vec<&str> = paths.iter().map(String::as_str).collect();
    files.sort_unstable();
    files.dedup();

    let mut budget: usize = std::env::var("CODE_GRAPH_RESYNC_BUDGET")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8);

    for f in files {
        // Only files already in the index are candidates (parity with cmd_grep):
        // indexing a brand-new path here could pull gitignored supplement files
        // into the index, diverging from scan_directory's scope.
        let stored: Option<String> = conn
            .query_row("SELECT blake3_hash FROM files WHERE path = ?1", [f], |r| {
                r.get(0)
            })
            .ok();
        let Some(stored_hash) = stored else { continue };
        let abs = root.join(f);
        if crate::indexer::merkle::hash_file(&abs).ok().as_deref() == Some(stored_hash.as_str()) {
            continue; // already fresh
        }
        // Dirty from here down.
        if budget == 0 {
            outcome.skipped_over_budget += 1;
            continue;
        }
        match crate::indexer::pipeline::ensure_file_indexed(db, root, f, None) {
            Ok(true) => {
                outcome.any_changed = true;
                outcome.refreshed += 1;
                budget -= 1;
            }
            // Hash differed but the reindex reported no node change — nothing
            // stale to re-query or disclose.
            Ok(false) => {}
            // SQLITE_BUSY / parse failure: keep the stale node, disclose below.
            Err(_) => outcome.failed += 1,
        }
    }
    outcome
}
