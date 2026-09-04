//! Content-hash embedding cache — reuse embeddings across index generations.
//!
//! `node_vectors` (vec0) is keyed by `node_id`, which is reassigned on every full rebuild
//! (INDEX_VERSION bump, `rebuild-index`), so its embeddings are thrown away and RECOMPUTED
//! even when a node's embedding INPUT (`context_string`) is byte-identical — the expensive
//! candle re-inference behind the "从 1% 重建" statusline churn. This module persists
//! embeddings keyed by `blake3(context_string)` in the plain `embedding_cache` table, which
//! survives the version-bump wipe (the wipe only DELETEs nodes/edges/files, not this table).
//! The backfill consults it before calling the model: a hit is a byte copy, not inference.
//!
//! Correctness: an embedding is a function of (context_string, MODEL). The cache is keyed by
//! content only, so a model-weights change — even at the SAME dim, which the `node_vectors`
//! dim-check misses — would otherwise serve stale vectors. `ensure_embedding_cache_valid`
//! ties the cache AND node_vectors to the model's content fingerprint and rebuilds both on a
//! mismatch (also closing the pre-existing gap where a same-dim model swap left node_vectors
//! stale). Dim changes are handled upstream — `ensure_embedding_dim_consistency` drops both.

use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashSet;

/// A cache hit: `(node_id, embedding)` ready to copy straight into `node_vectors`.
pub type CachedVector = (i64, Vec<f32>);
/// A cache miss: `(node_id, context_string)` that still needs the model.
pub type UnembeddedNode = (i64, String);

/// blake3 of the embedding input; the `embedding_cache` primary key.
///
/// INVARIANT: the hashed `context_string` must equal what is fed to `model.embed_batch`
/// (see `embed_and_store_batch`) up to a CONSTANT transformation. The cache is sound only
/// because equal content ⇒ equal embedding. If a query/document prefix is ever added at embed
/// time (e5 / nomic-style `query:` / `passage:`), this key must cover the ACTUAL model input,
/// and query embeddings must never read from this document cache — else a hit serves the wrong
/// vector. The model itself is pinned via `ensure_embedding_cache_valid` (fingerprint), not here.
pub fn cache_key(context_string: &str) -> [u8; 32] {
    *blake3::hash(context_string.as_bytes()).as_bytes()
}

/// Native-endian f32 slice → bytes, matching how `node_vectors` stores embeddings.
fn embedding_to_bytes(emb: &[f32]) -> &[u8] {
    bytemuck::cast_slice(emb)
}

/// Bytes → f32 without a `bytemuck::cast_slice` (a SQLite BLOB is not guaranteed 4-byte
/// aligned, which would PANIC the zero-copy cast). `as_chunks` + `from_ne_bytes` is
/// alignment-safe and pairs with the native-endian write above; like the
/// `chunks_exact(4)` it replaces, a trailing partial group is dropped.
fn bytes_to_embedding(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_ne_bytes(*c))
        .collect()
}

fn has_cache_table(conn: &Connection) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='embedding_cache'",
        [],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Store freshly-computed embeddings keyed by content hash. Idempotent (REPLACE). Caller
/// should wrap in a transaction for batch performance. No-op when the cache table is absent.
pub fn cache_put_embeddings(conn: &Connection, entries: &[([u8; 32], Vec<f32>)]) -> Result<()> {
    if entries.is_empty() || !has_cache_table(conn)? {
        return Ok(());
    }
    let mut stmt = conn.prepare_cached(
        "INSERT OR REPLACE INTO embedding_cache(context_hash, embedding) VALUES (?1, ?2)",
    )?;
    for (hash, emb) in entries {
        stmt.execute(rusqlite::params![&hash[..], embedding_to_bytes(emb)])?;
    }
    Ok(())
}

/// Split `nodes` (node_id, context_string) into cache HITS `(node_id, embedding)` and MISSES
/// `(node_id, context_string)`. The reuse path: hits go straight into `node_vectors` (a byte
/// copy, no inference), misses go to the model. Returns everything as a miss when the cache
/// table is absent or an entry's byte length doesn't match the current embedding dim (a
/// leftover from a different dim — treat as a miss so it gets recomputed, never mis-decoded).
pub fn partition_by_cache(
    conn: &Connection,
    nodes: &[(i64, String)],
) -> Result<(Vec<CachedVector>, Vec<UnembeddedNode>)> {
    if !has_cache_table(conn)? {
        return Ok((Vec::new(), nodes.to_vec()));
    }
    let expected_bytes = crate::domain::EMBEDDING_DIM * 4;
    let mut hits = Vec::new();
    let mut misses = Vec::new();
    let mut stmt =
        conn.prepare_cached("SELECT embedding FROM embedding_cache WHERE context_hash = ?1")?;
    for (id, ctx) in nodes {
        let key = cache_key(ctx);
        let cached: Option<Vec<u8>> = stmt.query_row([&key[..]], |r| r.get(0)).ok();
        match cached {
            Some(bytes) if bytes.len() == expected_bytes => {
                hits.push((*id, bytes_to_embedding(&bytes)));
            }
            _ => misses.push((*id, ctx.clone())),
        }
    }
    Ok((hits, misses))
}

