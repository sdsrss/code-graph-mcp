use super::*;

/// Remove empty legacy database files left behind from past naming migrations.
/// Pre-v0.5 iterations briefly used `code-graph.db`, `code_graph.db`, `graph.db`
/// before settling on `index.db`; the renames never deleted the old 0-byte stubs.
pub fn cleanup_legacy_db_files(code_graph_dir: &Path) {
    // Guarded HERE rather than at the three call sites: this deletes files by
    // fixed name inside a directory the repo can supply, and a guard that lives
    // in the callers is a guard the next caller does not inherit. Small blast
    // radius (0-byte files only) but the same primitive.
    if crate::utils::owned::reject_symlinked_dir(code_graph_dir).is_err() {
        return;
    }
    const LEGACY: &[&str] = &["code-graph.db", "code_graph.db", "graph.db"];
    for name in LEGACY {
        let p = code_graph_dir.join(name);
        if let Ok(meta) = std::fs::metadata(&p) {
            if meta.is_file() && meta.len() == 0 {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
}

// Internal notes — `//` (not `///`) so clap leaves them out of `--help`: only flag
// parsing lives in this struct; the git/index existence guard stays in main() — it
// must precede any resolve_project_root indexing side effect and may skip the run
// entirely (issue #8). The handler keeps its `quiet: bool` signature so the internal
// reindex/rebuild-index callers are unaffected.
/// CLI arguments for the `incremental-index` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp incremental-index",
    about = "Run incremental index update (full index when none exists)"
)]
pub struct IncrementalIndexArgs {
    /// Suppress progress output (used by the PostToolUse hook)
    #[arg(long)]
    pub quiet: bool,
    /// Index structure only (nodes/edges/FTS) and skip embeddings for a fast,
    /// query-ready index. Vectors backfill later (MCP server / a later run).
    #[arg(long)]
    pub no_embed: bool,
    /// Print the run's counters as one JSON object on stdout (progress stays on
    /// stderr). For CI and scripts: `--json` used to be a clap parse error here,
    /// so the only way to learn what an index run did was to scrape prose.
    #[arg(long)]
    pub json: bool,
}

/// Run incremental index update.
/// If `quiet` is true, suppress non-error output.
/// Auto-creates the database and runs a full index if no index exists.
/// Map SQLITE_BUSY ("database is locked", error code 5) to an actionable hint —
/// surfaces when two indexers / an MCP server race on the same index.db. Shared
/// by the full / incremental / embed paths.
pub(crate) fn wrap_index_busy<T>(r: Result<T>) -> Result<T> {
    r.map_err(|e| {
        let msg = format!("{:#}", e);
        if msg.contains("database is locked") || msg.contains("Error code 5") {
            anyhow::anyhow!(
                "Another `code-graph-mcp` process is writing to .code-graph/index.db \
                 (an indexer or MCP server). Wait for it to finish, then retry. \
                 Original error: {}",
                e
            )
        } else {
            e
        }
    })
}

/// Embed any nodes still missing vectors (synchronous, unlike the server's
/// background thread). No-op without the `embed-model` feature or when the model
/// can't load. Shared by the full / incremental / rebuild paths so embedding
/// behaviour can't drift between them.
pub(crate) fn embed_missing_nodes(db: &Database, quiet: bool) -> Result<()> {
    if !db.vec_enabled() {
        return Ok(());
    }
    use crate::embedding::model::EmbeddingModel;
    use crate::indexer::pipeline::embed_and_store_batch;
    if let Some(model) = EmbeddingModel::load()? {
        let mut total = 0usize;
        // This loop is the longest phase of an index run by far — 64 nodes per
        // batch through a CPU model — and it used to print NOTHING until it
        // finished. The caller has already printed its "Incremental index: N
        // files updated" summary by then, so a big repo showed a completed-looking
        // line and then sat silent for minutes: indistinguishable from a hang, and
        // the documented remedy for a hang is Ctrl-C. Announce the backlog and
        // tick while it drains. Threshold + time-throttle so the common
        // few-dozen-node run stays a one-liner.
        const PROGRESS_MIN_NODES: i64 = 500;
        const PROGRESS_EVERY: std::time::Duration = std::time::Duration::from_secs(3);
        let pending = queries::count_unembedded_nodes(db.conn()).unwrap_or(0);
        let show_progress = !quiet && pending >= PROGRESS_MIN_NODES;
        if show_progress {
            eprintln!("Embedding {pending} nodes (structure is already queryable)...");
        }
        let mut last_tick = std::time::Instant::now();
        // Skip nodes that fail to embed this run. This loop only stops on an empty
        // result, so without excluding failures a single deterministically-un-embeddable
        // node (which stays `node_vectors IS NULL` and sorts first by caller-count) would
        // be re-fetched at the head of every batch and spin the loop forever.
        let mut failed: std::collections::HashSet<i64> = std::collections::HashSet::new();
        loop {
            let exclude: Vec<i64> = failed.iter().copied().collect();
            let chunk = wrap_index_busy(queries::get_unembedded_nodes_excluding(
                db.conn(),
                64,
                &exclude,
            ))?;
            if chunk.is_empty() {
                break;
            }
            let chunk_len = chunk.len();
            let embedded_ids = wrap_index_busy(embed_and_store_batch(db, &model, &chunk))?;
            total += embedded_ids.len();
            if show_progress && last_tick.elapsed() >= PROGRESS_EVERY {
                eprintln!("  embedded {total}/{pending}...");
                last_tick = std::time::Instant::now();
            }
            if embedded_ids.len() < chunk_len {
                let ok: std::collections::HashSet<i64> = embedded_ids.into_iter().collect();
                for (id, _) in &chunk {
                    if !ok.contains(id) {
                        failed.insert(*id);
                    }
                }
            }
        }
        if total > 0 && !quiet {
            let (embedded, embeddable) = queries::count_nodes_with_vectors(db.conn())?;
            eprintln!("Embedded {} nodes ({}/{})", total, embedded, embeddable);
        }
        if !failed.is_empty() && !quiet {
            eprintln!("{} node(s) could not be embedded (skipped)", failed.len());
        }
    }
    Ok(())
}

/// Surface, on the CLI path, the count of files that parsed with tree-sitter
/// ERROR nodes (symbols may be incomplete). Dual-writes `tracing::warn!` AND a
/// stderr summary line: the CLI entry points install no tracing subscriber
/// (feedback_tracing_invisible_in_cli), so the eprintln is what the user
/// actually sees; the tracing line keeps it visible under a server/log setup.
/// Silent when the count is zero. `quiet` suppresses only the stderr line, like
/// the surrounding index summaries.
pub(crate) fn warn_parse_errors(stats: &crate::indexer::pipeline::IndexStats, quiet: bool) {
    let n = stats.files_with_parse_errors;
    if n == 0 {
        return;
    }
    tracing::warn!(
        "{} file(s) parsed with syntax errors (symbols may be incomplete)",
        n
    );
    if !quiet {
        eprintln!(
            "{} file(s) parsed with syntax errors (symbols may be incomplete)",
            n
        );
    }
}

/// Build a fresh FULL index into an explicit `db_path` and embed it. The DB is
/// opened and dropped within this call, so on return the WAL is checkpointed and
/// `db_path` is self-contained — which lets `rebuild-index` build into a temp
/// file and atomically rename it over `index.db`.
pub(crate) fn build_full_index_at(
    db_path: &Path,
    project_root: &Path,
    quiet: bool,
    no_embed: bool,
) -> Result<crate::indexer::pipeline::IndexResult> {
    if let Some(parent) = db_path.parent() {
        // Not `create_dir_all`: that silently succeeds when `.code-graph` is a
        // repo-supplied symlink, putting `index.db` and every telemetry file
        // outside the project root while reporting success (audit 2026-08-29
        // SEC-03). Refuse before anything is written.
        crate::utils::owned::ensure_owned_dir(parent)?;
        cleanup_legacy_db_files(parent);
    }
    // Same `.gitignore` upkeep the MCP server does when IT creates the dir — a
    // pure-CLI install (hook-driven indexing, server never started) otherwise
    // leaves `?? .code-graph/` for `git add -A` to commit (audit DB-4).
    crate::utils::gitignore::ensure_code_graph_dir_ignored(project_root);
    // Open with vec support so embeddings can be stored.
    let db = Database::open_with_vec(db_path)?;
    use crate::indexer::pipeline::run_full_index;
    let result = wrap_index_busy(run_full_index(&db, project_root, None, None))?;
    if !quiet {
        eprintln!(
            "Full index: {} files, {} nodes, {} edges",
            result.files_indexed, result.nodes_created, result.edges_created
        );
    }
    warn_parse_errors(&result.stats, quiet);
    finish_embedding(&db, quiet, no_embed)?;
    Ok(result)
}

/// The `--json` object shared by `incremental-index`, `rebuild-index` and
/// `reindex`: one line on stdout, emitted only after the run has actually
/// succeeded, so the tier-3 error contract in `main` stays the sole producer of
/// output on the failure path (a command must never print both).
///
/// `mode` names the path that really ran, not the subcommand asked for —
/// `incremental-index` on a fresh checkout reports `full`, and `reindex
/// --from-snapshot` reports whatever the post-install pass did. A CI script
/// reading `files_indexed` needs to know which of the two it is looking at.
pub(crate) fn emit_index_json(
    mode: &str,
    result: &crate::indexer::pipeline::IndexResult,
    started: std::time::Instant,
) {
    println!(
        "{}",
        serde_json::json!({
            "mode": mode,
            "files_indexed": result.files_indexed,
            "files_deleted": result.files_deleted,
            "nodes_created": result.nodes_created,
            "edges_created": result.edges_created,
            "files_with_parse_errors": result.stats.files_with_parse_errors,
            "elapsed_ms": started.elapsed().as_millis() as u64,
        })
    );
}

/// Shared structure-first → embedding handoff for the CLI index commands.
///
/// The structural graph (nodes/edges/FTS) is already committed and usable for
/// AST / grep / callgraph queries the moment indexing returns — embedding is a
/// separate, slow (CPU-bound) pass that only powers semantic/vector search. On a
/// large repo it dominates wall-clock (≈5 nodes/s), so a foreground `reindex`
/// could block for many minutes after the graph was already query-ready.
///
/// `--no-embed` skips it: the caller gets the fast structural index and the
/// vectors backfill later (the MCP server's background embedder fills any node
/// lacking a vector, resumably; or rerun without the flag to embed now).
pub(crate) fn finish_embedding(db: &Database, quiet: bool, no_embed: bool) -> Result<()> {
    if no_embed {
        if !quiet && db.vec_enabled() {
            let (embedded, embeddable) =
                queries::count_nodes_with_vectors(db.conn()).unwrap_or((0, 0));
            eprintln!(
                "Structure index ready (AST/grep/callgraph usable now). Skipping embeddings \
                 (--no-embed): {}/{} nodes have vectors; the rest backfill in the background \
                 or via `code-graph-mcp incremental-index`.",
                embedded, embeddable
            );
        }
        return Ok(());
    }
    embed_missing_nodes(db, quiet)
}

/// Warn if another process holds the index lock. A running MCP server holds the
/// flock for its whole lifetime, so a CLI incremental index now would race its
/// writes. Best-effort and non-blocking — the run still proceeds; we only surface
/// the hazard.
///
/// This used to say the worst case was "contention, not loss". It is not.
/// SQLite's own locking keeps each statement atomic, but the two writers do not
/// share the run-scoped state around them: a racing CLI run can delete node rows
/// the server's in-flight batch still holds ids for, and the server's incremental
/// then fails on a FOREIGN KEY constraint. Its recovery for that is
/// `DELETE FROM files` plus a full re-index — the whole index is discarded and
/// rebuilt from scratch, which on a large repo is minutes of degraded answers
/// (2026-08-16 audit §四). The message below says so.
///
/// `quiet` suppresses the stderr line ONLY. The probe itself always runs and the
/// finding always reaches `tracing` (same split as `warn_parse_errors`): a flag
/// whose job is to keep hook output clean must not also decide whether a hazard
/// is looked for. Destructive callers use
/// [`lock_index_for_replace`] instead — for them this is a refusal,
/// not a warning.
pub(crate) fn warn_if_index_locked(code_graph_dir: &Path, quiet: bool) {
    if !crate::indexer::lock::other_process_holds_index_lock(code_graph_dir) {
        return;
    }
    let lock = code_graph_dir.join("index.lock");
    tracing::warn!(
        "another process holds the index lock at {} — indexing now may race its writes",
        lock.display()
    );
    if !quiet {
        // Same holders as the replace-gate's refusal names: since the CLI takes
        // this lock too, "likely a running MCP server" was no longer true — it
        // sent the user to stop a server that may not exist while a concurrent
        // rebuild-index was the real holder.
        eprintln!(
            "[code-graph] Warning: another process (a running MCP server, or a \
             concurrent rebuild-index / reindex) holds the index lock at {}. \
             Indexing now races its writes; if that trips a foreign-key error on \
             the other side, its recovery is to discard the index and rebuild it \
             from scratch. Wait for it to finish, or stop the server.",
            lock.display()
        );
    }
}

/// Gate for commands that REPLACE `index.db` wholesale (`rebuild-index`'s atomic
/// rename, `reindex --from-snapshot`'s unlink).
///
/// A running MCP server holds an open fd on `index.db`. POSIX `rename(2)` /
/// `unlink(2)` swap the directory entry but leave that fd pointing at the old,
/// now-unlinked inode — so every subsequent write from that server (watcher
/// increments, embedding backfill) lands in a deleted file and is lost the
/// moment it closes, while its queries keep answering from the pre-rebuild
/// snapshot. Nothing detects it; the user sees a rebuild that "worked" and a
/// server that never picks it up. The MCP `rebuild_index` tool avoids the same
/// inode trap by rebuilding inside one transaction, and snapshot install avoids
/// it by landing before the DB is opened — this path was the one left unguarded
/// (audit 2026-08-02 P1-3).
///
/// Refusing is therefore the safe default; `--force` is the escape hatch for a
/// user who knows the lock holder is defunct. As with `warn_if_index_locked`,
/// `quiet` gates printing, never probing.
///
/// The gate also TAKES the lock and hands the guard back, instead of only
/// probing it. Probing alone excluded nothing among CLI runs: two concurrent
/// `rebuild-index --confirm` invocations both saw a free lock, both entered the
/// temp-file sweep (which deletes ANY `index.db.rebuild-*`, by design, to clear
/// crashed runs), and the loser died with a bare SQLite `disk I/O error` —
/// no corruption, thanks to the atomic rename, but nothing a user could act on
/// (QA ISSUE-008). Holding the lock turns that collision into this function's
/// existing, explanatory refusal. Keep the returned guard alive until the swap
/// is complete; dropping it releases the lock.
///
/// Failure modes are kept asymmetric on purpose: a lock HELD by someone else
/// refuses, but a lock we merely cannot open (read-only dir, exotic FS with no
/// flock) proceeds unlocked exactly as before — this gate must not be the reason
/// a rebuild that used to work stops working.
#[must_use = "the returned guard holds the index lock; dropping it early reopens the race"]
pub(crate) fn lock_index_for_replace(
    code_graph_dir: &Path,
    force: bool,
    quiet: bool,
) -> Result<Option<crate::indexer::lock::IndexLockGuard>> {
    let lock = code_graph_dir.join("index.lock");
    let refuse_or_force = |quiet: bool| -> Result<()> {
        if !force {
            anyhow::bail!(
                "another process (a running MCP server, or a concurrent rebuild-index / \
                 reindex) holds the index lock at {}. \
                 Replacing index.db now would leave that process writing into a deleted file — \
                 its indexing and embedding work would be lost silently, and its answers would \
                 stay on the pre-rebuild index until it restarts.\n  \
                 Stop the MCP server first (end the Claude Code session using this project) \
                 or wait for the other rebuild to finish, then rerun. Pass --force to replace \
                 the index anyway.",
                lock.display()
            );
        }
        tracing::warn!(
            "--force: replacing index.db while another process holds {} — its pending writes will be lost",
            lock.display()
        );
        if !quiet {
            eprintln!(
                "[code-graph] --force: another process holds the index lock at {}. \
                 Replacing the index anyway — that process's pending writes will be lost.",
                lock.display()
            );
        }
        Ok(())
    };

    if crate::indexer::lock::other_process_holds_index_lock(code_graph_dir) {
        refuse_or_force(quiet)?;
        return Ok(None);
    }
    // Free a moment ago — claim it, so a rebuild starting now refuses instead of
    // racing us. Losing this acquisition means someone took it in between, which
    // is the same situation as the probe above; anything else (open error) is a
    // non-answer and must not block the run.
    match crate::indexer::lock::acquire_index_lock_guard(code_graph_dir) {
        Some(guard) => Ok(Some(guard)),
        None if crate::indexer::lock::other_process_holds_index_lock(code_graph_dir) => {
            refuse_or_force(quiet)?;
            Ok(None)
        }
        None => {
            tracing::warn!(
                "could not take the index lock at {} (it is not held by anyone) — proceeding unlocked",
                lock.display()
            );
            Ok(None)
        }
    }
}

pub fn cmd_incremental_index(project_root: &Path, quiet: bool, no_embed: bool) -> Result<()> {
    cmd_incremental_index_opts(project_root, quiet, no_embed, false)
}

/// `cmd_incremental_index` plus the `--json` switch, split out the same way
/// `cmd_health_check_opts` is: the three-positional-bool entry point has a dozen
/// call sites (tests included) that have no opinion about output format.
pub fn cmd_incremental_index_opts(
    project_root: &Path,
    quiet: bool,
    no_embed: bool,
    json: bool,
) -> Result<()> {
    let started = std::time::Instant::now();
    let db_path = project_root.join(CODE_GRAPH_DIR).join("index.db");
    warn_if_index_locked(&project_root.join(CODE_GRAPH_DIR), quiet);
    // Covers the incremental path too, not just the full-index one inside
    // build_full_index_at: an index created before this existed (or by a user
    // who removed the line) gets the entry back on the next run (audit DB-4).
    crate::utils::gitignore::ensure_code_graph_dir_ignored(project_root);

    // The plugin hooks run this command periodically even when no MCP server is
    // alive — exactly the window where a killed server's indexing-status.json
    // would otherwise pin the statusline at a phantom "indexing N/M" forever.
    // Stale-only: a live server's file has a fresh mtime and is left alone.
    crate::indexer::pipeline::remove_stale_indexing_status(project_root);

    // No existing DB → full index. Delegate to build_full_index_at so the
    // full-index + embed path is shared with rebuild-index (no drift).
    if !db_path.exists() {
        if !quiet {
            eprintln!("No index found, creating full index...");
        }
        let result = build_full_index_at(&db_path, project_root, quiet, no_embed)?;
        if json {
            emit_index_json("full", &result, started);
        }
        return Ok(());
    }

    cleanup_legacy_db_files(&project_root.join(CODE_GRAPH_DIR));

    // Open with vec support so embeddings can be stored
    let db = Database::open_with_vec(&db_path)?;

    // Incremental index for the existing database.
    use crate::indexer::pipeline::run_incremental_index;
    let stats = wrap_index_busy(run_incremental_index(&db, project_root, None, None))?;
    if !quiet {
        if stats.files_deleted > 0 {
            eprintln!(
                "Incremental index: {} files updated, {} files removed, {} nodes created",
                stats.files_indexed, stats.files_deleted, stats.nodes_created
            );
        } else {
            eprintln!(
                "Incremental index: {} files updated, {} nodes created",
                stats.files_indexed, stats.nodes_created
            );
        }
    }
    warn_parse_errors(&stats.stats, quiet);

    finish_embedding(&db, quiet, no_embed)?;
    if json {
        emit_index_json("incremental", &stats, started);
    }
    Ok(())
}

/// SQLite sidecar path: `<db>-wal` / `<db>-shm`. Appends the literal suffix to
/// the FULL filename (not an extension swap) — required for temp db names like
/// `index.db.rebuild-<pid>`, whose WAL is `index.db.rebuild-<pid>-wal`.
pub(crate) fn db_sidecar(db_path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut s = db_path.as_os_str().to_os_string();
    s.push(suffix);
    std::path::PathBuf::from(s)
}

/// Drop the existing index.db (plus WAL/SHM) and trigger a full rebuild via
/// `cmd_incremental_index` (which auto-detects the missing DB and does a full
/// index). Mirrors MCP `rebuild_index` tool semantics.
/// `rebuild-index` arguments (clap-migrated, audit #4).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp rebuild-index",
    about = "Drop and rebuild the index from scratch (requires --confirm)"
)]
pub struct RebuildIndexArgs {
    /// Confirm the destructive drop-and-rebuild (required to proceed)
    #[arg(long)]
    pub confirm: bool,
    /// Suppress progress output
    #[arg(long)]
    pub quiet: bool,
    /// Index structure only and skip embeddings (vectors backfill later).
    #[arg(long)]
    pub no_embed: bool,
    /// Rebuild even while another process holds the index lock (its pending
    /// writes are lost — stop the MCP server instead when you can).
    #[arg(long)]
    pub force: bool,
    /// Print the rebuild's counters as one JSON object on stdout, after the
    /// atomic swap has succeeded (progress stays on stderr).
    #[arg(long)]
    pub json: bool,
}

