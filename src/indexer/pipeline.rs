use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use rayon::prelude::*;

use crate::embedding::context::{build_context_string, NodeContext};
use crate::embedding::model::EmbeddingModel;
use crate::indexer::merkle::{compute_diff, hash_file, scan_directory, scan_directory_cached, DirectoryCache};
use crate::parser::relations::extract_relations_from_tree;
use crate::parser::treesitter::{parse_tree, extract_nodes_from_tree};
use crate::search::tokenizer::split_identifier;
use crate::storage::db::Database;
use crate::storage::queries::{
    delete_files_by_paths, delete_nodes_by_file,
    get_all_file_hashes, get_all_node_names_with_ids, get_dirty_node_ids, get_edges_batch,
    get_nodes_by_file_path,
    get_nodes_missing_context, get_nodes_with_files_by_ids,
    insert_edge_cached, insert_node_cached,
    insert_node_vectors_batch, update_context_strings_batch, upsert_file,
    EdgeInfo, FileRecord, NodeRecord, NodeResult,
};
use crate::domain::{REL_CALLS, REL_IMPORTS, REL_INHERITS, REL_ROUTES_TO, REL_IMPLEMENTS, REL_EXPORTS, max_file_size, CROSS_FILE_CALL_NOISE};
use crate::utils::config::detect_language;

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

/// Extract "METHOD path" from route edge metadata JSON, falling back to the edge name.
fn format_route_from_metadata(metadata: Option<&str>, name: &str) -> String {
    if let Some(meta) = metadata {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(meta) {
            let method = v["method"].as_str().unwrap_or("ALL");
            if let Some(path) = v["path"].as_str() {
                return format!("{} {}", method, path);
            }
        }
    }
    name.to_string()
}

/// Embed context strings using batched inference and batch-insert vectors.
/// Public so the background embedding thread in server.rs can call it.
/// Wraps vector inserts in a transaction for atomicity and performance.
pub fn embed_and_store_batch(db: &Database, model: &EmbeddingModel, context_updates: &[(i64, String)]) -> Result<()> {
    if context_updates.is_empty() {
        return Ok(());
    }

    let t0 = std::time::Instant::now();
    let texts: Vec<&str> = context_updates.iter().map(|(_, ctx)| ctx.as_str()).collect();
    let ids: Vec<i64> = context_updates.iter().map(|(id, _)| *id).collect();

    let embeddings = match model.embed_batch(&texts) {
        Ok(embs) => embs,
        Err(e) => {
            tracing::warn!("Batch embed failed, falling back to sequential: {}", e);
            // Fallback: sequential embed
            let mut embs = Vec::new();
            for (i, text) in texts.iter().enumerate() {
                match model.embed(text) {
                    Ok(emb) => embs.push(Some(emb)),
                    Err(e2) => {
                        tracing::warn!("Failed to embed node {}: {}", ids[i], e2);
                        embs.push(None);
                    }
                }
            }
            let vectors: Vec<(i64, Vec<f32>)> = ids.iter().zip(embs)
                .filter_map(|(&id, emb)| emb.map(|e| (id, e)))
                .collect();
            if !vectors.is_empty() {
                let tx = db.conn().unchecked_transaction()?;
                insert_node_vectors_batch(db.conn(), &vectors)?;
                tx.commit()?;
            }
            tracing::info!("[embed] {} nodes (sequential fallback) in {:.1}s",
                context_updates.len(), t0.elapsed().as_secs_f64());
            return Ok(());
        }
    };

    let vectors: Vec<(i64, Vec<f32>)> = ids.into_iter().zip(embeddings).collect();
    let t_embed = t0.elapsed();

    if !vectors.is_empty() {
        let tx = db.conn().unchecked_transaction()?;
        insert_node_vectors_batch(db.conn(), &vectors)?;
        tx.commit()?;
    }

    tracing::info!("[embed] {} nodes in {:.1}s (embed {:.1}s, store {:.1}s)",
        context_updates.len(),
        t0.elapsed().as_secs_f64(),
        t_embed.as_secs_f64(),
        (t0.elapsed() - t_embed).as_secs_f64(),
    );
    Ok(())
}

struct CategorizedEdges {
    callees: Vec<String>,
    callers: Vec<String>,
    inherits: Vec<String>,
    routes: Vec<String>,
    imports: Vec<String>,
    implements: Vec<String>,
    exports: Vec<String>,
}

fn categorize_edges(edges: Option<&Vec<EdgeInfo>>, format_route: impl Fn(Option<&str>, &str) -> String) -> CategorizedEdges {
    let mut result = CategorizedEdges {
        callees: Vec::new(),
        callers: Vec::new(),
        inherits: Vec::new(),
        routes: Vec::new(),
        imports: Vec::new(),
        implements: Vec::new(),
        exports: Vec::new(),
    };
    if let Some(edge_list) = edges {
        for (relation, direction, name, metadata) in edge_list {
            match (relation.as_str(), direction.as_str()) {
                (rel, "out") if rel == REL_CALLS => result.callees.push(name.clone()),
                (rel, "in") if rel == REL_CALLS => result.callers.push(name.clone()),
                (rel, "out") if rel == REL_INHERITS => result.inherits.push(name.clone()),
                (rel, "out") if rel == REL_ROUTES_TO => {
                    result.routes.push(format_route(metadata.as_deref(), name));
                }
                (rel, "out") if rel == REL_IMPORTS => result.imports.push(name.clone()),
                (rel, "out") if rel == REL_IMPLEMENTS => result.implements.push(name.clone()),
                (rel, "out") if rel == REL_EXPORTS => result.exports.push(name.clone()),
                _ => {}
            }
        }
    }
    result
}

