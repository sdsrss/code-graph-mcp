use anyhow::Result;
use rusqlite::Connection;

pub fn insert_node_vector(conn: &Connection, node_id: i64, embedding: &[f32]) -> Result<()> {
    let bytes: &[u8] = bytemuck::cast_slice(embedding);
    conn.execute(
        "INSERT OR REPLACE INTO node_vectors(node_id, embedding) VALUES (?1, ?2)",
        rusqlite::params![node_id, bytes],
    )?;
    Ok(())
}

/// Batch insert vectors using a single prepared statement.
/// For best performance, caller should wrap in a transaction (avoids per-statement fsync).
pub fn insert_node_vectors_batch(conn: &Connection, vectors: &[(i64, Vec<f32>)]) -> Result<()> {
    if vectors.is_empty() {
        return Ok(());
    }
    // vec0 virtual tables do not support INSERT OR REPLACE, so delete first.
    let mut exists_stmt = conn.prepare_cached("SELECT 1 FROM nodes WHERE id = ?1")?;
    let mut del_stmt = conn.prepare_cached("DELETE FROM node_vectors WHERE node_id = ?1")?;
    let mut ins_stmt =
        conn.prepare_cached("INSERT INTO node_vectors(node_id, embedding) VALUES (?1, ?2)")?;
    for (node_id, embedding) in vectors {
        // Race guard: the background backfill snapshots node_ids, then spends a seconds-long
        // candle-inference window (embed.rs) before reaching this store, on a SEPARATE
        // connection from the server's incremental/version-bump deletes. If the node was
        // deleted in that window, its `nodes_vectors_ad` AFTER DELETE trigger already reaped
        // any vector; a late INSERT here would create a PERMANENT orphan (vec0 has no FK, and
        // the trigger never fires again for a node that is already gone) — this is exactly how
        // daagu accumulated 157 orphans at rowids past the live range. Skip vanished nodes;
        // SQLite serializes writers so the set can't shrink under this batch once it holds the
        // write lock, and reap_orphan_vectors backstops the residual first-row window.
        if !exists_stmt.exists(rusqlite::params![node_id])? {
            continue;
        }
        let bytes: &[u8] = bytemuck::cast_slice(embedding);
        del_stmt.execute(rusqlite::params![node_id])?;
        ins_stmt.execute(rusqlite::params![node_id, bytes])?;
    }
    Ok(())
}

