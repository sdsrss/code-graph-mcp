//! Doc ↔ CLI alignment: every `code-graph-mcp <cmd> [--flags]` invocation named
//! in the steering surfaces must reference a real clap subcommand and real flags.
//!
//! Why this test exists. The CLAUDE.md managed block, the `.claude/…` detail doc,
//! and the MCP `instructions` string are the three steering "sync faces" (see the
//! `project_claude_md_steering` memo). Before this test the block was only
//! *snapshot*-guarded (`steering_block_drift_check` byte-compares its `'generic'`
//! render against a Rust mirror; `adopt.test.js` `includes()` the variant rows as
//! literals) and the detail doc / instructions had no guard at all — none of those
//! check the tokens are *real CLI*. A flag rename or a hand-edit typo could stale
//! them silently (mem #703 was one such slip). This test grounds all three against
//! the live clap surface.
//!
//! Source of truth is each subcommand's own clap `Args` struct
//! (`XxxArgs::command()`), transcribed from the `main.rs` dispatch — so a flag set
//! cannot drift from the docs without the struct (and thus this test) changing.
//!
//! Checks, per source:
//!   1. command exists — every `code-graph-mcp <cmd>` in code context is a real
//!      clap/JS subcommand (reads code spans only, so prose like "code-graph-mcp
//!      ready." or the YAML front-matter title never false-trips).
//!   2. flag is attributed correctly — a flag written *attached* to a command in a
//!      code span must be valid for that command (catches a real flag put under the
//!      wrong command; clap would reject it at runtime).
//!   3. flag exists at all — a whole-text sweep (the docs wrap flag lists onto
//!      continuation lines away from the command) against the union of every flag.

use std::collections::{HashMap, HashSet};

use clap::CommandFactory;
use code_graph_mcp::cli;
use code_graph_mcp::mcp::server::{INSTRUCTIONS_NOISY, INSTRUCTIONS_QUIET};
use code_graph_mcp::outcome::OutcomeArgs;

/// Flags valid on any command regardless of its Args struct: clap injects help,
/// the top-level binary owns `--version`, and `--json` is documented as universal.
const GLOBAL_FLAGS: &[&str] = &["--help", "-h", "--version", "-V", "--json"];

/// Project types whose `buildTriggerRows` branch produces a distinct block: the
/// default rows, the web `trace` row, and the frontend `refs` row (web-rs/py/go
/// share web-node's row set). Scanning all three CLI-validates the variant rows,
/// which `steering_block_drift_check` (generic-only) and `adopt.test.js` (literal
/// snapshots) never do.
const BLOCK_PROJECT_TYPES: &[&str] = &["generic", "web-node", "frontend"];

/// (documented CLI name) → its clap `Command`. Names/structs transcribed 1:1 from
/// the `main.rs` subcommand dispatch. Adding a new documented subcommand means
/// adding it here; until then the doc referencing it fails the command check,
/// which is the intended nudge.
fn clap_commands() -> Vec<(&'static str, clap::Command)> {
    vec![
        ("grep", cli::GrepArgs::command()),
        ("search", cli::SearchArgs::command()),
        ("ast-search", cli::AstSearchArgs::command()),
        ("callgraph", cli::CallgraphArgs::command()),
        ("impact", cli::ImpactArgs::command()),
        ("map", cli::MapArgs::command()),
        ("tour", cli::TourArgs::command()),
        ("overview", cli::OverviewArgs::command()),
        ("show", cli::ShowArgs::command()),
        ("trace", cli::TraceArgs::command()),
        ("deps", cli::DepsArgs::command()),
        ("similar", cli::SimilarArgs::command()),
        ("refs", cli::RefsArgs::command()),
        ("dead-code", cli::DeadCodeArgs::command()),
        ("affected", cli::AffectedArgs::command()),
        ("centrality", cli::CentralityArgs::command()),
        ("cycles", cli::CyclesArgs::command()),
        ("surprising", cli::SurprisingArgs::command()),
        ("report", cli::ReportArgs::command()),
        ("benchmark", cli::BenchmarkArgs::command()),
        ("stats", cli::StatsArgs::command()),
        ("health-check", cli::HealthCheckArgs::command()),
        ("incremental-index", cli::IncrementalIndexArgs::command()),
        ("rebuild-index", cli::RebuildIndexArgs::command()),
        ("reindex", cli::ReindexArgs::command()),
        ("snapshot", cli::SnapshotArgs::command()),
        ("outcome", OutcomeArgs::command()),
    ]
}

