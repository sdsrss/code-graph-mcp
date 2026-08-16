use super::*;

/// CLI arguments for the `report` subcommand.
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp report",
    about = "Consolidated code-health report (summary, hot functions, chokepoints, cycles, surprising, dead code)"
)]
pub struct ReportArgs {
    /// Items per section (default: 5)
    #[arg(long, default_value_t = 5)]
    pub top: u32,
    /// Include test symbols in the analyses (excluded by default)
    #[arg(long)]
    pub include_tests: bool,
    /// JSON output
    #[arg(long)]
    pub json: bool,
}

/// One-shot architecture/health overview that bundles the structural analyses
/// (hot functions, betweenness chokepoints, import cycles, surprising
/// connections, dead code) plus a corpus summary with edge-confidence breakdown.
/// Pure read-time aggregation of existing analyses. CLI-only; not an MCP tool.
pub fn cmd_report(project_root: &Path, args: ReportArgs) -> Result<()> {
    use crate::domain::{CONF_AMBIGUOUS, CONF_EXTRACTED, CONF_INFERRED};
    let ReportArgs {
        top,
        include_tests,
        json: json_mode,
    } = args;
    let top = top as usize;

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    // Dead code is computed FIRST so the query-time freshness resync can run
    // before the rest of the report: this command prints dead-code start_line
    // (JSON `line` below, and the text `file:line` rows) straight from the
    // index, and was the one line-printing subcommand with no refresh at all —
    // its own standalone `dead-code` command has had one since the shared resync
    // landed (audit 2026-08-02 FRS-5). Refreshing here rather than after the
    // other analyses also keeps the whole report on ONE index state instead of
    // mixing pre- and post-reindex counts.
    let run_dead = |conn: &rusqlite::Connection| {
        crate::storage::queries::find_dead_code(conn, None, None, include_tests, 3, top as i64)
    };
    let mut dead = run_dead(conn)?;
    let files: Vec<String> = dead.iter().map(|d| d.file_path.clone()).collect();
    let freshness = refresh_files_if_stale(&ctx.db, &ctx.project_root, &files);
    if freshness.any_changed {
        dead = run_dead(conn)?;
    }
    freshness.disclose();

    let status = crate::storage::queries::get_index_status(conn, false)?;

    // Edge-confidence breakdown.
    let mut conf: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    {
        let mut stmt =
            conn.prepare("SELECT confidence, COUNT(*) FROM edges GROUP BY confidence")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            let (c, n) = row?;
            conf.insert(c, n);
        }
    }
    let conf_get = |k: &str| conf.get(k).copied().unwrap_or(0);

    let (_modules, _deps, _entry, hot) = crate::storage::queries::get_project_map(conn)?;
    let chokepoints = crate::graph::centrality::betweenness_centrality(conn, include_tests, top)?;
    let mut cycles = {
        let edges = crate::storage::queries::all_file_import_edges(conn)?;
        crate::graph::cycles::find_cycles(&edges)
    };
    cycles.truncate(top);
    let surprising = crate::graph::surprising::surprising_connections(conn, include_tests, top)?;

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        // Object envelope (sections may be empty arrays), per the CLI JSON contract.
        let mut report = serde_json::json!({
            "summary": {
                "files": status.files_count,
                "nodes": status.nodes_count,
                "edges": status.edges_count,
                "confidence": {
                    "extracted": conf_get(CONF_EXTRACTED),
                    "inferred": conf_get(CONF_INFERRED),
                    "ambiguous": conf_get(CONF_AMBIGUOUS),
                },
            },
            "hot_functions": hot.iter().take(top).map(|h| serde_json::json!({
                "name": h.name, "type": h.node_type, "file": h.file, "caller_count": h.caller_count,
            })).collect::<Vec<_>>(),
            "chokepoints": chokepoints.iter().map(|c| serde_json::json!({
                "name": c.name, "file": c.file_path, "betweenness": c.score, "caller_count": c.caller_count,
            })).collect::<Vec<_>>(),
            "import_cycles": cycles.iter().map(|c| serde_json::json!({
                "files": c.files, "size": c.size, "cycle": c.path,
            })).collect::<Vec<_>>(),
            "surprising_connections": surprising.iter().map(|c| serde_json::json!({
                "source": c.source, "target": c.target, "confidence": c.confidence, "score": c.score,
                "source_file": c.source_file, "target_file": c.target_file,
            })).collect::<Vec<_>>(),
            "dead_code": dead.iter().map(|d| serde_json::json!({
                "name": d.name, "type": d.node_type, "file": d.file_path, "line": d.start_line,
            })).collect::<Vec<_>>(),
        });
        // Object-shaped envelope, so the in-band marker applies (the stderr note
        // from `disclose()` is invisible under `--json 2>/dev/null`).
        freshness.attach_partial(&mut report);
        writeln!(stdout, "{}", serde_json::to_string(&report)?)?;
        return Ok(());
    }

    writeln!(stdout, "# Code Health Report\n")?;
    writeln!(stdout, "## Summary")?;
    writeln!(
        stdout,
        "  {} files · {} nodes · {} edges",
        status.files_count, status.nodes_count, status.edges_count
    )?;
    writeln!(
        stdout,
        "  edge confidence: {} extracted · {} inferred · {} ambiguous",
        conf_get(CONF_EXTRACTED),
        conf_get(CONF_INFERRED),
        conf_get(CONF_AMBIGUOUS)
    )?;

    writeln!(stdout, "\n## Hot functions (most-called)")?;
    if hot.is_empty() {
        writeln!(stdout, "  (none)")?;
    }
    for h in hot.iter().take(top) {
        writeln!(
            stdout,
            "  {:>4} callers  {} ({}) — {}",
            h.caller_count, h.name, h.node_type, h.file
        )?;
    }

    writeln!(stdout, "\n## Architectural chokepoints (betweenness)")?;
    if chokepoints.is_empty() {
        writeln!(stdout, "  (none)")?;
    }
    for c in &chokepoints {
        writeln!(stdout, "  {:>8.1}  {} — {}", c.score, c.name, c.file_path)?;
    }

    writeln!(stdout, "\n## Import cycles")?;
    if cycles.is_empty() {
        writeln!(stdout, "  (none)")?;
    }
    for c in &cycles {
        writeln!(stdout, "  {}", c.headline())?;
        // For larger SCCs the shortest loop omits members — name them so the report is actionable.
        if c.size + 1 > c.path.len() {
            writeln!(stdout, "    files: {}", c.files.join(", "))?;
        }
    }

    writeln!(stdout, "\n## Surprising connections")?;
    if surprising.is_empty() {
        writeln!(stdout, "  (none)")?;
    }
    for c in &surprising {
        writeln!(
            stdout,
            "  [{}] {} → {}  ({} {})",
            c.score, c.source, c.target, c.confidence, c.relation
        )?;
    }

    writeln!(stdout, "\n## Dead code (unused symbols)")?;
    if dead.is_empty() {
        writeln!(stdout, "  (none)")?;
    }
    for d in &dead {
        writeln!(
            stdout,
            "  {} ({}) — {}:{}",
            d.name, d.node_type, d.file_path, d.start_line
        )?;
    }

    Ok(())
}
