use super::*;

/// CLI arguments for the `deps` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(name = "code-graph-mcp deps", about = "File-level dependency graph")]
pub struct DepsArgs {
    /// File whose dependencies to show (absolute paths under root OK)
    pub file: String,
    // --direction stays a String validated in-handler (not a clap ValueEnum) so
    // the exact "must be one of" message + exit 1 are preserved for callers.
    /// Direction: outgoing, incoming, or both
    #[arg(long, default_value = "both")]
    pub direction: String,
    // clamp(1,10) stays in the handler; clap parse-errors (exit 2) on non-numeric.
    /// Max traversal depth
    #[arg(long, default_value_t = 2)]
    pub depth: i32,
    /// JSON output
    #[arg(long)]
    pub json: bool,
    /// Compact output
    #[arg(long)]
    pub compact: bool,
}

/// File-level dependency graph. CLI equivalent of MCP `dependency_graph`.
pub fn cmd_deps(project_root: &Path, args: DepsArgs) -> Result<()> {
    // clap requires the positional (missing → exit 2) but accepts ""; keep the
    // non-empty guard with the exact Usage string.
    let raw_file_path = args.file.as_str();
    if raw_file_path.is_empty() {
        anyhow::bail!("Usage: code-graph-mcp deps <file> [--direction outgoing|incoming|both] [--depth N] [--json]");
    }
    let file_path_owned = normalize_user_path(project_root, raw_file_path)?;
    let file_path = file_path_owned.as_str();

    let direction = crate::domain::normalize_dep_direction(args.direction.as_str())
        .ok_or_else(|| anyhow::anyhow!("--direction must be one of: outgoing, incoming, both"))?;
    let depth: i32 = args.depth.clamp(1, 10);
    let json_mode = args.json;
    let compact = args.compact;

    let ctx = CliContext::open(project_root)?;

    // Query-time freshness on the file the caller named. The MCP twin
    // `dependency_graph` already refreshes it (`advanced.rs`), and this command
    // even accepts `dependency_graph` as an alias — so without this the same
    // question asked two ways gave two answers for an edited file (audit
    // 2026-08-29 CON-03). `IncludeNew`, like the MCP side: an explicit file
    // argument is an assertion that the file matters.
    refresh_input_files(
        &ctx.db,
        &ctx.project_root,
        std::slice::from_ref(&file_path_owned),
    )
    .disclose();

    let conn = ctx.db.conn();

    let deps = queries::get_import_tree(conn, file_path, direction, depth)?;
    if deps.is_empty() {
        // Barrel / index-file fallback — scan source for re-export / import lines.
        // Rust `mod.rs` with only `pub mod X;` has no tracked edges in the graph.
        // ctx.project_root: the barrel scan echoes source lines WITH line
        // numbers, so it must read the same checkout the index describes — the
        // main one when this runs from a linked worktree (FRS-4, sibling of the
        // `show --context-lines` fix).
        if let Some(lines) = scan_barrel_patterns(&ctx.project_root, file_path) {
            let mut stdout = std::io::stdout().lock();
            if json_mode {
                let result = serde_json::json!({
                    "file": file_path,
                    "depends_on": [],
                    "depended_by": [],
                    "barrel_scan": lines.iter().map(|(ln, t)| {
                        serde_json::json!({"line": ln, "text": t.trim()})
                    }).collect::<Vec<_>>(),
                    "note": "no tracked dep edges; barrel_scan is raw re-export/import lines from file scan",
                });
                writeln!(stdout, "{}", serde_json::to_string(&result)?)?;
            } else {
                writeln!(stdout, "{}", file_path)?;
                writeln!(
                    stdout,
                    "  (no tracked dep edges \u{2014} raw re-export/import lines from file scan:)"
                )?;
                for (ln, text) in lines {
                    writeln!(stdout, "    {}: {}", ln, text.trim())?;
                }
            }
            return Ok(());
        }
        // Existence is judged against the checkout the index describes too —
        // otherwise a file that exists only on the worktree's branch is reported
        // as "no tracked dependencies" instead of "not found", and vice versa.
        let abs_path = ctx.project_root.join(file_path);
        let file_exists = abs_path.is_file();
        // A directory reaches here too (get_import_tree finds no file-node, the
        // barrel scan can't read it). Distinguish it from a genuinely missing path
        // so the error points at `overview` instead of the misleading "File not
        // found" (the directory plainly exists).
        let is_dir = !file_exists && abs_path.is_dir();
        if json_mode {
            let result = serde_json::json!({
                "file": file_path,
                "depends_on": [],
                "depended_by": [],
                "error": if file_exists {
                    "No tracked dependencies (not a barrel/import file)"
                } else if is_dir {
                    "Path is a directory (deps analyzes a single file; try overview)"
                } else {
                    "File not found"
                },
            });
            println!("{}", serde_json::to_string(&result)?);
        }
        let msg = if file_exists {
            format!(
                "[code-graph] No tracked dependencies for: {} (not a barrel/import file \u{2014} try `code-graph-mcp overview {}` or Read directly)",
                file_path, file_path
            )
        } else if is_dir {
            format!(
                "[code-graph] {} is a directory \u{2014} `deps` analyzes a single file. Try `code-graph-mcp overview {}` for a directory, or pass a file path.",
                file_path, file_path
            )
        } else {
            format!(
                "[code-graph] File not found: {} (run `code-graph-mcp incremental-index` if you just created it, or check the path)",
                file_path
            )
        };
        if json_mode {
            // The disclosure object above IS this command's JSON answer;
            // exiting through Err would make main's tier-3 catch (audit
            // 2026-08-02 P1-7) print a SECOND error object on stdout.
            eprintln!("{msg}");
            std::process::exit(1);
        }
        anyhow::bail!(msg);
    }

    // Filter out cross-language false edges (name-based resolution artifacts)
    // and the synthetic `<external>` bucket (unresolved imports, not a real file).
    let is_compatible_lang =
        |dep_path: &str| crate::utils::config::is_compatible_lang(file_path, dep_path);

    let outgoing: Vec<&_> = deps
        .iter()
        .filter(|d| d.direction == "outgoing" && is_compatible_lang(&d.file_path))
        .collect();
    let incoming: Vec<&_> = deps
        .iter()
        .filter(|d| d.direction == "incoming" && is_compatible_lang(&d.file_path))
        .collect();

    // Distinguish "no edges at all" (handled by the barrel-fallback branch above)
    // from "edges exist but all targets are <external> or cross-language" — the
    // latter previously rendered as a bare filename with no explanation, which
    // looked like a successful no-op even when the file had unresolved imports.
    let unresolved_outgoing = deps
        .iter()
        .filter(|d| d.direction == "outgoing" && !is_compatible_lang(&d.file_path))
        .count();
    let unresolved_incoming = deps
        .iter()
        .filter(|d| d.direction == "incoming" && !is_compatible_lang(&d.file_path))
        .count();

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        let mut result = serde_json::json!({
            "file": file_path,
            "depends_on": outgoing.iter().map(|d| {
                let mut obj = serde_json::json!({"file": d.file_path, "depth": d.depth});
                if !compact && d.depth == 1 { obj["symbols"] = serde_json::json!(d.symbol_count); }
                obj
            }).collect::<Vec<_>>(),
            "depended_by": incoming.iter().map(|d| {
                let mut obj = serde_json::json!({"file": d.file_path, "depth": d.depth});
                if !compact && d.depth == 1 { obj["symbols"] = serde_json::json!(d.symbol_count); }
                obj
            }).collect::<Vec<_>>(),
        });
        if unresolved_outgoing > 0 {
            result["unresolved_outgoing"] = serde_json::json!(unresolved_outgoing);
        }
        if unresolved_incoming > 0 {
            result["unresolved_incoming"] = serde_json::json!(unresolved_incoming);
        }
        writeln!(stdout, "{}", serde_json::to_string(&result)?)?;
        return Ok(());
    }

    writeln!(stdout, "{}", file_path)?;
    if !outgoing.is_empty() {
        writeln!(stdout, "  Depends on:")?;
        for d in &outgoing {
            if compact {
                writeln!(stdout, "    {}", d.file_path)?;
            } else if d.depth == 1 {
                writeln!(
                    stdout,
                    "    {} ({})",
                    d.file_path,
                    plural(d.symbol_count, "symbol")
                )?;
            } else {
                writeln!(stdout, "    {} (depth {})", d.file_path, d.depth)?;
            }
        }
    }
    if !incoming.is_empty() {
        writeln!(stdout, "  Depended by:")?;
        for d in &incoming {
            if compact {
                writeln!(stdout, "    {}", d.file_path)?;
            } else if d.depth == 1 {
                writeln!(
                    stdout,
                    "    {} ({})",
                    d.file_path,
                    plural(d.symbol_count, "symbol")
                )?;
            } else {
                writeln!(stdout, "    {} (depth {})", d.file_path, d.depth)?;
            }
        }
    }
    if outgoing.is_empty()
        && incoming.is_empty()
        && (unresolved_outgoing > 0 || unresolved_incoming > 0)
    {
        writeln!(
            stdout,
            "  (no resolved deps; {} unresolved outgoing, {} unresolved incoming — targets are <external> or in another language)",
            unresolved_outgoing, unresolved_incoming
        )?;
    }

    Ok(())
}

// --- similar subcommand ---
