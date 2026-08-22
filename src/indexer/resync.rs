//! Query-time freshness resync — the single implementation shared by every
//! read surface (the CLI's `refresh_files_if_stale`, `grep`'s AST annotations,
//! and the MCP server's result-set refresh).
//!
//! Before this module the same predicate — `SELECT blake3_hash` for the path,
//! `hash_file` the bytes, re-index on mismatch, decrement a budget — was
//! transcribed line-for-line in three places (audit 2026-08-22 P1-3). Three
//! copies of one rule is this repository's most-hit bug class: a fix lands in
//! one copy and the other two keep the defect.
//!
//! The copies also re-indexed one file at a time, and each of those calls
//! re-hashed the file the caller had just hashed and then paid a whole-graph
//! `get_all_node_names_with_ids` plus the global edge post-passes inside
//! `index_files`. A budget of 8 therefore cost up to eight whole-graph sweeps
//! for one query. Here the dirty set is classified once and handed to
//! [`apply_file_refreshes`] as ONE batch, which is what the indexer's
//! file-list interface was always for.

use std::collections::HashSet;
use std::path::Path;

use crate::indexer::pipeline::{
    apply_file_refreshes, plan_file_refresh, FileRefresh, RefreshScope,
};
use crate::storage::db::Database;

/// Files re-indexed for one query. Bounds interactive latency: a common name
/// spanning many edited files must not turn a read command into an index run.
pub const RESYNC_BUDGET: usize = 8;

/// Short busy_timeout for a query-time refresh, so a concurrent writer
/// (watcher, background embedding, another index run) cannot stall a read
/// command for the connection's default 5s.
pub const RESYNC_BUSY_TIMEOUT_MS: u32 = 250;

/// The resync budget, honouring `CODE_GRAPH_RESYNC_BUDGET` — one knob for
/// every surface.
pub fn resync_budget() -> usize {
    resync_budget_named("CODE_GRAPH_RESYNC_BUDGET")
}

/// As [`resync_budget`], but reads `name` first and falls back to the shared
/// knob. Exists for `grep`, which shipped `CODE_GRAPH_GREP_SYNC_BUDGET` before
/// the surfaces were unified: dropping that name outright would silently stop
/// honouring an override someone may already have set.
pub fn resync_budget_named(name: &str) -> usize {
    if let Some(v) = std::env::var(name).ok().and_then(|v| v.parse().ok()) {
        return v;
    }
    std::env::var("CODE_GRAPH_RESYNC_BUDGET")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(RESYNC_BUDGET)
}

/// Result of one resync sweep.
#[derive(Default, Debug, Clone)]
pub struct ResyncOutcome {
    /// Dirty files whose index rows were refreshed within budget.
    pub refreshed: usize,
    /// Dirty files left stale because the budget was exhausted.
    pub skipped_over_budget: usize,
    /// Dirty files that could not be refreshed (unreadable file, write
    /// contention) — their rows are kept as they were, never worse than before.
    pub failed: usize,
    /// The paths behind `skipped_over_budget` + `failed`, for callers that
    /// annotate per file (`grep` marks these `[stale]`).
    pub stale_paths: HashSet<String>,
}

impl ResyncOutcome {
    /// At least one displayed file was refreshed → a caller that already ran
    /// its query must re-run it, because those line numbers moved.
    pub fn any_changed(&self) -> bool {
        self.refreshed > 0
    }

    /// Some displayed file stayed stale, so its printed line numbers may
    /// predate the last edit.
    pub fn is_partial(&self) -> bool {
        self.skipped_over_budget > 0 || self.failed > 0
    }
}

