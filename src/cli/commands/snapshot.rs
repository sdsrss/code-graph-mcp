use super::*;

/// CLI arguments for the `snapshot` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp snapshot",
    about = "Build or inspect a portable graph snapshot"
)]
pub struct SnapshotArgs {
    #[command(subcommand)]
    pub command: SnapshotCommand,
}

/// `snapshot` sub-subcommands (replaces the hand-rolled args[2]/args[3] dispatch).
#[derive(Subcommand, Debug)]
pub enum SnapshotCommand {
    /// Build a portable graph snapshot (auto zstd when --out ends in .db.zst)
    Create(SnapshotCreateArgs),
    /// Print snapshot metadata as JSON (accepts .db or .db.zst)
    Inspect(SnapshotInspectArgs),
}

/// `snapshot create` arguments.
#[derive(Parser, Debug)]
pub struct SnapshotCreateArgs {
    /// Output path (auto zstd-compresses when it ends in .db.zst)
    #[arg(long)]
    pub out: String,
    /// Include embedding vectors in the snapshot
    #[arg(long)]
    pub include_embeddings: bool,
    /// Project root to snapshot (default: the resolved project root)
    #[arg(long)]
    pub root: Option<String>,
    /// Suppress the "snapshot created" confirmation
    #[arg(long)]
    pub quiet: bool,
}

/// `snapshot inspect` arguments.
#[derive(Parser, Debug)]
pub struct SnapshotInspectArgs {
    /// Snapshot file to inspect (.db or .db.zst; format from magic bytes)
    pub file: String,
}

/// Build a portable graph snapshot. `snapshot create --out <path>
/// [--include-embeddings] [--root <dir>] [--quiet]`.
pub fn cmd_snapshot_create(project_root: &Path, args: SnapshotCreateArgs) -> Result<()> {
    let SnapshotCreateArgs {
        out,
        include_embeddings: include,
        root,
        quiet,
    } = args;

    let root = root
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| project_root.to_path_buf());

    // Pre-flight checks for --out so SQLite VACUUM INTO doesn't leak its
    // raw "unable to open database file" error when the user passed a dir
    // or a path with a missing parent directory.
    let out_path = std::path::Path::new(&out);
    if out_path.is_dir() || out.ends_with('/') {
        anyhow::bail!(
            "--out '{}' is a directory; expected a file path (e.g. '{}snapshot.db' or '{}snapshot.db.zst')",
            out, out, out
        );
    }
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            anyhow::bail!(
                "--out parent directory does not exist: {} (create it first with `mkdir -p {}`)",
                parent.display(),
                parent.display()
            );
        }
    }

    crate::snapshot::create(&root, out_path, include)?;
    if !quiet {
        eprintln!("snapshot created: {}", out);
        if out.ends_with(".db.zst") {
            eprintln!(
                "integrity sidecar: {out}.blake3 \u{2014} upload BOTH to the release; \
                 consumers verify the checksum before decompressing"
            );
        }
    }
    Ok(())
}

/// Print snapshot metadata as JSON to stdout. Accepts `.db` or `.db.zst`
/// (format detected from magic bytes, not extension).
pub fn cmd_snapshot_inspect(args: SnapshotInspectArgs) -> Result<()> {
    let meta = crate::snapshot::inspect(std::path::Path::new(&args.file))?;
    println!("{}", serde_json::to_string_pretty(&meta)?);
    Ok(())
}

// --- reindex subcommand ---
