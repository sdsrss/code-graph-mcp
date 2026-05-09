//! Context-string assembly for a node + edge bundle, plus the two recovery
//! paths that re-run that assembly outside the main indexer:
//! - `regenerate_context_strings`: incremental dirty propagation (rebuilds
//!   ctx for nodes whose cross-file edges flipped during a re-index).
//! - `repair_null_context_strings`: startup repair when a prior Phase 3
//!   transaction failed and left rows with NULL context_string.
//!
//! `categorize_edges` and `format_route_from_metadata` are also used by the
//! main `index_files` Phase 3 builder, so they live here as `pub(super)`.

use anyhow::Result;
use std::collections::{HashMap, HashSet};

use crate::embedding::context::{build_context_string, NodeContext};
use crate::embedding::model::EmbeddingModel;
use crate::storage::db::Database;
use crate::storage::queries::{
    get_edges_batch, get_nodes_missing_context, get_nodes_with_files_by_ids,
    update_context_strings_batch, EdgeInfo, NodeResult,
};
use crate::domain::{REL_CALLS, REL_IMPORTS, REL_INHERITS, REL_ROUTES_TO, REL_IMPLEMENTS, REL_EXPORTS};

use super::embed::embed_and_store_batch;

/// Extract "METHOD path" from route edge metadata JSON, falling back to the edge name.
pub(super) fn format_route_from_metadata(metadata: Option<&str>, name: &str) -> String {
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

pub(super) struct CategorizedEdges {
    pub callees: Vec<String>,
    pub callers: Vec<String>,
    pub inherits: Vec<String>,
    pub routes: Vec<String>,
    pub imports: Vec<String>,
    pub implements: Vec<String>,
    pub exports: Vec<String>,
}

pub(super) fn categorize_edges(edges: Option<&Vec<EdgeInfo>>, format_route: impl Fn(Option<&str>, &str) -> String) -> CategorizedEdges {
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

/// Regenerate context strings (and embeddings) for the given set of dirty nodes.
pub(super) fn regenerate_context_strings(db: &Database, dirty_ids: &HashSet<i64>, model: Option<&EmbeddingModel>) -> Result<()> {
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