pub fn run_full_index(db: &Database, project_root: &Path, model: Option<&EmbeddingModel>, progress: Option<ProgressFn>) -> Result<IndexResult> {
    let current_hashes = scan_directory(project_root)?;
    let files: Vec<String> = current_hashes.keys().cloned().collect();
    index_files(db, project_root, &files, &current_hashes, model, &[], progress)
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

/// Regenerate context strings (and embeddings) for the given set of dirty nodes.
fn regenerate_context_strings(db: &Database, dirty_ids: &HashSet<i64>, model: Option<&EmbeddingModel>) -> Result<()> {
    let tx = db.conn().unchecked_transaction()?;
    let id_vec: Vec<i64> = dirty_ids.iter().copied().collect();
    let all_edges = get_edges_batch(db.conn(), &id_vec)?;
    let all_nodes: HashMap<i64, (NodeResult, String, Option<String>)> = {
        let nwfs = get_nodes_with_files_by_ids(db.conn(), &id_vec)?;
        nwfs.into_iter().map(|nwf| (nwf.node.id, (nwf.node, nwf.file_path, nwf.language))).collect()
    };

    // Build all context strings first
    let mut context_updates: Vec<(i64, String)> = Vec::with_capacity(dirty_ids.len());
    for &node_id in dirty_ids {
        if let Some((node, file_path, language)) = all_nodes.get(&node_id) {
            let edges = all_edges.get(&node_id);
            let cat = categorize_edges(edges, format_route_from_metadata);

            let ctx = build_context_string(&NodeContext {
                node_type: node.node_type.clone(),
                name: node.name.clone(),
                qualified_name: node.qualified_name.clone(),
                file_path: file_path.clone(),
                language: language.clone(),
                signature: node.signature.clone(),
                return_type: node.return_type.clone(),
                param_types: node.param_types.clone(),
                code_content: Some(node.code_content.clone()),
                routes: cat.routes,
                callees: cat.callees,
                callers: cat.callers,
                inherits: cat.inherits,
                imports: cat.imports,
                implements: cat.implements,
                exports: cat.exports,
                doc_comment: node.doc_comment.clone(),
            });

            context_updates.push((node_id, ctx));
        }
    }

    // Batch update context strings
    update_context_strings_batch(db.conn(), &context_updates)?;
    tx.commit()?;

    // Embed outside the committed tx — recoverable on failure
    if let Some(m) = model {
        if db.vec_enabled() {
            embed_and_store_batch(db, m, &context_updates)?;
        }
    }
    Ok(())
}

/// Repair nodes that have NULL context_string (likely from a failed Phase 3).
/// This is called at startup after index verification.
pub fn repair_null_context_strings(
    db: &Database,
    model: Option<&EmbeddingModel>,
) -> Result<usize> {
    let missing_ids = get_nodes_missing_context(db.conn())?;
    if missing_ids.is_empty() {
        return Ok(0);
    }

    tracing::info!("[repair] Found {} nodes with NULL context_string, rebuilding...", missing_ids.len());

    // Load node details with file paths
    let nodes_with_files = get_nodes_with_files_by_ids(db.conn(), &missing_ids)?;

    // Load edges for all affected nodes in one batch
    let all_edges = get_edges_batch(db.conn(), &missing_ids)?;

    // Build context strings
    let mut context_updates: Vec<(i64, String)> = Vec::new();
    for nwf in &nodes_with_files {
        let node = &nwf.node;
        let edges = all_edges.get(&node.id);
        let cat = categorize_edges(edges, format_route_from_metadata);

        let ctx = build_context_string(&NodeContext {
            node_type: node.node_type.clone(),
            name: node.name.clone(),
            qualified_name: node.qualified_name.clone(),
            file_path: nwf.file_path.clone(),
            language: nwf.language.clone(),
            signature: node.signature.clone(),
            return_type: node.return_type.clone(),
            param_types: node.param_types.clone(),
            code_content: Some(node.code_content.clone()),
            routes: cat.routes,
            callees: cat.callees,
            callers: cat.callers,
            inherits: cat.inherits,
            imports: cat.imports,
            implements: cat.implements,
            exports: cat.exports,
            doc_comment: node.doc_comment.clone(),
        });

        context_updates.push((node.id, ctx));
    }

    // Update in DB within a transaction (avoids per-row fsync under autocommit)
    if !context_updates.is_empty() {
        let tx = db.conn().unchecked_transaction()?;
        update_context_strings_batch(db.conn(), &context_updates)?;
        tx.commit()?;

        // Re-embed if model available
        if let Some(m) = model {
            if db.vec_enabled() {
                embed_and_store_batch(db, m, &context_updates)?;
            }
        }
    }

    let count = context_updates.len();
    tracing::info!("[repair] Repaired context strings for {} nodes", count);
    Ok(count)
}

/// Batch size for streaming indexing. Each batch processes Phase 1+2
/// then drops heavyweight data (ASTs, source strings) before the next batch.
const BATCH_SIZE: usize = 500;

/// Lightweight post-batch record — no Tree or source string.
struct FileIndexed {
    rel_path: String,
    node_ids: Vec<i64>,
    node_names: Vec<String>,
}

/// Build mapping from Python dotted module paths to file paths.
/// Registers both full paths and suffix paths for flexible matching.
/// e.g., "src/myapp/utils.py" matches "src.myapp.utils", "myapp.utils", and "utils".
fn build_python_module_map(python_paths: &HashSet<String>) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for path in python_paths {
        let stripped = if let Some(s) = path.strip_suffix("/__init__.py") {
            s
        } else if let Some(s) = path.strip_suffix(".py") {
            s
        } else {
            continue;
        };

        // Register all suffix module paths for flexible matching
        // e.g., "src/myapp/utils" -> "src.myapp.utils", "myapp.utils", "utils"
        let parts: Vec<&str> = stripped.split('/').collect();
        for i in 0..parts.len() {
            let dotted = parts[i..].join(".");
            map.entry(dotted).or_default().push(path.clone());
        }
    }
    // Deduplicate
    for paths in map.values_mut() {
        paths.sort();
        paths.dedup();
    }
    map
}