/// JS-dispatched subcommands bypass clap (`main.rs` `run_node_script`). They accept
/// only a hand-rolled `--help` interception, plus `--check-only` for doctor.
fn js_commands() -> Vec<(&'static str, &'static [&'static str])> {
    vec![
        ("doctor", &["--check-only"]),
        ("adopt", &[]),
        ("unadopt", &[]),
    ]
}

/// Recursively collect every `--long` / `-s` token (incl. visible aliases) from a
/// command and all nested subcommands, so `snapshot` picks up create/inspect flags.
fn collect_flags(cmd: &clap::Command, out: &mut HashSet<String>) {
    for arg in cmd.get_arguments() {
        if let Some(longs) = arg.get_long_and_visible_aliases() {
            for l in longs {
                out.insert(format!("--{l}"));
            }
        }
        if let Some(shorts) = arg.get_short_and_visible_aliases() {
            for s in shorts {
                out.insert(format!("-{s}"));
            }
        }
    }
    for sub in cmd.get_subcommands() {
        collect_flags(sub, out);
    }
}

struct CliSurface {
    names: HashSet<String>,
    union_flags: HashSet<String>,
    /// Per-command valid-flag set (each command's own flags + the globals), for
    /// attributing flags written attached to a command.
    per_cmd: HashMap<String, HashSet<String>>,
}

fn cli_surface() -> CliSurface {
    let global: HashSet<String> = GLOBAL_FLAGS.iter().map(|s| s.to_string()).collect();
    let mut names = HashSet::new();
    let mut union_flags = global.clone();
    let mut per_cmd: HashMap<String, HashSet<String>> = HashMap::new();

    for (name, cmd) in clap_commands() {
        names.insert(name.to_string());
        let mut flags = global.clone();
        collect_flags(&cmd, &mut flags);
        union_flags.extend(flags.iter().cloned());
        per_cmd.insert(name.to_string(), flags);
    }
    for (name, extra) in js_commands() {
        names.insert(name.to_string());
        let mut flags = global.clone();
        flags.extend(extra.iter().map(|s| s.to_string()));
        union_flags.extend(flags.iter().cloned());
        per_cmd.insert(name.to_string(), flags);
    }
    CliSurface {
        names,
        union_flags,
        per_cmd,
    }
}

/// P2 (2026-08-16 audit §四): the README's "CLI Commands" table is the list a
/// user reads to learn what this tool can do, and it had drifted eleven
/// subcommands behind the dispatch — `affected`, `tour`, `centrality`, `cycles`,
/// `surprising`, `report`, `stats`, `outcome`, `reindex`, `snapshot`, `serve`
/// were all missing. A hand-maintained inventory with nothing checking it.
#[test]
fn readme_cli_table_lists_every_subcommand() {
    let readme =
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
            .expect("README.md must be readable");
    let table = readme
        .split("## CLI Commands")
        .nth(1)
        .and_then(|s| s.split("## ").next())
        .expect("README must have a CLI Commands section");

    let mut expected = cli_surface().names;
    expected.insert("serve".to_string());

    // Matched inside a leading `| \u{60}name` cell, so a mention in the prose
    // below the table cannot satisfy the check.
    let missing: Vec<&String> = expected
        .iter()
        .filter(|name| {
            !table
                .lines()
                .filter(|l| l.starts_with("| `"))
                .any(|l| l[3..].starts_with(name.as_str()))
        })
        .collect();
    assert!(
        missing.is_empty(),
        "README's CLI Commands table is missing {missing:?} — a user reading it \
         would not know these exist"
    );
}

/// P2 (2026-08-16 audit §四): `--help`'s OPTIONS block claims `--json` covers
/// "every subcommand except serve, doctor, adopt and unadopt". It previously
/// claimed the opposite for the index commands ("not the index commands"), which
/// was wrong — `incremental-index`, `rebuild-index` and `reindex` all emit a JSON
/// envelope. A blanket claim in help text is only safe if something enforces it.
#[test]
fn every_clap_command_accepts_json() {
    // `snapshot` is named as an exception in the help text: `snapshot inspect`
    // prints JSON unconditionally, so a `--json` flag there would be a no-op.
    // This test found that on its first run, against a claim written one edit
    // earlier — which is exactly the drift the guard is for.
    const NO_JSON_BY_DESIGN: &[&str] = &["snapshot"];
    let missing: Vec<&str> = clap_commands()
        .into_iter()
        .filter(|(name, _)| !NO_JSON_BY_DESIGN.contains(name))
        .filter(|(_, cmd)| {
            let mut flags = HashSet::new();
            collect_flags(cmd, &mut flags);
            !flags.contains("--json")
        })
        .map(|(name, _)| name)
        .collect();
    assert!(
        missing.is_empty(),
        "--help says every subcommand but serve/doctor/adopt/unadopt takes --json; \
         these clap commands do not: {missing:?}. Either add the flag or narrow the claim."
    );
    // The stated exceptions are the JS-dispatched ones, which own no clap Args at
    // all — so the claim's exception list is exactly `js_commands()` plus `serve`.
    let js: Vec<&str> = js_commands().into_iter().map(|(n, _)| n).collect();
    assert_eq!(
        js,
        vec!["doctor", "adopt", "unadopt"],
        "the help text names these three by hand; a new JS command must be added there too"
    );
    // Negative control: the exception list must not be a place to hide a command
    // that simply forgot the flag. `snapshot` earns it by printing JSON already.
    let snapshot = clap_commands()
        .into_iter()
        .find(|(n, _)| *n == "snapshot")
        .expect("snapshot must exist for its exception to mean anything");
    assert!(
        snapshot
            .1
            .get_subcommands()
            .any(|s| s.get_name() == "inspect"),
        "snapshot's --json exemption rests on `snapshot inspect` being JSON-only"
    );
}

/// P2 (2026-08-16 audit §四): the unknown-subcommand typo suggester reads
/// `cli::SUBCOMMANDS`, a hand-maintained list with nothing tying it to the
/// commands that actually dispatch. It was missing `affected` and `tour`, so a
/// typo of either got the generic "run --help" line instead of the fix — the
/// unguarded-hardcoded-list shape this repo has been bitten by before.
///
/// `cli_surface()` is the authoritative set (it is transcribed from the `main.rs`
/// dispatch and is itself the thing every doc check grounds against), so tying
/// the typo table to it means adding a command can no longer half-land. `serve`
/// is added explicitly: it dispatches but owns no `Args` struct.
#[test]
fn typo_table_covers_every_dispatchable_subcommand() {
    let table: HashSet<&str> = cli::SUBCOMMANDS.iter().copied().collect();
    let mut expected = cli_surface().names;
    expected.insert("serve".to_string());

    let missing: Vec<&String> = expected
        .iter()
        .filter(|n| !table.contains(n.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "cli::SUBCOMMANDS (the typo suggester's whole world) is missing {missing:?}. \
         A typo of these gets no suggestion."
    );

    // And the reverse direction, so the table cannot accumulate names that no
    // longer dispatch — suggesting a dead command is worse than suggesting
    // nothing. MCP tool aliases are exempt: they dispatch through `main.rs`
    // alias arms, not through a clap `Args` struct of their own.
    let alias_exempt = |n: &str| n.contains('_');
    let stale: Vec<&&str> = table
        .iter()
        .filter(|n| !alias_exempt(n) && **n != "serve" && !expected.contains(**n))
        .collect();
    assert!(
        stale.is_empty(),
        "cli::SUBCOMMANDS suggests {stale:?}, which no longer dispatch"
    );
}

/// Render the CLAUDE.md managed block for a project type by invoking the JS
/// generator, exactly as `steering_block_drift_check` does — so the block is
/// checked against the *live* clap surface, not just a Rust mirror.
fn build_block(project_type: &str) -> String {
    // project_type is a hard-coded literal from BLOCK_PROJECT_TYPES — no injection.
    let script =
        format!("process.stdout.write(require('./claude-plugin/scripts/adopt.js').buildBlock('{project_type}'))");
    let out = std::process::Command::new("node")
        .args(["-e", &script])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("node is required to render the CLAUDE.md block for doc↔CLI alignment");
    assert!(
        out.status.success(),
        "node buildBlock('{project_type}') failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let block = String::from_utf8(out.stdout).expect("managed block is utf-8");
    // Non-vacuity guard: a broken generator returning empty/garbage would make the
    // block scan silently pass. Require at least one real invocation to scan.
    assert!(
        block.contains("`code-graph-mcp "),
        "buildBlock('{project_type}') produced no `code-graph-mcp …` invocation — generator broken? got: {block:?}"
    );
    block
}

/// Concatenated Markdown code context: the content of ``` fenced blocks plus the
/// text inside inline `` `…` `` spans, one entry per line. Prose (front-matter,
/// headings, sentences that merely mention the binary) is excluded — only here do
/// real `code-graph-mcp …` invocations appear.
fn code_spans(text: &str) -> String {
    let mut out = String::new();
    let mut in_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            out.push_str(line);
            out.push('\n');
        } else {
            // Inline code: odd-indexed segments of a backtick split are inside `…`.
            for (i, seg) in line.split('`').enumerate() {
                if i % 2 == 1 {
                    out.push_str(seg);
                    out.push('\n');
                }
            }
        }
    }
    out
}

/// Strip wrapper punctuation the docs put around tokens (ASCII + CJK brackets,
/// quotes, backticks, angle brackets, pipes, trailing sentence punctuation)
/// without touching a flag's leading `-`.
fn clean(tok: &str) -> String {
    tok.trim_matches(|c: char| {
        "[](){}\"'`<>|.,;:。，、；：（）【】「」".contains(c) || c.is_whitespace()
    })
    .to_string()
}

/// Extract the leading flag from a (cleaned) token: `-x` / `--xyz`, letter-led,
/// consuming the maximal `[A-Za-z0-9-]` run and stopping at the first foreign char
/// (so `--compact）看架构` → `--compact`, `` --help`. `` → `--help`). Returns `None`
/// for non-flags, bare `--`, markdown rules (`---`), and numeric args (`-3`).
fn lead_flag(tok: &str) -> Option<String> {
    if !tok.starts_with('-') {
        return None;
    }
    let dashes = if tok.starts_with("--") { 2 } else { 1 };
    let rest = &tok[dashes..];
    let mut end = 0;
    for (i, c) in rest.char_indices() {
        if c.is_ascii_alphanumeric() || c == '-' {
            end = i + c.len_utf8();
        } else {
            break;
        }
    }
    let name = &rest[..end];
    if !name.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return None; // must have a letter immediately after the dashes
    }
    Some(format!("{}{name}", &tok[..dashes]))
}

