use super::*;

/// CLI arguments for the `dead-code` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp dead-code",
    about = "Find unused code (orphans and exported-unused symbols)"
)]
pub struct DeadCodeArgs {
    /// Restrict the scan to this path prefix (absolute paths under root OK)
    pub path: Option<String>,
    // --node-type is preferred (matches `search` CLI + MCP param); --type is the
    // legacy alias. clap accepts any string here — the handler validates it via
    // normalize_type_filter so a typo errors loudly instead of false-clean exit 0.
    // --node-type and --type are ONE arg (alias), so supplying both is a clap
    // duplicate-arg error (exit 2) — deliberately stricter than the old parser,
    // which silently honored --node-type and ignored --type (masking a bad --type).
    #[arg(long = "node-type", alias = "type",
          help = crate::domain::TYPE_FILTER_HELP_ARG)]
    pub node_type: Option<String>,
    /// Show test callers (hidden by default)
    #[arg(long)]
    pub include_tests: bool,
    // clap parse-errors (exit 2) on a non-numeric value, replacing the hand
    // parser's warn-and-fallback — consistent with `stats --last` under flavor B.
    /// Minimum lines to report
    #[arg(long, default_value_t = 3)]
    pub min_lines: u32,
    /// Show full code snippets (default: compact, names only)
    #[arg(long)]
    pub no_compact: bool,
    /// Exclude a path prefix (repeatable; default: claude-plugin/, benches/)
    #[arg(long)]
    pub ignore: Vec<String>,
    /// Disable the default --ignore prefixes
    #[arg(long)]
    pub no_ignore: bool,
    /// JSON output
    #[arg(long)]
    pub json: bool,
}