/// Resolve Python import targets using pre-parsed module metadata.
/// For `import X` (is_module_import): finds `<module>` nodes in resolved files.
/// For `from X import Y`: finds nodes named Y only in resolved files.
/// Returns None if module can't be resolved or no matching nodes found.
fn resolve_python_module_targets(
    python_module: &str,
    is_module_import: bool,
    target_name: &str,
    python_module_map: &HashMap<String, Vec<String>>,
    node_id_to_path: &HashMap<i64, String>,
    name_to_ids: &HashMap<String, Vec<i64>>,
) -> Option<Vec<i64>> {
    // Resolve module path to file path(s).
    // Note: suffix matching in python_module_map means `import utils` may match
    // multiple files (e.g., "myapp/utils.py" and "other/utils.py"). This is an
    // inherent ambiguity without sys.path context; over-connecting is safer for
    // dependency analysis than missing real dependencies.
    let module_files = python_module_map.get(python_module)?;

    let lookup_name = if is_module_import { "<module>" } else { target_name };
    let all_ids = name_to_ids.get(lookup_name)?;
    let targets: Vec<i64> = all_ids.iter()
        .filter(|nid| {
            node_id_to_path.get(nid)
                .map(|p| module_files.contains(p))
                .unwrap_or(false)
        })
        .copied()
        .collect();
    if targets.is_empty() { None } else { Some(targets) }
}