/// Normalize an attached short-flag value (`-A2` → `-A`) so grep's attached-numeric
/// context forms match the bare clap short.
fn normalize_flag(tok: &str) -> String {
    let b = tok.as_bytes();
    if b.len() >= 3
        && b[0] == b'-'
        && b[1].is_ascii_alphabetic()
        && b[2..].iter().all(u8::is_ascii_digit)
    {
        return format!("-{}", b[1] as char);
    }
    tok.to_string()
}

/// Command names invoked as `code-graph-mcp <cmd>` within code context. Handles
/// pipe-joined alternation (`impact|similar|deps`) and rejects flag tokens
/// (`--help`) and non-command shapes (CJK, trailing punctuation).
fn command_names(code: &str) -> Vec<String> {
    const MARK: &str = "code-graph-mcp ";
    let mut out = Vec::new();
    let mut rest = code;
    while let Some(pos) = rest.find(MARK) {
        let after = &rest[pos + MARK.len()..];
        let end = after
            .find(|c: char| c.is_whitespace() || "`\n#()（）。".contains(c))
            .unwrap_or(after.len());
        for part in after[..end].split('|') {
            if let Some(cmd) = command_shape(part) {
                out.push(cmd);
            }
        }
        rest = after;
    }
    out
}

/// `Some(name)` if the cleaned token looks like a subcommand: lowercase ASCII +
/// hyphens, not a flag. `None` for placeholders, flags, and CJK/prose.
fn command_shape(tok: &str) -> Option<String> {
    let cand = clean(tok);
    let ok = !cand.is_empty()
        && !cand.starts_with('-')
        && cand.chars().all(|c| c.is_ascii_lowercase() || c == '-');
    ok.then_some(cand)
}