/// Re-index the files in `paths` whose on-disk bytes no longer match the index,
/// at most `budget` of them, in one batch.
///
/// `paths` must be the POST-limit result set (what the command will actually
/// print), not the whole index. Entries that were never indexed are skipped on
/// purpose: indexing a brand-new path from a read command would pull
/// gitignored supplement files into the index, diverging from
/// `scan_directory`'s scope. Non-file entries (a `project_map` module path, the
/// `<external>` sentinel) fall out of the same checks as harmless no-ops.
///
/// Failure is always "keep the stale row and report it" — a read command must
/// never become an error because of a refresh it only attempted as a courtesy.
pub fn resync_stale_files(
    db: &Database,
    root: &Path,
    paths: &[String],
    budget: usize,
) -> ResyncOutcome {
    let mut outcome = ResyncOutcome::default();

    let mut files: Vec<&str> = paths.iter().map(String::as_str).collect();
    files.sort_unstable();
    files.dedup();
    if files.is_empty() {
        return outcome;
    }

    // Pass 1 — classify. This is the only place the bytes are hashed; the hash
    // travels with the plan into the indexer.
    let mut drop_rows: Vec<String> = Vec::new();
    let mut reindex: Vec<(String, String)> = Vec::new();
    for f in files {
        match plan_file_refresh(db, root, f, RefreshScope::IndexedOnly) {
            Ok(FileRefresh::Fresh) => {}
            Ok(FileRefresh::DropStaleRow) => drop_rows.push(f.to_string()),
            Ok(FileRefresh::Reindex(hash)) => reindex.push((f.to_string(), hash)),
            // Indexed but unreadable now (EACCES / EIO — a file that is merely
            // GONE is planned as DropStaleRow, not an error). Nothing can be
            // refreshed, so say it is stale rather than imply it is current.
            Err(_) => {
                outcome.failed += 1;
                outcome.stale_paths.insert(f.to_string());
            }
        }
    }
    if drop_rows.is_empty() && reindex.is_empty() {
        return outcome;
    }

    // Pass 2 — spend the budget. It bounds re-indexing, the expensive half;
    // dropping the rows of deleted files is a single statement and would leave
    // phantom nodes in the answer if it were deferred.
    if reindex.len() > budget {
        for (f, _) in reindex.drain(budget..) {
            outcome.skipped_over_budget += 1;
            outcome.stale_paths.insert(f);
        }
    }

    // Pass 3 — one batched write for everything that survived. A batch fails as
    // a unit (it runs inside one savepoint), which is also the honest report:
    // every file in it is still stale.
    let changed = drop_rows.len() + reindex.len();
    match apply_file_refreshes(db, root, &drop_rows, &reindex, None) {
        Ok(()) => outcome.refreshed += changed,
        Err(_) => {
            outcome.failed += changed;
            outcome.stale_paths.extend(drop_rows);
            outcome
                .stale_paths
                .extend(reindex.into_iter().map(|(f, _)| f));
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::pipeline::run_full_index;
    use crate::storage::db::Database;
    use tempfile::TempDir;

    struct Fixture {
        _tmp: TempDir,
        root: std::path::PathBuf,
        db: Database,
    }

    /// `n` indexed Rust files, each with one function.
    fn indexed_project(n: usize) -> Fixture {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::create_dir_all(root.join("src")).unwrap();
        for i in 0..n {
            std::fs::write(
                root.join(format!("src/f{i}.rs")),
                format!("pub fn f{i}() -> i32 {{ {i} }}\n"),
            )
            .unwrap();
        }
        std::fs::create_dir_all(root.join(".code-graph")).unwrap();
        let db = Database::open(&root.join(".code-graph/graph.db")).unwrap();
        run_full_index(&db, &root, None, None).unwrap();
        Fixture {
            _tmp: tmp,
            root,
            db,
        }
    }

    fn touch(root: &std::path::Path, i: usize) {
        std::fs::write(
            root.join(format!("src/f{i}.rs")),
            format!("// edited\npub fn f{i}() -> i32 {{ {i} }}\n"),
        )
        .unwrap();
    }

    fn paths(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("src/f{i}.rs")).collect()
    }

    #[test]
    fn an_untouched_result_set_is_a_no_op() {
        let f = indexed_project(3);
        let out = resync_stale_files(&f.db, &f.root, &paths(3), RESYNC_BUDGET);
        assert_eq!(out.refreshed, 0);
        assert!(!out.any_changed());
        assert!(!out.is_partial());
        assert!(out.stale_paths.is_empty());
    }

    #[test]
    fn dirty_files_are_refreshed_and_move_their_line_numbers() {
        let f = indexed_project(3);
        touch(&f.root, 1);
        let out = resync_stale_files(&f.db, &f.root, &paths(3), RESYNC_BUDGET);
        assert_eq!(out.refreshed, 1, "only the edited file is dirty");
        assert!(out.any_changed());
        assert!(!out.is_partial());
        // The edit prepended a line — the index must agree, which is the whole
        // point of the resync.
        let line: i64 =
            f.db.conn()
                .query_row(
                    "SELECT n.start_line FROM nodes n JOIN files fl ON fl.id = n.file_id
                 WHERE fl.path = 'src/f1.rs' AND n.name = 'f1'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(line, 2, "re-indexed start_line must reflect the edit");
    }

    #[test]
    fn the_budget_bounds_the_reindex_and_names_what_stayed_stale() {
        let f = indexed_project(5);
        for i in 0..5 {
            touch(&f.root, i);
        }
        let out = resync_stale_files(&f.db, &f.root, &paths(5), 2);
        assert_eq!(out.refreshed, 2, "budget caps the re-index at 2");
        assert_eq!(out.skipped_over_budget, 3);
        assert!(out.is_partial());
        assert_eq!(
            out.stale_paths.len(),
            3,
            "every skipped file must be named so callers can mark it [stale]: {:?}",
            out.stale_paths
        );
        // Sorted candidates, so the survivors are deterministic — a budget that
        // picked a different subset run to run would make `grep`'s [stale]
        // annotations flicker.
        assert!(out.stale_paths.contains("src/f2.rs"));
        assert!(out.stale_paths.contains("src/f4.rs"));
    }

    #[test]
    fn a_path_that_was_never_indexed_is_left_alone() {
        let f = indexed_project(1);
        std::fs::write(f.root.join("src/new.rs"), "pub fn brand_new() {}\n").unwrap();
        let out = resync_stale_files(
            &f.db,
            &f.root,
            &["src/new.rs".to_string(), "<external>".to_string()],
            RESYNC_BUDGET,
        );
        assert_eq!(out.refreshed, 0, "a read command must not widen the index");
        assert!(!out.is_partial());
        let indexed: i64 =
            f.db.conn()
                .query_row(
                    "SELECT COUNT(*) FROM files WHERE path = 'src/new.rs'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(indexed, 0);
    }

    #[test]
    fn a_deleted_file_loses_its_stale_rows() {
        let f = indexed_project(2);
        std::fs::remove_file(f.root.join("src/f0.rs")).unwrap();
        let out = resync_stale_files(&f.db, &f.root, &paths(2), RESYNC_BUDGET);
        assert_eq!(out.refreshed, 1);
        let rows: i64 =
            f.db.conn()
                .query_row(
                    "SELECT COUNT(*) FROM files WHERE path = 'src/f0.rs'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(rows, 0, "phantom nodes must not survive the file");
    }

    #[test]
    fn the_grep_knob_still_wins_over_the_shared_one() {
        // Both names have to keep working: `CODE_GRAPH_GREP_SYNC_BUDGET` shipped
        // first and someone may have it set.
        let prior_grep = std::env::var("CODE_GRAPH_GREP_SYNC_BUDGET").ok();
        let prior_shared = std::env::var("CODE_GRAPH_RESYNC_BUDGET").ok();
        std::env::set_var("CODE_GRAPH_RESYNC_BUDGET", "3");
        assert_eq!(resync_budget(), 3);
        assert_eq!(resync_budget_named("CODE_GRAPH_GREP_SYNC_BUDGET"), 3);
        std::env::set_var("CODE_GRAPH_GREP_SYNC_BUDGET", "1");
        assert_eq!(resync_budget_named("CODE_GRAPH_GREP_SYNC_BUDGET"), 1);
        match prior_grep {
            Some(v) => std::env::set_var("CODE_GRAPH_GREP_SYNC_BUDGET", v),
            None => std::env::remove_var("CODE_GRAPH_GREP_SYNC_BUDGET"),
        }
        match prior_shared {
            Some(v) => std::env::set_var("CODE_GRAPH_RESYNC_BUDGET", v),
            None => std::env::remove_var("CODE_GRAPH_RESYNC_BUDGET"),
        }
    }
}