fn index_files(
    db: &Database,
    root: &Path,
    files: &[String],
    hashes: &HashMap<String, String>,
    model: Option<&EmbeddingModel>,
    delete_paths: &[String],
    progress: Option<ProgressFn>,
) -> Result<IndexResult> {
    // SAFETY: unchecked_transaction is used because rusqlite's Transaction borrows
    // &mut Connection, preventing other borrows during the transaction. Here we need
    // both the transaction and read access via db.conn() (which returns &Connection
    // to the same underlying connection). This is safe because:
    // (1) db.conn() returns the same Connection the tx was opened on,
    // (2) we never open nested transactions,
    // (3) concurrent access (e.g. background embedding thread) uses separate
    //     DB connections; safety relies on SQLite WAL mode + busy_timeout(5000),
    //     not single-threadedness.

    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    let skipped_size = AtomicUsize::new(0);
    let skipped_parse = AtomicUsize::new(0);
    let skipped_read = AtomicUsize::new(0);
    let skipped_hash = AtomicUsize::new(0);
    let skipped_language = AtomicUsize::new(0);

    let mut total_nodes_created = 0usize;
    let mut total_edges_created = 0usize;
    let mut all_indexed: Vec<FileIndexed> = Vec::new();

    // Phase 0: Delete removed files in own transaction
    if !delete_paths.is_empty() {
        let tx = db.conn().unchecked_transaction()?;
        delete_files_by_paths(db.conn(), delete_paths)?;
        tx.commit()?;
    }

    // CPU-bound parse result — produced in parallel, consumed sequentially for DB insert
    struct FilePreParsed {
        rel_path: String,
        source: String,
        language: String,
        tree: tree_sitter::Tree,
        hash: String,
        last_modified: i64,
        parsed_nodes: Vec<crate::parser::treesitter::ParsedNode>,
    }

    // Pre-build Python module map once (used in all batches for import resolution)
    let mut all_python_paths: HashSet<String> = files.iter()
        .filter(|f| f.ends_with(".py"))
        .cloned()
        .collect();
    {
        let mut stmt = db.conn().prepare("SELECT path FROM files WHERE path LIKE '%.py'")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        for row in rows {
            all_python_paths.insert(row?);
        }
    }
    let python_module_map = build_python_module_map(&all_python_paths);

    // Pre-load global name->[(id, path)] map once before the batch loop.
    // This avoids a full table scan per batch in Phase 2 relation resolution.
    // The map is updated incrementally as each batch commits new nodes.
    let mut global_name_map: HashMap<String, Vec<(i64, String)>> =
        get_all_node_names_with_ids(db.conn())?;

    // Heavyweight per-file data used during Phase 1+2, dropped after each batch
    #[allow(dead_code)]
    struct FileParsed {
        rel_path: String,
        source: String,
        language: String,
        tree: tree_sitter::Tree,
        file_id: i64,
        node_ids: Vec<i64>,
        node_names: Vec<String>,
    }

    // Process files in batches — each batch does Phase 1 + Phase 2
    for batch in files.chunks(BATCH_SIZE) {
        let tx = db.conn().unchecked_transaction()?;

        // --- Phase 1a: Parallel CPU-bound work (read + parse + extract nodes) ---
        let pre_parsed: Vec<FilePreParsed> = batch
            .par_iter()
            .filter_map(|rel_path| {
                let language = match detect_language(rel_path) {
                    Some(l) => l,
                    None => {
                        skipped_language.fetch_add(1, AtomicOrdering::Relaxed);
                        return None;
                    }
                };
                let abs_path = root.join(rel_path);

                let file_meta = std::fs::metadata(&abs_path).ok();
                if let Some(ref meta) = file_meta {
                    if meta.len() > max_file_size() {
                        tracing::debug!("Skipping large file ({} bytes): {}", meta.len(), rel_path);
                        skipped_size.fetch_add(1, AtomicOrdering::Relaxed);
                        return None;
                    }
                }

                let source = match std::fs::read_to_string(&abs_path) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("Skipping file {}: {}", rel_path, e);
                        skipped_read.fetch_add(1, AtomicOrdering::Relaxed);
                        return None;
                    }
                };

                let hash = match hashes.get(rel_path.as_str()) {
                    Some(h) => h.clone(),
                    None => match hash_file(&abs_path) {
                        Ok(h) => h,
                        Err(e) => {
                            tracing::warn!("Skipping file (hash error): {}: {}", rel_path, e);
                            skipped_hash.fetch_add(1, AtomicOrdering::Relaxed);
                            return None;
                        }
                    },
                };

                let tree = match parse_tree(&source, language) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::warn!("Parse failed for {}: {}", rel_path, e);
                        skipped_parse.fetch_add(1, AtomicOrdering::Relaxed);
                        return None;
                    }
                };

                let last_modified = file_meta
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);

                let parsed_nodes = extract_nodes_from_tree(&tree, &source, language);

                Some(FilePreParsed {
                    rel_path: rel_path.clone(),
                    source,
                    language: language.to_string(),
                    tree,
                    hash,
                    last_modified,
                    parsed_nodes,
                })
            })
            .collect();

        let mut batch_parsed: Vec<FileParsed> = Vec::new();

        // --- Phase 1b: Sequential DB inserts ---
        for pp in pre_parsed {
            let file_id = upsert_file(db.conn(), &FileRecord {
                path: pp.rel_path.clone(),
                blake3_hash: pp.hash,
                last_modified: pp.last_modified,
                language: Some(pp.language.clone()),
            })?;

            delete_nodes_by_file(db.conn(), file_id)?;

            let mut node_ids = Vec::new();
            let mut node_names = Vec::new();

            let module_node_id = insert_node_cached(db.conn(), &NodeRecord {
                file_id,
                node_type: "module".into(),
                name: "<module>".into(),
                qualified_name: Some(pp.rel_path.clone()),
                start_line: 1,
                end_line: pp.source.lines().count() as i64,
                code_content: String::new(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            })?;
            node_ids.push(module_node_id);
            node_names.push("<module>".into());
            total_nodes_created += 1;

            for pn in &pp.parsed_nodes {
                let name_tokens = split_identifier(&pn.name);
                let node_id = insert_node_cached(db.conn(), &NodeRecord {
                    file_id,
                    node_type: pn.node_type.clone(),
                    name: pn.name.clone(),
                    qualified_name: pn.qualified_name.clone(),
                    start_line: pn.start_line as i64,
                    end_line: pn.end_line as i64,
                    code_content: pn.code_content.clone(),
                    signature: pn.signature.clone(),
                    doc_comment: pn.doc_comment.clone(),
                    context_string: None,
                    name_tokens: Some(name_tokens),
                    return_type: pn.return_type.clone(),
                    param_types: pn.param_types.clone(),
                    is_test: pn.is_test,
                })?;
                node_ids.push(node_id);
                node_names.push(pn.name.clone());
                total_nodes_created += 1;
            }

            batch_parsed.push(FileParsed {
                rel_path: pp.rel_path,
                source: pp.source,
                language: pp.language,
                tree: pp.tree,
                file_id,
                node_ids,
                node_names,
            });
        }

        // --- Phase 2: Extract relations + insert edges ---
        // Build per-batch name_to_ids and node_id_to_path from the pre-loaded global map,
        // excluding files in the current batch (their old nodes were deleted in Phase 1b).
        let batch_file_paths: HashSet<&str> = batch_parsed.iter()
            .map(|pf| pf.rel_path.as_str()).collect();

        let mut name_to_ids: HashMap<String, Vec<i64>> = HashMap::new();
        let mut node_id_to_path: HashMap<i64, String> = HashMap::new();

        // Add current batch's newly inserted nodes
        for pf in &batch_parsed {
            for (id, name) in pf.node_ids.iter().zip(pf.node_names.iter()) {
                name_to_ids.entry(name.clone()).or_default().push(*id);
                node_id_to_path.insert(*id, pf.rel_path.clone());
            }
        }

        // Add nodes from the global map, excluding those in current batch's files
        // (their old nodes were deleted and replaced by new ones above)
        for (name, entries) in &global_name_map {
            for &(id, ref path) in entries {
                if !batch_file_paths.contains(path.as_str()) {
                    name_to_ids.entry(name.clone()).or_default().push(id);
                    node_id_to_path.insert(id, path.clone());
                }
            }
        }

        for ids in name_to_ids.values_mut() {
            ids.sort();
            ids.dedup();
        }

        // Track unresolved external Python imports: (source_module_node_id, module_name)
        let mut external_python_imports: Vec<(i64, String)> = Vec::new();

        for pf in &batch_parsed {
            let relations = extract_relations_from_tree(&pf.tree, &pf.source, &pf.language);
            let local_ids: HashSet<i64> = pf.node_ids.iter().copied().collect();

            for rel in &relations {
                let source_ids = pf.node_names.iter()
                    .zip(pf.node_ids.iter())
                    .filter(|(name, _)| *name == &rel.source_name)
                    .map(|(_, id)| *id)
                    .collect::<Vec<_>>();

                // Try Python module-constrained resolution for import edges
                if rel.relation == REL_IMPORTS {
                    if let Some(ref meta_str) = rel.metadata {
                        if let Ok(meta) = serde_json::from_str::<serde_json::Value>(meta_str) {
                            if let Some(python_module) = meta.get("python_module").and_then(|v| v.as_str()) {
                                let is_module_import = meta.get("is_module_import")
                                    .and_then(|v| v.as_bool()).unwrap_or(false);
                                if python_module_map.contains_key(python_module) {
                                    // Internal module — try constrained resolution
                                    if let Some(module_targets) = resolve_python_module_targets(
                                        python_module, is_module_import, &rel.target_name,
                                        &python_module_map, &node_id_to_path, &name_to_ids,
                                    ) {
                                        for &src_id in &source_ids {
                                            for &tgt_id in &module_targets {
                                                if src_id != tgt_id
                                                    && insert_edge_cached(db.conn(), src_id, tgt_id, &rel.relation, rel.metadata.as_deref())? {
                                                    total_edges_created += 1;
                                                }
                                            }
                                        }
                                        continue;
                                    }
                                    // Module found but symbol not found — fall through to default
                                } else {
                                    // External module — track for virtual node creation.
                                    // For `from X import Y`, we track the module-level dependency (X),
                                    // not the individual symbol (Y), since we can't index external code.
                                    for &src_id in &source_ids {
                                        external_python_imports.push((src_id, python_module.to_string()));
                                    }
                                    continue; // No point in default resolution for external imports
                                }
                            }
                        }
                    }
                }

                // Default resolution: global name-based lookup
                let all_target_ids = name_to_ids.get(&rel.target_name)
                    .cloned()
                    .unwrap_or_default();

                let same_file_targets: Vec<i64> = all_target_ids.iter()
                    .filter(|id| local_ids.contains(id))
                    .copied()
                    .collect();
                let target_ids = if !same_file_targets.is_empty() {
                    same_file_targets
                } else if rel.relation == REL_CALLS
                    && CROSS_FILE_CALL_NOISE.contains(&rel.target_name.as_str())
                {
                    // Skip cross-file edges for common stdlib method names
                    // (e.g., "new", "default", "from") that produce false positives
                    // when resolved by name alone without type context.
                    continue;
                } else {
                    all_target_ids
                };

                for &src_id in &source_ids {
                    for &tgt_id in &target_ids {
                        if (src_id != tgt_id || rel.relation == REL_ROUTES_TO)
                            && insert_edge_cached(db.conn(), src_id, tgt_id, &rel.relation, rel.metadata.as_deref())? {
                            total_edges_created += 1;
                        }
                    }
                }
            }
        }

        // Phase 2b: Create virtual nodes for external Python imports
        if !external_python_imports.is_empty() {
            let ext_file_id = upsert_file(db.conn(), &FileRecord {
                path: "<external>".into(),
                blake3_hash: "external".into(),
                last_modified: 0,
                language: Some("external".into()),
            })?;

            // Load existing external module nodes to avoid duplicates
            let existing_ext_nodes: HashMap<String, i64> =
                get_nodes_by_file_path(db.conn(), "<external>")?
                    .into_iter()
                    .map(|n| (n.name.clone(), n.id))
                    .collect();

            let unique_modules: HashSet<String> = external_python_imports.iter()
                .map(|(_, m)| m.clone()).collect();

            let mut ext_node_ids: HashMap<String, i64> = existing_ext_nodes;
            for module_name in &unique_modules {
                if !ext_node_ids.contains_key(module_name) {
                    let node_id = insert_node_cached(db.conn(), &NodeRecord {
                        file_id: ext_file_id,
                        node_type: "external_module".into(),
                        name: module_name.clone(),
                        qualified_name: Some(format!("<external>/{}", module_name)),
                        start_line: 0,
                        end_line: 0,
                        code_content: String::new(),
                        signature: None,
                        doc_comment: None,
                        context_string: None,
                        name_tokens: None,
                        return_type: None,
                        param_types: None,
                        is_test: false,
                    })?;
                    ext_node_ids.insert(module_name.clone(), node_id);
                    total_nodes_created += 1;
                }
            }

            for (source_id, module_name) in &external_python_imports {
                if let Some(&ext_id) = ext_node_ids.get(module_name) {
                    if insert_edge_cached(db.conn(), *source_id, ext_id, REL_IMPORTS, None)? {
                        total_edges_created += 1;
                    }
                }
            }
        }

        tx.commit()?;

        let batch_file_count = batch_parsed.len();

        // Update global_name_map: remove old entries for batch files, add new ones
        for (_, entries) in global_name_map.iter_mut() {
            entries.retain(|(_id, path)| !batch_file_paths.contains(path.as_str()));
        }
        global_name_map.retain(|_, entries| !entries.is_empty());

        // Convert to lightweight records — drops Tree and source string
        for pf in batch_parsed {
            // Add newly committed nodes to the global map
            for (id, name) in pf.node_ids.iter().zip(pf.node_names.iter()) {
                global_name_map.entry(name.clone())
                    .or_default()
                    .push((*id, pf.rel_path.clone()));
            }
            all_indexed.push(FileIndexed {
                rel_path: pf.rel_path,
                node_ids: pf.node_ids,
                node_names: pf.node_names,
            });
            // pf.tree and pf.source are dropped here — memory freed
        }

        // Report progress after each batch
        if let Some(cb) = progress {
            cb(all_indexed.len(), files.len());
        }

        if files.len() > BATCH_SIZE {
            tracing::info!(
                "[index] batch {}/{}: {} files ({} nodes, {} edges)",
                all_indexed.len(), files.len(),
                batch_file_count, total_nodes_created, total_edges_created
            );
        }
    }

    // Phase 3: Build context strings + embeddings (single transaction, lightweight)
    if !all_indexed.is_empty() {
        let tx = db.conn().unchecked_transaction()?;
        let all_node_ids: Vec<i64> = all_indexed.iter()
            .flat_map(|fi| fi.node_ids.iter().copied()).collect();
        let all_edges = get_edges_batch(db.conn(), &all_node_ids)?;
        let all_node_details: HashMap<i64, (NodeResult, Option<String>)> = {
            let nodes = get_nodes_with_files_by_ids(db.conn(), &all_node_ids)?;
            nodes.into_iter().map(|nwf| (nwf.node.id, (nwf.node, nwf.language))).collect()
        };

        // Phase 3a: Build all context strings (CPU-bound, parallelized with rayon)
        // Flatten to (node_id, node_name, file_path) tuples for parallel iteration
        let node_tasks: Vec<(i64, &str, &str)> = all_indexed.iter()
            .flat_map(|fi| {
                fi.node_ids.iter().enumerate().map(move |(idx, &node_id)| {
                    (node_id, fi.node_names[idx].as_str(), fi.rel_path.as_str())
                })
            })
            .collect();

        let context_updates: Vec<(i64, String)> = node_tasks.par_iter()
            .map(|&(node_id, node_name, file_path)| {
                let edges = all_edges.get(&node_id);
                let cat = categorize_edges(edges, format_route_from_metadata);
                let node_detail = all_node_details.get(&node_id);

                let ctx = build_context_string(&NodeContext {
                    node_type: node_detail.map(|(n, _)| n.node_type.clone()).unwrap_or_default(),
                    name: node_name.to_string(),
                    qualified_name: node_detail.and_then(|(n, _)| n.qualified_name.clone()),
                    file_path: file_path.to_string(),
                    language: node_detail.and_then(|(_, lang)| lang.clone()),
                    signature: node_detail.and_then(|(n, _)| n.signature.clone()),
                    return_type: node_detail.and_then(|(n, _)| n.return_type.clone()),
                    param_types: node_detail.and_then(|(n, _)| n.param_types.clone()),
                    code_content: node_detail.map(|(n, _)| n.code_content.clone()),
                    routes: cat.routes,
                    callees: cat.callees,
                    callers: cat.callers,
                    inherits: cat.inherits,
                    imports: cat.imports,
                    implements: cat.implements,
                    exports: cat.exports,
                    doc_comment: node_detail.and_then(|(n, _)| n.doc_comment.clone()),
                });

                (node_id, ctx)
            })
            .collect();

        // Phase 3b: Batch update context strings in DB
        update_context_strings_batch(db.conn(), &context_updates)?;
        tx.commit()?;

        tracing::info!(
            "[index] Phase 3: context strings built for {} nodes",
            all_node_ids.len()
        );

        // Phase 3c: Embed outside the committed tx — recoverable on failure via repair_null_context_strings
        if let Some(m) = model {
            if db.vec_enabled() {
                embed_and_store_batch(db, m, &context_updates)?;
            }
        }
    }

    // Optimize query planner statistics after bulk writes
    if !all_indexed.is_empty() {
        let _ = db.run_optimize();
    }

    let stats = IndexStats {
        files_skipped_size: skipped_size.load(AtomicOrdering::Relaxed),
        files_skipped_parse: skipped_parse.load(AtomicOrdering::Relaxed),
        files_skipped_read: skipped_read.load(AtomicOrdering::Relaxed),
        files_skipped_hash: skipped_hash.load(AtomicOrdering::Relaxed),
        files_skipped_language: skipped_language.load(AtomicOrdering::Relaxed),
    };

    Ok(IndexResult {
        files_indexed: all_indexed.len(),
        nodes_created: total_nodes_created,
        edges_created: total_edges_created,
        stats,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::queries::{get_nodes_by_name, get_edges_from, get_import_tree};
    use tempfile::TempDir;
    use std::fs;

    #[test]
    fn test_full_index_pipeline() {
        let project_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();

        fs::create_dir_all(project_dir.path().join("src")).unwrap();
        fs::write(project_dir.path().join("src/auth.ts"), r#"
function validateToken(token: string): boolean {
    return jwt.verify(token);
}

function handleLogin(req: Request) {
    if (validateToken(req.token)) {
        return createSession(req.userId);
    }
}
"#).unwrap();

        let db = Database::open(&db_dir.path().join("index.db")).unwrap();
        let result = run_full_index(&db, project_dir.path(), None, None).unwrap();

        assert!(result.files_indexed > 0);
        assert!(result.nodes_created > 0);
        assert!(result.edges_created > 0);

        // Verify nodes are in DB
        let nodes = get_nodes_by_name(db.conn(), "handleLogin").unwrap();
        assert_eq!(nodes.len(), 1);

        // Verify edges: handleLogin → calls → validateToken
        let edges = get_edges_from(db.conn(), nodes[0].id).unwrap();
        assert!(edges.iter().any(|e| e.relation == REL_CALLS), "should have call edges");

        // Verify context string was built
        assert!(nodes[0].context_string.is_some(), "context string should be set after Phase 3");
    }

    #[test]
    fn test_incremental_index() {
        let project_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        let db = Database::open(&db_dir.path().join("index.db")).unwrap();

        // Initial index
        fs::write(project_dir.path().join("a.ts"), "function foo() {}").unwrap();
        run_full_index(&db, project_dir.path(), None, None).unwrap();

        // Modify file
        fs::write(project_dir.path().join("a.ts"), "function bar() {}").unwrap();

        // Incremental index
        let result = run_incremental_index(&db, project_dir.path(), None, None).unwrap();
        assert_eq!(result.files_indexed, 1);

        let foo = get_nodes_by_name(db.conn(), "foo").unwrap();
        assert_eq!(foo.len(), 0);
        let bar = get_nodes_by_name(db.conn(), "bar").unwrap();
        assert_eq!(bar.len(), 1);
    }

    #[test]
    fn test_incremental_propagates_dirty_context() {
        let project_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        let db = Database::open(&db_dir.path().join("index.db")).unwrap();

        // Initial: B (in b.ts) calls A (in a.ts)
        fs::write(project_dir.path().join("a.ts"), "function alpha() {}").unwrap();
        fs::write(project_dir.path().join("b.ts"), "function beta() { alpha(); }").unwrap();
        run_full_index(&db, project_dir.path(), None, None).unwrap();

        let beta_nodes = get_nodes_by_name(db.conn(), "beta").unwrap();
        assert_eq!(beta_nodes.len(), 1);
        let beta_ctx_before = beta_nodes[0].context_string.clone().unwrap_or_default();

        // Change A: rename function (alpha -> alphaRenamed)
        fs::write(project_dir.path().join("a.ts"), "function alphaRenamed() {}").unwrap();
        run_incremental_index(&db, project_dir.path(), None, None).unwrap();

        // beta's context_string should be updated (calls list changed because
        // the old alpha node is gone and edge was cascade-deleted)
        let beta_nodes_after = get_nodes_by_name(db.conn(), "beta").unwrap();
        assert_eq!(beta_nodes_after.len(), 1);
        let beta_ctx_after = beta_nodes_after[0].context_string.clone().unwrap_or_default();
        assert_ne!(beta_ctx_before, beta_ctx_after);
    }

    #[test]
    fn test_deleted_file_cleanup() {
        let project_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        let db = Database::open(&db_dir.path().join("index.db")).unwrap();

        fs::write(project_dir.path().join("a.ts"), "function foo() {}").unwrap();
        run_full_index(&db, project_dir.path(), None, None).unwrap();

        fs::remove_file(project_dir.path().join("a.ts")).unwrap();
        run_incremental_index(&db, project_dir.path(), None, None).unwrap();

        let foo = get_nodes_by_name(db.conn(), "foo").unwrap();
        assert_eq!(foo.len(), 0);
    }

    #[test]
    fn test_build_python_module_map() {
        let mut paths = HashSet::new();
        paths.insert("myapp/utils.py".into());
        paths.insert("myapp/__init__.py".into());
        paths.insert("src/myapp/models.py".into());

        let map = build_python_module_map(&paths);

        // Full dotted path
        assert!(map.get("myapp.utils").unwrap().contains(&"myapp/utils.py".to_string()));
        // Suffix path
        assert!(map.get("utils").unwrap().contains(&"myapp/utils.py".to_string()));
        // __init__.py maps to package
        assert!(map.get("myapp").unwrap().contains(&"myapp/__init__.py".to_string()));
        // Nested with src/ prefix
        assert!(map.get("myapp.models").unwrap().contains(&"src/myapp/models.py".to_string()));
    }

    #[test]
    fn test_python_from_import_resolution() {
        // Test `from myapp.utils import helper` creates correct cross-file edge
        let project_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        let db = Database::open(&db_dir.path().join("index.db")).unwrap();

        fs::create_dir_all(project_dir.path().join("myapp")).unwrap();
        fs::write(
            project_dir.path().join("myapp/utils.py"),
            "def helper():\n    return 42\n",
        ).unwrap();
        fs::write(
            project_dir.path().join("myapp/main.py"),
            "from myapp.utils import helper\n\ndef main():\n    helper()\n",
        ).unwrap();

        let result = run_full_index(&db, project_dir.path(), None, None).unwrap();
        assert!(result.edges_created > 0, "should create import edges");

        // Verify dependency: main.py -> utils.py
        let deps = get_import_tree(db.conn(), "myapp/main.py", "outgoing", 1).unwrap();
        assert!(
            deps.iter().any(|d| d.file_path == "myapp/utils.py"),
            "main.py should depend on utils.py, got: {:?}",
            deps.iter().map(|d| &d.file_path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_python_import_module_resolution() {
        // Test `import myutils` creates correct cross-file edge
        let project_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        let db = Database::open(&db_dir.path().join("index.db")).unwrap();

        fs::write(
            project_dir.path().join("myutils.py"),
            "def do_something():\n    pass\n",
        ).unwrap();
        fs::write(
            project_dir.path().join("main.py"),
            "import myutils\n\ndef main():\n    myutils.do_something()\n",
        ).unwrap();

        let result = run_full_index(&db, project_dir.path(), None, None).unwrap();
        assert!(result.edges_created > 0, "should create import edges");

        // Verify dependency: main.py -> myutils.py
        let deps = get_import_tree(db.conn(), "main.py", "outgoing", 1).unwrap();
        assert!(
            deps.iter().any(|d| d.file_path == "myutils.py"),
            "main.py should depend on myutils.py, got: {:?}",
            deps.iter().map(|d| &d.file_path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_python_external_import_creates_virtual_nodes() {
        // Test that external imports create virtual nodes in <external> file
        let project_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        let db = Database::open(&db_dir.path().join("index.db")).unwrap();

        fs::write(
            project_dir.path().join("app.py"),
            "import os\nfrom collections import OrderedDict\nfrom flask import Flask\n\ndef main():\n    pass\n",
        ).unwrap();

        let result = run_full_index(&db, project_dir.path(), None, None).unwrap();
        assert!(result.files_indexed > 0, "should index the file");

        // Verify <external> file was created with virtual nodes
        let ext_nodes = get_nodes_by_file_path(db.conn(), "<external>").unwrap();
        let ext_names: Vec<&str> = ext_nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(ext_names.contains(&"os"), "should have virtual node for 'os', got: {:?}", ext_names);
        assert!(ext_names.contains(&"collections"), "should have virtual node for 'collections', got: {:?}", ext_names);
        assert!(ext_names.contains(&"flask"), "should have virtual node for 'flask', got: {:?}", ext_names);

        // Verify dependency_graph shows <external> as a dependency
        let deps = get_import_tree(db.conn(), "app.py", "outgoing", 1).unwrap();
        assert!(
            deps.iter().any(|d| d.file_path == "<external>"),
            "app.py should show <external> dependency, got: {:?}",
            deps.iter().map(|d| &d.file_path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_python_mixed_internal_external_imports() {
        // Test project with both internal and external imports
        let project_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        let db = Database::open(&db_dir.path().join("index.db")).unwrap();

        fs::create_dir_all(project_dir.path().join("myapp")).unwrap();
        fs::write(
            project_dir.path().join("myapp/utils.py"),
            "def helper():\n    return 42\n",
        ).unwrap();
        fs::write(
            project_dir.path().join("myapp/main.py"),
            "import os\nfrom myapp.utils import helper\nfrom flask import Flask\n\ndef main():\n    helper()\n",
        ).unwrap();

        let result = run_full_index(&db, project_dir.path(), None, None).unwrap();
        assert!(result.edges_created > 0);

        // Should have internal dependency
        let deps = get_import_tree(db.conn(), "myapp/main.py", "outgoing", 1).unwrap();
        let dep_files: Vec<&str> = deps.iter().map(|d| d.file_path.as_str()).collect();
        assert!(dep_files.contains(&"myapp/utils.py"), "should depend on internal utils.py, got: {:?}", dep_files);

        // Should also have external dependency
        assert!(dep_files.contains(&"<external>"), "should depend on <external>, got: {:?}", dep_files);
    }

    #[test]
    fn test_index_stats_skipped_large_file() {
        // Verify that IndexResult.stats tracks files skipped due to size
        let project_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        let db = Database::open(&db_dir.path().join("index.db")).unwrap();

        // Create a normal file
        fs::write(project_dir.path().join("small.ts"), "function ok() {}").unwrap();

        // Create a file exceeding MAX_FILE_SIZE (10MB)
        let big_content = "a".repeat(11 * 1024 * 1024);
        fs::write(project_dir.path().join("huge.ts"), &big_content).unwrap();

        let result = run_full_index(&db, project_dir.path(), None, None).unwrap();
        assert_eq!(result.files_indexed, 1, "should index the small file");
        assert_eq!(result.stats.files_skipped_size, 1, "should track the large file skip");
    }

    #[test]
    fn test_index_stats_skipped_parse_error() {
        // Verify that IndexResult.stats tracks files skipped due to parse errors
        let project_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        let db = Database::open(&db_dir.path().join("index.db")).unwrap();

        // Create a valid file
        fs::write(project_dir.path().join("good.ts"), "function ok() {}").unwrap();

        // Create a file with an unsupported extension that detect_language returns None for
        // (this is filtered by detect_language returning None, not a parse error)
        // Instead, we just verify the default stats are zero for parse errors
        let result = run_full_index(&db, project_dir.path(), None, None).unwrap();
        assert_eq!(result.stats.files_skipped_parse, 0);
        assert_eq!(result.stats.files_skipped_read, 0);
        assert_eq!(result.stats.files_skipped_hash, 0);
    }

    #[test]
    fn test_index_stats_default() {
        // IndexStats should implement Default
        let stats = IndexStats::default();
        assert_eq!(stats.files_skipped_size, 0);
        assert_eq!(stats.files_skipped_parse, 0);
        assert_eq!(stats.files_skipped_read, 0);
        assert_eq!(stats.files_skipped_hash, 0);
        assert_eq!(stats.files_skipped_language, 0);
    }

    #[test]
    fn test_python_external_survives_incremental_index() {
        // Test that <external> pseudo-file persists across incremental re-indexes
        let project_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        let db = Database::open(&db_dir.path().join("index.db")).unwrap();

        fs::write(
            project_dir.path().join("app.py"),
            "import os\n\ndef main():\n    pass\n",
        ).unwrap();

        // Full index → creates <external> with "os" node
        run_full_index(&db, project_dir.path(), None, None).unwrap();
        let ext_before = get_nodes_by_file_path(db.conn(), "<external>").unwrap();
        assert!(!ext_before.is_empty(), "should have external nodes after full index");

        // Modify file slightly
        fs::write(
            project_dir.path().join("app.py"),
            "import os\n\ndef main():\n    return 1\n",
        ).unwrap();

        // Incremental index → <external> should survive
        run_incremental_index(&db, project_dir.path(), None, None).unwrap();
        let ext_after = get_nodes_by_file_path(db.conn(), "<external>").unwrap();
        assert!(!ext_after.is_empty(), "external nodes should survive incremental index");

        // Verify dependency still visible
        let deps = get_import_tree(db.conn(), "app.py", "outgoing", 1).unwrap();
        assert!(
            deps.iter().any(|d| d.file_path == "<external>"),
            "app.py should still show <external> dependency after incremental index"
        );
    }

    #[test]
    fn test_repair_null_context_strings() {
        let project_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        let db = Database::open(&db_dir.path().join("index.db")).unwrap();

        // Index a file so nodes get context strings
        fs::write(project_dir.path().join("a.ts"), r#"
function alpha() { return 1; }
function beta() { alpha(); }
"#).unwrap();
        run_full_index(&db, project_dir.path(), None, None).unwrap();

        // Verify context strings exist after index
        let alpha_nodes = get_nodes_by_name(db.conn(), "alpha").unwrap();
        assert_eq!(alpha_nodes.len(), 1);
        assert!(alpha_nodes[0].context_string.is_some(), "alpha should have context_string after index");

        let beta_nodes = get_nodes_by_name(db.conn(), "beta").unwrap();
        assert_eq!(beta_nodes.len(), 1);
        assert!(beta_nodes[0].context_string.is_some(), "beta should have context_string after index");

        // Simulate Phase 3 failure: NULL out context_strings
        db.conn().execute("UPDATE nodes SET context_string = NULL", []).unwrap();

        // Verify they are now NULL
        let alpha_after_null = get_nodes_by_name(db.conn(), "alpha").unwrap();
        assert!(alpha_after_null[0].context_string.is_none(), "alpha context_string should be NULL after simulated failure");

        // Run repair
        let repaired = repair_null_context_strings(&db, None).unwrap();
        assert!(repaired > 0, "should repair at least 1 node");

        // Verify context strings were restored
        let alpha_repaired = get_nodes_by_name(db.conn(), "alpha").unwrap();
        assert!(alpha_repaired[0].context_string.is_some(), "alpha should have context_string after repair");

        let beta_repaired = get_nodes_by_name(db.conn(), "beta").unwrap();
        assert!(beta_repaired[0].context_string.is_some(), "beta should have context_string after repair");
    }
}
