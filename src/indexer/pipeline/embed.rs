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
///
/// Returns the node IDs that ACTUALLY got a vector (≤ input). The backfill driver uses
/// this to advance past nodes that failed to embed — a deterministically un-embeddable
/// node (or a transient per-node inference error in the sequential fallback) otherwise
/// stays `WHERE node_vectors IS NULL` and, being ordered first by caller-count, would be
/// re-fetched at the head of every batch and starve the embeddable nodes behind it.
/// Indexing callers ignore the return.
pub fn embed_and_store_batch(
    db: &Database,
    model: &EmbeddingModel,
    context_updates: &[(i64, String)],
) -> Result<Vec<i64>> {
    if context_updates.is_empty() {
        return Ok(Vec::new());
    }

    let t0 = std::time::Instant::now();

    // Same-dim model-swap guard AT THE CHOKEPOINT. Every embed path funnels through this function
    // (foreground Phase 3 / index_files, CLI rebuild-index / reindex, incremental / context, and
    // the background backfill), so validating the model's content fingerprint HERE — before the
    // cache read below — closes the parity gap where only the MCP backfill checked it. Without
    // this, a CLI `rebuild-index` after a same-dim model change (invisible to the vec-table dim
    // check) would reuse STALE old-model embeddings from the cache with NO self-heal on a
    // CLI-only deployment. Drops stale cache + node_vectors on a real change; a no-op meta compare
    // when unchanged / first run. (The backfill ALSO checks before its work-loop, so an
    // all-already-embedded index still invalidates when zero nodes route through here.)
    #[cfg(feature = "embed-model")]
    if let Err(e) = crate::storage::queries::ensure_embedding_cache_valid(
        db.conn(),
        EmbeddingModel::MODEL_CONTENT_BLAKE3,
    ) {
        tracing::warn!("[embed-cache] validity check failed (continuing): {}", e);
    }

    // Reuse embeddings for unchanged content instead of re-running the model — this is what
    // turns a rebuild "从 1% 重建" into a byte copy. partition_by_cache splits into cache HITS
    // (already computed in a prior generation / seeded from surviving vectors, copied straight
    // in) and MISSES (embedded below). Centralised HERE so EVERY embed path reuses: foreground
    // Phase 3 (index_files), CLI rebuild-index, repair, and the background backfill all funnel
    // through this function.
    let (cached, to_embed) =
        crate::storage::queries::partition_by_cache(db.conn(), context_updates)?;
    let reused = cached.len();
    let mut embedded_ids: Vec<i64> = Vec::with_capacity(context_updates.len());
    if !cached.is_empty() {
        // insert_node_vectors_batch carries the orphan-race existence guard, so a hit for a
        // node deleted since the chunk was fetched is silently skipped (no orphan).
        let tx = db.conn().unchecked_transaction()?;
        insert_node_vectors_batch(db.conn(), &cached)?;
        tx.commit()?;
        embedded_ids.extend(cached.iter().map(|(id, _)| *id));
    }
    if to_embed.is_empty() {
        if reused > 0 {
            tracing::info!(
                "[embed] {} nodes reused from cache in {:.1}s",
                reused,
                t0.elapsed().as_secs_f64()
            );
        }
        return Ok(embedded_ids);
    }

    let texts: Vec<&str> = to_embed.iter().map(|(_, ctx)| ctx.as_str()).collect();
    let ids: Vec<i64> = to_embed.iter().map(|(id, _)| *id).collect();
    // Index context by node_id so both store paths can key content-hash cache entries. Every
    // freshly-computed embedding is also written to embedding_cache, so a later full rebuild
    // reuses it by content instead of re-running the model.
    let ctx_by_id: std::collections::HashMap<i64, &str> = to_embed
        .iter()
        .map(|(id, ctx)| (*id, ctx.as_str()))
        .collect();
    let cache_entries_for = |vectors: &[(i64, Vec<f32>)]| -> Vec<([u8; 32], Vec<f32>)> {
        vectors
            .iter()
            .filter_map(|(id, emb)| {
                ctx_by_id
                    .get(id)
                    .map(|ctx| (crate::storage::queries::cache_key(ctx), emb.clone()))
            })
            .collect()
    };

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
            let vectors: Vec<(i64, Vec<f32>)> = ids
                .iter()
                .zip(embs)
                .filter_map(|(&id, emb)| emb.map(|e| (id, e)))
                .collect();
            if !vectors.is_empty() {
                let cache_entries = cache_entries_for(&vectors);
                let tx = db.conn().unchecked_transaction()?;
                insert_node_vectors_batch(db.conn(), &vectors)?;
                crate::storage::queries::cache_put_embeddings(db.conn(), &cache_entries)?;
                tx.commit()?;
            }
            embedded_ids.extend(vectors.iter().map(|(id, _)| *id));
            tracing::info!(
                "[embed] {} embedded + {} reused (sequential fallback) in {:.1}s",
                vectors.len(),
                reused,
                t0.elapsed().as_secs_f64()
            );
            return Ok(embedded_ids);
        }
    };

    let vectors: Vec<(i64, Vec<f32>)> = ids.into_iter().zip(embeddings).collect();
    let t_embed = t0.elapsed();

    if !vectors.is_empty() {
        let cache_entries = cache_entries_for(&vectors);
        let tx = db.conn().unchecked_transaction()?;
        insert_node_vectors_batch(db.conn(), &vectors)?;
        crate::storage::queries::cache_put_embeddings(db.conn(), &cache_entries)?;
        tx.commit()?;
    }
    embedded_ids.extend(vectors.iter().map(|(id, _)| *id));

    tracing::info!(
        "[embed] {} embedded + {} reused in {:.1}s (embed {:.1}s)",
        vectors.len(),
        reused,
        t0.elapsed().as_secs_f64(),
        t_embed.as_secs_f64(),
    );
    Ok(embedded_ids)
}

