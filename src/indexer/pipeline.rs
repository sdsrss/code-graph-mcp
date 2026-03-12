use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::embedding::context::{build_context_string, NodeContext};
use crate::embedding::model::EmbeddingModel;
use crate::indexer::merkle::{compute_diff, hash_file, scan_directory, scan_directory_cached, DirectoryCache};
use crate::parser::relations::extract_relations_from_tree;
use crate::parser::treesitter::{parse_tree, extract_nodes_from_tree};
use crate::storage::db::Database;
use crate::storage::queries::*;
use crate::domain::{REL_CALLS, REL_IMPORTS, REL_INHERITS, REL_ROUTES_TO, REL_IMPLEMENTS, REL_EXPORTS, MAX_FILE_SIZE};
use crate::utils::config::detect_language;

pub struct IndexResult {
    pub files_indexed: usize,
    pub nodes_created: usize,
    pub edges_created: usize,
}

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

fn try_embed_and_store(db: &Database, model: Option<&EmbeddingModel>, node_id: i64, ctx: &str) {
    if let Some(m) = model {
        if db.vec_enabled() {
            match m.embed(ctx) {
                Ok(embedding) => {
                    if let Err(e) = insert_node_vector(db.conn(), node_id, &embedding) {
                        tracing::warn!("Failed to insert vector for node {}: {}", node_id, e);
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to embed node {}: {}", node_id, e);
                }
            }
        }
    }
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

pub fn run_full_index(db: &Database, project_root: &Path, model: Option<&EmbeddingModel>) -> Result<IndexResult> {
    let current_hashes = scan_directory(project_root)?;
    let files: Vec<String> = current_hashes.keys().cloned().collect();
    index_files(db, project_root, &files, &current_hashes, model, &[])
}

pub fn run_incremental_index(db: &Database, project_root: &Path, model: Option<&EmbeddingModel>) -> Result<IndexResult> {
    let stored_hashes = get_all_file_hashes(db.conn())?;
    let current_hashes = scan_directory(project_root)?;
    let diff = compute_diff(&stored_hashes, &current_hashes);

    // Index changed + new files (deletion of removed files happens inside index_files transaction)
    let deleted_files = diff.deleted_files;
    let to_index: Vec<String> = [diff.new_files, diff.changed_files].concat();

    // Dirty-node propagation: identify dirty nodes BEFORE re-indexing
    // (because cascade delete will remove old edges)
    let dirty_node_ids = if !to_index.is_empty() {
        collect_dirty_node_ids(db, &to_index)?
    } else {
        HashSet::new()
    };

    let result = index_files(db, project_root, &to_index, &current_hashes, model, &deleted_files)?;

    // Regenerate context strings (and embeddings) for dirty nodes in other files
    if !dirty_node_ids.is_empty() {
        regenerate_context_strings(db, &dirty_node_ids, model)?;
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
) -> Result<(IndexResult, DirectoryCache)> {
    let stored_hashes = get_all_file_hashes(db.conn())?;
    let (mut current_hashes, new_cache) = scan_directory_cached(project_root, dir_cache)?;

    // Merge stored hashes for files in unchanged directories.
    // scan_directory_cached skips files in unchanged dirs, so we need to
    // carry forward their stored hashes to prevent false "deleted" diffs.
    for (path, hash) in &stored_hashes {
        if !current_hashes.contains_key(path) {
            // File wasn't scanned (in unchanged dir) — carry forward stored hash
            // Only if the file still exists on disk
            if project_root.join(path).exists() {
                current_hashes.insert(path.clone(), hash.clone());
            }
        }
    }

    let diff = compute_diff(&stored_hashes, &current_hashes);

    let deleted_files = diff.deleted_files;
    let to_index: Vec<String> = [diff.new_files, diff.changed_files].concat();

    let dirty_node_ids = if !to_index.is_empty() {
        collect_dirty_node_ids(db, &to_index)?
    } else {
        HashSet::new()
    };

    let result = index_files(db, project_root, &to_index, &current_hashes, model, &deleted_files)?;

    if !dirty_node_ids.is_empty() {
        regenerate_context_strings(db, &dirty_node_ids, model)?;
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
    let id_vec: Vec<i64> = dirty_ids.iter().copied().collect();
    let all_edges = get_edges_batch(db.conn(), &id_vec)?;
    let all_nodes: HashMap<i64, (NodeResult, String)> = {
        let nwfs = get_nodes_with_files_by_ids(db.conn(), &id_vec)?;
        nwfs.into_iter().map(|nwf| (nwf.node.id, (nwf.node, nwf.file_path))).collect()
    };

    for &node_id in dirty_ids {
        if let Some((node, file_path)) = all_nodes.get(&node_id) {
            let edges = all_edges.get(&node_id);
            let cat = categorize_edges(edges, format_route_from_metadata);

            let ctx = build_context_string(&NodeContext {
                node_type: node.node_type.clone(),
                name: node.name.clone(),
                file_path: file_path.clone(),
                signature: node.signature.clone(),
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

            update_context_string(db.conn(), node_id, &ctx)?;
            try_embed_and_store(db, model, node_id, &ctx);
        }
    }
    Ok(())
}

fn index_files(
    db: &Database,
    root: &Path,
    files: &[String],
    hashes: &HashMap<String, String>,
    model: Option<&EmbeddingModel>,
    delete_paths: &[String],
) -> Result<IndexResult> {
    let tx = db.conn().unchecked_transaction()?;

    // Phase 0: Delete removed files inside transaction
    if !delete_paths.is_empty() {
        delete_files_by_paths(db.conn(), delete_paths)?;
    }

    let mut nodes_created = 0usize;
    let mut edges_created = 0usize;

    // Collect all parsed data first
    struct FileParsed {
        rel_path: String,
        source: String,
        language: String,
        tree: tree_sitter::Tree,
        file_id: i64,
        node_ids: Vec<i64>,
        node_names: Vec<String>,
    }

    let mut parsed_files: Vec<FileParsed> = Vec::new();

    // Phase 1: Parse files once, extract nodes, insert into DB
    for rel_path in files {
        let abs_path = root.join(rel_path);
        let language = match detect_language(rel_path) {
            Some(lang) => lang,
            None => continue, // Skip unsupported file types
        };

        let hash = match hashes.get(rel_path.as_str()) {
            Some(h) => h.clone(),
            None => hash_file(&abs_path)?,
        };

        // Skip files larger than 1MB to avoid OOM on generated/bundled files
        let file_meta = std::fs::metadata(&abs_path).ok();
        if let Some(ref meta) = file_meta {
            if meta.len() > MAX_FILE_SIZE {
                tracing::debug!("Skipping large file ({} bytes): {}", meta.len(), rel_path);
                continue;
            }
        }

        let source = match std::fs::read_to_string(&abs_path) {
            Ok(s) => s,
            Err(_) => continue,
        };

        // Parse once — shared by Phase 1 (nodes) and Phase 2 (relations)
        let tree = match parse_tree(&source, language) {
            Ok(t) => t,
            Err(_) => continue, // Skip files that fail to parse
        };

        // Delete old nodes for this file if it was previously indexed
        let last_modified = file_meta
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let file_id = upsert_file(db.conn(), &FileRecord {
            path: rel_path.clone(),
            blake3_hash: hash,
            last_modified,
            language: Some(language.to_string()),
        })?;

        // Delete old nodes for this file (they'll be re-created)
        delete_nodes_by_file(db.conn(), file_id)?;

        let parsed_nodes = extract_nodes_from_tree(&tree, &source, language);

        let mut node_ids = Vec::new();
        let mut node_names = Vec::new();

        // Synthetic <module> node — anchor for import/export edges
        let module_node_id = insert_node(db.conn(), &NodeRecord {
            file_id,
            node_type: "module".into(),
            name: "<module>".into(),
            qualified_name: Some(rel_path.clone()),
            start_line: 1,
            end_line: source.lines().count() as i64,
            code_content: String::new(),
            signature: None,
            doc_comment: None,
            context_string: None,
        })?;
        node_ids.push(module_node_id);
        node_names.push("<module>".into());
        nodes_created += 1;

        for pn in &parsed_nodes {
            let node_id = insert_node(db.conn(), &NodeRecord {
                file_id,
                node_type: pn.node_type.clone(),
                name: pn.name.clone(),
                qualified_name: pn.qualified_name.clone(),
                start_line: pn.start_line as i64,
                end_line: pn.end_line as i64,
                code_content: pn.code_content.clone(),
                signature: pn.signature.clone(),
                doc_comment: pn.doc_comment.clone(),
                context_string: None, // Phase 3 will fill this
            })?;
            node_ids.push(node_id);
            node_names.push(pn.name.clone());
            nodes_created += 1;
        }

        parsed_files.push(FileParsed {
            rel_path: rel_path.clone(),
            source,
            language: language.to_string(),
            tree,
            file_id,
            node_ids,
            node_names,
        });
    }

    // Phase 2: Extract relations, insert edges
    // Build a lookup from node name -> node_ids (for cross-file resolution)
    let mut name_to_ids: HashMap<String, Vec<i64>> = HashMap::new();
    for pf in &parsed_files {
        for (id, name) in pf.node_ids.iter().zip(pf.node_names.iter()) {
            name_to_ids.entry(name.clone()).or_default().push(*id);
        }
    }
    // Also include existing nodes not being re-indexed (exclude files we just re-indexed)
    let indexed_file_ids: Vec<i64> = parsed_files.iter().map(|pf| pf.file_id).collect();
    let all_existing = get_node_names_excluding_files(db.conn(), &indexed_file_ids)?;
    for (name, id) in &all_existing {
        name_to_ids.entry(name.clone()).or_default().push(*id);
    }
    // Deduplicate
    for ids in name_to_ids.values_mut() {
        ids.sort();
        ids.dedup();
    }

    for pf in &parsed_files {
        let relations = extract_relations_from_tree(&pf.tree, &pf.source, &pf.language);
        let local_ids: HashSet<i64> = pf.node_ids.iter().copied().collect();

        for rel in &relations {
            // Find source node ID
            let source_ids = pf.node_names.iter()
                .zip(pf.node_ids.iter())
                .filter(|(name, _)| *name == &rel.source_name)
                .map(|(_, id)| *id)
                .collect::<Vec<_>>();

            let all_target_ids = name_to_ids.get(&rel.target_name)
                .cloned()
                .unwrap_or_default();

            // Prefer same-file targets to reduce false-positive cross-file edges
            let same_file_targets: Vec<i64> = all_target_ids.iter()
                .filter(|id| local_ids.contains(id))
                .copied()
                .collect();
            let target_ids = if !same_file_targets.is_empty() {
                same_file_targets
            } else {
                all_target_ids
            };

            for &src_id in &source_ids {
                for &tgt_id in &target_ids {
                    // Allow self-edges for routes (routes_to maps handler to itself with metadata)
                    if (src_id != tgt_id || rel.relation == REL_ROUTES_TO)
                        && insert_edge(db.conn(), src_id, tgt_id, &rel.relation, rel.metadata.as_deref())? {
                        edges_created += 1;
                    }
                }
            }
        }
    }

    // Phase 3: Build context strings and update nodes (batch edge fetch)
    let all_node_ids: Vec<i64> = parsed_files.iter().flat_map(|pf| pf.node_ids.iter().copied()).collect();
    let all_edges = get_edges_batch(db.conn(), &all_node_ids)?;
    // Batch-fetch node details for signature/doc
    let all_node_details: HashMap<i64, NodeResult> = {
        let nodes = get_nodes_with_files_by_ids(db.conn(), &all_node_ids)?;
        nodes.into_iter().map(|nwf| (nwf.node.id, nwf.node)).collect()
    };

    for pf in &parsed_files {
        for (idx, &node_id) in pf.node_ids.iter().enumerate() {
            let node_name = &pf.node_names[idx];
            let edges = all_edges.get(&node_id);
            let cat = categorize_edges(edges, format_route_from_metadata);

            let node_detail = all_node_details.get(&node_id);

            let ctx = build_context_string(&NodeContext {
                node_type: node_detail.map(|n| n.node_type.clone()).unwrap_or_default(),
                name: node_name.clone(),
                file_path: pf.rel_path.clone(),
                signature: node_detail.and_then(|n| n.signature.clone()),
                code_content: node_detail.map(|n| n.code_content.clone()),
                routes: cat.routes,
                callees: cat.callees,
                callers: cat.callers,
                inherits: cat.inherits,
                imports: cat.imports,
                implements: cat.implements,
                exports: cat.exports,
                doc_comment: node_detail.and_then(|n| n.doc_comment.clone()),
            });

            update_context_string(db.conn(), node_id, &ctx)?;
            try_embed_and_store(db, model, node_id, &ctx);
        }
    }

    tx.commit()?;

    Ok(IndexResult {
        files_indexed: parsed_files.len(),
        nodes_created,
        edges_created,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let result = run_full_index(&db, project_dir.path(), None).unwrap();

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
        run_full_index(&db, project_dir.path(), None).unwrap();

        // Modify file
        fs::write(project_dir.path().join("a.ts"), "function bar() {}").unwrap();

        // Incremental index
        let result = run_incremental_index(&db, project_dir.path(), None).unwrap();
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
        run_full_index(&db, project_dir.path(), None).unwrap();

        let beta_nodes = get_nodes_by_name(db.conn(), "beta").unwrap();
        assert_eq!(beta_nodes.len(), 1);
        let beta_ctx_before = beta_nodes[0].context_string.clone().unwrap_or_default();

        // Change A: rename function (alpha -> alphaRenamed)
        fs::write(project_dir.path().join("a.ts"), "function alphaRenamed() {}").unwrap();
        run_incremental_index(&db, project_dir.path(), None).unwrap();

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
        run_full_index(&db, project_dir.path(), None).unwrap();

        fs::remove_file(project_dir.path().join("a.ts")).unwrap();
        run_incremental_index(&db, project_dir.path(), None).unwrap();

        let foo = get_nodes_by_name(db.conn(), "foo").unwrap();
        assert_eq!(foo.len(), 0);
    }
}
