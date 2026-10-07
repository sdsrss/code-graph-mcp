use super::*;

/// CLI arguments for the `overview` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp overview",
    about = "Module overview (symbols grouped by file and type)"
)]
pub struct OverviewArgs {
    /// Directory or file to scan ('.' = whole project; absolute paths under root OK)
    pub path: String,
    /// JSON output
    #[arg(long)]
    pub json: bool,
    /// Compact output (no caller counts)
    #[arg(long)]
    pub compact: bool,
    /// Token budget for the text answer (bytes/3, 100-100000): symbols with
    /// the fewest callers lose their signature first (a single file), files
    /// with the fewest callers shrink to a count first (a directory), then the
    /// lowest-ranked are left out; one file gets at most 70% of the budget;
    /// ends with a command that prints the rest
    #[arg(long, conflicts_with_all = ["json", "compact"])]
    pub budget: Option<u64>,
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

    // How many symbols the per-file export rule withheld. Zero for
    // Python/Rust/Go/CommonJS trees; non-zero only for ESM files with private
    // helpers, where the unannotated output ("routes.js / function:
    // authenticateSession" for a file holding four functions) reads as the whole
    // file. `overview`'s own doc calls itself a replacement for Read on a large
    // file, which is a promise the silent version could not keep.
    //
    // The query applies `is_test_node_sql`, the SQL mirror of the very
    // `is_test_symbol` call `run_query` uses on the visible half, so both halves
    // are filtered by one rule (parity pinned by `test_is_test_node_sql_matches_rust`).
    //
    // An Err is NOT folded into 0: "the query failed" and "nothing was withheld"
    // are different facts, and collapsing them is the silent-absence class this
    // release exists to close. Same shape as the `*_unavailable` fields.
    let hidden_result = queries::count_export_filtered_out(conn, path_prefix);
    // One sentence for all three output arms: the count when something was
    // withheld, the failure when the count could not be taken, nothing when the
    // export rule narrowed nothing (the Python/Rust/Go case — a note there would
    // be noise, and false).
    let disclosure: Option<String> = match &hidden_result {
        Ok(0) => None,
        Ok(n) => Some(queries::export_filter_note(*n)),
        Err(e) => Some(format!(
            "not-exported symbol count unavailable ({e}) — this listing may be \
             narrower than the files it names"
        )),
    };

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
        // stderr, NOT a wrapper object: this command's `--json` contract is a bare
        // array and every consumer indexes it directly, so switching shape when a
        // repo happens to contain ESM would break them on a subset of inputs —
        // a worse failure than the one being disclosed. Same split `clamp_arg`
        // and `affected` use.
        if let Some(msg) = &disclosure {
            eprintln!("[code-graph] {}", msg);
        }
        return Ok(());
    }

    // Group by file
    let mut by_file: std::collections::BTreeMap<&str, Vec<&queries::ModuleExport>> =
        std::collections::BTreeMap::new();
    for e in &exports {
        by_file.entry(&e.file_path).or_default().push(e);
    }

    if let Some(requested) = args.budget {
        let tokens = clamp_arg(
            "--budget",
            requested,
            crate::budget::MIN_BUDGET_TOKENS,
            crate::budget::MAX_BUDGET_TOKENS,
        ) as usize;
        let next = crate::budget::NextCommand::new("overview").path(raw_path);
        let text = overview_budget_text(&by_file, disclosure.as_deref(), tokens, &next);
        write!(stdout, "{}", text)?;
        return Ok(());
    }

    // Single-file path → outline format (sorted by line, signature + line range visible).
    // Replaces Read on huge files: a 3000+ line source emits ~symbol-count lines instead.
    if by_file.len() == 1 {
        let (file, symbols) = by_file.iter().next().unwrap();
        writeln!(stdout, "{}", file)?;
        let mut sorted: Vec<&queries::ModuleExport> = symbols.to_vec();
        sorted.sort_by_key(|e| e.start_line);
        for s in sorted {
            writeln!(stdout, "{}", outline_line(s, !compact))?;
        }
        if let Some(msg) = &disclosure {
            writeln!(stdout, "  ({})", msg)?;
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
            let names: Vec<String> = syms.iter().map(|s| listed_name(s, compact)).collect();
            writeln!(stdout, "  {}: {}", typ, names.join(", "))?;
        }
    }
    if let Some(msg) = &disclosure {
        writeln!(stdout, "({})", msg)?;
    }

    Ok(())
}

