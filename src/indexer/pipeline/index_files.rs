//! Single-pass batched indexer. Phases share local state (transaction,
//! atomics, batch_parsed, name_to_ids, global_name_map) so the function
//! itself stays whole — the *helpers* that feed it (context, embedding,
//! Python module map, ambiguity refinement, pending-call sweep) live in
//! sibling modules.
//!
//! Phase outline:
//! - 0: delete files; pre-cascade-buffer inbound calls into pending so
//!   B → A.foo doesn't silently vanish when only A is in `delete_paths`.
//! - 1a: parallel CPU work (read + parse + extract nodes) via rayon.
//! - 1b: sequential DB inserts (file row, node rows; cascades old nodes).
//! - 2: extract relations, resolve to edges with same-file → same-language
//!   → drop/global tier order; buffer unresolved bare-name same-language
//!   calls into pending instead of dropping; track external imports/symbols.
//! - 2b / 2b-ext: virtual `<external>` nodes for unresolved imports/traits.
//! - 2c: restore cross-file inbound edges that cascade-delete just stripped.
//! - 3: build context strings (parallel), batch-update, then embed outside tx.
//! - 2c sweep: drain `pending_unresolved_calls` against the new node state.

use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use rayon::prelude::*;

use crate::embedding::context::{build_context_string, NodeContext};
use crate::embedding::model::EmbeddingModel;
use crate::indexer::merkle::hash_file;
use crate::parser::relations::extract_relations_from_tree;
use crate::parser::treesitter::{parse_tree, extract_nodes_from_tree};
use crate::search::tokenizer::split_identifier;
use crate::storage::db::Database;
use crate::storage::queries::{
    delete_files_by_paths, delete_nodes_by_file,
    get_all_node_names_with_ids, get_edges_batch,
    get_inbound_cross_file_edges,
    get_nodes_by_file_path,
    get_nodes_with_files_by_ids,
    insert_edge_cached, insert_node_cached,
    update_context_strings_batch, upsert_file,
    FileRecord, NodeRecord, NodeResult,
};
use crate::domain::{REL_CALLS, REL_IMPORTS, REL_ROUTES_TO, REL_IMPLEMENTS, max_file_size, CROSS_FILE_CALL_NOISE};
use crate::utils::config::detect_language;

use super::{IndexResult, IndexStats, ProgressFn};
use super::context::{categorize_edges, format_route_from_metadata};
use super::embed::embed_and_store_batch;
use super::python_modules::{build_python_module_map, resolve_python_module_targets};
use super::resolve::{refine_ambiguous_targets, resolve_pending_calls};

/// Batch size for streaming indexing. Each batch processes Phase 1+2
/// then drops heavyweight data (ASTs, source strings) before the next batch.
const BATCH_SIZE: usize = 500;

/// Lightweight post-batch record — no Tree or source string.
pub(super) struct FileIndexed {
    pub rel_path: String,
    pub node_ids: Vec<i64>,
    pub node_names: Vec<String>,
}

