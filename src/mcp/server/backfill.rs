//! The unembedded-node backfill: a background sweep that embeds nodes an
//! interrupted or feature-gated index run left without vectors.
//!
//! Split out of `server/mod.rs` (audit 2026-08-22 P2-8). It is one of the two
//! seams that file offered — self-contained, driven by its own timer thread,
//! and touching the server only through the shared `AtomicBool` in-progress
//! flag and the retry counter. The other seam is `freshness`.

use super::{McpServer, PERIODIC_BACKFILL_SECS};
use crate::domain::CODE_GRAPH_DIR;
use crate::embedding::model::EmbeddingModel;
use crate::indexer::pipeline::embed_and_store_batch;
use crate::storage::db::Database;
use crate::storage::queries;
use anyhow::Result;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// How many consecutive `Stalled` drains the periodic backfill driver tolerates at
/// a given floor before it pins the floor and stops re-attempting. A stall is
/// ambiguous — a transient model/device error (which a retry recovers) or genuinely
/// un-embeddable residue (which would spin forever). A small budget lets the
/// transient case self-heal while bounding wasted model reloads on real residue.
pub(super) const MAX_BACKFILL_STALL_RETRIES: u32 = 3;

/// Why an unembedded-node backfill pass stopped — the signal the periodic driver
/// needs to decide whether its "confirmed un-embeddable" floor may advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BackfillOutcome {
    /// No embedding model on disk yet (download in flight, or FTS5-only env). The
    /// pass embedded nothing, but that says NOTHING about node embeddability — the
    /// floor must NOT advance, or every node strands until restart.
    NoModel,
    /// Every embeddable node now has a vector (the unembedded set emptied).
    Drained,
    /// A non-empty batch produced no new vectors. `progressed` distinguishes the two
    /// causes the driver must treat differently:
    /// - `true`: earlier batches this pass DID embed nodes, so the model is present and
    ///   working — the remainder is genuine un-embeddable residue. Pin the floor now;
    ///   no point retrying a proven-working model on content that can't embed.
    /// - `false`: the very first batch stalled, so we can't tell a transient model/device
    ///   error (a retry recovers) from residue. Bounded-retry before trusting it.
    Stalled { progressed: bool },
}
/// Decide the periodic backfill driver's next `(floor, stall_retries)` from one
/// drain attempt's [`BackfillOutcome`] and the freshly re-measured unembedded count.
///
/// `floor` = the count of nodes the driver has confirmed it cannot embed right now,
/// so it stops re-attempting them. It advances ONLY on evidence a model-present drain
/// ran and could make no further progress — never on a no-op caused by an absent
/// model. Pure (no I/O) so the branch logic is unit-testable without a live server.
pub(super) fn apply_backfill_outcome(
    floor: i64,
    stall_retries: u32,
    outcome: BackfillOutcome,
    remeasured: i64,
) -> (i64, u32) {
    match outcome {
        // Learned nothing — leave the floor low so the very next tick re-attempts;
        // the drain fires for real the moment the model finishes downloading.
        BackfillOutcome::NoModel => (floor, stall_retries),
        // Embeddable set emptied. Reset to 0: any count observed later is fresh work
        // that must be picked up, not residue to skip. Clears the retry budget too.
        BackfillOutcome::Drained => (0, 0),
        // Model proven working this pass — the remainder is genuine residue. Pin the
        // floor to it immediately and reset the retry budget; retrying a working model
        // on un-embeddable content would just churn.
        BackfillOutcome::Stalled { progressed: true } => (remeasured, 0),
        // Zero-progress stall (ambiguous: transient vs residue). Keep the floor low so
        // the next tick re-attempts and a transient failure self-heals, until the retry
        // budget is spent — then pin to the residue so we stop reloading the model to
        // spin on nodes that truly cannot embed this session.
        BackfillOutcome::Stalled { progressed: false } => {
            let retries = stall_retries + 1;
            if retries >= MAX_BACKFILL_STALL_RETRIES {
                (remeasured, 0)
            } else {
                (floor, retries)
            }
        }
    }
}