/// One symbol of the single-file outline: `  L{start}-{end}  {type}  {name}
/// ({callers}×)  {signature}`; `with_signature: false` is the compact line.
fn outline_line(s: &queries::ModuleExport, with_signature: bool) -> String {
    let callers = if s.caller_count > 0 {
        format!(" ({}×)", s.caller_count)
    } else {
        String::new()
    };
    let sig = if with_signature {
        s.signature.as_deref().unwrap_or("")
    } else {
        ""
    };
    let sig_display = if sig.is_empty() {
        String::new()
    } else {
        format!("  {}", sig.lines().next().unwrap_or("").trim())
    };
    format!(
        "  L{}-{}  {}  {}{}{}",
        s.start_line,
        s.end_line,
        s.node_type,
        s.display_name(),
        callers,
        sig_display
    )
}

/// One name in a directory listing's `  type: a (3×), b` line.
fn listed_name(s: &queries::ModuleExport, compact: bool) -> String {
    if compact || s.caller_count <= 0 {
        s.display_name().to_string()
    } else {
        format!("{} ({}×)", s.display_name(), s.caller_count)
    }
}

/// `overview --budget`: the text answer fitted to `tokens`.
///
/// A single file: one unit per symbol, ranked by caller count; the shorter
/// form drops the signature. A directory: one unit per file, ranked by the sum
/// of its symbols' caller counts; the shorter form is `path (N symbols)`.
/// Where files are the unit, one file's block is first held to
/// [`crate::budget::FILE_SHARE_PERCENT`] of the budget by leaving out its
/// lowest-ranked names.
pub(crate) fn overview_budget_text(
    by_file: &std::collections::BTreeMap<&str, Vec<&queries::ModuleExport>>,
    disclosure: Option<&str>,
    tokens: usize,
    next: &crate::budget::NextCommand,
) -> String {
    use crate::budget::{self, Level};
    use std::cmp::Reverse;
    let budget_b = budget::budget_bytes(tokens);
    let disclosure_line = |indent: &str| disclosure.map(|m| format!("{indent}({m})\n"));

    if by_file.len() == 1 {
        let (file, symbols) = by_file.iter().next().unwrap();
        let mut sorted: Vec<&queries::ModuleExport> = symbols.to_vec();
        sorted.sort_by_key(|e| e.start_line);
        let n = sorted.len();
        let order = budget::order_by_importance(n, |i| (sorted[i].caller_count, Reverse(i)));
        let has_sig = |i: usize| {
            sorted[i]
                .signature
                .as_deref()
                .is_some_and(|s| !s.is_empty())
        };
        let steps = budget::standard_steps(&order, has_sig);
        let render = |levels: &[Level]| -> String {
            let mut out = format!("{file}\n");
            for (i, s) in sorted.iter().enumerate() {
                match levels[i] {
                    Level::Full => out.push_str(&outline_line(s, true)),
                    Level::Skeleton => out.push_str(&outline_line(s, false)),
                    Level::Dropped => continue,
                }
                out.push('\n');
            }
            if let Some(d) = disclosure_line("  ") {
                out.push_str(&d);
            }
            let dropped = levels.iter().filter(|l| **l == Level::Dropped).count();
            let skel = levels.iter().filter(|l| **l == Level::Skeleton).count();
            if let Some(n) = budget::notice(
                "  ",
                tokens,
                &[
                    (dropped, "symbol omitted", "symbols omitted"),
                    (skel, "without signature", "without signature"),
                ],
            ) {
                out.push_str(&format!("{n}\n  next: {next}\n"));
            }
            out
        };
        return budget::fit(&vec![Level::Full; n], &steps, budget_b, |l| {
            let s = render(l);
            let len = s.len();
            (s, len)
        })
        .output;
    }

    // Directory: files are the unit.
    let files: Vec<(&str, &Vec<&queries::ModuleExport>)> =
        by_file.iter().map(|(f, v)| (*f, v)).collect();
    // Per file, the names kept after the 70% share (in rank order, most first).
    let share = budget_b * budget::FILE_SHARE_PERCENT / 100;
    let file_block = |file: &str, syms: &[&queries::ModuleExport], keep: &[bool]| -> String {
        let mut by_type: std::collections::BTreeMap<&str, Vec<String>> =
            std::collections::BTreeMap::new();
        for (i, s) in syms.iter().enumerate() {
            if keep[i] {
                by_type
                    .entry(&s.node_type)
                    .or_default()
                    .push(listed_name(s, false));
            }
        }
        let mut out = format!("{file}\n");
        for (typ, names) in &by_type {
            out.push_str(&format!("  {}: {}\n", typ, names.join(", ")));
        }
        let cut = keep.iter().filter(|k| !**k).count();
        if cut > 0 {
            out.push_str(&format!("  (+{cut} lower-ranked symbols not shown)\n"));
        }
        out
    };
    let mut kept_names: Vec<Vec<bool>> = Vec::with_capacity(files.len());
    for (file, syms) in &files {
        let m = syms.len();
        let mut levels = vec![Level::Full; m];
        let order = budget::order_by_importance(m, |i| (syms[i].caller_count, Reverse(i)));
        // Additive per name: the name plus its ", " separator.
        let cost = |i: usize, l: Level| {
            if l == Level::Full {
                listed_name(syms[i], false).len() + 2
            } else {
                0
            }
        };
        let head = file.len() + 64;
        budget::cap_group(
            &mut levels,
            &order,
            |_| false,
            cost,
            share.saturating_sub(head),
        );
        let keep: Vec<bool> = levels.iter().map(|l| *l == Level::Full).collect();
        kept_names.push(keep);
    }
    let blocks: Vec<String> = files
        .iter()
        .zip(&kept_names)
        .map(|((f, syms), keep)| file_block(f, syms, keep))
        .collect();
    let n = files.len();
    let order = budget::order_by_importance(n, |i| {
        let callers: i64 = files[i].1.iter().map(|s| s.caller_count.max(0)).sum();
        (callers, files[i].1.len(), Reverse(i))
    });
    let steps = budget::standard_steps(&order, |_| true);
    let render = |levels: &[Level]| -> String {
        let mut out = String::new();
        for (i, (file, syms)) in files.iter().enumerate() {
            match levels[i] {
                Level::Full => out.push_str(&blocks[i]),
                Level::Skeleton => out.push_str(&format!(
                    "{file} ({})\n",
                    plural(syms.len() as i64, "symbol")
                )),
                Level::Dropped => {}
            }
        }
        if let Some(d) = disclosure_line("") {
            out.push_str(&d);
        }
        let count = |lv: Level| levels.iter().filter(|l| **l == lv).count();
        let cut_in_full: usize = (0..n)
            .filter(|&i| levels[i] == Level::Full)
            .map(|i| kept_names[i].iter().filter(|k| !**k).count())
            .sum();
        if let Some(line) = budget::notice(
            "",
            tokens,
            &[
                (count(Level::Dropped), "file omitted", "files omitted"),
                (
                    count(Level::Skeleton),
                    "file as a count only",
                    "files as a count only",
                ),
                (
                    cut_in_full,
                    "symbol past its file's 70% share",
                    "symbols past their file's 70% share",
                ),
            ],
        ) {
            out.push_str(&format!("{line}\nnext: {next}\n"));
        }
        out
    };
    budget::fit(&vec![Level::Full; n], &steps, budget_b, |l| {
        let s = render(l);
        let len = s.len();
        (s, len)
    })
    .output
}

// --- show subcommand ---