/// Invalidate the cache (and node_vectors) when the embedding model's content fingerprint
/// changed since they were written — a same-dim weight swap that the `node_vectors` dim-check
/// alone would miss (leaving stale vectors that the backfill never revisits because the nodes
/// already "have" a vector). Records the fingerprint in `meta[embedding_model]`. A first-ever
/// call (no stored fingerprint) just records it — nothing prior is stale. On a genuine change
/// it DROPs + recreates both tables (the proven dim-change pattern), so the backfill re-embeds
/// every node with the new model. Returns true if it cleared stale data. No-op if the cache
/// table is absent (vec disabled).
pub fn ensure_embedding_cache_valid(conn: &Connection, model_id: &str) -> Result<bool> {
    if !has_cache_table(conn)? {
        return Ok(false);
    }
    let stored: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [crate::storage::schema::META_KEY_EMBEDDING_MODEL],
            |r| r.get(0),
        )
        .ok();
    let cleared = match stored.as_deref() {
        Some(s) if s == model_id => false,
        Some(_) => {
            tracing::warn!(
                "[embed-cache] Embedding model changed; clearing cache + node_vectors so \
                 every node re-embeds with the new model."
            );
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(
                "DROP TABLE IF EXISTS node_vectors; DROP TABLE IF EXISTS embedding_cache;",
            )?;
            tx.execute_batch(&crate::storage::schema::create_vec_tables_sql())?;
            tx.commit()?;
            true
        }
        None => false, // first run — nothing prior to invalidate
    };
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [crate::storage::schema::META_KEY_EMBEDDING_MODEL, model_id],
    )?;
    Ok(cleared)
}

/// Prune cache entries whose content hash matches no current node (accumulated as code
/// churns). Bounds the cache to ~the live node count. Returns the number removed.
///
/// SAFETY (same invariant as `reap_orphan_vectors`): never prune against an EMPTY `nodes`
/// table. An empty nodes set is a mid-rebuild / version-bump wipe transient, and pruning then
/// would delete the ENTIRE cache — destroying exactly the cross-generation reuse it exists for
/// (turning the next rebuild back into a full re-embed). If nodes is genuinely empty there is
/// nothing to reuse anyway, so skipping is free.
pub fn gc_embedding_cache(conn: &Connection) -> Result<usize> {
    if !has_cache_table(conn)? {
        return Ok(0);
    }
    // Enumerate the live set and the stale keys WITHIN an IMMEDIATE (acquire-write-lock-now)
    // transaction, for the same reason as reap_orphan_vectors: a DEFERRED txn takes its write
    // snapshot only at the first DELETE, so a key enumerated as "stale" (no live node hashes to
    // it) could be made live by a concurrent writer inserting a node with that content between
    // enumerate and delete — and we would then delete a now-valid cache entry, forcing a needless
    // re-embed on its next reuse. Holding the write lock across enumerate+delete serializes
    // against that writer. Lower impact than the reap (a re-embed, not a dropped live vector), but
    // the fix is identical and keeping them symmetric avoids a sibling-hole. blake3 is not a SQL
    // function, so the live set must be built in Rust — hence enumerate-then-delete, not a set
    // DELETE.
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    // Empty-nodes valve (see reap_orphan_vectors): never prune against a mid-rebuild / version-
    // bump wipe transient — that would delete the ENTIRE cache and turn the next rebuild back into
    // a full re-embed. Read inside the txn so the check shares the delete's snapshot.
    let node_count: i64 = tx.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))?;
    if node_count == 0 {
        return Ok(0); // tx rolls back on drop; nothing was written
    }
    // Live content hashes (stream context_strings; retain only the 32-byte hashes).
    let mut live: HashSet<[u8; 32]> = HashSet::new();
    {
        let mut stmt =
            tx.prepare("SELECT context_string FROM nodes WHERE context_string IS NOT NULL")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        for ctx in rows {
            live.insert(cache_key(&ctx?));
        }
    }
    // Collect stale cache keys (not live, or malformed length).
    let stale: Vec<Vec<u8>> = {
        let mut stmt = tx.prepare("SELECT context_hash FROM embedding_cache")?;
        let rows = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
        let mut stale = Vec::new();
        for h in rows {
            let h = h?;
            let is_live = h.len() == 32 && {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&h);
                live.contains(&arr)
            };
            if !is_live {
                stale.push(h);
            }
        }
        stale
    };
    if stale.is_empty() {
        return Ok(0);
    }
    {
        let mut del = tx.prepare_cached("DELETE FROM embedding_cache WHERE context_hash = ?1")?;
        for h in &stale {
            del.execute([h])?;
        }
    }
    tx.commit()?;
    Ok(stale.len())
}