/// Drop vectors for the given node IDs so the background embedder re-selects them
/// via the `node_vectors.node_id IS NULL` convention in `get_unembedded_nodes`.
/// Used by the incremental path when context strings changed but no model was
/// available to re-embed inline (the watcher/drift path passes model=None to avoid
/// holding the model lock across I/O). Wrapped in a transaction to avoid per-row fsync.
pub fn delete_node_vectors_batch(conn: &Connection, ids: &[i64]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    {
        let mut del_stmt = conn.prepare_cached("DELETE FROM node_vectors WHERE node_id = ?1")?;
        for id in ids {
            del_stmt.execute(rusqlite::params![id])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// `(live vectors, allocated slots)` in the sqlite-vec chunk allocator.
///
/// sqlite-vec stores vectors in fixed-size chunks and only ever inserts into the
/// NEWEST one (`vendor/sqlite-vec/sqlite-vec.c:3518`, `:7703-7716`); a DELETE
/// clears a validity bit and the slot is never reused. So the allocated total is
/// a high-water mark across every generation of the index, not a measure of what
/// is stored now.
pub fn vec_slot_occupancy(conn: &Connection) -> Result<(i64, i64)> {
    let live: i64 = conn.query_row("SELECT COUNT(*) FROM node_vectors_rowids", [], |r| r.get(0))?;
    let slots: i64 = conn.query_row(
        "SELECT COALESCE(SUM(size), 0) FROM node_vectors_chunks",
        [],
        |r| r.get(0),
    )?;
    Ok((live, slots))
}

/// Bytes the chunk allocator has claimed for vectors, live or not.
fn allocated_vector_bytes(slots: i64) -> i64 {
    slots * crate::domain::EMBEDDING_DIM as i64 * std::mem::size_of::<f32>() as i64
}

/// Rewrite `node_vectors` so its chunks hold only the vectors that are still
/// live, and return how many were carried over.
///
/// Every churn generation strands slots — an incremental re-index cascade-deletes
/// a file's nodes (the `nodes_vectors_ad` trigger takes their vectors with them)
/// and re-embeds under new node ids, `delete_node_vectors_batch` drops vectors
/// for re-embedding, an INDEX_VERSION bump wipes the whole table. None of that
/// space comes back, so `index.db` grows monotonically under churn: measured on
/// this repo, 5,015 live vectors against 129,024 allocated slots (3.9%) and a
/// 189 MB `node_vectors_vector_chunks00` holding ~7.7 MB of vectors.
///
/// Rewrites rather than dropping and letting the backfill re-embed (the audit's
/// suggested shape). A rewrite is a byte copy of vectors that already exist, so
/// it needs neither a loadable model nor complete `embedding_cache` coverage —
/// it is therefore correct in a `--no-default-features` build, and it cannot
/// turn a cache miss into inference on a startup path. Vectors stream through a
/// temp table instead of Rust memory: at 384 dims a 100k-node index would be
/// 150 MB held at once.
///
/// The whole rewrite is one transaction: a failure anywhere rolls back and the
/// vectors stay exactly as they were. Orphans (a vector whose node is gone) are
/// dropped in passing by the join — the same rows [`reap_orphan_vectors`] takes.
pub fn compact_node_vectors(conn: &Connection) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "DROP TABLE IF EXISTS temp.vec_compact;
         CREATE TEMP TABLE vec_compact(
             node_id   INTEGER PRIMARY KEY,
             embedding BLOB NOT NULL
         );",
    )?;
    tx.execute(
        "INSERT INTO temp.vec_compact(node_id, embedding)
         SELECT v.node_id, v.embedding
         FROM node_vectors v
         JOIN nodes n ON n.id = v.node_id",
        [],
    )?;
    tx.execute_batch("DROP TABLE IF EXISTS node_vectors;")?;
    tx.execute_batch(&crate::storage::schema::create_vec_tables_sql())?;
    let restored = tx.execute(
        "INSERT INTO node_vectors(node_id, embedding)
         SELECT node_id, embedding FROM temp.vec_compact",
        [],
    )?;
    tx.execute_batch("DROP TABLE IF EXISTS temp.vec_compact;")?;
    tx.commit()?;
    Ok(restored)
}

/// Compact the vector table when the allocator is mostly holding dead slots,
/// then return the freed pages to the filesystem. Returns the number of vectors
/// rewritten, or 0 when nothing was worth doing.
///
/// Two thresholds, both required:
/// - occupancy below [`VEC_COMPACT_MAX_OCCUPANCY`], because a table that is
///   mostly live has nothing to win and a rewrite is not free;
/// - at least [`VEC_COMPACT_MIN_ALLOCATED_BYTES`] claimed, so a small index never
///   pays for a rewrite that would save a few hundred KB.
///
/// Skipped entirely when `nodes` is empty — the same safety valve
/// [`reap_orphan_vectors`] uses. A mid-rebuild or version-bump window looks
/// exactly like "0% occupancy" from here, and compacting through it would race
/// the rebuild for the write lock for no gain.
///
/// The VACUUM is what actually returns the disk: DROP TABLE moves the chunk
/// pages onto the freelist, and the file itself does not shrink until the
/// database is rewritten. It is best-effort — VACUUM needs the whole database
/// briefly, so a concurrent CLI writer makes it fail with SQLITE_BUSY, and the
/// only cost of that is that the pages stay on the freelist for the next
/// session (they are reused before the file grows again).
pub fn compact_node_vectors_if_wasteful(conn: &Connection) -> Result<usize> {
    compact_node_vectors_when(
        conn,
        VEC_COMPACT_MAX_OCCUPANCY,
        VEC_COMPACT_MIN_ALLOCATED_BYTES,
    )
}

/// [`compact_node_vectors_if_wasteful`] with the thresholds as parameters, so a
/// test can exercise the gate without writing the 16 MB of vectors the
/// production floor requires.
pub(crate) fn compact_node_vectors_when(
    conn: &Connection,
    max_occupancy: f64,
    min_allocated_bytes: i64,
) -> Result<usize> {
    let nodes_present: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM nodes)", [], |r| {
        r.get::<_, i64>(0)
    })? == 1;
    if !nodes_present {
        return Ok(0);
    }
    let (live, slots) = vec_slot_occupancy(conn)?;
    let allocated = allocated_vector_bytes(slots);
    if allocated < min_allocated_bytes {
        return Ok(0);
    }
    if slots == 0 || (live as f64 / slots as f64) >= max_occupancy {
        return Ok(0);
    }

    let restored = compact_node_vectors(conn)?;
    tracing::info!(
        "[vec-compact] rewrote {} live vector(s); allocator was holding {} slot(s) ({:.1} MB) at {:.1}% occupancy",
        restored,
        slots,
        allocated as f64 / 1_048_576.0,
        live as f64 / slots as f64 * 100.0,
    );
    if let Err(e) = conn.execute_batch("VACUUM;") {
        tracing::debug!(
            "[vec-compact] VACUUM skipped ({}); freed pages stay on the freelist",
            e
        );
    }
    Ok(restored)
}

/// Compact below this live-to-allocated ratio.
const VEC_COMPACT_MAX_OCCUPANCY: f64 = 0.25;
/// ...and only once the allocator holds at least this much.
const VEC_COMPACT_MIN_ALLOCATED_BYTES: i64 = 16 * 1024 * 1024;

pub fn vector_search(
    conn: &Connection,
    query_embedding: &[f32],
    limit: i64,
) -> Result<Vec<(i64, f64)>> {
    let bytes: &[u8] = bytemuck::cast_slice(query_embedding);
    let mut stmt = conn.prepare(
        "SELECT node_id, distance FROM node_vectors WHERE embedding MATCH ?1 ORDER BY distance LIMIT ?2"
    )?;
    let rows = stmt.query_map(rusqlite::params![bytes, limit], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
    })?;
    let results = rows.collect::<Result<Vec<_>, _>>()?;
    Ok(results)
}

pub fn get_node_embedding(conn: &Connection, node_id: i64) -> Result<Vec<u8>> {
    let bytes: Vec<u8> = conn.query_row(
        "SELECT embedding FROM node_vectors WHERE node_id = ?1",
        [node_id],
        |row| row.get(0),
    )?;
    Ok(bytes)
}