pub(super) fn index_files(
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

    // Phase 0: Delete removed files in own transaction.
    //
    // Before cascade strips inbound REL_CALLS edges, capture them as pending
    // rows. Without this, deleting file A wipes B's edge to A.foo and B is
    // not in `delete_paths` (so Phase 2 won't re-extract it), leaving B with
    // neither an edge nor a pending row — the same staleness window the
    // "callee added later" buffering closes, just from the deletion side.
    // Both directions need to round-trip through pending or the v0.18.2 fix
    // is only half-complete.
    if !delete_paths.is_empty() {
        let tx = db.conn().unchecked_transaction()?;

        // Resolve file IDs once (delete_files_by_paths drops them) so we can
        // query inbound calls before cascade fires.
        let mut deleted_file_ids: Vec<i64> = Vec::with_capacity(delete_paths.len());
        for path in delete_paths {
            if let Ok(Some(fid)) = db.conn().query_row(
                "SELECT id FROM files WHERE path = ?1",
                [path],
                |row| row.get::<_, Option<i64>>(0),
            ) {
                deleted_file_ids.push(fid);
            }
        }

        let mut buffered = 0usize;
        for fid in &deleted_file_ids {
            let inbound = crate::storage::queries::get_inbound_calls_for_pending(db.conn(), *fid)?;
            for (source_id, target_name, source_language, metadata) in inbound {
                crate::storage::queries::insert_pending_unresolved_call(
                    db.conn(),
                    source_id,
                    &target_name,
                    &source_language,
                    metadata.as_deref(),
                )?;
                buffered += 1;
            }
        }
        if buffered > 0 {
            tracing::info!(
                "[index] Phase 0: buffered {} inbound calls before cascade-deleting {} file(s)",
                buffered, deleted_file_ids.len()
            );
        }

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

    // Pre-load global name->[(id, path, language)] map once before the batch loop.
    // This avoids a full table scan per batch in Phase 2 relation resolution.
    // The map is updated incrementally as each batch commits new nodes.
    // `language` drives same-language-preferred resolution to avoid cross-language
    // bare-name collisions (e.g. Rust `hasher.update()` resolving to JS `function update`).
    let mut global_name_map: HashMap<String, Vec<crate::storage::queries::NameEntry>> =
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
        // Saved inbound edges from other files → batch files (to restore after cascade delete)
        // Tuple: (source_id, source_file_id, target_name, relation, metadata)
        let mut saved_inbound_edges: Vec<(i64, i64, String, String, Option<String>)> = Vec::new();
        // Track file_ids in this batch to filter intra-batch edges in Phase 2c
        let mut batch_file_ids: HashSet<i64> = HashSet::new();

        // --- Phase 1b: Sequential DB inserts ---
        for pp in pre_parsed {
            let file_id = upsert_file(db.conn(), &FileRecord {
                path: pp.rel_path.clone(),
                blake3_hash: pp.hash,
                last_modified: pp.last_modified,
                language: Some(pp.language.clone()),
            })?;

            // Save cross-file inbound edges before cascade delete destroys them
            saved_inbound_edges.extend(get_inbound_cross_file_edges(db.conn(), file_id)?);
            batch_file_ids.insert(file_id);

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
        // Per-node language for same-language-preferred edge resolution (§ cross-lang collision).
        let mut node_id_to_language: HashMap<i64, Option<String>> = HashMap::new();

        // Add current batch's newly inserted nodes
        for pf in &batch_parsed {
            for (id, name) in pf.node_ids.iter().zip(pf.node_names.iter()) {
                name_to_ids.entry(name.clone()).or_default().push(*id);
                node_id_to_path.insert(*id, pf.rel_path.clone());
                node_id_to_language.insert(*id, Some(pf.language.clone()));
            }
        }

        // Add nodes from the global map, excluding those in current batch's files
        // (their old nodes were deleted and replaced by new ones above)
        for (name, entries) in &global_name_map {
            for (id, path, language) in entries {
                if !batch_file_paths.contains(path.as_str()) {
                    name_to_ids.entry(name.clone()).or_default().push(*id);
                    node_id_to_path.insert(*id, path.clone());
                    node_id_to_language.insert(*id, language.clone());
                }
            }
        }

        for ids in name_to_ids.values_mut() {
            ids.sort();
            ids.dedup();
        }

        // Track unresolved external Python imports: (source_module_node_id, module_name)
        let mut external_python_imports: Vec<(i64, String)> = Vec::new();
        // Track unresolved external symbols for sentinel node creation:
        // (source_id, target_name, relation) — e.g., implements edges to external traits
        let mut unresolved_externals: Vec<(i64, String, String)> = Vec::new();

        for pf in &batch_parsed {
            let relations = extract_relations_from_tree(&pf.tree, &pf.source, &pf.language);
            let local_ids: HashSet<i64> = pf.node_ids.iter().copied().collect();

            for rel in &relations {
                // Contract: extract_relations_from_tree stamps every relation with
                // source_language equal to the language argument. The
                // same-language resolution at line 811+ depends on it. Hard
                // error instead of debug_assert so a parser regression fails
                // loudly in release builds too (one string compare per
                // relation is negligible against the SQL writes below).
                if rel.source_language != pf.language {
                    anyhow::bail!(
                        "ParsedRelation.source_language ({}) does not match file language ({}); \
                         parser regressed the source_language contract",
                        rel.source_language, pf.language
                    );
                }

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

                // Default resolution: global name-based lookup with language-aware layering.
                // Tier order: same-file → same-language → (calls: drop) / (other: global).
                // Dropping calls without a same-language match prevents Rust `hasher.update()`
                // binding to an unrelated JS `function update()` via bare-name collision.
                let all_target_ids = name_to_ids.get(&rel.target_name)
                    .cloned()
                    .unwrap_or_default();

                let same_file_targets: Vec<i64> = all_target_ids.iter()
                    .filter(|id| local_ids.contains(id))
                    .copied()
                    .collect();

                let source_lang = pf.language.as_str();
                let same_language_targets: Vec<i64> = all_target_ids.iter()
                    .filter(|id| !local_ids.contains(id))
                    .filter(|id| matches!(
                        node_id_to_language.get(id).and_then(|l| l.as_deref()),
                        Some(l) if l == source_lang
                    ))
                    .copied()
                    .collect();

                let target_ids = if !same_file_targets.is_empty() {
                    same_file_targets
                } else if rel.relation == REL_CALLS
                    && CROSS_FILE_CALL_NOISE.contains(&rel.target_name.as_str())
                {
                    // Stdlib method names (new/default/from) — drop regardless of language.
                    continue;
                } else if !same_language_targets.is_empty() {
                    // Ambiguous cross-file same-language candidates (e.g. a helper
                    // name like `readJson` defined in multiple JS files) used to
                    // fan out — every same-name target got an edge, producing
                    // phantom callers across unrelated modules. Refine by
                    // non-test preference + longest common path prefix with the
                    // caller file. See `refine_ambiguous_targets` for fallback
                    // policy (keeps remaining pool on ambiguity to avoid
                    // regressing dead-code on bare-name Rust scoped calls).
                    refine_ambiguous_targets(
                        &same_language_targets,
                        &pf.rel_path,
                        &node_id_to_path,
                    )
                } else if rel.relation == REL_CALLS {
                    // No same-file, no same-language candidate → buffer in
                    // pending_unresolved_calls instead of silently dropping.
                    // The post-Phase-2 sweep below promotes the row to a real
                    // edge as soon as a same-language target appears (e.g.
                    // sibling file added in a later incremental pass). Memory
                    // `feedback_incremental_edge_timing.md` documented the bug
                    // this closes: B's bare-name call to `foo()` got dropped
                    // when foo didn't exist yet, and never re-resolved when A
                    // later added `foo`. Schema cascade on source_id self-cleans
                    // when callers are removed/reindexed.
                    for &src_id in &source_ids {
                        crate::storage::queries::insert_pending_unresolved_call(
                            db.conn(),
                            src_id,
                            &rel.target_name,
                            &pf.language,
                            rel.metadata.as_deref(),
                        )?;
                    }
                    continue;
                } else {
                    all_target_ids
                };

                if target_ids.is_empty()
                    && (rel.relation == REL_IMPLEMENTS || rel.relation == REL_IMPORTS)
                {
                    // Unresolved implements target (external trait like Write, Default)
                    // OR unresolved import target (JS `require('fs')`, unresolved JS
                    // ES-import binding). Phase 2b-ext creates `<external>/<name>`
                    // sentinel nodes so the dependency graph shows the link.
                    for &src_id in &source_ids {
                        unresolved_externals.push((src_id, rel.target_name.clone(), rel.relation.clone()));
                    }
                } else {
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

        // Phase 2b-ext: Create sentinel nodes for unresolved external symbols
        // (e.g., Rust `impl Write for SharedStdout` where Write is from std::io)
        if !unresolved_externals.is_empty() {
            let ext_file_id = upsert_file(db.conn(), &FileRecord {
                path: "<external>".into(),
                blake3_hash: "external".into(),
                last_modified: 0,
                language: Some("external".into()),
            })?;

            let existing_ext_nodes: HashMap<String, i64> =
                get_nodes_by_file_path(db.conn(), "<external>")?
                    .into_iter()
                    .map(|n| (n.name.clone(), n.id))
                    .collect();

            let mut ext_node_ids: HashMap<String, i64> = existing_ext_nodes;

            // Collect unique targets with inferred type
            let unique_targets: HashMap<&str, &str> = unresolved_externals.iter()
                .map(|(_, name, rel)| {
                    let node_type = if rel == REL_IMPLEMENTS { "trait" } else { "module" };
                    (name.as_str(), node_type)
                })
                .collect();

            for (&name, &node_type) in &unique_targets {
                if !ext_node_ids.contains_key(name) {
                    let node_id = insert_node_cached(db.conn(), &NodeRecord {
                        file_id: ext_file_id,
                        node_type: node_type.into(),
                        name: name.into(),
                        qualified_name: Some(format!("<external>/{}", name)),
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
                    ext_node_ids.insert(name.into(), node_id);
                    total_nodes_created += 1;
                }
            }

            for (source_id, target_name, relation) in &unresolved_externals {
                if let Some(&ext_id) = ext_node_ids.get(target_name.as_str()) {
                    if insert_edge_cached(db.conn(), *source_id, ext_id, relation, None)? {
                        total_edges_created += 1;
                    }
                }
            }
        }

        // Phase 2c: Restore cross-file inbound edges lost to cascade delete.
        // When a file is re-indexed, its old nodes are deleted (cascade-deleting edges).
        // Edges from OTHER files into the re-indexed file must be rebuilt using new node IDs.
        if !saved_inbound_edges.is_empty() {
            // Build name → new_node_id map for batch files only
            let mut batch_name_to_ids: HashMap<&str, Vec<i64>> = HashMap::new();
            for pf in &batch_parsed {
                for (id, name) in pf.node_ids.iter().zip(pf.node_names.iter()) {
                    batch_name_to_ids.entry(name.as_str()).or_default().push(*id);
                }
            }

            let mut restored = 0usize;
            let mut skipped_intra_batch = 0usize;
            for (source_id, source_file_id, target_name, relation, metadata) in &saved_inbound_edges {
                // Source file is also in this batch — source_id is stale (deleted + re-created).
                // Phase 2 already resolves cross-file edges for intra-batch files.
                if batch_file_ids.contains(source_file_id) {
                    skipped_intra_batch += 1;
                    continue;
                }
                if let Some(new_target_ids) = batch_name_to_ids.get(target_name.as_str()) {
                    for &new_tgt_id in new_target_ids {
                        if *source_id != new_tgt_id
                            && insert_edge_cached(db.conn(), *source_id, new_tgt_id, relation, metadata.as_deref())? {
                            total_edges_created += 1;
                            restored += 1;
                        }
                    }
                }
            }
            if restored > 0 || skipped_intra_batch > 0 {
                tracing::debug!("[index] Restored {} cross-file inbound edges, skipped {} intra-batch", restored, skipped_intra_batch);
            }
        }

        tx.commit()?;

        let batch_file_count = batch_parsed.len();

        // Update global_name_map: remove old entries for batch files, add new ones
        for (_, entries) in global_name_map.iter_mut() {
            entries.retain(|(_id, path, _lang)| !batch_file_paths.contains(path.as_str()));
        }
        global_name_map.retain(|_, entries| !entries.is_empty());

        // Convert to lightweight records — drops Tree and source string
        for pf in batch_parsed {
            // Add newly committed nodes to the global map
            let pf_lang = Some(pf.language.clone());
            for (id, name) in pf.node_ids.iter().zip(pf.node_names.iter()) {
                global_name_map.entry(name.clone())
                    .or_default()
                    .push((*id, pf.rel_path.clone(), pf_lang.clone()));
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

    // Phase 2c: sweep pending_unresolved_calls — promote any rows whose
    // target_name now resolves against a same-language node. Cheap when the
    // table is empty (typical after a full index of a self-contained codebase).
    let pending_resolved = resolve_pending_calls(db)?;
    total_edges_created += pending_resolved;
    if pending_resolved > 0 {
        tracing::info!(
            "[index] Phase 2c: resolved {} pending unresolved calls",
            pending_resolved
        );
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