/// (command, flag) for each flag appearing in the same code span (single line of
/// `code_spans`, up to a `#` comment) as its `code-graph-mcp <cmd>` invocation.
/// Enables per-command attribution; detached / continuation-line flags fall through
/// to the whole-text union sweep instead.
fn attached_flags(code: &str) -> Vec<(String, String)> {
    const MARK: &str = "code-graph-mcp ";
    let mut out = Vec::new();
    let mut rest = code;
    while let Some(pos) = rest.find(MARK) {
        let after = &rest[pos + MARK.len()..];
        let end = after.find(['\n', '#']).unwrap_or(after.len());
        let mut cmd: Option<String> = None;
        let mut flags = Vec::new();
        for tok in after[..end].split(char::is_whitespace) {
            let cand = clean(tok);
            if let Some(f) = lead_flag(&cand) {
                flags.push(normalize_flag(&f));
            } else if cmd.is_none() {
                cmd = cand.split('|').next().and_then(command_shape);
            }
        }
        if let Some(c) = cmd {
            out.extend(flags.into_iter().map(|f| (c.clone(), f)));
        }
        rest = after;
    }
    out
}

/// Every flag token anywhere in `text` (whole-text sweep — the docs wrap flag lists
/// onto continuation lines away from the command).
fn flag_tokens(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || c == '/' || c == ',' || c == '|')
        .filter_map(|raw| lead_flag(&clean(raw)))
        .map(|f| normalize_flag(&f))
        .collect()
}