impl McpServer {
    /// Spawn the no-traffic embedding backfill driver (exactly once per process).
    ///
    /// The server only re-checks freshness and backfills on an MCP tool call
    /// (`ensure_indexed`). A session that exercises code-graph purely through the
    /// PreToolUse CLI hooks adds nodes via `ensure_file_indexed` (`model=None`) without
    /// ever sending a tool call, and with the watcher off nothing embeds them — they
    /// strand at <100% vector coverage until restart. This driver gives the primary
    /// server a watcher- and traffic-independent path to drain that backlog.
    ///
    /// It re-runs the guarded backfill only when the unembedded count rises ABOVE the
    /// residue the previous run left behind, so it never reloads the model to spin on
    /// permanently un-embeddable nodes (empty-content symbols that keep a
    /// `context_string` but yield no vector). An idle tick (no new work) is a single
    /// cheap COUNT; only a tick that finds new work pays for a drain + a residue re-count.
    pub(super) fn spawn_periodic_backfill(&self) {
        if !self.is_primary() {
            return;
        }
        if self
            .indexing
            .periodic_backfill_started
            .swap(true, Ordering::AcqRel)
        {
            return; // already spawned this session
        }
        // Gate only on vector storage — NOT on `self.embedding_model`, which stays None
        // until a tool call lazily loads it (exactly the no-tool-call sessions this driver
        // exists for). `run_unembedded_backfill` loads its own model and no-ops if absent,
        // mirroring the startup-index backfill path.
        if !self.db.vec_enabled() {
            return;
        }
        let db_path = match &self.project_root {
            Some(p) => p.join(CODE_GRAPH_DIR).join("index.db"),
            None => return,
        };
        let flag = Arc::clone(&self.indexing.embedding_in_progress);
        std::thread::spawn(move || {
            // `floor` = unembedded count left after the last drain (the un-embeddable
            // residue). Start at 0 so the first non-empty observation — including nodes
            // stranded by a prior session — triggers one run that establishes the true
            // floor. Re-opening the DB each tick (rather than holding a connection) keeps
            // us correct across a rebuild-index atomic swap.
            let mut floor: i64 = 0;
            let mut stall_retries: u32 = 0;
            loop {
                std::thread::sleep(std::time::Duration::from_secs(PERIODIC_BACKFILL_SECS));
                // Open NON-destructively for the read-only count: the destructive
                // `open_with_vec` revalidates and could WIPE the index on an
                // INDEX_VERSION skew window (a downgraded sibling binary), and a poller
                // doing that every tick forever is a standing hazard. sqlite-vec is
                // registered process-globally regardless, so the vec0 `node_vectors`
                // join still resolves. Re-opening each tick (vs holding a connection)
                // keeps us correct across a rebuild-index atomic swap.
                let unembedded = match Database::open_nondestructive(&db_path) {
                    Ok(db) => queries::count_unembedded_nodes(db.conn()).unwrap_or(floor),
                    Err(_) => continue, // transient (e.g. mid-swap) — retry next tick
                };
                if unembedded <= floor {
                    continue; // no embeddable work above the confirmed floor
                }
                // Embeddable work exists above the floor. Attempt a drain. The guard
                // no-ops (None) if a tool-call/startup backfill is already draining — leave
                // the floor untouched and retry next tick rather than trusting that other
                // run's mid-drain count.
                let Some(outcome) = Self::run_guarded_backfill(&db_path, &flag) else {
                    continue;
                };
                // Re-measure the residue for the floor decision. `apply_backfill_outcome`
                // only consumes this for the Stalled-pin branches; computing it
                // unconditionally keeps the driver loop trivial (a 60s nondestructive read
                // is negligible) and the decision logic pure + unit-testable.
                //
                // TOCTOU note (accepted, self-healing): this read happens AFTER the drain
                // released the flag, so a node added in that sub-second window inflates
                // `remeasured` and, on a pin, gets folded into the floor — stranding it
                // until the count next rises above that floor (the next write) or restart.
                // This only bites in the rare pin path (healthy installs always Drain, never
                // pin) and resolves on the next file change. Do NOT "fix" it by pinning to a
                // count captured before the drain — that reintroduces the original bug of
                // pinning a stale-high floor.
                let remeasured = match Database::open_nondestructive(&db_path) {
                    Ok(db) => queries::count_unembedded_nodes(db.conn()).unwrap_or(floor),
                    Err(_) => floor,
                };
                let (new_floor, new_retries) =
                    apply_backfill_outcome(floor, stall_retries, outcome, remeasured);
                floor = new_floor;
                stall_retries = new_retries;
            }
        });
    }
    /// Claim the `embedding_in_progress` flag and run [`Self::run_unembedded_backfill`]
    /// to completion, releasing the flag on return — and on unwind, via the drop guard
    /// (release builds use `panic = "abort"`, where a panic ends the process and resets
    /// the in-memory flag regardless). No-ops if a backfill is already running. Blocks
    /// the caller, so it must be
    /// invoked on a background thread (the embedding thread, or the startup-index
    /// thread once its own work is committed and the indexing flag is clear).
    ///
    /// Returns `Some(outcome)` if this call claimed the flag and ran the backfill, or
    /// `None` if it no-op'd because another backfill already held it. The periodic driver
    /// uses both the `None` (don't trust a residue re-measurement taken while a *different*
    /// backfill is mid-drain) and the [`BackfillOutcome`] (don't advance the floor on a
    /// model-absent no-op) to decide whether its floor may advance.
    pub(super) fn run_guarded_backfill(
        db_path: &Path,
        in_progress: &AtomicBool,
    ) -> Option<BackfillOutcome> {
        if in_progress.swap(true, Ordering::AcqRel) {
            return None; // a backfill is already running
        }
        // Drop guard ensures the flag is always cleared, even on panic.
        struct FlagGuard<'a>(&'a AtomicBool);
        impl Drop for FlagGuard<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _guard = FlagGuard(in_progress);
        match Self::run_unembedded_backfill(db_path) {
            Ok(outcome) => Some(outcome),
            Err(e) => {
                // A hard error (DB open / count query failed) embedded nothing — a
                // zero-progress stall. Report it that way (not a clean drain) so the driver
                // bounded-retries instead of pinning the floor on what may be a transient
                // (e.g. mid-swap) failure.
                tracing::warn!("[embed-bg] Failed: {}", e);
                Some(BackfillOutcome::Stalled { progressed: false })
            }
        }
    }
    /// Embed every node that still lacks a vector, hot-path first, in batches.
    /// Loads its own model + DB connection (EmbeddingModel is `!Send`, so it can't
    /// cross the thread boundary) and no-ops when no model is available locally or
    /// vec is disabled. Append-only writes — safe to run alongside a reader.
    fn run_unembedded_backfill(db_path: &Path) -> Result<BackfillOutcome> {
        let model = match EmbeddingModel::load()? {
            Some(m) => m,
            // Model not on disk yet (download in flight). Report NoModel so the periodic
            // driver keeps its floor low and re-attempts once the download lands, instead
            // of pinning the floor and stranding every node until restart.
            None => return Ok(BackfillOutcome::NoModel),
        };
        let db = Database::open_with_vec(db_path)?;
        if !db.vec_enabled() {
            // Vectors disabled for the session — nothing is embeddable; treat as drained.
            // Safe for the periodic driver despite `Drained` resetting the floor to 0:
            // that driver only spawns when `self.db.vec_enabled()` is already true (and
            // sqlite-vec is process-global), so it never reaches this branch in a loop —
            // i.e. this can't cause a per-tick full-model-load spin.
            return Ok(BackfillOutcome::Drained);
        }

        // Same-dim model-swap safety: if the embedding model's content fingerprint changed
        // since these vectors/cache were written, they are stale (a same-dim weight change is
        // invisible to the vec-table dim check). Clear both so every node re-embeds with the
        // new model. Cheap once-per-run meta compare; best-effort — never block embedding.
        #[cfg(feature = "embed-model")]
        if let Err(e) =
            queries::ensure_embedding_cache_valid(db.conn(), EmbeddingModel::MODEL_CONTENT_BLAKE3)
        {
            tracing::warn!("[embed-cache] validity check failed (continuing): {}", e);
        }

        const EMBED_BATCH: usize = 32;
        let mut total_embedded = 0usize;
        let t0 = std::time::Instant::now();
        // Nodes that failed to embed THIS run (a deterministically un-embeddable
        // context_string, or a transient per-node inference error in the sequential
        // fallback). embed_and_store_batch returns the IDs that actually got a vector,
        // so we learn failures directly; we exclude them from the next query so a poison
        // node at the head of the caller-count ordering can't be re-fetched forever and
        // starve the embeddable nodes behind it — we advance past it instead of stopping.
        let mut failed: std::collections::HashSet<i64> = std::collections::HashSet::new();

        loop {
            let exclude: Vec<i64> = failed.iter().copied().collect();
            let chunk = queries::get_unembedded_nodes_excluding(db.conn(), EMBED_BATCH, &exclude)?;
            if chunk.is_empty() {
                break;
            }
            let chunk_len = chunk.len();
            // embed_and_store_batch reuses cached embeddings for unchanged content (a byte copy,
            // no inference) and embeds the rest, managing its own transaction + cache writes.
            let embedded_ids = embed_and_store_batch(&db, &model, &chunk)?;
            total_embedded += embedded_ids.len();
            if embedded_ids.len() < chunk_len {
                let ok: std::collections::HashSet<i64> = embedded_ids.into_iter().collect();
                for (id, _) in &chunk {
                    if !ok.contains(id) {
                        failed.insert(*id);
                    }
                }
            }
        }

        if total_embedded > 0 {
            tracing::info!(
                "[embed-bg] Complete: {} nodes now embedded (some may be cache reuse) in {:.1}s",
                total_embedded,
                t0.elapsed().as_secs_f64()
            );
        }
        if !failed.is_empty() {
            tracing::warn!(
                "[embed-bg] {} node(s) could not be embedded this run (skipped as residue)",
                failed.len()
            );
        }
        // No failures → every embeddable node now has a vector. Failures remain → genuine
        // residue we advanced past (not starved on); `progressed` lets the periodic driver
        // pin that residue immediately when the model is proven working this pass.
        let outcome = if failed.is_empty() {
            BackfillOutcome::Drained
        } else {
            BackfillOutcome::Stalled {
                progressed: total_embedded > 0,
            }
        };
        Ok(outcome)
    }
}
