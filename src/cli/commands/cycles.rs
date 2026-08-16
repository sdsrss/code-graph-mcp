use super::*;

/// CLI arguments for the `cycles` subcommand.
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp cycles",
    about = "Detect circular import dependencies (file-level)"
)]
pub struct CyclesArgs {
    /// Maximum number of cycles to report (default: 50)
    #[arg(long, default_value_t = 50)]
    pub limit: u32,
    /// JSON output
    #[arg(long)]
    pub json: bool,
}

/// Detect circular import dependencies — strongly-connected components of the
/// file-level `imports` graph. Each cycle is a set of files that transitively
/// import each other, shown with a representative shortest loop `a → b → … → a`.
/// Reported over imports only: a `calls` cycle is mutual recursion, not a
/// circular import. Most actionable for JS/TS/Python/Go; Rust intra-crate module
/// cycles are frequently benign. CLI-only; not exposed as an MCP tool.
pub fn cmd_cycles(project_root: &Path, args: CyclesArgs) -> Result<()> {
    let CyclesArgs {
        limit,
        json: json_mode,
    } = args;

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    let edges = crate::storage::queries::all_file_import_edges(conn)?;
    let mut cycles = crate::graph::cycles::find_cycles(&edges);
    // Record the pre-truncation total: printing "(N found)" from the truncated
    // length under-reported ("50 found" when 80 exist) with no truncation marker
    // (disclosure-gap class, roadmap 2026-07-18 §1.5).
    let total_found = cycles.len();
    cycles.truncate(limit as usize);
    let truncated = total_found > cycles.len();

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        // Empty → `[]` (array-shaped success), per the CLI JSON-empty contract.
        let items: Vec<serde_json::Value> = cycles
            .iter()
            .map(|c| {
                serde_json::json!({
                    "files": c.files,
                    "size": c.size,
                    "cycle": c.path,
                })
            })
            .collect();
        if truncated {
            // Disclosure envelope only when --limit actually cut results
            // (mirrors callgraph's `limit_hit`); the common untruncated case
            // keeps the plain array shape.
            writeln!(
                stdout,
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "results": items,
                    "total_found": total_found,
                    "truncated": true,
                }))?
            )?;
        } else {
            writeln!(stdout, "{}", serde_json::to_string(&items)?)?;
        }
        return Ok(());
    }

    if cycles.is_empty() {
        eprintln!("[code-graph] No circular import dependencies found.");
        return Ok(());
    }

    if truncated {
        writeln!(
            stdout,
            "Circular import dependencies (showing {} of {} found — raise --limit for the rest):",
            cycles.len(),
            total_found
        )?;
    } else {
        writeln!(
            stdout,
            "Circular import dependencies ({} found):",
            cycles.len()
        )?;
    }
    writeln!(
        stdout,
        "(files that transitively import each other — a → b → … → a)\n"
    )?;
    for c in &cycles {
        writeln!(stdout, "  {}", c.headline())?;
        // When the SCC has more files than the representative loop visits, list them all.
        if c.size + 1 > c.path.len() {
            writeln!(stdout, "    files: {}", c.files.join(", "))?;
        }
    }

    Ok(())
}
