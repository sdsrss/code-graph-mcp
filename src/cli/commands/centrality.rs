use super::*;

/// CLI arguments for the `centrality` subcommand.
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp centrality",
    about = "Rank architectural chokepoints by betweenness centrality (call graph)"
)]
pub struct CentralityArgs {
    /// Number of functions to report (default: 15)
    #[arg(long, default_value_t = 15)]
    pub limit: u32,
    /// Include test symbols in the graph (excluded by default)
    #[arg(long)]
    pub include_tests: bool,
    /// JSON output
    #[arg(long)]
    pub json: bool,
}

/// Rank functions by betweenness centrality over the `calls` graph — the
/// structural bridges that lie on the most shortest call paths between other
/// functions. Complements `map`'s caller_count "hot functions" (degree
/// centrality): a chokepoint can have few callers yet route most cross-cluster
/// traffic. CLI-only; not exposed as an MCP tool.
pub fn cmd_centrality(project_root: &Path, args: CentralityArgs) -> Result<()> {
    let CentralityArgs {
        limit,
        include_tests,
        json: json_mode,
    } = args;
    // Clamp to >=1 (mirrors cmd_callgraph's depth.max(1)): --limit 0 would return
    // an empty ranking and trip the "No chokepoints found (graph has no multi-hop
    // call paths)" branch below — a message that falsely blames the graph when the
    // user merely asked for zero rows.
    let limit = limit.max(1);

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    let mut ranked =
        crate::graph::centrality::betweenness_centrality(conn, include_tests, limit as usize)?;
    // Query-time freshness over the files this answer names — `centrality` emits
    // `file_path` per ranked symbol and was never swept when freshness was wired
    // command by command (audit 2026-08-29 CON-03).
    {
        let files: Vec<String> = ranked.iter().map(|c| c.file_path.clone()).collect();
        let outcome = refresh_files_if_stale(&ctx.db, &ctx.project_root, &files);
        if outcome.any_changed {
            ranked = crate::graph::centrality::betweenness_centrality(
                conn,
                include_tests,
                limit as usize,
            )?;
        }
        outcome.disclose();
    }

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        // Empty → `[]` (array-shaped success), per the CLI JSON-empty contract.
        let items: Vec<serde_json::Value> = ranked
            .iter()
            .map(|c| {
                serde_json::json!({
                    "name": c.name,
                    "type": c.node_type,
                    "file_path": c.file_path,
                    "betweenness": c.score,
                    "normalized": c.normalized,
                    "caller_count": c.caller_count,
                })
            })
            .collect();
        writeln!(stdout, "{}", serde_json::to_string(&items)?)?;
        return Ok(());
    }

    if ranked.is_empty() {
        eprintln!(
            "[code-graph] No chokepoints found (graph has no multi-hop call paths{}).",
            if include_tests {
                ""
            } else {
                "; try --include-tests"
            }
        );
        return Ok(());
    }

    writeln!(
        stdout,
        "Architectural chokepoints (betweenness centrality, top {}):",
        ranked.len()
    )?;
    writeln!(stdout, "(functions on the most shortest call paths between others — high score = structural bridge)\n")?;
    for c in &ranked {
        writeln!(
            stdout,
            "  {:>8.1} ({:.3}) {} {} — {} ({})",
            c.score,
            c.normalized,
            c.node_type,
            c.name,
            plural(c.caller_count as i64, "caller"),
            c.file_path
        )?;
    }

    Ok(())
}