// --- Unembedded nodes ---

/// Get (node_id, context_string) for nodes that have context strings but no vectors.
/// Returns at most `limit` rows per call to bound memory usage.
pub fn get_unembedded_nodes(conn: &Connection, limit: usize) -> Result<Vec<(i64, String)>> {
    // Priority: embed hot-path nodes first (most referenced = highest value for search)
    // Uses LEFT JOIN + GROUP BY instead of correlated subquery for better performance
    let mut stmt = conn.prepare(
        "SELECT n.id, n.context_string
         FROM nodes n
         LEFT JOIN node_vectors nv ON n.id = nv.node_id
         LEFT JOIN edges e ON e.target_id = n.id
         WHERE nv.node_id IS NULL AND n.context_string IS NOT NULL
         GROUP BY n.id
         ORDER BY COUNT(e.target_id) DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit as i64], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Like [`get_unembedded_nodes`] but skips `exclude` node IDs in SQL. The backfill loops
/// pass the set of nodes that failed to embed THIS run so the same hot-path-first poison
/// node isn't re-fetched at the head of every batch (which would starve the embeddable
/// nodes behind it, or — in the CLI loop that only stops on an empty result — spin forever).
pub fn get_unembedded_nodes_excluding(
    conn: &Connection,
    limit: usize,
    exclude: &[i64],
) -> Result<Vec<(i64, String)>> {
    if exclude.is_empty() {
        return get_unembedded_nodes(conn, limit);
    }
    // Don't bind one parameter per excluded id: on a large repo the backfill
    // loop's `failed` set can grow toward the full node count, and a single
    // `NOT IN (?,?,…)` would exceed SQLite's variable cap (issue #30). The
    // GROUP BY / ORDER BY / LIMIT ranking can't be split across NOT-IN chunks,
    // so instead over-fetch by |exclude| and drop the excluded ids in Rust:
    // the limit-th non-excluded row sits at position <= limit + |exclude| in
    // the ranked stream, so this window always yields the same top-`limit` set
    // the SQL filter would have.
    let exclude_set: std::collections::HashSet<i64> = exclude.iter().copied().collect();
    let over_fetch = limit.saturating_add(exclude.len());
    let rows = get_unembedded_nodes(conn, over_fetch)?;
    Ok(rows
        .into_iter()
        .filter(|(id, _)| !exclude_set.contains(id))
        .take(limit)
        .collect())
}

/// Count nodes with embeddings vs total embeddable nodes.
/// Returns (with_vectors, total_embeddable).
pub fn count_nodes_with_vectors(conn: &Connection) -> Result<(i64, i64)> {
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM nodes WHERE context_string IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    // Probe for the vec table explicitly so its ABSENCE (embed-model disabled) returns 0 coverage,
    // while a genuine read error on the count below (e.g. SQLITE_BUSY under writer contention —
    // the JOIN is more contention-prone than the old flat count) PROPAGATES as Err instead of
    // masquerading as a misleading `0/total`. Mirrors count_unembedded_nodes; a blanket
    // `.unwrap_or(0)` on the JOIN would swallow that transient as "nothing embedded".
    let has_vectors_table: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='node_vectors'",
        [],
        |r| r.get(0),
    )?;
    if has_vectors_table == 0 {
        return Ok((0, total));
    }
    // Count embeddable nodes that actually HAVE a vector — NOT raw COUNT(*) FROM node_vectors.
    // The raw count includes orphans (vectors whose node was deleted; see reap_orphan_vectors)
    // and can EXCEED `total`, producing >100% coverage and — the real hazard — a FALSE
    // "complete" when orphan count masks genuinely unembedded nodes (numerator >= denominator
    // while N embeddable nodes still have no vector, so semantic search silently misses them).
    // The inner join to nodes drops orphans and caps the numerator at `total`.
    let with_vectors: i64 = conn.query_row(
        "SELECT COUNT(*) FROM nodes n JOIN node_vectors nv ON nv.node_id = n.id \
         WHERE n.context_string IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    Ok((with_vectors, total))
}

/// Count embeddable-but-unembedded nodes (have a `context_string`, no vector yet).
/// Mirrors the `WHERE` filter of [`get_unembedded_nodes`] but returns only the count,
/// so the periodic backfill driver can cheaply detect whether NEW un-embedded work has
/// appeared (e.g. nodes added by a CLI/hook `ensure_file_indexed` with `model=None`)
/// without fetching payloads or loading the embedding model. Returns 0 when the vector
/// table is absent (embed-model feature disabled).
pub fn count_unembedded_nodes(conn: &Connection) -> Result<i64> {
    // Probe for the vec table explicitly so its ABSENCE (embed-model disabled) returns 0,
    // while a genuine read error on the count below (e.g. SQLITE_BUSY under writer
    // contention) PROPAGATES as Err. A blanket `.unwrap_or(0)` would mask that transient
    // as "no work", and the periodic backfill driver — which falls back to its current
    // floor on Err — would instead reset its floor to 0 and futilely reload the model.
    let has_vectors_table: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='node_vectors'",
        [],
        |r| r.get(0),
    )?;
    if has_vectors_table == 0 {
        return Ok(0);
    }
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM nodes n \
         LEFT JOIN node_vectors nv ON n.id = nv.node_id \
         WHERE nv.node_id IS NULL AND n.context_string IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    Ok(n)
}

