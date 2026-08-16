use super::*;

/// CLI arguments for the `reindex` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp reindex",
    about = "Incremental index refresh; --from-snapshot drops the index and refetches the published snapshot (rebuild-index for an unconditional rebuild)"
)]
pub struct ReindexArgs {
    /// Refetch the published snapshot before indexing (falls back to full index)
    #[arg(long)]
    pub from_snapshot: bool,
    /// Index structure only and skip embeddings (vectors backfill later).
    #[arg(long)]
    pub no_embed: bool,
    /// With --from-snapshot: drop the index even while another process holds the
    /// index lock (its pending writes are lost).
    #[arg(long)]
    pub force: bool,
    /// Print the resulting index run's counters as one JSON object on stdout
    /// (progress and snapshot notices stay on stderr).
    #[arg(long)]
    pub json: bool,
}

/// `reindex [--from-snapshot]` — wipe `.code-graph/` index files and re-fetch
/// snapshot (or full-index if no snapshot available). Without `--from-snapshot`,
/// behaves identically to `incremental-index`.
///
/// Equivalent to user-side `rm -rf .code-graph/index.db*` + restarting the
/// MCP server, but with optional snapshot-bootstrap acceleration.
pub fn cmd_reindex(project_root: &Path, args: ReindexArgs) -> Result<()> {
    let from_snapshot = args.from_snapshot;
    let no_embed = args.no_embed;
    let cg_dir = project_root.join(crate::domain::CODE_GRAPH_DIR);

    // Held across the unlink AND the snapshot install — the whole window in
    // which index.db is missing or half-landed — then released explicitly before
    // the incremental step below. It cannot stay held through that call:
    // `cmd_incremental_index` probes the same lock, and flock is per open file
    // DESCRIPTION, so our own guard would answer "another process holds it" and
    // print a warning about ourselves.
    let mut index_lock: Option<crate::indexer::lock::IndexLockGuard> = None;
    if from_snapshot && cg_dir.exists() {
        // Same door as `rebuild-index`: unlinking index.db under a running
        // server strands its open fd on the deleted inode (audit P1-3). Taken
        // BEFORE the removal so a refusal leaves the index untouched.
        index_lock = lock_index_for_replace(&cg_dir, args.force, false)?;
        // Remove just index.db + WAL files; leave usage.jsonl etc. intact.
        for name in ["index.db", "index.db-wal", "index.db-shm"] {
            let _ = std::fs::remove_file(cg_dir.join(name));
        }
    }

    if from_snapshot {
        if let Some(url) = crate::snapshot::resolve_snapshot_source(project_root) {
            match crate::snapshot::try_install(&url, project_root) {
                Ok(commit) => {
                    eprintln!("Snapshot installed at commit {commit}");
                    drop(index_lock);
                    return cmd_incremental_index_opts(project_root, false, no_embed, args.json);
                }
                Err(e) => eprintln!("Snapshot install failed ({e}), falling back to full index"),
            }
        } else {
            eprintln!("No snapshot source resolved, falling back to full index");
        }
    }

    drop(index_lock);
    cmd_incremental_index_opts(project_root, false, no_embed, args.json)
}
