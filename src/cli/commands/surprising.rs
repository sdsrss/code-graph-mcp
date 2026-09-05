use super::*;

/// CLI arguments for the `surprising` subcommand.
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp surprising",
    about = "Surface unexpected cross-module couplings (uncertain / sole-bridge edges)"
)]
pub struct SurprisingArgs {
    /// Number of connections to report (default: 15)
    #[arg(long, default_value_t = 15)]
    pub limit: u32,
    /// Include test symbols (excluded by default)
    #[arg(long)]
    pub include_tests: bool,
    /// JSON output
    #[arg(long)]
    pub json: bool,
}

/// Rank "surprising connections" — cross-file `calls`/`references` edges scored by
/// resolution confidence (ambiguous > inferred > extracted), whether they cross
/// module boundaries, and whether they are the sole edge between two modules.
/// Surfaces uncertain or non-obvious couplings for review/audit; structural edges
/// (imports/inherits) are excluded. CLI-only; not exposed as an MCP tool.
pub fn cmd_surprising(project_root: &Path, args: SurprisingArgs) -> Result<()> {
    let SurprisingArgs {
        limit,
        include_tests,
        json: json_mode,
    } = args;

    // `--limit 0` had NO floor at all: it returned an empty ranking and fell into
    // the "No surprising connections found (try --include-tests)" branch below —
    // a false diagnosis manufactured out of the user's own argument, which is
    // worse than the silent clamps elsewhere. Same `.max(1)` reasoning as
    // `centrality`, but disclosed.
    let limit = floor_arg("--limit", limit, 1);

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    let found =
        crate::graph::surprising::surprising_connections(conn, include_tests, limit as usize)?;

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        // Empty → `[]` (array-shaped success), per the CLI JSON-empty contract.
        let items: Vec<serde_json::Value> = found
            .iter()
            .map(|c| {
                serde_json::json!({
                    "source": c.source,
                    "source_file": c.source_file,
                    "target": c.target,
                    "target_file": c.target_file,
                    "relation": c.relation,
                    "confidence": c.confidence,
                    "score": c.score,
                    "why": c.reasons,
                })
            })
            .collect();
        writeln!(stdout, "{}", serde_json::to_string(&items)?)?;
        return Ok(());
    }

    if found.is_empty() {
        eprintln!(
            "[code-graph] No surprising connections found{}.",
            if include_tests {
                ""
            } else {
                " (try --include-tests)"
            }
        );
        return Ok(());
    }

    writeln!(stdout, "Surprising connections (top {}):", found.len())?;
    writeln!(
        stdout,
        "(score = low resolution confidence + crosses modules + sole bridge between them)\n"
    )?;
    for c in &found {
        writeln!(
            stdout,
            "  [{}] {} → {}  ({} {})",
            c.score, c.source, c.target, c.confidence, c.relation
        )?;
        writeln!(stdout, "      {} → {}", c.source_file, c.target_file)?;
        writeln!(stdout, "      {}", c.reasons.join("; "))?;
    }

    Ok(())
}