// Minimal regression coverage for the one embed chokepoint. Both tests build
// the stub `EmbeddingModel` (the `not(feature = "embed-model")` unit struct),
// so they run on the FTS5-only CI leg where this file otherwise had 0/70
// covered lines (audit baseline 2026-09-02). The stub's `embed*` always fails,
// which is exactly the sequential-fallback path: a cache HIT must still be
// copied into `node_vectors` and reported, a MISS must be reported as NOT
// embedded (so the backfill can advance past it) without failing the call.
#[cfg(all(test, not(feature = "embed-model")))]
mod tests {
    use super::*;
    use crate::storage::queries::{
        cache_key, cache_put_embeddings, get_node_embedding, insert_node, upsert_file, FileRecord,
        NodeRecord,
    };

    fn vec_db() -> (Database, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = Database::open_with_vec(&tmp.path().join("index.db")).unwrap();
        assert!(
            db.vec_enabled(),
            "test needs the vec0 + embedding_cache tables"
        );
        (db, tmp)
    }

    fn add_node(db: &Database, path: &str, ctx: &str) -> i64 {
        let fid = upsert_file(
            db.conn(),
            &FileRecord {
                path: path.into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: None,
            },
        )
        .unwrap();
        insert_node(
            db.conn(),
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "f".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 2,
                code_content: String::new(),
                signature: None,
                doc_comment: None,
                context_string: Some(ctx.into()),
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap()
    }

    #[test]
    fn empty_input_is_a_noop() {
        let (db, _tmp) = vec_db();
        let got = embed_and_store_batch(&db, &EmbeddingModel, &[]).unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn cache_hit_is_copied_and_model_miss_is_reported_as_not_embedded() {
        let (db, _tmp) = vec_db();
        let hit = add_node(&db, "src/a.rs", "ctx-cached");
        let miss = add_node(&db, "src/b.rs", "ctx-fresh");
        let seeded = vec![0.25f32; crate::domain::EMBEDDING_DIM];
        cache_put_embeddings(db.conn(), &[(cache_key("ctx-cached"), seeded.clone())]).unwrap();

        let got = embed_and_store_batch(
            &db,
            &EmbeddingModel,
            &[(hit, "ctx-cached".into()), (miss, "ctx-fresh".into())],
        )
        .unwrap();

        // Only the cache hit got a vector; the stub model cannot embed the miss,
        // and that must surface as "not in the returned ids", not as an Err.
        assert_eq!(got, vec![hit]);
        let bytes = get_node_embedding(db.conn(), hit).unwrap();
        assert_eq!(bytes.len(), crate::domain::EMBEDDING_DIM * 4);
        assert!(
            get_node_embedding(db.conn(), miss).is_err(),
            "miss must not receive a vector from a failing model"
        );
    }
}
