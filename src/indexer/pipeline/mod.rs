//! Indexer pipeline. Public entry points + per-concern submodules:
//! - `embed`: batch embedding store
//! - `context`: context-string assembly + recovery paths
//! - `python_modules`: dotted-path → file-path resolution map
//! - `resolve`: ambiguous-target refinement + pending-call sweep
//! - `index_files`: the giant Phase-0..3 orchestrator (kept whole — its
//!   phases share local transaction/atomics/batch state)

use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::embedding::model::EmbeddingModel;
use crate::indexer::merkle::{compute_diff, scan_directory, scan_directory_cached, DirectoryCache};
use crate::storage::db::Database;
use crate::storage::queries::{get_all_file_hashes, get_dirty_node_ids};

mod context;
mod embed;
mod index_files;
mod js_modules;
mod python_modules;
mod resolve;

#[cfg(test)]
mod tests;

pub use context::repair_null_context_strings;
pub use embed::embed_and_store_batch;

use context::regenerate_context_strings;
use index_files::index_files;

/// Counters for indexing observability — tracks skipped items.
#[derive(Debug, Clone, Default)]
pub struct IndexStats {
    pub files_skipped_size: usize,
    pub files_skipped_parse: usize,
    pub files_skipped_read: usize,
    pub files_skipped_hash: usize,
    pub files_skipped_language: usize,
    /// Files that parsed into a tree carrying tree-sitter ERROR node(s). Unlike the
    /// `files_skipped_*` counters, these files WERE indexed — tree-sitter recovers
    /// from syntax errors and still yields a tree — but symbol extraction ran over a
    /// damaged parse, so some symbols may be missing. Observability only.
    pub files_with_parse_errors: usize,
}

pub struct IndexResult {
    pub files_indexed: usize,
    pub files_deleted: usize,
    pub nodes_created: usize,
    pub edges_created: usize,
    pub stats: IndexStats,
}

/// Which stage of an index run a progress event describes.
/// `Files` fires after each parsed batch with a moving `done` count; `Finalizing`
/// fires as a heartbeat between the post-batch full-graph phases (context strings,
/// pending-call sweep, import bind/prune, ANALYZE) where `done` no longer moves.
/// Consumers use the distinction to render "finalizing" instead of a frozen
/// `done/total` — and the heartbeat itself keeps the progress file's mtime fresh
/// so a stale-file gate can tell "long tail phase" apart from "indexer died".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexPhase {
    Files,
    Finalizing,
}

/// Progress callback: called with (phase, files_done, files_total).
pub type ProgressFn<'a> = &'a dyn Fn(IndexPhase, usize, usize);

/// Name of the statusline progress file under `.code-graph/`, written by the MCP
/// server's startup indexing thread and read by the plugin statusline.
pub const INDEXING_STATUS_FILE: &str = "indexing-status.json";

/// Age past which `indexing-status.json` is a leftover from a killed process, not
/// a live indexer: live runs heartbeat at least once per batch / finalize phase,
/// orders of magnitude more often than this. The statusline applies the same
/// threshold (INDEXING_STALE_MS in statusline.js) before trusting the file.
pub const INDEXING_STATUS_STALE_SECS: u64 = 120;

/// Remove a leftover `.code-graph/indexing-status.json` older than
/// [`INDEXING_STATUS_STALE_SECS`]. A killed session (SIGKILL, session exit)
/// skips the IndexGuard drop that normally deletes the file, pinning the
/// statusline at a phantom "indexing N/M" forever. Safe against a live indexer:
/// its file is fresh and left alone, and even a racing removal is repaired by
/// the indexer's next heartbeat write.
pub fn remove_stale_indexing_status(project_root: &Path) {
    remove_indexing_status_older_than(
        project_root,
        std::time::Duration::from_secs(INDEXING_STATUS_STALE_SECS),
    );
}

/// Testable inner: remove the progress file when its mtime is at least `max_age`
/// old (or unreadable — an mtime we can't read cannot prove liveness).
pub fn remove_indexing_status_older_than(project_root: &Path, max_age: std::time::Duration) {
    let path = project_root
        .join(crate::domain::CODE_GRAPH_DIR)
        .join(INDEXING_STATUS_FILE);
    let Ok(meta) = std::fs::metadata(&path) else {
        return;
    };
    let stale = meta
        .modified()
        .ok()
        .and_then(|m| m.elapsed().ok())
        .map(|age| age >= max_age)
        .unwrap_or(true);
    if stale {
        let _ = std::fs::remove_file(&path);
    }
}

