use super::*;

#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp affected",
    about = "Changed files → test files to re-run (+ full blast radius)"
)]
pub struct AffectedArgs {
    /// Changed file paths (relative to project root, or absolute under it)
    pub files: Vec<String>,
    /// Also read newline-separated paths from stdin (e.g. `git diff --name-only | …`)
    #[arg(long)]
    pub stdin: bool,
    /// Max reverse-dependency traversal depth (default: 10; clamped 1..=10)
    #[arg(long, default_value_t = 10)]
    pub depth: i32,
    /// JSON output
    #[arg(long)]
    pub json: bool,
}

/// Reverse-impact: given changed files, list the test files that transitively
/// depend on them (primary) plus the full affected-file set (secondary).
pub fn cmd_affected(project_root: &Path, args: AffectedArgs) -> Result<()> {
    use std::collections::{BTreeMap, HashSet};
    use std::io::Read;

    let depth = args.depth.clamp(1, 10);
    // Disclose the clamp. `callgraph` already warns ("⚠ depth capped to 10
    // (requested 999)") for the identical situation; here `--depth 999` printed
    // "depth <= 10" and `--depth 0` printed "depth <= 1" with nothing to say the
    // requested value had been overridden — a reader checking whether the blast
    // radius really was exhaustive had to know the cap to spot it. stderr, so the
    // `--json` envelope on stdout stays clean.
    if depth != args.depth {
        eprintln!(
            "[code-graph] depth clamped to {} (requested {}) — valid range is 1..=10",
            depth, args.depth
        );
    }

    // 1. Gather raw paths: positional + optional stdin. read_to_end + lossy UTF-8 so a
    //    non-UTF-8 path (legal on Linux) cannot break the --json envelope (F6).
    let mut raw: Vec<String> = args.files.clone();
    if args.stdin {
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf)?;
        raw.extend(
            String::from_utf8_lossy(&buf)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty()),
        );
    }

    // Bare invocation (no positional files AND no --stdin) has no input to work
    // from — the run then prints "0 test file(s) to re-run", indistinguishable
    // from a genuine "nothing is affected" result and easy to misread as "no
    // tests needed" when the real cause is a forgotten argument. `affected` takes
    // an explicit file list by design (it does NOT auto-diff git), so point the
    // user at the intended pipe. Stderr only: stdout keeps its same-shape (empty)
    // output/JSON envelope. Gated on `!args.stdin` so a real empty pipe (clean
    // `git diff`) stays silent — that path used --stdin correctly, just found no
    // changes.
    if args.files.is_empty() && !args.stdin {
        eprintln!(
            "[code-graph] No files given — nothing to analyze. Pass changed files as \
             arguments, or pipe them from git:\n  \
             git diff --name-only HEAD | code-graph-mcp affected --stdin"
        );
    }

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    // 2. Classify each raw input. `changed` holds normalized, INDEXED paths only;
    //    `not_indexed` reports the user's RAW input (one consistent form, F7). Inputs
    //    that normalize to "" (e.g. `.` / project root) are skipped — not a file (F2).
    let mut changed: Vec<String> = Vec::new();
    let mut not_indexed: Vec<String> = Vec::new();
    let mut seen_changed: HashSet<String> = HashSet::new();
    for r in &raw {
        let norm = match normalize_user_path(project_root, r) {
            Ok(p) => p,
            Err(_) => {
                if !not_indexed.contains(r) {
                    not_indexed.push(r.clone());
                }
                continue;
            }
        };
        if norm.is_empty() {
            continue;
        }
        if !queries::file_is_indexed(conn, &norm)? {
            if !not_indexed.contains(r) {
                not_indexed.push(r.clone());
            }
            continue;
        }
        if seen_changed.insert(norm.clone()) {
            changed.push(norm);
        }
    }

    // 3. Union reverse dependents across all changed files over EVERY dependency
    //    relation (imports∪calls∪references∪implements∪inherits, F1), keeping only
    //    language-compatible dependents (F10) and excluding the changed files
    //    themselves from the blast radius (F4).
    let changed_set: HashSet<&str> = changed.iter().map(|s| s.as_str()).collect();
    let mut affected: BTreeMap<String, i32> = BTreeMap::new();
    for f in &changed {
        for (dep_path, dep_depth) in queries::get_reverse_dependents(conn, f, depth)? {
            if !crate::utils::config::is_compatible_lang(f, &dep_path) {
                continue;
            }
            if changed_set.contains(dep_path.as_str()) {
                continue;
            }
            affected
                .entry(dep_path)
                .and_modify(|d| {
                    if dep_depth < *d {
                        *d = dep_depth
                    }
                })
                .or_insert(dep_depth);
        }
    }

    // 4. Primary output: test files among the dependents ∪ changed files that are
    //    themselves tests. `changed` is indexed-only, so a nonexistent test path can no
    //    longer land in both `tests` and `not_indexed` (F3).
    let mut tests: Vec<String> = affected
        .keys()
        .filter(|p| crate::domain::is_test_path(p))
        .cloned()
        .collect();
    for f in &changed {
        if crate::domain::is_test_path(f) && !tests.contains(f) {
            tests.push(f.clone());
        }
    }
    tests.sort();

    // 5. Emit (same-shape JSON on every path — empty included).
    let mut stdout = std::io::stdout().lock();
    if args.json {
        let affected_files: Vec<_> = affected
            .iter()
            .map(|(p, d)| {
                serde_json::json!({
                    "path": p, "depth": d, "is_test": crate::domain::is_test_path(p),
                })
            })
            .collect();
        let result = serde_json::json!({
            "changed": changed,
            "tests": tests,
            "affected_files": affected_files,
            "not_indexed": not_indexed,
        });
        writeln!(stdout, "{}", serde_json::to_string(&result)?)?;
        return Ok(());
    }

    writeln!(
        stdout,
        "Affected by {} changed file(s) — {} test file(s) to re-run:",
        changed.len(),
        tests.len()
    )?;
    for t in &tests {
        writeln!(stdout, "  {}", t)?;
    }
    // Blast radius, grouped by proximity. A flat depth-ordered-by-path dump put
    // the depth-1 dependents a developer would actually inspect in among
    // hundreds of depth-8..10 transitive hits — on a monorepo with a shared core
    // that is "12% of the repo, unranked" and nobody can act on it (issue #36).
    // Grouping + a display cap keeps the actionable head; `--json` is uncapped
    // and ungrouped, so scripted consumers are unaffected.
    const AFFECTED_DISPLAY_CAP: usize = 40;
    let mut by_depth: BTreeMap<i32, Vec<&String>> = BTreeMap::new();
    for (p, d) in &affected {
        by_depth.entry(*d).or_default().push(p);
    }
    let capped = affected.len() > AFFECTED_DISPLAY_CAP;
    writeln!(
        stdout,
        "Full blast radius: {} file(s) (depth <= {}){}",
        affected.len(),
        depth,
        if capped {
            format!(", nearest {} shown", AFFECTED_DISPLAY_CAP)
        } else {
            String::new()
        }
    )?;
    let mut shown = 0usize;
    let mut withheld = 0usize;
    let mut withheld_from_depth: Option<i32> = None;
    for (d, paths) in &by_depth {
        if shown >= AFFECTED_DISPLAY_CAP {
            withheld += paths.len();
            withheld_from_depth.get_or_insert(*d);
            continue;
        }
        // Header counts must describe THIS listing, not the ungrouped total: with
        // 300 files at depth 1 and a cap of 40, a bare `(300 file(s))` sat above
        // 40 paths, and the `… N more` footer below attributes the remainder to
        // the whole `depth X-Y` range rather than to this group. A reader — or a
        // script scraping the header — could not reconcile the two. Show
        // `shown/total` whenever the cap truncates this group.
        let room = AFFECTED_DISPLAY_CAP - shown;
        if room < paths.len() {
            writeln!(
                stdout,
                "  depth {} ({} of {} file(s) shown):",
                d,
                room,
                paths.len()
            )?;
        } else {
            writeln!(stdout, "  depth {} ({} file(s)):", d, paths.len())?;
        }
        for p in paths {
            if shown >= AFFECTED_DISPLAY_CAP {
                withheld += 1;
                withheld_from_depth.get_or_insert(*d);
                continue;
            }
            writeln!(stdout, "    {}", p)?;
            shown += 1;
        }
    }
    if withheld > 0 {
        writeln!(
            stdout,
            "  … {} more at depth {}-{} — narrow with --depth N, or use --json for the full list",
            withheld,
            withheld_from_depth.unwrap_or(depth),
            by_depth.keys().next_back().copied().unwrap_or(depth)
        )?;
    }
    if !not_indexed.is_empty() {
        writeln!(
            stdout,
            "{} input file(s) not in index: {}",
            not_indexed.len(),
            not_indexed.join(", ")
        )?;
    }
    Ok(())
}

// --- map subcommand ---
