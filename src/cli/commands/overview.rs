use super::*;

/// CLI arguments for the `overview` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp overview",
    about = "Module overview (symbols grouped by file and type)"
)]
pub struct OverviewArgs {
    /// Path prefix to scan ('.' = whole project; absolute paths under root OK)
    pub path: String,
    /// JSON output
    #[arg(long)]
    pub json: bool,
    /// Compact output (no caller counts)
    #[arg(long)]
    pub compact: bool,
}

/// Module overview: all symbols in files under a path prefix.
pub fn cmd_overview(project_root: &Path, args: OverviewArgs) -> Result<()> {
    // clap requires the positional (missing → exit 2), but accepts an empty
    // string; preserve the empty-path guard below for unset-shell-var `overview "$X"`.
    let raw_path = args.path.as_str();
    // Reject empty-string path: mirrors MCP `tool_module_overview` (script users
    // hit this when a shell variable is unset and overview "$X" expands to "").
    if raw_path.is_empty() {
        anyhow::bail!("path must not be empty — use '.' to scan the whole project root");
    }
    // Normalize: strip leading "./", treat bare "." as empty prefix, and resolve
    // absolute paths under the project root to their relative portion. Mirrors MCP
    // `tool_module_overview` for "./"/"." and additionally supports paste-from-IDE
    // absolute paths (the indexed `file_path` column is project-relative, so
    // unnormalized absolute paths returned "No symbols found").
    let path_prefix_owned = normalize_user_path(project_root, raw_path)?;
    let path_prefix = path_prefix_owned.as_str();

    let json_mode = args.json;
    let compact = args.compact;

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    // Filter out test symbols (align with MCP module_overview behavior).
    let run_query = |conn: &rusqlite::Connection| -> Result<Vec<queries::ModuleExport>> {
        Ok(queries::get_module_exports(conn, path_prefix)?
            .into_iter()
            .filter(|e| !crate::domain::is_test_symbol(&e.name, &e.file_path))
            .collect())
    };
    let mut exports = run_query(conn)?;
    // Query-time freshness (shared resync with show/refs/…): re-index any displayed
    // file edited since indexing so the printed L{start}-{end} ranges are post-edit,
    // then re-run the query against the refreshed index.
    let files: Vec<String> = exports.iter().map(|e| e.file_path.clone()).collect();
    let outcome = refresh_files_if_stale(&ctx.db, &ctx.project_root, &files);
    if outcome.any_changed {
        exports = run_query(conn)?;
    }
    outcome.disclose();

    if exports.is_empty() {
        // JSON empty-result contract (feedback_cli_json_empty_contract):
        // stdout must always be valid JSON. Use a clean eprintln + exit 1
        // instead of `anyhow::bail!` so the JSON-mode stderr doesn't carry
        // the anyhow `Error:` prefix that confuses log consumers.
        if json_mode {
            // In-band error object (roadmap 2026-07-18 §1.3): a bare `[]` under
            // `2>/dev/null` is indistinguishable from an empty-but-indexed dir.
            println!(
                "{}",
                serde_json::json!({
                    "error": "No symbols found", "path": raw_path,
                })
            );
            eprintln!("[code-graph] No symbols found under: {}", raw_path);
            std::process::exit(1);
        }
        anyhow::bail!("[code-graph] No symbols found under: {}", raw_path);
    }

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        // `caller_count` matches MCP `module_overview.active_exports[].caller_count`.
        let results: Vec<serde_json::Value> = exports
            .iter()
            .map(|e| {
                let mut obj = serde_json::json!({
                    "name": e.name,
                    "type": e.node_type,
                    "file": e.file_path,
                    "signature": e.signature,
                    "caller_count": e.caller_count,
                    "start_line": e.start_line,
                    "end_line": e.end_line,
                });
                // Disambiguate same-named methods of different classes (parity with
                // MCP module_overview active_exports). Present only when it adds info.
                if e.qualified_name != e.name {
                    obj["qualified_name"] = serde_json::json!(e.qualified_name);
                }
                obj
            })
            .collect();
        writeln!(stdout, "{}", serde_json::to_string(&results)?)?;
        return Ok(());
    }

    // Group by file
    let mut by_file: std::collections::BTreeMap<&str, Vec<&queries::ModuleExport>> =
        std::collections::BTreeMap::new();
    for e in &exports {
        by_file.entry(&e.file_path).or_default().push(e);
    }

    // Single-file path → outline format (sorted by line, signature + line range visible).
    // Replaces Read on huge files: a 3000+ line source emits ~symbol-count lines instead.
    if by_file.len() == 1 {
        let (file, symbols) = by_file.iter().next().unwrap();
        writeln!(stdout, "{}", file)?;
        let mut sorted: Vec<&queries::ModuleExport> = symbols.to_vec();
        sorted.sort_by_key(|e| e.start_line);
        for s in sorted {
            let callers = if s.caller_count > 0 {
                format!(" ({}×)", s.caller_count)
            } else {
                String::new()
            };
            if compact {
                writeln!(
                    stdout,
                    "  L{}-{}  {}  {}{}",
                    s.start_line,
                    s.end_line,
                    s.node_type,
                    s.display_name(),
                    callers
                )?;
            } else {
                let sig = s.signature.as_deref().unwrap_or("");
                let sig_display = if sig.is_empty() {
                    String::new()
                } else {
                    format!("  {}", sig.lines().next().unwrap_or("").trim())
                };
                writeln!(
                    stdout,
                    "  L{}-{}  {}  {}{}{}",
                    s.start_line,
                    s.end_line,
                    s.node_type,
                    s.display_name(),
                    callers,
                    sig_display
                )?;
            }
        }
        return Ok(());
    }

    for (file, symbols) in &by_file {
        writeln!(stdout, "{}", file)?;
        // Group by type within file
        let mut by_type: std::collections::BTreeMap<&str, Vec<&&queries::ModuleExport>> =
            std::collections::BTreeMap::new();
        for s in symbols {
            by_type.entry(&s.node_type).or_default().push(s);
        }
        for (typ, syms) in &by_type {
            let names: Vec<String> = syms
                .iter()
                .map(|s| {
                    if compact {
                        s.display_name().to_string()
                    } else if s.caller_count > 0 {
                        format!("{} ({}×)", s.display_name(), s.caller_count)
                    } else {
                        s.display_name().to_string()
                    }
                })
                .collect();
            writeln!(stdout, "  {}: {}", typ, names.join(", "))?;
        }
    }

    Ok(())
}

// --- show subcommand ---