pub fn cmd_rebuild_index(project_root: &Path, args: RebuildIndexArgs) -> Result<()> {
    let started = std::time::Instant::now();
    let confirm = args.confirm;
    let quiet = args.quiet;
    let no_embed = args.no_embed;
    // `--confirm` is a business-logic confirmation gate, NOT a clap-required arg:
    // a missing confirm is a deliberate exit-1 anyhow bail (not a parse error),
    // preserving the prior contract (test_cli_rebuild_index_requires_confirm).
    if !confirm {
        anyhow::bail!(
            "rebuild-index drops the existing index and re-parses every file. \
             Pass --confirm to proceed. Use `incremental-index` for incremental updates."
        );
    }
    // Destructive-op sanity: refuse to operate on degenerate roots. Guards against
    // a resolve_project_root regression that could return `/` or `""`.
    if project_root.as_os_str().is_empty() || project_root == Path::new("/") {
        anyhow::bail!(
            "refusing to rebuild-index with degenerate project_root ({}). \
             Run from within a git-tracked project directory.",
            project_root.display()
        );
    }
    let code_graph_dir = project_root.join(CODE_GRAPH_DIR);
    // Sibling of the degenerate-root refusal above, and for the same reason:
    // everything below this line is destructive. A symlinked `.code-graph` sends
    // the lock's PID write, the `index.db.rebuild-*` sweep and the final rename
    // outside the project root, and the per-file guards cannot see it — they
    // judge the last path component, not the directory above it.
    crate::utils::owned::ensure_owned_dir(&code_graph_dir)?;
    let db_path = code_graph_dir.join("index.db");
    // Before any work: refuse to rename over an index another process has open,
    // and hold the lock for the whole rebuild so a concurrent one refuses here
    // rather than colliding in the temp sweep below. `_index_lock` must stay
    // bound to the end of the function — `let _ = …` would drop it immediately.
    let _index_lock = lock_index_for_replace(&code_graph_dir, args.force, quiet)?;

    // Atomic rebuild: build the fresh index into a temp file in the SAME dir,
    // then rename it over index.db in one syscall. Concurrent readers (a second
    // CLI invocation, or the MCP server reopening) therefore always see a
    // COMPLETE index — the old one until the rename, the new one after — instead
    // of the empty/partial window the old "remove index.db then rebuild in place"
    // left open for the entire (multi-second on large repos) rebuild.
    let temp_path = code_graph_dir.join(format!("index.db.rebuild-{}", std::process::id()));
    let temp_files = [
        temp_path.clone(),
        db_sidecar(&temp_path, "-wal"),
        db_sidecar(&temp_path, "-shm"),
    ];
    let remove_all = |paths: &[std::path::PathBuf]| {
        for p in paths {
            if p.exists() {
                let _ = std::fs::remove_file(p);
            }
        }
    };
    // Clear leftover temp files from previously-killed rebuilds (ANY pid). The
    // `index.db.rebuild-<pid>` prefix also matches their `-wal`/`-shm` sidecars.
    // A concurrent rebuild's in-progress temp could be swept too — that only
    // makes the other run's final rename fail (an error, never corruption);
    // concurrent rebuild-index runs were never supported.
    if let Ok(entries) = std::fs::read_dir(&code_graph_dir) {
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("index.db.rebuild-")
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    // Build into the temp file. On failure, drop the temp and keep the existing
    // index.db intact — the rename below is the only mutation of the live index,
    // so a failed rebuild no longer leaves the user with NO index (the old
    // remove-first path did).
    let result = match build_full_index_at(&temp_path, project_root, quiet, no_embed) {
        Ok(result) => result,
        Err(e) => {
            remove_all(&temp_files);
            return Err(e);
        }
    };
    // The temp DB closed cleanly inside build_full_index_at (WAL checkpointed);
    // remove any residual temp -wal/-shm so the renamed file is self-contained.
    remove_all(&temp_files[1..]);

    // Drop the OLD index's -wal/-shm BEFORE the rename: afterwards a stale
    // index.db-wal would be (wrongly) replayed by SQLite onto the NEW index.db.
    // The old WAL is discardable here — we're replacing the whole index. A reader
    // in the sub-millisecond gap sees the old index.db (a valid, complete file).
    remove_all(&[db_sidecar(&db_path, "-wal"), db_sidecar(&db_path, "-shm")]);

    // Atomic swap (temp and index.db share .code-graph/ → POSIX rename is atomic).
    std::fs::rename(&temp_path, &db_path)?;
    // After the swap, never before: until the rename lands, the counters describe
    // a temp file the user cannot query.
    if args.json {
        emit_index_json("rebuild", &result, started);
    }
    Ok(())
}