/// Find dead code: orphans and exported-unused symbols.
/// CLI equivalent of MCP `find_dead_code`.
pub fn cmd_dead_code(project_root: &Path, args: DeadCodeArgs) -> Result<()> {
    let DeadCodeArgs {
        path,
        node_type,
        include_tests,
        min_lines,
        no_compact,
        ignore,
        no_ignore,
        json: json_mode,
    } = args;

    let path_filter_owned: Option<String> = match path.as_deref() {
        Some(p) => Some(normalize_user_path(project_root, p)?),
        None => None,
    };
    let path_filter = path_filter_owned.as_deref();
    // --node-type (preferred) and its --type alias both land in `node_type`.
    let type_filter = node_type.as_deref();
    // Validate --type/--node-type up-front: an unknown alias normalizes to an
    // empty Vec, and find_dead_code then falls through to a literal `n.type = :x`
    // match that returns zero rows — so a typo'd `--type fucntion` prints a
    // false-clean "No dead code found" with exit 0. Mirror the cmd_ast_search guard.
    queries::validate_dead_code_type_filter(type_filter)?;
    let compact = !no_compact;

    // --ignore <pref>: repeatable, prefix-match exclusion. --no-ignore disables defaults.
    // Defaults are owned by `domain::default_dead_code_ignores()` (claude-plugin/, benches/).
    // Separator-normalized like every other path argument: these are matched with
    // `starts_with` against `/`-stored paths, so a Windows user's
    // `--ignore src\generated` would exclude nothing and silently over-report.
    // Not routed through `normalize_user_path` — a PREFIX is not required to name
    // an existing file, and the escape check would reject legitimate ones.
    let ignore_prefixes: Vec<String> = if no_ignore {
        Vec::new()
    } else if ignore.is_empty() {
        crate::domain::default_dead_code_ignores()
    } else {
        ignore
            .iter()
            .map(|p| crate::indexer::merkle::normalize_rel_str(p))
            .collect()
    };

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();
    let run_query = |conn: &rusqlite::Connection| -> Result<queries::DeadCodeReport> {
        queries::dead_code_report(
            conn,
            path_filter,
            type_filter,
            include_tests,
            min_lines,
            &ignore_prefixes,
        )
    };
    let mut report = run_query(conn)?;
    // Query-time freshness (shared resync with show/refs/…): re-index any displayed
    // candidate's file edited since indexing so its start_line/end_line are post-edit,
    // then re-run against the refreshed index.
    let files: Vec<String> = report.items.iter().map(|it| it.file_path.clone()).collect();
    let outcome = refresh_files_if_stale(&ctx.db, &ctx.project_root, &files);
    if outcome.any_changed {
        report = run_query(conn)?;
    }
    outcome.disclose();

    if report.is_empty() {
        // Empty-but-something-hidden discloses IN-BAND (stdout/JSON), not only
        // stderr: under `--json 2>/dev/null` a bare `[]` reads as "clean" even
        // when --ignore/--min-lines hid real candidates (disclosure-gap class,
        // roadmap 2026-07-18 §1.2). True clean keeps the plain `[]`.
        // A path filter that matches NO indexed file is zero coverage, not a
        // clean bill of health, and it is the one empty case `dead-code` still
        // reported as `[]` + exit 0. `overview` answers the same input with an
        // error object + exit 1 (:5641), and `normalize_user_path`'s own doc
        // names this failure mode — a path can be in-root and well-formed while
        // naming nothing indexed, so normalization cannot catch it. Under
        // `--json 2>/dev/null` the old answer was indistinguishable from "this
        // directory genuinely has no dead code".
        //
        // The probe itself (incl. the `.` / trailing-slash spellings that must
        // NOT count as a miss) now lives in `queries::unindexed_path_prefix`,
        // shared with MCP `tool_find_dead_code` — which had no probe at all and
        // answered the same input with a clean report (audit 2026-08-16 P1-22).
        if let Some(prefix) = queries::unindexed_path_prefix(conn, path_filter) {
            if json_mode {
                println!(
                    "{}",
                    serde_json::json!({ "error": "No indexed files under path", "path": prefix })
                );
                eprintln!("[code-graph] No indexed files under: {prefix}");
                std::process::exit(1);
            }
            anyhow::bail!("[code-graph] No indexed files under: {prefix}");
        }

        if report.ignored_count > 0 {
            if json_mode {
                println!(
                    "{}",
                    serde_json::json!({
                        "results": [],
                        "ignored_count": report.ignored_count,
                    })
                );
            } else {
                println!(
                    "[code-graph] No dead code found after filtering; {} suppressed by --ignore (use --no-ignore to see them).",
                    report.ignored_count,
                );
            }
            eprintln!(
                "[code-graph] No dead code found after filtering; {} suppressed by --ignore (use --no-ignore to see them).",
                report.ignored_count,
            );
        } else if report.hidden_below_threshold > 0 {
            if json_mode {
                println!(
                    "{}",
                    serde_json::json!({
                        "results": [],
                        "below_threshold_count": report.hidden_below_threshold,
                        "min_lines": min_lines,
                    })
                );
            } else {
                println!(
                    "[code-graph] No dead code found at \u{2265}{min_lines} lines ({} shorter symbol(s) below the threshold; rerun with --min-lines 1 to include them).",
                    report.hidden_below_threshold
                );
            }
            eprintln!(
                "[code-graph] No dead code found at \u{2265}{min_lines} lines ({} shorter symbol(s) below the threshold; rerun with --min-lines 1 to include them).",
                report.hidden_below_threshold
            );
        } else {
            if json_mode {
                writeln!(std::io::stdout().lock(), "[]")?;
            }
            eprintln!("[code-graph] No dead code found.");
        }
        return Ok(());
    }

    let mut stdout = std::io::stdout().lock();

    if json_mode {
        let items: Vec<serde_json::Value> = report
            .items
            .iter()
            .map(|it| {
                let mut obj = serde_json::json!({
                    "name": it.name,
                    "type": it.node_type,
                    "file_path": it.file_path,
                    "start_line": it.start_line,
                    "end_line": it.end_line,
                    "category": if it.is_exported { "exported_unused" } else { "orphan" },
                    "lines": it.end_line - it.start_line + 1,
                });
                if !compact {
                    obj["code"] = serde_json::json!(it.code_content);
                }
                obj
            })
            .collect();
        writeln!(stdout, "{}", serde_json::to_string(&items)?)?;
        return Ok(());
    }

    writeln!(
        stdout,
        "Dead code: {} candidates ({} orphan, {} exported-unused)",
        report.items.len(),
        report.orphan_count,
        report.exported_count
    )?;
    writeln!(stdout, "(candidates to verify — receiver-method calls (obj.method()) and cross-file const/type uses are not edge-tracked)")?;
    // The `hidden_below_threshold` probe only runs when NOTHING is visible, so a
    // non-empty report never disclosed that the default cut hides every symbol
    // shorter than `--min-lines`. A one-line `export function f() { return 42 }`
    // is exactly the shape of dead code a user wants listed, and it was silently
    // absent from a report that read as complete. Naming the active threshold
    // costs no query.
    if min_lines > 1 {
        writeln!(
            stdout,
            "(showing symbols \u{2265}{min_lines} lines — pass --min-lines 1 to include shorter ones)"
        )?;
    }
    writeln!(stdout)?;

    let (orphans, exported_unused): (Vec<_>, Vec<_>) =
        report.items.iter().partition(|it| !it.is_exported);

    if !orphans.is_empty() {
        writeln!(
            stdout,
            "ORPHAN ({}) — no tracked references, not exported",
            orphans.len()
        )?;
        for it in &orphans {
            let lines = it.end_line - it.start_line + 1;
            writeln!(
                stdout,
                "  {} {} {}:{} ({})",
                it.node_type,
                it.name,
                it.file_path,
                it.start_line,
                plural(lines, "line")
            )?;
            if !compact {
                for line in it.code_content.lines().take(5) {
                    writeln!(stdout, "    {}", line)?;
                }
                if it.code_content.lines().count() > 5 {
                    writeln!(stdout, "    ...")?;
                }
            }
        }
    }

    if !exported_unused.is_empty() {
        if !orphans.is_empty() {
            writeln!(stdout)?;
        }
        writeln!(
            stdout,
            "EXPORTED-UNUSED ({}) — exported/public, no tracked callers",
            exported_unused.len()
        )?;
        for it in &exported_unused {
            let lines = it.end_line - it.start_line + 1;
            writeln!(
                stdout,
                "  {} {} {}:{} ({})",
                it.node_type,
                it.name,
                it.file_path,
                it.start_line,
                plural(lines, "line")
            )?;
            if !compact {
                for line in it.code_content.lines().take(5) {
                    writeln!(stdout, "    {}", line)?;
                }
                if it.code_content.lines().count() > 5 {
                    writeln!(stdout, "    ...")?;
                }
            }
        }
    }

    Ok(())
}

// --- centrality subcommand ---