fn check(source: &str, text: &str, cli: &CliSurface) -> Vec<String> {
    let mut errs = Vec::new();
    let code = code_spans(text);

    // 1. command exists.
    for cmd in command_names(&code) {
        if !cli.names.contains(&cmd) {
            errs.push(format!(
                "[{source}] references `code-graph-mcp {cmd}` — no such subcommand"
            ));
        }
    }
    // 2. flag attributed to the right command (skip unknown cmds — flagged above).
    for (cmd, flag) in attached_flags(&code) {
        if let Some(valid) = cli.per_cmd.get(&cmd) {
            if !valid.contains(&flag) {
                errs.push(format!(
                    "[{source}] `code-graph-mcp {cmd} … {flag}` — `{flag}` is not a flag of `{cmd}`"
                ));
            }
        }
    }
    // 3. flag exists on some subcommand (catches detached / prose flags too).
    for f in flag_tokens(text) {
        if !cli.union_flags.contains(&f) {
            errs.push(format!(
                "[{source}] references flag `{f}` — not a flag on any subcommand"
            ));
        }
    }
    errs
}

#[test]
fn detail_doc_and_instructions_match_cli() {
    let cli = cli_surface();
    let mut errs = Vec::new();

    let doc_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/claude-plugin/templates/plugin_code_graph_mcp.md"
    );
    let doc = std::fs::read_to_string(doc_path)
        .unwrap_or_else(|e| panic!("cannot read detail doc {doc_path}: {e}"));

    errs.extend(check(".claude/plugin_code_graph_mcp.md", &doc, &cli));
    errs.extend(check("MCP instructions (noisy)", INSTRUCTIONS_NOISY, &cli));
    errs.extend(check("MCP instructions (quiet)", INSTRUCTIONS_QUIET, &cli));
    for pt in BLOCK_PROJECT_TYPES {
        errs.extend(check(
            &format!("CLAUDE.md block ({pt})"),
            &build_block(pt),
            &cli,
        ));
    }

    errs.sort();
    errs.dedup();

    assert!(
        errs.is_empty(),
        "Steering doc ↔ CLI drift ({} issue(s)). Each names a command/flag in a steering \
         surface the live clap CLI no longer has (or attributes a flag to the wrong command). \
         Fix the doc/instructions/block generator, or add the command/flag. Sources: \
         claude-plugin/templates/plugin_code_graph_mcp.md, src/mcp/server/mod.rs \
         INSTRUCTIONS_{{NOISY,QUIET}}, and claude-plugin/scripts/adopt.js buildBlock().\n  {}",
        errs.len(),
        errs.join("\n  ")
    );
}

