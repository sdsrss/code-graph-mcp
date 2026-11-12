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

    // Clamp + disclose in one call (`clamp_arg`). `--depth 999` printed
    // "depth <= 10" and `--depth 0` printed "depth <= 1" with nothing to say the
    // requested value had been overridden — a reader checking whether the blast
    // radius really was exhaustive had to know the cap to spot it.
    let depth = clamp_arg("--depth", args.depth, 1, 10);

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

    // Query-time freshness BEFORE the `file_is_indexed` gate below, and with
    // `IncludeNew` scope (see `refresh_input_files`): these are paths the caller
    // named, typically straight out of `git diff --name-only`, so a file the
    // branch just added is the normal case rather than an unknown path a query
    // happened to produce. Without this the new file failed the gate, dropped
    // out of the reverse closure, and the command printed "0 test file(s) to
    // re-run" — a false empty in the one output a CI hook acts on (audit
    // 2026-08-29 CON-03). Nonexistent paths are unaffected: they plan as
    // `Fresh`, so they still reach `not_indexed` and are still reported.
    //
    // Paths that no longer EXIST are excluded on purpose. `git diff --name-only`
    // lists deletions, and for a deleted file the refresh would plan
    // `DropStaleRow` — removing the very rows this command then reads, so the
    // file failed the `file_is_indexed` gate below and its dependents vanished
    // from the answer. That is the worst case to lose: the callers of a file
    // that no longer exists are exactly the ones that will break. A read command
    // has no business retiring a deletion from the graph anyway; the next real
    // index run does that. (Caught by the v0.127.0 pre-tag review — this
    // regression arrived with the refresh itself.)
    //
    // Each input is normalized once, so a near-miss rebase notes it once.
    let normalized: Vec<Result<String>> = raw
        .iter()
        .map(|r| normalize_user_path(project_root, r))
        .collect();
    let fresh_inputs: Vec<String> = normalized
        .iter()
        .filter_map(|n| n.as_ref().ok())
        .filter(|p| !p.is_empty())
        .filter(|p| project_root.join(p).exists())
        .cloned()
        .collect();
    let freshness = refresh_input_files(&ctx.db, &ctx.project_root, &fresh_inputs);
    freshness.disclose();

    // The guard above does not outlive the next index run: the hooks and the MCP
    // watcher run one after an edit, and another query's refresh drops a
    // deleted file too, taking the edges into it. What depended on it is then
    // the record the dropping run kept (`META_KEY_DELETED_DEPENDENTS`).
    let deleted_record: Vec<(String, Vec<String>)> =
        queries::get_meta(conn, crate::storage::schema::META_KEY_DELETED_DEPENDENTS)?
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default();
    let mut dropped: HashMap<String, Vec<String>> = HashMap::new();
    let known = |path: &str| -> Result<bool> {
        Ok(queries::file_is_indexed(conn, path)? || deleted_record.iter().any(|(p, _)| p == path))
    };

    for (r, norm) in raw.iter().zip(normalized) {
        let mut norm = match norm {
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
        // `git diff --name-only` prints root-relative paths from any directory,
        // and the near-miss rebase takes the root reading only for a file that
        // exists: a deleted one exists under neither reading. The index knows
        // which one it held.
        if !is_cwd_anchored(r) && !known(&norm)? {
            if let Ok(root_reading) = normalize_user_path_from(project_root, project_root, r) {
                if known(&root_reading)? {
                    note_root_rebase(r, project_root);
                    norm = root_reading;
                }
            }
        }
        if !queries::file_is_indexed(conn, &norm)? {
            if let Some((_, dependents)) = deleted_record.iter().find(|(path, _)| *path == norm) {
                if seen_changed.insert(norm.clone()) {
                    dropped.insert(norm.clone(), dependents.clone());
                    changed.push(norm);
                }
                continue;
            }
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
        // A dropped file's recorded dependents are its depth 1; the closure goes
        // on from those the index still holds.
        let dependents = match dropped.get(f) {
            None => queries::get_reverse_dependents(conn, f, depth)?,
            Some(recorded) => {
                let mut found = Vec::new();
                for dep in recorded {
                    if !queries::file_is_indexed(conn, dep)? {
                        continue;
                    }
                    found.push((dep.clone(), 1));
                    if depth > 1 {
                        for (p, d) in queries::get_reverse_dependents(conn, dep, depth - 1)? {
                            found.push((p, d + 1));
                        }
                    }
                }
                found
            }
        };
        for (dep_path, dep_depth) in dependents {
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
    //
    //    C/C++ has no test file naming `is_test_path` can know (`*_test.cc`,
    //    `*_unittest.cc`, `test_*.cpp` all occur), but the parser marks its gtest
    //    cases `is_test`. Without this leg every leveldb change answered "0 test
    //    file(s) to re-run" (E2E 2026-10-16). Scoped to C/C++ on purpose: in Rust
    //    the same flag marks inline `#[cfg(test)]` modules, which would list most
    //    of `src/` as test files.
    let is_test_file = |p: &str| -> Result<bool> {
        if crate::domain::is_test_path(p) {
            return Ok(true);
        }
        if matches!(crate::utils::config::detect_language(p), Some("c" | "cpp")) {
            return queries::file_has_test_node(conn, p);
        }
        Ok(false)
    };
    let mut test_flags: HashMap<String, bool> = HashMap::new();
    for p in affected.keys().chain(changed.iter()) {
        if !test_flags.contains_key(p) {
            test_flags.insert(p.clone(), is_test_file(p)?);
        }
    }
    let mut tests: Vec<String> = affected
        .keys()
        .filter(|p| test_flags[p.as_str()])
        .cloned()
        .collect();
    for f in &changed {
        // A deleted test file is no test to re-run: a runner handed it fails.
        if test_flags[f.as_str()] && !tests.contains(f) && project_root.join(f).exists() {
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
                    "path": p, "depth": d, "is_test": test_flags[p.as_str()],
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