/// Delete vectors whose `node_id` no longer exists in `nodes` (orphans). vec0 is a virtual
/// table with no foreign key, and the `nodes_vectors_ad` AFTER DELETE trigger only reaps a
/// vector when its MATCHING node is deleted — a vector inserted for an already-deleted node
/// (the backfill race, now guarded in [`insert_node_vectors_batch`]) or one predating that
/// guard is never reaped and accumulates across index generations (daagu carried 157 at
/// rowids far past the live node range). Returns the count removed. Cheap: one anti-join
/// enumerate + point deletes. No-op (Ok(0)) when the vec table is absent (embed-model off).
pub fn reap_orphan_vectors(conn: &Connection) -> Result<usize> {
    let has_vectors_table: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='node_vectors'",
        [],
        |r| r.get(0),
    )?;
    if has_vectors_table == 0 {
        return Ok(0);
    }
    // Open an IMMEDIATE (acquire-the-write-lock-now) transaction and enumerate the orphans
    // WITHIN it, so the anti-join is read on the SAME snapshot the point-deletes run against.
    // A DEFERRED txn (rusqlite's `unchecked_transaction`) — or the prior autocommit enumerate —
    // takes its write snapshot only at the first DELETE, leaving a window between "enumerate as
    // orphan" and "delete" where a concurrent writer (the foreground indexer / another
    // connection) could insert a node that REUSES a just-enumerated orphan's rowid and give it a
    // vector; `nodes.id` is a plain INTEGER PRIMARY KEY, so after a wipe rowids restart at 1 and
    // can land on a surviving race-orphan's id. This reap would then point-delete that now-LIVE
    // node's vector — a search gap until the backfill re-embeds it. IMMEDIATE holds the write
    // lock across enumerate+delete, so a racing writer either committed before our snapshot (its
    // node is seen as live, never enumerated as an orphan) or is serialized after us. vec0 still
    // requires point deletes by primary key, so the enumerate-then-delete shape is unchanged.
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    // Safety: never sweep against an EMPTY `nodes` table. An empty nodes set is far more likely a
    // transient (mid-rebuild / INDEX_VERSION-bump wipe window) than a real zero-symbol project —
    // sweeping then would see EVERY vector as an "orphan" and delete the whole index, forcing a
    // full re-embed (the "从 1% 重建" cost this hardening avoids). Read inside the txn so the
    // check shares the delete's snapshot. If nodes is genuinely empty there is no legitimate
    // vector to keep anyway, so skipping is free.
    let node_count: i64 = tx.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))?;
    if node_count == 0 {
        return Ok(0); // tx rolls back on drop; nothing was written
    }
    // Enumerate orphan node_ids first (collect before mutating), then delete by primary key —
    // the only vec0-safe delete form. Anti-join against nodes finds vectors with no live node.
    let orphan_ids: Vec<i64> = {
        let mut stmt = tx.prepare(
            "SELECT node_id FROM node_vectors WHERE node_id NOT IN (SELECT id FROM nodes)",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    if orphan_ids.is_empty() {
        return Ok(0);
    }
    {
        let mut del = tx.prepare_cached("DELETE FROM node_vectors WHERE node_id = ?1")?;
        for id in &orphan_ids {
            del.execute(rusqlite::params![id])?;
        }
    }
    tx.commit()?;
    Ok(orphan_ids.len())
}

#[cfg(test)]
mod tests {
    use super::super::files::{upsert_file, FileRecord};
    use super::super::helpers::test_db;
    use super::super::nodes::{insert_node, NodeRecord};
    use super::*;

    #[test]
    fn node_delete_reaps_vector_no_orphan_either_path() {
        // The v0.79.1 audit flagged "orphan vectors never GC'd: vec0 has no FK". In
        // fact the `nodes_vectors_ad` AFTER DELETE trigger reaps a node's vector on
        // BOTH a direct node delete AND an FK-cascade delete (file removal): SQLite
        // fires a child table's AFTER DELETE trigger on FK cascade even with
        // recursive_triggers off (production: foreign_keys=ON, recursive_triggers
        // unset). So no orphan arises from a delete that HAS a matching node — this
        // guards that invariant against a future change to the trigger, the delete
        // path, or the pragmas. SCOPE: this covers only sequential deletes of a live
        // node. It does NOT cover the async-backfill race — a vector INSERTed for a
        // node deleted during the seconds-long inference window — which the trigger
        // cannot catch because the node is already gone. That path is guarded at the
        // insert site (see backfill_late_insert_for_deleted_node_creates_no_orphan)
        // and backstopped by reap_orphan_vectors_removes_strays_keeps_live.
        use super::super::files::delete_files_by_paths;
        use super::super::nodes::delete_nodes_by_file;
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute_batch(&crate::storage::schema::create_vec_tables_sql())
            .unwrap();
        let vec_count = |c: &Connection| -> i64 {
            c.query_row("SELECT COUNT(*) FROM node_vectors", [], |r| r.get(0))
                .unwrap()
        };
        let add_embedded = |c: &Connection, path: &str| -> i64 {
            let fid = upsert_file(
                c,
                &FileRecord {
                    path: path.into(),
                    blake3_hash: "h".into(),
                    last_modified: 1,
                    language: None,
                },
            )
            .unwrap();
            let nid = insert_node(
                c,
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
                    context_string: Some("ctx".into()),
                    name_tokens: None,
                    return_type: None,
                    param_types: None,
                    is_test: false,
                },
            )
            .unwrap();
            insert_node_vector(c, nid, &vec![0.0f32; crate::domain::EMBEDDING_DIM]).unwrap();
            fid
        };

        // Path 1 — file removal → FK cascade deletes the node → trigger reaps the vector.
        add_embedded(conn, "a.ts");
        assert_eq!(vec_count(conn), 1, "vector inserted");
        delete_files_by_paths(conn, &["a.ts".into()]).unwrap();
        assert_eq!(
            vec_count(conn),
            0,
            "FK-cascade delete must reap the vector (no orphan)"
        );

        // Path 2 — direct node delete (the changed-file reindex path) → trigger reaps it.
        let fid2 = add_embedded(conn, "b.ts");
        assert_eq!(vec_count(conn), 1, "vector inserted");
        delete_nodes_by_file(conn, fid2).unwrap();
        assert_eq!(
            vec_count(conn),
            0,
            "direct node delete must reap the vector (no orphan)"
        );
    }

    /// Insert `n` nodes each carrying a distinct vector, and return their ids.
    fn seed_vectors(conn: &Connection, path: &str, n: usize) -> Vec<i64> {
        let fid = upsert_file(
            conn,
            &FileRecord {
                path: path.into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: None,
            },
        )
        .unwrap();
        (0..n)
            .map(|i| {
                let nid = insert_node(
                    conn,
                    &NodeRecord {
                        file_id: fid,
                        node_type: "function".into(),
                        name: format!("f{i}"),
                        qualified_name: None,
                        start_line: 1,
                        end_line: 2,
                        code_content: String::new(),
                        signature: None,
                        doc_comment: None,
                        context_string: Some("ctx".into()),
                        name_tokens: None,
                        return_type: None,
                        param_types: None,
                        is_test: false,
                    },
                )
                .unwrap();
                let mut emb = vec![0.0f32; crate::domain::EMBEDDING_DIM];
                emb[0] = i as f32;
                emb[1] = (i * 2) as f32;
                insert_node_vector(conn, nid, &emb).unwrap();
                nid
            })
            .collect()
    }

    /// STO-01. A rewrite must return the SAME bytes for every surviving vector —
    /// this is the whole safety argument for compacting on a startup path, since
    /// a silently-corrupted embedding would degrade semantic search without ever
    /// failing anything.
    #[test]
    fn compaction_preserves_every_live_vector_byte_for_byte() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute_batch(&crate::storage::schema::create_vec_tables_sql())
            .unwrap();
        let ids = seed_vectors(conn, "a.ts", 40);
        let before: Vec<(i64, Vec<u8>)> = ids
            .iter()
            .map(|&id| (id, get_node_embedding(conn, id).unwrap()))
            .collect();

        let restored = compact_node_vectors(conn).unwrap();

        assert_eq!(
            restored,
            ids.len(),
            "every live vector must be carried over"
        );
        for (id, bytes) in &before {
            assert_eq!(
                &get_node_embedding(conn, *id).unwrap(),
                bytes,
                "vector for node {id} must survive the rewrite unchanged"
            );
        }
        // The table must still be a working vec0 index, not just a byte store.
        let mut probe = vec![0.0f32; crate::domain::EMBEDDING_DIM];
        probe[0] = 7.0;
        probe[1] = 14.0;
        let hits = vector_search(conn, &probe, 1).unwrap();
        assert_eq!(
            hits.first().map(|(id, _)| *id),
            Some(ids[7]),
            "KNN search must still find the nearest vector after the rewrite"
        );
    }

    /// The rewrite drops vectors whose node is gone — the rows `reap_orphan_vectors`
    /// targets — rather than carrying dead weight into the new chunks.
    #[test]
    fn compaction_drops_orphans_and_keeps_live() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute_batch(&crate::storage::schema::create_vec_tables_sql())
            .unwrap();
        let ids = seed_vectors(conn, "a.ts", 5);
        // Orphan one vector the way the backfill race does: insert past the trigger.
        conn.execute("DROP TRIGGER IF EXISTS nodes_vectors_ad", [])
            .unwrap();
        conn.execute("DELETE FROM nodes WHERE id = ?1", [ids[0]])
            .unwrap();
        conn.execute_batch(&crate::storage::schema::create_vec_tables_sql())
            .unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM node_vectors", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            5,
            "precondition: the orphan is still in the table"
        );

        let restored = compact_node_vectors(conn).unwrap();

        assert_eq!(restored, 4, "the orphan must not be carried over");
        assert!(
            get_node_embedding(conn, ids[0]).is_err(),
            "orphan vector must be gone"
        );
        for id in &ids[1..] {
            assert!(
                get_node_embedding(conn, *id).is_ok(),
                "live vector {id} must remain"
            );
        }
    }

    /// The gate, in both directions.
    ///
    /// Note what the first assertion says about the allocator: sqlite-vec claims a
    /// whole 1024-slot chunk at the first insert, so a fresh index with 30
    /// vectors already reads as ~3% occupied. The
    /// occupancy ratio ALONE would therefore rewrite a small, perfectly healthy
    /// index on every startup — it is the size floor, not the ratio, that makes
    /// this safe, which is why both thresholds are required.
    #[test]
    fn compaction_gate_fires_only_on_a_wasteful_allocator() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute_batch(&crate::storage::schema::create_vec_tables_sql())
            .unwrap();
        let ids = seed_vectors(conn, "a.ts", 2100);
        let (live, slots) = vec_slot_occupancy(conn).unwrap();
        assert_eq!(live, 2100);
        assert!(
            (live as f64 / slots as f64) >= 0.25,
            "precondition: {live}/{slots} is a healthy allocator by the production ratio"
        );
        assert_eq!(
            compact_node_vectors_when(conn, 0.25, 0).unwrap(),
            0,
            "a healthy allocator must not be rewritten"
        );

        // Strand slots the way churn does: delete most vectors, keep the chunk.
        for id in &ids[..2080] {
            conn.execute("DELETE FROM nodes WHERE id = ?1", [id])
                .unwrap();
        }
        let (live, slots) = vec_slot_occupancy(conn).unwrap();
        assert_eq!(live, 20, "20 vectors left alive");
        assert!(
            slots >= 3072,
            "the allocator still holds the stranded slots ({slots})"
        );

        // Below the size floor the gate stays shut even at 2% occupancy...
        assert_eq!(
            compact_node_vectors_when(conn, 0.25, 16 * 1024 * 1024).unwrap(),
            0,
            "a small table must not pay for a rewrite"
        );
        // ...and fires once the floor is met.
        assert_eq!(
            compact_node_vectors_when(conn, 0.25, 0).unwrap(),
            20,
            "a mostly-dead allocator over the floor must be rewritten"
        );
        let (live_after, slots_after) = vec_slot_occupancy(conn).unwrap();
        assert_eq!(live_after, 20, "the live vectors survive");
        assert!(
            slots_after < slots,
            "the rewrite must release stranded slots ({slots} -> {slots_after})"
        );
    }

    /// The same empty-nodes valve `reap_orphan_vectors` carries: a mid-rebuild or
    /// version-bump window reads as 0% occupancy, and compacting through it would
    /// fight the rebuild for the write lock to reclaim space the rebuild is about
    /// to reclaim anyway.
    #[test]
    fn compaction_never_runs_against_an_empty_nodes_table() {
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute_batch(&crate::storage::schema::create_vec_tables_sql())
            .unwrap();
        let ids = seed_vectors(conn, "a.ts", 10);
        for id in &ids {
            conn.execute("DELETE FROM nodes WHERE id = ?1", [id])
                .unwrap();
        }
        assert_eq!(
            compact_node_vectors_when(conn, 0.99, 0).unwrap(),
            0,
            "no nodes means no compaction, whatever the occupancy says"
        );
    }

    #[test]
    fn test_get_unembedded_nodes_priority_order() {
        // Verify that get_unembedded_nodes returns nodes ordered by edge reference count (most referenced first)
        let (db, _tmp) = test_db();
        let conn = db.conn();
        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "t.ts".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: None,
            },
        )
        .unwrap();

        // Create 3 nodes with context strings
        let nid1 = insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "popular".into(),
                qualified_name: None,
                start_line: 1,
                end_line: 5,
                code_content: "function popular() {}".into(),
                signature: None,
                doc_comment: None,
                context_string: Some("function popular".into()),
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        let nid2 = insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "moderate".into(),
                qualified_name: None,
                start_line: 10,
                end_line: 15,
                code_content: "function moderate() {}".into(),
                signature: None,
                doc_comment: None,
                context_string: Some("function moderate".into()),
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        let nid3 = insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "lonely".into(),
                qualified_name: None,
                start_line: 20,
                end_line: 25,
                code_content: "function lonely() {}".into(),
                signature: None,
                doc_comment: None,
                context_string: Some("function lonely".into()),
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        // Create a caller node (no context string so it won't appear in results)
        let caller = insert_node(
            conn,
            &NodeRecord {
                file_id: fid,
                node_type: "function".into(),
                name: "caller".into(),
                qualified_name: None,
                start_line: 30,
                end_line: 35,
                code_content: "function caller() {}".into(),
                signature: None,
                doc_comment: None,
                context_string: None,
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();

        // "popular" gets 3 incoming edges, "moderate" gets 1, "lonely" gets 0
        for _ in 0..3 {
            // Use different callers for unique edges - but we only have one caller node
            // Use different relations to make them unique
            conn.execute(
                "INSERT OR IGNORE INTO edges (source_id, target_id, relation) VALUES (?1, ?2, ?3)",
                rusqlite::params![caller, nid1, "calls"],
            )
            .unwrap();
        }
        // Add additional edges with different metadata to make them unique
        conn.execute(
            "INSERT INTO edges (source_id, target_id, relation, metadata) VALUES (?1, ?2, 'calls', 'a')",
            rusqlite::params![caller, nid1],
        ).unwrap();
        conn.execute(
            "INSERT INTO edges (source_id, target_id, relation, metadata) VALUES (?1, ?2, 'calls', 'b')",
            rusqlite::params![caller, nid1],
        ).unwrap();
        conn.execute(
            "INSERT INTO edges (source_id, target_id, relation) VALUES (?1, ?2, 'calls')",
            rusqlite::params![caller, nid2],
        )
        .unwrap();

        // Create vec tables for the LEFT JOIN to work
        conn.execute_batch(&crate::storage::schema::create_vec_tables_sql())
            .unwrap();

        let results = get_unembedded_nodes(conn, 10).unwrap();
        assert_eq!(
            results.len(),
            3,
            "should return all 3 nodes with context strings"
        );

        // First result should be "popular" (most referenced: 3 edges)
        assert_eq!(results[0].0, nid1, "most referenced node should be first");
        // Second should be "moderate" (1 edge)
        assert_eq!(
            results[1].0, nid2,
            "moderately referenced node should be second"
        );
        // Third should be "lonely" (0 edges)
        assert_eq!(results[2].0, nid3, "unreferenced node should be last");
    }

    #[test]
    fn test_get_unembedded_nodes_excluding_skips_ids() {
        // The backfill loops use this to advance past nodes that failed to embed; verify
        // excluded IDs are never returned even though they're still unembedded, and that
        // excluding the whole set yields empty (so the loop terminates instead of spinning).
        let (db, _tmp) = test_db();
        let conn = db.conn();
        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "t.ts".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: None,
            },
        )
        .unwrap();
        let mk = |name: &str| {
            insert_node(
                conn,
                &NodeRecord {
                    file_id: fid,
                    node_type: "function".into(),
                    name: name.into(),
                    qualified_name: None,
                    start_line: 1,
                    end_line: 2,
                    code_content: format!("function {name}() {{}}"),
                    signature: None,
                    doc_comment: None,
                    context_string: Some(format!("function {name}")),
                    name_tokens: None,
                    return_type: None,
                    param_types: None,
                    is_test: false,
                },
            )
            .unwrap()
        };
        let a = mk("aa");
        let b = mk("bb");
        let c = mk("cc");
        conn.execute_batch(&crate::storage::schema::create_vec_tables_sql())
            .unwrap();

        // No exclusion → all three (delegates to get_unembedded_nodes).
        assert_eq!(
            get_unembedded_nodes_excluding(conn, 10, &[]).unwrap().len(),
            3
        );

        // Excluding b → only a and c; b never appears though it's still unembedded.
        let got = get_unembedded_nodes_excluding(conn, 10, &[b]).unwrap();
        let ids: Vec<i64> = got.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids.len(), 2, "excluded node must be skipped");
        assert!(ids.contains(&a) && ids.contains(&c) && !ids.contains(&b));

        // Excluding every unembedded node → empty, the backfill loop's termination signal.
        assert!(get_unembedded_nodes_excluding(conn, 10, &[a, b, c])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn backfill_late_insert_for_deleted_node_creates_no_orphan() {
        // Reproduces daagu's 157 orphan vectors (rowids 109220+, far past the live node
        // range). The background backfill snapshots node_ids, spends SECONDS in candle
        // inference (embed.rs), then inserts vectors on a SEPARATE connection. If the node
        // was deleted (incremental reindex / version-bump wipe) during that window, its
        // AFTER DELETE trigger already fired with no vector present — so this late insert
        // must NOT resurrect a PERMANENT orphan (vec0 has no FK; nothing reaps a vector
        // whose matching node is already gone). Guarded by the node-existence check in
        // insert_node_vectors_batch.
        use super::super::nodes::delete_nodes_by_file;
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute_batch(&crate::storage::schema::create_vec_tables_sql())
            .unwrap();
        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "a.ts".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: None,
            },
        )
        .unwrap();
        let nid = insert_node(
            conn,
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
                context_string: Some("ctx".into()),
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        // Node deleted mid-inference (its file changed) — trigger fires, no vector yet.
        delete_nodes_by_file(conn, fid).unwrap();
        // Late backfill write for the now-dead node_id.
        insert_node_vectors_batch(conn, &[(nid, vec![0.0f32; crate::domain::EMBEDDING_DIM])])
            .unwrap();
        let vec_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM node_vectors", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            vec_count, 0,
            "late insert for a deleted node must not create an orphan vector"
        );
    }

    #[test]
    fn reap_orphan_vectors_removes_strays_keeps_live() {
        // The sweep backstop: any orphan that slips past the insert guard (or predates it,
        // like daagu's accumulated 157) must be reapable. Verify it deletes strays and
        // leaves a live node's vector untouched.
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute_batch(&crate::storage::schema::create_vec_tables_sql())
            .unwrap();
        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "a.ts".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: None,
            },
        )
        .unwrap();
        let live = insert_node(
            conn,
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
                context_string: Some("ctx".into()),
                name_tokens: None,
                return_type: None,
                param_types: None,
                is_test: false,
            },
        )
        .unwrap();
        insert_node_vector(conn, live, &vec![0.1f32; crate::domain::EMBEDDING_DIM]).unwrap();
        // Inject two orphans directly (bypass the insert guard) at high ids, like real residue.
        for id in [90001i64, 90002i64] {
            conn.execute(
                "INSERT INTO node_vectors(node_id, embedding) VALUES (?1, ?2)",
                rusqlite::params![
                    id,
                    bytemuck::cast_slice(&vec![0.0f32; crate::domain::EMBEDDING_DIM])
                ],
            )
            .unwrap();
        }
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM node_vectors", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            3
        );
        let reaped = reap_orphan_vectors(conn).unwrap();
        assert_eq!(reaped, 2, "both orphans reaped");
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM node_vectors", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1,
            "only the live node's vector remains"
        );
        // Idempotent: a second sweep with no orphans reaps nothing.
        assert_eq!(reap_orphan_vectors(conn).unwrap(), 0, "sweep is idempotent");
    }

    #[test]
    fn reap_orphan_vectors_skips_empty_nodes_table() {
        // DB-availability invariant: an empty `nodes` table is a mid-rebuild / version-bump
        // wipe transient, NOT a signal to delete every vector. reap must no-op then, so a
        // transient empty window can never nuke a live vector index and force a full re-embed.
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute_batch(&crate::storage::schema::create_vec_tables_sql())
            .unwrap();
        for id in [1i64, 2, 3] {
            conn.execute(
                "INSERT INTO node_vectors(node_id, embedding) VALUES (?1, ?2)",
                rusqlite::params![
                    id,
                    bytemuck::cast_slice(&vec![0.0f32; crate::domain::EMBEDDING_DIM])
                ],
            )
            .unwrap();
        }
        assert_eq!(
            reap_orphan_vectors(conn).unwrap(),
            0,
            "must not sweep against empty nodes"
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM node_vectors", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            3,
            "all vectors preserved during the transient empty-nodes window"
        );
    }

    #[test]
    fn coverage_count_excludes_orphans_and_cannot_exceed_total() {
        // count_nodes_with_vectors must count embeddable nodes that HAVE a vector, not raw
        // node_vectors rows: orphans otherwise inflate the numerator past the denominator →
        // >100% coverage AND a FALSE "complete" that masks genuinely unembedded nodes.
        let (db, _tmp) = test_db();
        let conn = db.conn();
        conn.execute_batch(&crate::storage::schema::create_vec_tables_sql())
            .unwrap();
        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "a.ts".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: None,
            },
        )
        .unwrap();
        let mk = |name: &str| {
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
                    context_string: Some("ctx".into()),
                    name_tokens: None,
                    return_type: None,
                    param_types: None,
                    is_test: false,
                },
            )
            .unwrap()
        };
        mk("a");
        mk("b");
        // Three orphan vectors (node_ids that don't exist) — more than the 2 embeddable nodes.
        for id in [90001i64, 90002i64, 90003i64] {
            conn.execute(
                "INSERT INTO node_vectors(node_id, embedding) VALUES (?1, ?2)",
                rusqlite::params![
                    id,
                    bytemuck::cast_slice(&vec![0.0f32; crate::domain::EMBEDDING_DIM])
                ],
            )
            .unwrap();
        }
        let (with_vectors, total) = count_nodes_with_vectors(conn).unwrap();
        assert_eq!(total, 2, "two embeddable nodes");
        assert!(
            with_vectors <= total,
            "numerator {with_vectors} must not exceed embeddable total {total} (orphans excluded)"
        );
        assert_eq!(
            with_vectors, 0,
            "no real node embedded yet — orphans must not count as coverage"
        );
    }

    #[test]
    fn test_get_unembedded_nodes_excluding_large_set() {
        // Regression for issue #30: the old NOT IN (?,?,…) bound one parameter
        // per excluded id, so a `failed` set near the node count blew SQLite's
        // variable cap. The over-fetch+filter path must still return the right
        // non-excluded nodes when |exclude| > MAX_IN_PARAMS.
        use super::super::helpers::MAX_IN_PARAMS;
        let (db, _tmp) = test_db();
        let conn = db.conn();
        let fid = upsert_file(
            conn,
            &FileRecord {
                path: "t.ts".into(),
                blake3_hash: "h".into(),
                last_modified: 1,
                language: None,
            },
        )
        .unwrap();
        conn.execute_batch(&crate::storage::schema::create_vec_tables_sql())
            .unwrap();

        let n = MAX_IN_PARAMS + 3; // 503 unembedded nodes
        let mut ids = Vec::with_capacity(n);
        for i in 0..n {
            ids.push(
                insert_node(
                    conn,
                    &NodeRecord {
                        file_id: fid,
                        node_type: "function".into(),
                        name: format!("f{i}"),
                        qualified_name: None,
                        start_line: i as i64 + 1,
                        end_line: i as i64 + 1,
                        code_content: String::new(),
                        signature: None,
                        doc_comment: None,
                        context_string: Some(format!("ctx{i}")),
                        name_tokens: None,
                        return_type: None,
                        param_types: None,
                        is_test: false,
                    },
                )
                .unwrap(),
            );
        }

        // Exclude the first MAX_IN_PARAMS + 1 ids (crosses the old IN-clause cap).
        let exclude = &ids[..MAX_IN_PARAMS + 1];
        let got = get_unembedded_nodes_excluding(conn, 10, exclude).unwrap();
        let got_ids: std::collections::HashSet<i64> = got.iter().map(|(id, _)| *id).collect();
        let expected: std::collections::HashSet<i64> =
            ids[MAX_IN_PARAMS + 1..].iter().copied().collect();
        assert_eq!(
            got_ids, expected,
            "exactly the non-excluded nodes must remain"
        );

        // Excluding everything still terminates with an empty result.
        assert!(get_unembedded_nodes_excluding(conn, 10, &ids)
            .unwrap()
            .is_empty());
    }
}