/// Guards the alignment test against vacuous green: the checker MUST reject a
/// fabricated command, a fabricated flag, and a real flag put under the wrong
/// command. Without this, a tokenizer bug that silently found nothing would let the
/// test pass no matter how stale the docs got.
#[test]
fn checker_rejects_fabricated_and_misattributed() {
    let cli = cli_surface();

    let bad =
        "run `code-graph-mcp frobnicate X`, then `code-graph-mcp grep pat --nonexistent-flag`.";
    let errs = check("synthetic", bad, &cli);
    assert!(
        errs.iter().any(|e| e.contains("frobnicate")),
        "checker failed to flag a fabricated command; errs={errs:?}"
    );
    assert!(
        errs.iter().any(|e| e.contains("--nonexistent-flag")),
        "checker failed to flag a fabricated flag; errs={errs:?}"
    );

    // Per-command precision: `--ignore` is real (dead-code) but not on `callgraph`,
    // so it passes the union sweep and MUST be caught by attribution instead.
    let misattributed = "`code-graph-mcp callgraph X --ignore foo`";
    let errs2 = check("synthetic", misattributed, &cli);
    assert!(
        errs2
            .iter()
            .any(|e| e.contains("--ignore") && e.contains("callgraph")),
        "checker failed to catch a flag misattributed to the wrong command; errs={errs2:?}"
    );
}

/// P2 (2026-08-16 audit §四): both module-layout blocks — README's "Architecture"
/// and the project `CLAUDE.md` — had drifted from `src/`. Neither listed `cli/`
/// (31 files, the largest module), `snapshot/`, `outcome.rs` or `resolve.rs`.
///
/// Held against the real `src/` directory rather than against a transcription.
///
/// COVERAGE LIMIT, stated here because a skip line printed at runtime is captured
/// by libtest and invisible under the `cargo test` CI runs: **CLAUDE.md is
/// gitignored**, so in every automated run this test checks README.md ALONE. The
/// CLAUDE.md leg only fires in a working tree that has the file.
///
/// That is a smaller hole than it sounds, and worth stating precisely rather than
/// implying either more or less coverage than exists: the steering surfaces an
/// agent actually consumes elsewhere — the adopt-generated managed block, the
/// `.claude/…` detail doc and the MCP `instructions` string — are tracked and are
/// guarded by `detail_doc_and_instructions_match_cli` in this same file. The
/// repo's own CLAUDE.md is a local developer file; keeping its module map honest
/// is a working-tree check by construction.
#[test]
fn module_layout_blocks_list_every_top_level_module() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    // `lib.rs` / `main.rs` are crate roots, not modules anyone navigates to.
    const NOT_A_MODULE: &[&str] = &["lib.rs", "main.rs"];

    let mut actual: Vec<String> = std::fs::read_dir(root.join("src"))
        .expect("src/ must be readable")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !NOT_A_MODULE.contains(&n.as_str()))
        .filter(|n| n.ends_with(".rs") || root.join("src").join(n).is_dir())
        .collect();
    actual.sort();
    assert!(
        actual.len() >= 10,
        "sanity: found only {actual:?} under src/"
    );

    // (label, path, required). README.md is tracked, so its absence is a real
    // failure. CLAUDE.md is NOT tracked (`.gitignore:39`), so a clean CI checkout
    // does not have it — reading it unconditionally made this guard fail on every
    // platform of the v0.118.0 pre-tag run while passing in every working tree.
    // A test that depends on an untracked file can only ever be environment-
    // dependent; the honest shape is to enforce it where it exists and say so
    // where it does not.
    let mut checked_labels: Vec<&str> = Vec::new();
    for (label, path, required) in [
        ("README.md", root.join("README.md"), true),
        ("CLAUDE.md", root.join("CLAUDE.md"), false),
    ] {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if !required => {
                eprintln!(
                    "skip: {label} not present ({e}) — untracked, checked in working trees only"
                );
                continue;
            }
            Err(e) => panic!("{label}: {e}"),
        };
        checked_labels.push(label);
        // Only the fenced layout block, so a passing mention elsewhere in the
        // prose cannot stand in for a row in the map.
        let block = text
            .split("src/\n")
            .nth(1)
            .and_then(|s| s.split("```").next())
            .unwrap_or_else(|| panic!("{label} must contain an `src/` layout block"));
        let missing: Vec<&String> = actual
            .iter()
            .filter(|m| {
                let entry = if m.ends_with(".rs") {
                    (*m).clone()
                } else {
                    format!("{m}/")
                };
                !block.contains(&entry)
            })
            .collect();
        assert!(
            missing.is_empty(),
            "{label}'s src/ layout block omits {missing:?} — a reader (or an agent \
             routing off this map) would not know they exist"
        );
    }
    // README is `required`, so it either incremented `checked` or panicked above —
    // which makes a bare `checked >= 1` a tautology that can never go red. Assert
    // the thing that actually matters instead: the REQUIRED document was among the
    // ones checked. (The v0.118.0 pre-tag review caught the earlier version, and
    // the commit message that introduced it credited the tautology with preventing
    // hollowing — what really prevents it is README being tracked and required.)
    assert!(
        checked_labels.contains(&"README.md"),
        "the required layout block was not checked; checked: {checked_labels:?}"
    );
}