pub fn run_full_index(
    db: &Database,
    project_root: &Path,
    model: Option<&EmbeddingModel>,
    progress: Option<ProgressFn>,
) -> Result<IndexResult> {
    // Walk only — no pre-hashing. A full index diffs against nothing, and the
    // pipeline reads each file's bytes to parse it anyway, so hashing here made
    // every file a double full read (audit 2026-08-22 P2-16). `pre_parse_batch`
    // computes the hash from the bytes it already holds when the caller supplies
    // none; only files it never reads (over the size gate) pay a read to be
    // hashed, which is what they cost before too.
    let files: Vec<String> = crate::indexer::merkle::walk_indexable_files(project_root)?
        .into_iter()
        .map(|(rel, _abs)| rel)
        .collect();
    index_files(
        db,
        project_root,
        &files,
        &HashMap::new(),
        model,
        &[],
        progress,
    )
}

/// True when `rel_path` is a safe project-relative path: no absolute root and no
/// leading `..` that climbs above the project. [`ensure_file_indexed`] keys the
/// files table by this relative path, so anything else is meaningless as a key
/// *and* a path-traversal risk — the MCP `file_path` args reach the freshness
/// path (`ensure_file_fresh_opt`) without going through `normalize_user_path`.
pub(crate) fn is_safe_relative_path(rel_path: &str) -> bool {
    use std::path::Component;
    let mut depth: i32 = 0;
    for comp in Path::new(rel_path).components() {
        match comp {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            // An absolute root or a Windows prefix (`C:\`) escapes the project.
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

/// Reindex a single file when its on-disk hash differs from the stored hash.
/// No-op when the hashes match (or `rel_path` was never indexed in a way that
/// would currently reindex it). Returns true when a reindex (or stale-row
/// cleanup) actually fired.
///
/// Used by query-time freshness: when an MCP tool receives an explicit
/// `file_path` argument, the agent is signaling "I just edited this; please
/// answer against the current bytes." The 30s `last_incremental_check`
/// debounce in the server is too coarse for tight Edit→search loops.
///
/// A thin composition of [`plan_file_refresh`] and [`apply_file_refreshes`] so
/// this single-file entry point and the batched
/// [`crate::indexer::resync::resync_stale_files`] cannot drift apart.
pub fn ensure_file_indexed(
    db: &Database,
    project_root: &Path,
    rel_path: &str,
    model: Option<&EmbeddingModel>,
) -> Result<bool> {
    match plan_file_refresh(db, project_root, rel_path, RefreshScope::IncludeNew)? {
        FileRefresh::Fresh => Ok(false),
        FileRefresh::DropStaleRow => {
            apply_file_refreshes(db, project_root, &[rel_path.to_string()], &[], model)?;
            Ok(true)
        }
        FileRefresh::Reindex(hash) => {
            apply_file_refreshes(
                db,
                project_root,
                &[],
                &[(rel_path.to_string(), hash)],
                model,
            )?;
            Ok(true)
        }
    }
}

/// What a query-time refresh must do with one candidate path, decided without
/// writing anything. Separated from the write so a caller with many candidates
/// can classify them all, apply its budget, and then re-index the survivors in
/// ONE batch — see [`crate::indexer::resync`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileRefresh {
    /// Nothing to do: unsafe path, pseudo-file, not indexable, never indexed,
    /// or the bytes still match the index.
    Fresh,
    /// The file is gone from disk but still has an index row, which would keep
    /// serving phantom nodes.
    DropStaleRow,
    /// Dirty. Payload is the on-disk hash, carried through to the indexer so
    /// the file is not hashed a second time.
    Reindex(String),
}

/// Which paths a refresh is allowed to touch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshScope {
    /// Refresh whatever the path names, indexing it even if it is new to the
    /// index. What an explicit MCP `file_path` argument means: the agent named
    /// this file, so answer against its current bytes.
    IncludeNew,
    /// Only refresh paths the index already knows. What a result-set resync
    /// means: pulling a brand-new path in from a READ command would index
    /// gitignored supplement files, widening the index past
    /// `scan_directory`'s scope on nothing but a query.
    IndexedOnly,
}

/// Classify one path for [`apply_file_refreshes`]. Read-only.
pub fn plan_file_refresh(
    db: &Database,
    project_root: &Path,
    rel_path: &str,
    scope: RefreshScope,
) -> Result<FileRefresh> {
    // Defense-in-depth: rel_path is a project-relative DB key by contract, but a
    // caller that forwards an unnormalized client path (absolute, or `..` climbing
    // out of the project) must not make us stat/hash/index a file outside
    // project_root. Treat such a path as "nothing to refresh" — consistent with
    // the not-indexable early returns below, not a hard error.
    if !is_safe_relative_path(rel_path) {
        return Ok(FileRefresh::Fresh);
    }
    // `<external>` is a PSEUDO-file: the row that anchors sentinel nodes for
    // imports binding outside the project. It has no on-disk counterpart, so the
    // missing-file branch below classified it as deleted and dropped the row —
    // CASCADE taking every sentinel node and every edge into them with it. Any
    // read command that displays or resolves an external name reaches here
    // (`show HashMap` did it while printing "Symbol not found"), which makes a
    // read-only query destructive — the same defect class as an indexer-mode
    // open in a reader. A later incremental pass does not restore them, because
    // only a file whose CONTENT changed re-emits its import relations.
    if rel_path == crate::domain::EXTERNAL_FILE_PATH {
        return Ok(FileRefresh::Fresh);
    }
    let abs_path = project_root.join(rel_path);

    // Missing-file path: drop stale row so future queries don't return phantom nodes.
    if !abs_path.is_file() {
        let exists_in_db: Option<i64> = db
            .conn()
            .query_row("SELECT id FROM files WHERE path = ?1", [rel_path], |row| {
                row.get(0)
            })
            .ok();
        return Ok(if exists_in_db.is_some() {
            FileRefresh::DropStaleRow
        } else {
            FileRefresh::Fresh
        });
    }

    // Skip files we wouldn't index in the first place (binary / wrong language).
    if crate::utils::config::detect_language(rel_path).is_none() {
        return Ok(FileRefresh::Fresh);
    }

    // Size gate (2026-08-16 audit §四). Every branch below starts by hashing the
    // whole file, and this function runs on the QUERY path — `refresh_files_if_stale`
    // calls it for each file a result set touches. The indexer refuses to parse
    // anything over `max_file_size()` (1 MiB by default, so a minified bundle or a
    // generated source qualifies), which means for such a file the hash buys
    // exactly one thing: nothing. It is re-read in full on every `show`, `search`
    // and `callgraph` that mentions it.
    //
    // Not an unconditional early return, because one case still has work to do:
    // a file that was UNDER the limit when it was indexed and has since grown past
    // it still carries its old symbols, and they must be purged or they stay
    // visible forever. So the gate fires only once the DB agrees the file is
    // symbol-less, which is the steady state after the first refresh records it as
    // skipped — turning a per-query full read into a per-query indexed lookup.
    if std::fs::metadata(&abs_path).is_ok_and(|m| m.len() > crate::domain::max_file_size()) {
        let has_nodes = db
            .conn()
            .query_row(
                "SELECT 1 FROM nodes WHERE file_id = (SELECT id FROM files WHERE path = ?1) LIMIT 1",
                [rel_path],
                |_| Ok(()),
            )
            .is_ok();
        if !has_nodes {
            return Ok(FileRefresh::Fresh);
        }
    }

    let stored_hash: Option<String> = db
        .conn()
        .query_row(
            "SELECT blake3_hash FROM files WHERE path = ?1",
            [rel_path],
            |row| row.get(0),
        )
        .ok();
    if stored_hash.is_none() && scope == RefreshScope::IndexedOnly {
        return Ok(FileRefresh::Fresh);
    }

    // Hashed after the scope check, not before: for `IndexedOnly` an unknown
    // path must cost a keyed lookup, not a full read of a file we would refuse
    // to index anyway.
    let on_disk_hash = crate::indexer::merkle::hash_file(&abs_path)?;
    if stored_hash.as_deref() == Some(&on_disk_hash) {
        return Ok(FileRefresh::Fresh);
    }
    Ok(FileRefresh::Reindex(on_disk_hash))
}

/// Carry out the plans [`plan_file_refresh`] produced, as ONE unit of indexer
/// work: a single row-delete transaction and a single `index_files` call for
/// however many files were dirty. Per-file calls used to pay a whole-graph
/// `get_all_node_names_with_ids` plus the global edge post-passes EACH time,
/// so a query touching the 8-file budget cost eight whole-graph sweeps
/// (audit 2026-08-22 P1-3).
///
/// `reindex` pairs each path with the on-disk hash the plan already computed,
/// so the bytes are hashed once per refresh, not twice.
///
/// Cross-file dirty-edge handling mirrors `run_incremental_index`: collect
/// dirty node IDs **before** re-indexing (cascade delete strips old edges),
/// then regenerate context strings + embeddings once the new nodes exist.
pub fn apply_file_refreshes(
    db: &Database,
    project_root: &Path,
    drop_rows: &[String],
    reindex: &[(String, String)],
    model: Option<&EmbeddingModel>,
) -> Result<()> {
    if drop_rows.is_empty() && reindex.is_empty() {
        return Ok(());
    }

    let files: Vec<String> = reindex.iter().map(|(p, _)| p.clone()).collect();
    let hashes: HashMap<String, String> = reindex.iter().cloned().collect();

    // Cross-file edges into these files' nodes need their context strings rebuilt
    // *after* the node IDs are replaced — capture the dirty set BEFORE re-indexing.
    let dirty_node_ids = collect_dirty_node_ids(db, &files)?;

    // `index_files` clears the interrupted-run marker when it finishes, which is
    // right for a run that covered the whole diff and wrong for this one: a
    // query-time refresh re-extracts the relations of the files it was handed and
    // leaves every other file the killed run abandoned exactly as it was. Clearing
    // here would retire the evidence before the full re-index it exists to
    // trigger, so a marker that was already set goes back (audit 2026-08-16 P1-2).
    let was_interrupted = index_run_was_interrupted(db)?;
    // `drop_rows` goes through `index_files`' own `delete_paths`, not a bare
    // `delete_files_by_paths` before it. That parameter is what runs Phase 0
    // `buffer_then_delete_files` — the mechanism added in v59 precisely so a
    // cascade delete does not silently destroy inbound calls from files that did
    // not change. Deleting the rows here and passing `&[]` skipped it, so any
    // read command touching a deleted file permanently orphaned its callers and
    // importers with no recovery channel, while the incremental path buffered
    // them into `pending_unresolved_calls` (audit 2026-08-29 PIPE-01).
    index_files(db, project_root, &files, &hashes, model, drop_rows, None)?;
    if was_interrupted {
        crate::storage::queries::set_meta(
            db.conn(),
            crate::storage::schema::META_KEY_INDEX_RUN_IN_FLIGHT,
            "1",
        )?;
    }

    if !dirty_node_ids.is_empty() {
        regenerate_context_strings(db, &dirty_node_ids, model)?;
    }
    Ok(())
}

pub fn run_incremental_index(
    db: &Database,
    project_root: &Path,
    model: Option<&EmbeddingModel>,
    progress: Option<ProgressFn>,
) -> Result<IndexResult> {
    let start = std::time::Instant::now();
    let stored_hashes = get_all_file_hashes(db.conn())?;
    let current_hashes = scan_directory(project_root)?;
    let diff = compute_diff(&stored_hashes, &current_hashes);

    // Preserve <external> pseudo-file across incremental indexes
    let deleted_files: Vec<String> = diff
        .deleted_files
        .into_iter()
        .filter(|p| p != crate::domain::EXTERNAL_FILE_PATH)
        .collect();
    let to_index = to_index_after_interrupt_check(
        db,
        [diff.new_files, diff.changed_files].concat(),
        &current_hashes,
    )?;

    let dirty_node_ids = if !to_index.is_empty() {
        collect_dirty_node_ids(db, &to_index)?
    } else {
        HashSet::new()
    };

    let result = index_files(
        db,
        project_root,
        &to_index,
        &current_hashes,
        model,
        &deleted_files,
        progress,
    )?;

    if !dirty_node_ids.is_empty() {
        // Heartbeat: context-string regeneration for dirty dependents runs after
        // index_files' own finalize ticks and can take a while on wide fan-in.
        if let Some(cb) = progress {
            cb(IndexPhase::Finalizing, result.files_indexed, to_index.len());
        }
        regenerate_context_strings(db, &dirty_node_ids, model)?;
    }

    if result.files_indexed > 0 || !deleted_files.is_empty() {
        tracing::info!(
            "[incremental] {} files changed, {} deleted, {} nodes, {} edges, {:.1}s",
            result.files_indexed,
            deleted_files.len(),
            result.nodes_created,
            result.edges_created,
            start.elapsed().as_secs_f64()
        );
    }

    Ok(result)
}

/// Incremental index with directory mtime cache for faster scanning.
/// Files in unchanged directories are skipped entirely.
pub fn run_incremental_index_cached(
    db: &Database,
    project_root: &Path,
    model: Option<&EmbeddingModel>,
    dir_cache: Option<&DirectoryCache>,
    progress: Option<ProgressFn>,
) -> Result<(IndexResult, DirectoryCache)> {
    let start = std::time::Instant::now();
    let stored_hashes = get_all_file_hashes(db.conn())?;
    let (mut current_hashes, new_cache) = scan_directory_cached(project_root, dir_cache)?;

    // Merge stored hashes for files in unchanged directories.
    // scan_directory_cached skips files in unchanged dirs, so we need to
    // carry forward their stored hashes to prevent false "deleted" diffs.
    // Use new_cache.seen_files (populated for ALL walked files) to check existence
    // without per-file stat calls.
    for (path, hash) in &stored_hashes {
        if !current_hashes.contains_key(path) && new_cache.file_exists(path) {
            current_hashes.insert(path.clone(), hash.clone());
        }
    }

    let diff = compute_diff(&stored_hashes, &current_hashes);

    // Preserve <external> pseudo-file across incremental indexes
    let deleted_files: Vec<String> = diff
        .deleted_files
        .into_iter()
        .filter(|p| p != crate::domain::EXTERNAL_FILE_PATH)
        .collect();
    // Same interrupted-run escalation as `run_incremental_index`. `current_hashes`
    // is complete here too — the merge above carries forward the stored hash of
    // every file in a directory the cache let us skip walking.
    let to_index = to_index_after_interrupt_check(
        db,
        [diff.new_files, diff.changed_files].concat(),
        &current_hashes,
    )?;

    let dirty_node_ids = if !to_index.is_empty() {
        collect_dirty_node_ids(db, &to_index)?
    } else {
        HashSet::new()
    };

    let result = index_files(
        db,
        project_root,
        &to_index,
        &current_hashes,
        model,
        &deleted_files,
        progress,
    )?;

    if !dirty_node_ids.is_empty() {
        // Heartbeat: context-string regeneration for dirty dependents runs after
        // index_files' own finalize ticks and can take a while on wide fan-in.
        if let Some(cb) = progress {
            cb(IndexPhase::Finalizing, result.files_indexed, to_index.len());
        }
        regenerate_context_strings(db, &dirty_node_ids, model)?;
    }

    if result.files_indexed > 0 || !deleted_files.is_empty() {
        tracing::info!(
            "[incremental] {} files changed, {} deleted, {} nodes, {} edges, {:.1}s",
            result.files_indexed,
            deleted_files.len(),
            result.nodes_created,
            result.edges_created,
            start.elapsed().as_secs_f64()
        );
    }

    Ok((result, new_cache))
}

/// True when a previous index run was killed after committing file hashes but
/// before its cross-file edges reached the database (audit 2026-08-16 P1-2).
///
/// The killed run's hashes make `compute_diff` report those files as unchanged
/// forever, so the missing edges have no other route back — the caller escalates
/// its incremental to a full re-index, which re-extracts every relation. Reading
/// an absent key as "clean" is what makes this safe on indexes built before the
/// marker existed: they are no worse off than before, just not covered.
fn index_run_was_interrupted(db: &Database) -> Result<bool> {
    Ok(crate::storage::queries::get_meta(
        db.conn(),
        crate::storage::schema::META_KEY_INDEX_RUN_IN_FLIGHT,
    )?
    .is_some())
}

/// The file set an incremental run should process: its diff normally, or the
/// whole tree when the previous run was interrupted. `index_files` re-sets and
/// clears the marker itself, so the escalated run needs no extra bookkeeping.
fn to_index_after_interrupt_check(
    db: &Database,
    diff_files: Vec<String>,
    current_hashes: &HashMap<String, String>,
) -> Result<Vec<String>> {
    if !index_run_was_interrupted(db)? {
        return Ok(diff_files);
    }
    tracing::warn!(
        "[incremental] previous index run did not finish (cross-file edges were never \
         committed while file hashes were) — re-indexing all {} file(s) instead of the \
         {} the diff reports",
        current_hashes.len(),
        diff_files.len()
    );
    Ok(current_hashes.keys().cloned().collect())
}

/// Collect node IDs in OTHER files that have edges pointing to nodes in the changed files.
/// Must be called BEFORE re-indexing (cascade delete removes old edges).
fn collect_dirty_node_ids(db: &Database, changed_paths: &[String]) -> Result<HashSet<i64>> {
    let mut changed_file_ids = Vec::new();
    for path in changed_paths {
        let file_id: Option<i64> = db
            .conn()
            .query_row("SELECT id FROM files WHERE path = ?1", [path], |row| {
                row.get(0)
            })
            .ok();
        if let Some(id) = file_id {
            changed_file_ids.push(id);
        }
    }
    let ids = get_dirty_node_ids(db.conn(), &changed_file_ids)?;
    Ok(ids.into_iter().collect())
}