/// Seed the cache from EXISTING `node_vectors` so an already-embedded index — one built before
/// this cache existed (every current install, e.g. daagu's 14.5k vectors) or freshly
/// full-embedded — benefits on its NEXT rebuild instead of paying one more full re-embed. Runs
/// once: no-op if the cache is already non-empty. For each embedded node with a context_string,
/// records `blake3(context_string) -> its stored embedding`. Returns the number seeded. Without
/// this, C only helps one version bump LATER (the first post-upgrade bump would re-embed to
/// populate the cache); with it, the very next bump is a byte copy.
pub fn seed_embedding_cache_from_vectors(conn: &Connection) -> Result<usize> {
    if !has_cache_table(conn)? {
        return Ok(0);
    }
    let already: i64 = conn.query_row("SELECT COUNT(*) FROM embedding_cache", [], |r| r.get(0))?;
    if already > 0 {
        return Ok(0); // already seeded, or populated by live embedding — nothing to backfill
    }
    let expected_bytes = crate::domain::EMBEDDING_DIM * 4;
    // One pass joining surviving vectors to their node's context. The join drops any vector
    // whose node/context is gone (orphans), so we never seed a dead embedding.
    let entries: Vec<([u8; 32], Vec<f32>)> = {
        let mut stmt = conn.prepare(
            "SELECT n.context_string, v.embedding \
             FROM node_vectors v JOIN nodes n ON n.id = v.node_id \
             WHERE n.context_string IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (ctx, bytes) = row?;
            if bytes.len() == expected_bytes {
                out.push((cache_key(&ctx), bytes_to_embedding(&bytes)));
            }
        }
        out
    };
    if entries.is_empty() {
        return Ok(0);
    }
    let tx = conn.unchecked_transaction()?;
    cache_put_embeddings(conn, &entries)?;
    tx.commit()?;
    Ok(entries.len())
}

#[cfg(test)]
mod tests {
    use super::super::files::{upsert_file, FileRecord};
    use super::super::helpers::test_db;
    use super::super::nodes::{insert_node, NodeRecord};
    use super::super::vectors::insert_node_vector;
    use super::*;

    fn mk_node(conn: &Connection, name: &str, ctx: Option<&str>) -> i64 {
        let fid = upsert_file(
            conn,
            &FileRecord {
                path: format!("{name}.ts"),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: None,
            },
        )
        .unwrap();
        insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: name.into(),
                qualified_name: None,
                start_line: 1,
                end_line: 2,
                code_content: String::new(),
                signature: None,
                doc_comment: None,
                context_string: ctx.map(|s| s.to_string()),
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap()
    }