/// Every `CODE_GRAPH_*` environment variable the code READS must appear in the
/// README's environment table.
///
/// Audit 2026-08-22 P2-14: of the variables in the tree, seven were documented.
/// The rest — `NO_AUTO_ADOPT`, `NO_INJECT`, `RESYNC_BUDGET`, `PARSE_TIMEOUT_MS`,
/// `MAX_FILE_SIZE` and more — were user-visible switches discoverable only by
/// reading the source. Documenting them once fixes today; this keeps the next
/// one from repeating it, which is the difference between a doc edit and a
/// guard.
#[test]
fn readme_documents_every_env_var_the_code_reads() {
    use std::collections::BTreeSet;
    use std::path::Path;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    /// Collect `CODE_GRAPH_*` names from actual env READS, not from every
    /// mention: `domain::CODE_GRAPH_DIR` is a Rust constant that happens to
    /// share the prefix, and a table row for it would be a lie.
    fn collect_reads(dir: &Path, out: &mut BTreeSet<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if name == "target" || name == "node_modules" || name.starts_with('.') {
                    continue;
                }
                collect_reads(&path, out);
                continue;
            }
            let is_src = path.extension().and_then(|e| e.to_str());
            if !matches!(is_src, Some("rs") | Some("js")) {
                continue;
            }
            // Tests set variables they do not document, on purpose.
            if name.ends_with(".test.js") || name == "tests.rs" {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for marker in ["env::var(\"", "env::var_os(\"", "process.env.", "env."] {
                let mut rest = text.as_str();
                while let Some(i) = rest.find(marker) {
                    rest = &rest[i + marker.len()..];
                    let name: String = rest
                        .chars()
                        .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_')
                        .collect();
                    if name.starts_with("CODE_GRAPH_") {
                        out.insert(name);
                    }
                }
            }
        }
    }

    let mut read: BTreeSet<String> = BTreeSet::new();
    collect_reads(&root.join("src"), &mut read);
    collect_reads(&root.join("claude-plugin").join("scripts"), &mut read);
    collect_reads(&root.join("scripts"), &mut read);
    assert!(
        read.len() > 20,
        "the scanner found only {} variables — it stopped matching the source, \
         which would make this guard vacuous",
        read.len()
    );

    let readme = std::fs::read_to_string(root.join("README.md")).expect("README.md must exist");
    let section = readme
        .split_once("## Environment variables")
        .map(|(_, rest)| rest.split("\n## ").next().unwrap_or(rest).to_string())
        .expect("README must have an `## Environment variables` section");

    let missing: Vec<&String> = read.iter().filter(|v| !section.contains(*v)).collect();
    assert!(
        missing.is_empty(),
        "README's environment table omits {missing:?} — a switch nobody can find \
         is a switch that does not exist. Add a row (user-facing) or a row in the \
         internal/test-only block."
    );
}
