//! Batch embedding + vector store. Wraps `EmbeddingModel::embed_batch` with
//! a per-batch DB transaction; on batch failure falls back to per-row embed
//! so a single malformed input doesn't tank the whole sweep.
//!
//! Public so `mcp::server` can call it from the background embedding thread
//! (separate from the indexer's foreground Phase 3 path).

use anyhow::Result;

use crate::embedding::model::EmbeddingModel;
use crate::storage::db::Database;
use crate::storage::queries::insert_node_vectors_batch;

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