    #[test]
    fn partition_splits_hits_and_misses_and_round_trips() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute_batch(&crate::storage::schema::create_vec_tables_sql())
            .unwrap();
        // Seed the cache for context "A" with a distinctive vector.
        let sentinel: Vec<f32> = (0..crate::domain::EMBEDDING_DIM)
            .map(|i| i as f32 * 0.01)
            .collect();
        cache_put_embeddings(conn, &[(cache_key("ctx-A"), sentinel.clone())]).unwrap();
        let (hits, misses) =
            partition_by_cache(conn, &[(1, "ctx-A".into()), (2, "ctx-B".into())]).unwrap();
        assert_eq!(hits.len(), 1, "ctx-A is a cache hit");
        assert_eq!(hits[0].0, 1);
        assert_eq!(
            hits[0].1, sentinel,
            "hit round-trips the exact embedding bytes"
        );
        assert_eq!(misses.len(), 1, "ctx-B is a miss");
        assert_eq!(misses[0], (2, "ctx-B".to_string()));
    }

    #[test]
    fn partition_all_misses_when_no_cache_table() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        // No create_vec_tables_sql → no embedding_cache table.
        let (hits, misses) = partition_by_cache(conn, &[(1, "x".into())]).unwrap();
        assert!(hits.is_empty());
        assert_eq!(misses.len(), 1, "absent cache table → everything is a miss");
    }

    #[test]
    fn ensure_valid_clears_on_model_change_only() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute_batch(&crate::storage::schema::create_vec_tables_sql())
            .unwrap();
        cache_put_embeddings(
            conn,
            &[(cache_key("A"), vec![0.0; crate::domain::EMBEDDING_DIM])],
        )
        .unwrap();
        let count = |c: &Connection| {
            c.query_row("SELECT COUNT(*) FROM embedding_cache", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
        };
        // First call records the fingerprint, clears nothing.
        assert!(!ensure_embedding_cache_valid(conn, "model-v1").unwrap());
        assert_eq!(count(conn), 1, "first call must not clear");
        // Same model → no clear.
        assert!(!ensure_embedding_cache_valid(conn, "model-v1").unwrap());
        assert_eq!(count(conn), 1);
        // Different model → clears cache (and node_vectors).
        assert!(ensure_embedding_cache_valid(conn, "model-v2").unwrap());
        assert_eq!(count(conn), 0, "model change must clear the stale cache");
    }

    #[test]
    fn gc_prunes_orphaned_content_keeps_live() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute_batch(&crate::storage::schema::create_vec_tables_sql())
            .unwrap();
        // A live node with context "live"; cache entries for "live" and a stale "gone".
        mk_node(conn, "n", Some("live"));
        cache_put_embeddings(
            conn,
            &[
                (cache_key("live"), vec![0.0; crate::domain::EMBEDDING_DIM]),
                (cache_key("gone"), vec![0.0; crate::domain::EMBEDDING_DIM]),
            ],
        )
        .unwrap();
        assert_eq!(
            gc_embedding_cache(conn).unwrap(),
            1,
            "prunes the one orphaned entry"
        );
        // Idempotent + kept the live one.
        assert_eq!(gc_embedding_cache(conn).unwrap(), 0);
        let (hits, _) = partition_by_cache(conn, &[(1, "live".into())]).unwrap();
        assert_eq!(hits.len(), 1, "live content survived GC");
    }

    #[test]
    fn seed_populates_cache_from_existing_vectors_once() {
        // An already-embedded index (empty cache, populated node_vectors — every current
        // install, like daagu) must seed the cache so its NEXT bump reuses instead of
        // re-embedding. Idempotent, and the seeded content is reusable via partition.
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute_batch(&crate::storage::schema::create_vec_tables_sql())
            .unwrap();
        let a = mk_node(conn, "a", Some("ctx-a"));
        let b = mk_node(conn, "b", Some("ctx-b"));
        insert_node_vector(conn, a, &vec![0.1; crate::domain::EMBEDDING_DIM]).unwrap();
        insert_node_vector(conn, b, &vec![0.2; crate::domain::EMBEDDING_DIM]).unwrap();
        // Cache empty → seed populates from the two surviving vectors.
        assert_eq!(
            seed_embedding_cache_from_vectors(conn).unwrap(),
            2,
            "seeds both vectors"
        );
        // Idempotent: a populated cache is not re-seeded.
        assert_eq!(
            seed_embedding_cache_from_vectors(conn).unwrap(),
            0,
            "no-op once populated"
        );
        // Seeded content is reusable with the exact stored embedding.
        let (hits, misses) = partition_by_cache(conn, &[(a, "ctx-a".into())]).unwrap();
        assert_eq!(hits.len(), 1, "seeded content is a cache hit");
        assert_eq!(
            hits[0].1,
            vec![0.1; crate::domain::EMBEDDING_DIM],
            "exact embedding round-trips"
        );
        assert!(misses.is_empty());
    }

    #[test]
    fn gc_skips_empty_nodes_table() {
        // Mirror of reap's empty-nodes valve: a mid-rebuild empty window must NOT wipe the
        // cache — that would turn the next rebuild back into a full re-embed.
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute_batch(&crate::storage::schema::create_vec_tables_sql())
            .unwrap();
        cache_put_embeddings(
            conn,
            &[(cache_key("A"), vec![0.0; crate::domain::EMBEDDING_DIM])],
        )
        .unwrap();
        assert_eq!(
            gc_embedding_cache(conn).unwrap(),
            0,
            "must not prune against empty nodes"
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM embedding_cache", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1,
            "cache preserved during the transient empty-nodes window"
        );
    }
}
