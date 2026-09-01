use super::*;

/// CLI arguments for the `map` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp map",
    about = "Project architecture map (modules, deps, entry points)"
)]
pub struct MapArgs {
    /// JSON output
    #[arg(long)]
    pub json: bool,
    /// Compact output (top modules/deps/hot functions only)
    #[arg(long)]
    pub compact: bool,
}

/// Project map — aider repo-map style.
///
/// Output format:
/// ```text
/// src/mcp/server.rs (158KB, 98 symbols)
///   McpServer: handle_tool_call, process_message, flush_metrics
/// ```
pub fn cmd_map(project_root: &Path, args: MapArgs) -> Result<()> {
    let json_mode = args.json;
    let compact = args.compact;

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    let mut map = queries::get_project_map(conn)?;
    // Query-time freshness over the files this answer names — the same shared
    // resync every other read command runs. `map` was never swept when freshness
    // was wired command by command (audit 2026-08-29 CON-03): the architecture-
    // level commands were added before it and never revisited, so an edited file
    // kept its pre-edit hot-function and entry-point rows.
    {
        let mut files: Vec<String> = map.3.iter().map(|h| h.file.clone()).collect();
        files.extend(map.2.iter().map(|e| e.file.clone()));
        let outcome = refresh_files_if_stale(&ctx.db, &ctx.project_root, &files);
        if outcome.any_changed {
            map = queries::get_project_map(conn)?;
        }
        outcome.disclose();
    }
    let (modules, deps, entry_points, hot_functions) = map;

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        // Field names (`caller_count` / `test_caller_count`) and `--compact`
        // cap (top-10) match MCP `project_map`. CLI default returns top-15
        // (the DB LIMIT in get_project_map).
        let hot_cap = if compact { 10 } else { hot_functions.len() };
        let hot_json: Vec<serde_json::Value> = hot_functions
            .iter()
            .take(hot_cap)
            .map(|h| {
                let mut obj = serde_json::json!({
                    "name": h.name,
                    "type": h.node_type,
                    "file": h.file,
                    "caller_count": h.caller_count,
                });
                if h.test_caller_count > 0 {
                    obj["test_caller_count"] = serde_json::json!(h.test_caller_count);
                }
                obj
            })
            .collect();

        let mut result = serde_json::json!({
            "modules": modules.iter().map(|m| serde_json::json!({
                "path": m.path,
                "files": m.files,
                "functions": m.functions,
                "classes": m.classes,
                "interfaces_traits": m.interfaces_traits,
                "constants": m.constants,
                "other": m.other,
                "languages": m.languages,
                "key_symbols": m.key_symbols,
            })).collect::<Vec<_>>(),
            "module_dependencies": deps.iter().map(|d| serde_json::json!({
                "from": d.from,
                "to": d.to,
                "imports": d.import_count,
            })).collect::<Vec<_>>(),
            "entry_points": entry_points.iter().map(|e| serde_json::json!({
                "route": e.route,
                "handler": e.handler,
                "file": e.file,
                "kind": e.kind,
            })).collect::<Vec<_>>(),
            "hot_functions": hot_json,
        });
        // Text mode already prints "... and N more hot functions"; JSON mode cut
        // the same rows with no marker, so a `--compact --json` consumer read the
        // short list as the whole list. Same disclosure, same key names as MCP
        // `project_map` compact.
        if hot_functions.len() > hot_cap {
            result["hot_functions_truncated"] = serde_json::json!(true);
            result["hot_functions_total"] = serde_json::json!(hot_functions.len());
        }
        writeln!(stdout, "{}", serde_json::to_string(&result)?)?;
        return Ok(());
    }

    // Entry points
    if !entry_points.is_empty() {
        writeln!(stdout, "Entry Points:")?;
        for ep in &entry_points {
            writeln!(stdout, "  {} → {} ({})", ep.route, ep.handler, ep.file)?;
        }
        writeln!(stdout)?;
    }

    // Modules
    if modules.is_empty() {
        if entry_points.is_empty() {
            writeln!(stdout, "(empty project — no indexed source files)")?;
        }
        return Ok(());
    }
    writeln!(stdout, "Modules:")?;
    let max_modules = if compact { 15 } else { modules.len() };
    for m in modules.iter().take(max_modules) {
        // Include constants: key_symbols can list exported consts (e.g. a TS
        // `export const db`), so leaving them out of the total made the header
        // claim fewer symbols than the names printed right under it. `other`
        // closes the same hole for every remaining type — a markdown-only module
        // (headings) or a types-only module (TS `type` aliases) reported
        // "0 symbols" here while `overview <path>` listed them.
        let total_symbols = m.functions + m.classes + m.interfaces_traits + m.constants + m.other;
        write!(
            stdout,
            "{} ({}, {}",
            m.path,
            plural(m.files as i64, "file"),
            plural(total_symbols as i64, "symbol")
        )?;
        if !m.languages.is_empty() {
            write!(stdout, ", {}", m.languages.join("/"))?;
        }
        writeln!(stdout, ")")?;
        if !m.key_symbols.is_empty() {
            writeln!(stdout, "  {}", m.key_symbols.join(", "))?;
        }
    }
    if compact && modules.len() > max_modules {
        writeln!(
            stdout,
            "  ... and {} more modules",
            modules.len() - max_modules
        )?;
    }

    // Dependencies (compact: top 10)
    if !deps.is_empty() {
        writeln!(stdout)?;
        writeln!(stdout, "Dependencies:")?;
        let max_deps = if compact { 10 } else { deps.len().min(30) };
        for d in deps.iter().take(max_deps) {
            writeln!(
                stdout,
                "  {} → {} ({} imports)",
                d.from, d.to, d.import_count
            )?;
        }
        // Truncation marker (roadmap 2026-07-18 §1.7): the silent .min(30) cap
        // read as "that's every dependency" — same pattern as the modules cap.
        if deps.len() > max_deps {
            writeln!(
                stdout,
                "  ... and {} more dependencies",
                deps.len() - max_deps
            )?;
        }
    }

    // Hot functions (compact: top 5)
    if !hot_functions.is_empty() {
        writeln!(stdout)?;
        writeln!(stdout, "Hot Functions:")?;
        let max_hot = if compact { 5 } else { hot_functions.len() };
        for h in hot_functions.iter().take(max_hot) {
            if h.test_caller_count > 0 {
                writeln!(
                    stdout,
                    "  {} ({}) — {} + {} test ({})",
                    h.name,
                    h.node_type,
                    plural(h.caller_count as i64, "caller"),
                    h.test_caller_count,
                    h.file
                )?;
            } else {
                writeln!(
                    stdout,
                    "  {} ({}) — {} ({})",
                    h.name,
                    h.node_type,
                    plural(h.caller_count as i64, "caller"),
                    h.file
                )?;
            }
        }
        if hot_functions.len() > max_hot {
            writeln!(
                stdout,
                "  ... and {} more hot functions",
                hot_functions.len() - max_hot
            )?;
        }
    }

    Ok(())
}

// --- tour subcommand ---
