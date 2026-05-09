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
use crate::storage::queries::{
    delete_files_by_paths, get_all_file_hashes, get_dirty_node_ids,
};

mod embed;
mod context;
mod python_modules;
mod resolve;
mod index_files;

#[cfg(test)]
mod tests;

pub use embed::embed_and_store_batch;
pub use context::repair_null_context_strings;

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
}

pub struct IndexResult {
    pub files_indexed: usize,
    pub nodes_created: usize,
    pub edges_created: usize,
    pub stats: IndexStats,
}

/// Progress callback: called with (files_done, files_total) after each batch.
pub type ProgressFn<'a> = &'a dyn Fn(usize, usize);

pub fn run_full_index(db: &Database, project_root: &Path, model: Option<&EmbeddingModel>, progress: Option<ProgressFn>) -> Result<IndexResult> {
    let current_hashes = scan_directory(project_root)?;
    let files: Vec<String> = current_hashes.keys().cloned().collect();
    index_files(db, project_root, &files, &current_hashes, model, &[], progress)
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
/// Cross-file dirty-edge handling mirrors `run_incremental_index`: collect
/// dirty node IDs **before** re-indexing (cascade delete strips old edges),
/// then regenerate context strings + embeddings once the new nodes exist.
pub fn ensure_file_indexed(
    db: &Database,
    project_root: &Path,
    rel_path: &str,
    model: Option<&EmbeddingModel>,
) -> Result<bool> {
    let abs_path = project_root.join(rel_path);

    // Missing-file path: drop stale row so future queries don't return phantom nodes.
    if !abs_path.is_file() {
        let exists_in_db: Option<i64> = db.conn().query_row(
            "SELECT id FROM files WHERE path = ?1",
            [rel_path],
            |row| row.get(0),
        ).ok();
        if exists_in_db.is_some() {
            let tx = db.conn().unchecked_transaction()?;
            delete_files_by_paths(db.conn(), &[rel_path.to_string()])?;
            tx.commit()?;
            return Ok(true);
        }
        return Ok(false);
    }

    // Skip files we wouldn't index in the first place (binary / wrong language).
    if crate::utils::config::detect_language(rel_path).is_none() {
        return Ok(false);
    }

    let on_disk_hash = crate::indexer::merkle::hash_file(&abs_path)?;
    let stored_hash: Option<String> = db.conn().query_row(
        "SELECT blake3_hash FROM files WHERE path = ?1",
        [rel_path],
        |row| row.get(0),
    ).ok();

    if stored_hash.as_deref() == Some(&on_disk_hash) {
        return Ok(false);
    }

    // Cross-file edges into this file's nodes need their context strings rebuilt
    // *after* the node IDs are replaced — capture the dirty set BEFORE re-indexing.
    let dirty_node_ids = collect_dirty_node_ids(db, std::slice::from_ref(&rel_path.to_string()))?;

    let mut hashes: HashMap<String, String> = HashMap::new();
    hashes.insert(rel_path.to_string(), on_disk_hash);
    let files = vec![rel_path.to_string()];
    index_files(db, project_root, &files, &hashes, model, &[], None)?;

    if !dirty_node_ids.is_empty() {
        regenerate_context_strings(db, &dirty_node_ids, model)?;
    }
    Ok(true)
}

pub fn run_incremental_index(db: &Database, project_root: &Path, model: Option<&EmbeddingModel>, progress: Option<ProgressFn>) -> Result<IndexResult> {
    let start = std::time::Instant::now();
    let stored_hashes = get_all_file_hashes(db.conn())?;
    let current_hashes = scan_directory(project_root)?;
    let diff = compute_diff(&stored_hashes, &current_hashes);

    // Preserve <external> pseudo-file across incremental indexes
    let deleted_files: Vec<String> = diff.deleted_files.into_iter()
        .filter(|p| p != "<external>")
        .collect();
    let to_index: Vec<String> = [diff.new_files, diff.changed_files].concat();

    let dirty_node_ids = if !to_index.is_empty() {
        collect_dirty_node_ids(db, &to_index)?
    } else {
        HashSet::new()
    };

    let result = index_files(db, project_root, &to_index, &current_hashes, model, &deleted_files, progress)?;

    if !dirty_node_ids.is_empty() {
        regenerate_context_strings(db, &dirty_node_ids, model)?;
    }

    if result.files_indexed > 0 || !deleted_files.is_empty() {
        tracing::info!(
            "[incremental] {} files changed, {} deleted, {} nodes, {} edges, {:.1}s",
            result.files_indexed, deleted_files.len(),
            result.nodes_created, result.edges_created,
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
    // Use new_cache.file_mtimes (populated for ALL walked files) to check existence
    // without per-file stat calls.
    for (path, hash) in &stored_hashes {
        if !current_hashes.contains_key(path) && new_cache.file_exists(path) {
            current_hashes.insert(path.clone(), hash.clone());
        }
    }

    let diff = compute_diff(&stored_hashes, &current_hashes);

    // Preserve <external> pseudo-file across incremental indexes
    let deleted_files: Vec<String> = diff.deleted_files.into_iter()
        .filter(|p| p != "<external>")
        .collect();
    let to_index: Vec<String> = [diff.new_files, diff.changed_files].concat();

    let dirty_node_ids = if !to_index.is_empty() {
        collect_dirty_node_ids(db, &to_index)?
    } else {
        HashSet::new()
    };

    let result = index_files(db, project_root, &to_index, &current_hashes, model, &deleted_files, progress)?;

    if !dirty_node_ids.is_empty() {
        regenerate_context_strings(db, &dirty_node_ids, model)?;
    }

    if result.files_indexed > 0 || !deleted_files.is_empty() {
        tracing::info!(
            "[incremental] {} files changed, {} deleted, {} nodes, {} edges, {:.1}s",
            result.files_indexed, deleted_files.len(),
            result.nodes_created, result.edges_created,
            start.elapsed().as_secs_f64()
        );
    }

    Ok((result, new_cache))
}

/// Collect node IDs in OTHER files that have edges pointing to nodes in the changed files.
/// Must be called BEFORE re-indexing (cascade delete removes old edges).
fn collect_dirty_node_ids(db: &Database, changed_paths: &[String]) -> Result<HashSet<i64>> {
    let mut changed_file_ids = Vec::new();
    for path in changed_paths {
        let file_id: Option<i64> = db.conn().query_row(
            "SELECT id FROM files WHERE path = ?1",
            [path],
            |row| row.get(0),
        ).ok();
        if let Some(id) = file_id {
            changed_file_ids.push(id);
        }
    }
    let ids = get_dirty_node_ids(db.conn(), &changed_file_ids)?;
    Ok(ids.into_iter().collect())
}
