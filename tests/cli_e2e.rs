/// End-to-end tests for CLI subcommands.
///
/// These tests create a temp project, index it using the library,
/// then run CLI subcommands as subprocesses and verify output.
use std::process::Command;

use tempfile::TempDir;

fn binary_path() -> String {
    env!("CARGO_BIN_EXE_code-graph-mcp").to_string()
}

/// Create a temp project with TypeScript source files and index it.
/// Returns the TempDir (dropping it cleans up).
fn setup_indexed_project() -> TempDir {
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();

    std::fs::write(src.join("auth.ts"), r#"
import jwt from 'jsonwebtoken';

export function validateToken(token: string): boolean {
    const decoded = jwt.verify(token, process.env.SECRET);
    return decoded !== null;
}

export function hashPassword(password: string): string {
    return password; // stub
}
"#).unwrap();

    std::fs::write(src.join("api.ts"), r#"
import { validateToken } from './auth';

export function handleLogin(req: Request, res: Response) {
    const user = validateToken(req.headers.authorization);
    if (!user) { res.status(401); return; }
    res.json({ userId: user.id });
}

export function handleLogout(req: Request, res: Response) {
    res.json({ ok: true });
}
"#).unwrap();

    std::fs::write(src.join("utils.ts"), r#"
export function formatDate(date: Date): string {
    return date.toISOString();
}

export class Logger {
    log(msg: string) {
        console.log(msg);
    }
}
"#).unwrap();

    // Index using the library directly
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db_path = db_dir.join("index.db");
    let db = code_graph_mcp::storage::db::Database::open(&db_path).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();

    project
}

/// Run a CLI command and return (stdout, stderr, exit_code).
fn run_cli(project: &TempDir, args: &[&str]) -> (String, String, i32) {
    let output = Command::new(binary_path())
        .current_dir(project.path())
        .args(args)
        .output()
        .expect("failed to run binary");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let code = output.status.code().unwrap_or(-1);
    (stdout, stderr, code)
}

// ============================================================
// clap migration (audit #4) — cross-command --help hygiene
// ============================================================

// Regression guard for the doc-comment-as-long_about leak found by smoke-testing
// Step 1: clap renders a struct's multi-paragraph `///` doc as `--help` long-about,
// so internal migration notes (audit refs, shim/impl details, the mis-attached
// function docs) MUST stay out of the clap struct docs. Asserts the generated help
// for every clap-migrated subcommand exposes only its user-facing `about`.
#[test]
fn test_cli_migrated_help_has_no_internal_notes() {
    let project = setup_indexed_project();
    let internal_tokens = ["audit #", "clap-migrat", "resolved_format", "plan §", "issue #"];
    for cmd in [
        "stats", "benchmark", "incremental-index", "reindex", "rebuild-index", "health-check",
        "map", "grep", "overview", "dead-code", "search", "ast-search", "deps", "trace",
        "snapshot", "callgraph", "impact", "show", "refs", "similar",
    ] {
        let (stdout, _, code) = run_cli(&project, &[cmd, "--help"]);
        assert_eq!(code, 0, "{cmd} --help should exit 0");
        let low = stdout.to_lowercase();
        for tok in internal_tokens {
            assert!(
                !low.contains(&tok.to_lowercase()),
                "{cmd} --help leaked internal note {tok:?}; full help:\n{stdout}"
            );
        }
    }
}

// ============================================================
// health-check
// ============================================================

#[test]
fn test_cli_health_check() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["health-check"]);
    assert_eq!(code, 0);
    assert!(stdout.starts_with("OK:"), "expected OK, got: {}", stdout);
    assert!(stdout.contains("nodes"), "should mention nodes");
}

#[test]
fn test_cli_health_check_json() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["health-check", "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["healthy"], true);
    assert!(v["nodes"].as_i64().unwrap() > 0);
}

#[test]
fn test_cli_health_check_unhealthy_exit_code() {
    let project = TempDir::new().unwrap();
    // No index — should fail
    let (_, stderr, code) = run_cli(&project, &["health-check"]);
    assert_ne!(code, 0, "unhealthy should exit non-zero, stderr: {}", stderr);
}

// clap-migrated (audit #4) contract lock. The --json/--format duality is now
// normalized by HealthCheckArgs::resolved_format; --json and --format json must
// stay interchangeable, and clap owns help + unknown-flag rejection.
#[test]
fn test_cli_health_check_format_json_equiv_to_json() {
    // --format json must produce the same JSON envelope as the --json shorthand.
    let project = setup_indexed_project();
    let (out_flag, _, code_flag) = run_cli(&project, &["health-check", "--json"]);
    let (out_fmt, _, code_fmt) = run_cli(&project, &["health-check", "--format", "json"]);
    assert_eq!(code_flag, 0);
    assert_eq!(code_fmt, 0);
    let v_flag: serde_json::Value = serde_json::from_str(out_flag.trim()).unwrap();
    let v_fmt: serde_json::Value = serde_json::from_str(out_fmt.trim()).unwrap();
    assert_eq!(v_flag["healthy"], v_fmt["healthy"], "--format json must mirror --json");
    assert_eq!(v_flag["nodes"], v_fmt["nodes"]);
}

#[test]
fn test_cli_health_check_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["health-check", "--help"]);
    assert_eq!(code, 0, "health-check --help should exit 0 (clap help)");
    assert!(stdout.contains("index status") || stdout.contains("--format"),
        "help should describe the command; got: {stdout:?}");
}

#[test]
fn test_cli_health_check_unknown_flag_errors() {
    // Flavor-B: clap rejects unknown flags (was: silently ignored by the hand parser).
    let project = setup_indexed_project();
    let (_, _, code) = run_cli(&project, &["health-check", "--bogus"]);
    assert_eq!(code, 2, "unknown flag must error under clap");
}

// ============================================================
// search
// ============================================================

#[test]
fn test_cli_search() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["search", "validateToken"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("validateToken"), "should find validateToken, got: {}", stdout);
}

#[test]
fn test_cli_search_no_results() {
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["search", "xyznonexistent"]);
    assert_eq!(code, 0);
    assert!(stderr.contains("No results"), "should show no results message");
}

#[test]
fn test_cli_search_json() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["search", "validateToken", "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(v.is_array(), "JSON output should be array");
    assert!(!v.as_array().unwrap().is_empty());
}

#[test]
fn test_cli_search_language_filter() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["search", "validate", "--language", "typescript"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("validateToken"));
}

#[test]
fn test_cli_search_compact() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["search", "validate", "--compact"]);
    assert_eq!(code, 0);
    // Compact: no signature info, just name + location
    assert!(stdout.contains("validateToken"));
    // Should NOT contain parameter types in compact mode
    let lines: Vec<&str> = stdout.lines().collect();
    for line in &lines {
        if line.contains("validateToken") {
            assert!(!line.contains("(token:"), "compact should not include params, got: {}", line);
        }
    }
}

#[test]
fn test_cli_search_limit() {
    let project = setup_indexed_project();
    let (stdout, _, _) = run_cli(&project, &["search", "function", "--limit", "2"]);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert!(lines.len() <= 2, "should respect --limit, got {} lines", lines.len());
}

// clap-migrated (audit #4): clap owns --help + unknown-flag rejection; --top-k is
// a hidden alias of --limit; the non-empty query guard is preserved in the handler.
#[test]
fn test_cli_search_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["search", "--help"]);
    assert_eq!(code, 0, "search --help should exit 0 (clap help)");
    assert!(stdout.contains("FTS5") || stdout.contains("QUERY"),
        "help should describe the command; got: {stdout:?}");
}

#[test]
fn test_cli_search_unknown_flag_errors() {
    let project = setup_indexed_project();
    let (_, _, code) = run_cli(&project, &["search", "validateToken", "--bogus"]);
    assert_eq!(code, 2, "unknown flag must error under clap");
}

#[test]
fn test_cli_search_top_k_alias_matches_limit() {
    let project = setup_indexed_project();
    let (out_limit, _, code_limit) = run_cli(&project, &["search", "function", "--limit", "2", "--json"]);
    let (out_topk, _, code_topk) = run_cli(&project, &["search", "function", "--top-k", "2", "--json"]);
    assert_eq!(code_limit, code_topk, "--top-k must mirror --limit exit code");
    assert_eq!(out_limit.trim(), out_topk.trim(), "--top-k 2 must equal --limit 2");
}

// ============================================================
// grep (requires ripgrep `rg` binary)
// ============================================================

fn has_ripgrep() -> bool {
    Command::new("rg").arg("--version").output().is_ok()
}

#[test]
fn test_cli_grep() {
    if !has_ripgrep() { eprintln!("skipping: rg not installed"); return; }
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["grep", "validateToken"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("validateToken"), "should find matches");
    assert!(stdout.contains("→"), "should include AST context arrows");
}

#[test]
fn test_cli_grep_no_matches() {
    if !has_ripgrep() { eprintln!("skipping: rg not installed"); return; }
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["grep", "xyznonexistent"]);
    assert_eq!(code, 0);
    assert!(stderr.contains("No matches"), "should show no matches message");
}

#[test]
fn test_cli_grep_invalid_regex_exits_nonzero() {
    if !has_ripgrep() { eprintln!("skipping: rg not installed"); return; }
    let project = setup_indexed_project();
    // Unescaped `(` is an invalid regex — ripgrep exits 2. The CLI must surface
    // a non-zero exit (not silently succeed like a no-match).
    let (_, stderr, code) = run_cli(&project, &["grep", "res.json("]);
    assert_ne!(code, 0, "invalid regex must exit non-zero, not silently succeed");
    assert!(stderr.contains("ripgrep error") || stderr.to_lowercase().contains("regex"),
        "should surface the ripgrep error, got: {stderr}");
}

#[test]
fn test_cli_grep_with_path() {
    if !has_ripgrep() { eprintln!("skipping: rg not installed"); return; }
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["grep", "validateToken", "src/auth.ts"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("validateToken"));
}

// clap-migrated (audit #4): clap owns --help + unknown-flag rejection; the
// non-empty pattern guard (exit 1 + Usage) is preserved in the handler because
// clap accepts an empty-string positional.
#[test]
fn test_cli_grep_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["grep", "--help"]);
    assert_eq!(code, 0, "grep --help should exit 0 (clap help)");
    assert!(stdout.contains("AST-context grep") || stdout.contains("PATTERN"),
        "help should describe the command; got: {stdout:?}");
}

#[test]
fn test_cli_grep_unknown_flag_errors() {
    let project = setup_indexed_project();
    let (_, _, code) = run_cli(&project, &["grep", "validateToken", "--bogus"]);
    assert_eq!(code, 2, "unknown flag must error under clap");
}

#[test]
fn test_cli_grep_empty_pattern_errors() {
    // An empty-string pattern (e.g. unset `grep "$X"` shell var) must keep
    // erroring with the Usage hint, not run ripgrep against an empty regex.
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["grep", ""]);
    assert_eq!(code, 1, "empty pattern should exit 1 with Usage; stderr={stderr:?}");
    assert!(stderr.contains("Usage:"), "should show usage on empty pattern; got: {stderr:?}");
}

// ============================================================
// callgraph
// ============================================================

#[test]
fn test_cli_callgraph() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["callgraph", "validateToken"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("validateToken"), "should show root symbol");
    // handleLogin calls validateToken
    assert!(stdout.contains("handleLogin"), "should show caller handleLogin, got: {}", stdout);
}

#[test]
fn test_cli_callgraph_compact() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["callgraph", "validateToken", "--compact"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("validateToken"));
    // Compact: no [function] type annotation
    assert!(!stdout.contains("[function]"), "compact should not have type annotation");
}

#[test]
fn test_cli_callgraph_direction() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["callgraph", "handleLogin", "--direction", "callees"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("validateToken"), "handleLogin should call validateToken");
}

#[test]
fn test_cli_callgraph_nonexistent() {
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["callgraph", "nonexistent_fn"]);
    assert_ne!(code, 0, "nonexistent symbol should return non-zero exit code");
    assert!(stderr.contains("No call graph results"), "should report not found");
}

// Regression: `--direction` must be validated at the CLI layer (like cmd_deps does).
// Without early validation a typo only surfaced after ambiguity resolution: user
// got "Ambiguous symbol" first, retried with --file, then was told "invalid direction" —
// two error messages for one mistake.
#[test]
fn test_cli_callgraph_invalid_direction_errors_early() {
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["callgraph", "validateToken", "--direction", "bogus"]);
    assert_eq!(code, 1, "bad --direction should error; stderr={stderr:?}");
    assert!(stderr.contains("--direction must be one of"),
        "stderr should explain the valid set; got: {stderr:?}");
}

#[test]
fn test_cli_callgraph_json() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["callgraph", "validateToken", "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(v["results"].is_array());
}

// clap-migrated (audit #4 Step 5): clap owns --help + unknown-flag rejection;
// --direction stays an in-handler String so invalid-direction is exit-1 (above).
#[test]
fn test_cli_callgraph_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["callgraph", "--help"]);
    assert_eq!(code, 0, "callgraph --help should exit 0 (clap help)");
    assert!(stdout.contains("call graph") || stdout.contains("--direction"),
        "help should describe the command; got: {stdout:?}");
}

#[test]
fn test_cli_callgraph_unknown_flag_errors() {
    let project = setup_indexed_project();
    let (_, _, code) = run_cli(&project, &["callgraph", "validateToken", "--bogus"]);
    assert_eq!(code, 2, "unknown flag must error under clap");
}

// ============================================================
// impact
// ============================================================

#[test]
fn test_cli_impact() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["impact", "validateToken"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("Risk:"), "should show risk level");
    assert!(stdout.contains("validateToken"));
}

#[test]
fn test_cli_impact_nonexistent() {
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["impact", "nonexistent_fn"]);
    assert_ne!(code, 0, "nonexistent symbol should return non-zero exit code");
    assert!(stderr.contains("Symbol not found"), "should report symbol not found");
}

#[test]
fn test_cli_impact_change_type_remove() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["impact", "validateToken", "--change-type", "remove"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("Risk:"));
}

#[test]
fn test_cli_impact_invalid_change_type() {
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["impact", "validateToken", "--change-type", "invalid"]);
    assert_ne!(code, 0, "invalid change-type should fail");
    assert!(stderr.contains("must be one of"), "should show valid options");
}

#[test]
fn test_cli_impact_json() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["impact", "validateToken", "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(v["risk"].is_string());
    assert!(v["symbol"].is_string());
}

// clap-migrated (audit #4 Step 5): clap owns --help + unknown-flag rejection;
// --change-type stays an in-handler String so invalid-change-type is exit-1 (above).
#[test]
fn test_cli_impact_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["impact", "--help"]);
    assert_eq!(code, 0, "impact --help should exit 0 (clap help)");
    assert!(stdout.contains("Impact analysis") || stdout.contains("--change-type"),
        "help should describe the command; got: {stdout:?}");
}

#[test]
fn test_cli_impact_unknown_flag_errors() {
    let project = setup_indexed_project();
    let (_, _, code) = run_cli(&project, &["impact", "validateToken", "--bogus"]);
    assert_eq!(code, 2, "unknown flag must error under clap");
}

// ============================================================
// same-file overload ambiguity (audit 2026-06-03 #6)
// ============================================================

/// Index a project with two non-test `fn new()` in the *same* file (distinct
/// impl blocks). `file_path` cannot disambiguate these — only `node_id` can.
fn setup_same_file_overload_project() -> TempDir {
    let project = TempDir::new().unwrap();
    std::fs::write(project.path().join("lib.rs"), r#"
pub struct Foo;
pub struct Bar;

impl Foo {
    pub fn new() -> Self { Foo }
}

impl Bar {
    pub fn new() -> Self { Bar }
}

pub fn make_them() {
    let _ = Foo::new();
    let _ = Bar::new();
}
"#).unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db_path = db_dir.join("index.db");
    let db = code_graph_mcp::storage::db::Database::open(&db_path).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    project
}

// Regression (audit #6): a bare name with ≥2 non-test definitions in the SAME
// file must be flagged ambiguous, matching MCP `get_call_graph`. Before the fix
// the CLI gated ambiguity on distinct *files*, so same-file overloads silently
// merged the call graphs of two distinct `new` functions (exit 0, wrong answer).
#[test]
fn test_cli_callgraph_same_file_overload_is_ambiguous() {
    let project = setup_same_file_overload_project();
    let (_, stderr, code) = run_cli(&project, &["callgraph", "new"]);
    assert_eq!(code, 1, "same-file overload `new` must error, not silently merge; stderr={stderr:?}");
    assert!(stderr.contains("Ambiguous symbol 'new'"),
        "should report ambiguity; got: {stderr:?}");
    // The guidance must be accurate for same-file overloads: file_path can't
    // split them, so point at the node_id-capable tools instead.
    assert!(stderr.contains("same file") && stderr.contains("node-id"),
        "same-file message must mention 'same file' + a node-id path; got: {stderr:?}");
}

#[test]
fn test_cli_callgraph_same_file_overload_is_ambiguous_json() {
    let project = setup_same_file_overload_project();
    let (stdout, _, code) = run_cli(&project, &["callgraph", "new", "--json"]);
    assert_eq!(code, 1, "same-file overload must error in --json mode too");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(v["error"].as_str().unwrap_or("").contains("Ambiguous"),
        "json error field should report ambiguity; got: {stdout}");
    let sugg = v["suggestions"].as_array().expect("suggestions array");
    assert!(sugg.len() >= 2, "expected ≥2 node_id suggestions; got: {stdout}");
    for s in sugg {
        assert!(s["node_id"].as_i64().is_some(), "suggestion needs node_id: {s}");
        assert!(s["start_line"].as_i64().is_some(), "suggestion needs start_line: {s}");
    }
}

#[test]
fn test_cli_impact_same_file_overload_is_ambiguous() {
    let project = setup_same_file_overload_project();
    let (_, stderr, code) = run_cli(&project, &["impact", "new"]);
    assert_eq!(code, 1, "same-file overload `new` must error in impact, not merge callers; stderr={stderr:?}");
    assert!(stderr.contains("Ambiguous symbol 'new'"),
        "should report ambiguity; got: {stderr:?}");
}

#[test]
fn test_cli_callgraph_import_disambiguates_same_name() {
    // Regression (Phase 2d): `run()` does `from db import save` and calls save()
    // once, but ambiguous same-language resolution fanned the bare call out to
    // EVERY same-name `save` (db.py AND cache.py). The import edge binds the name
    // to db.save, so the cache.save edge is a FALSE caller — it inflated
    // impact/call-graph and hid cache.save from dead-code. The prune drops
    // import-contradicted bare call edges: the correct edge survives, the false
    // one is dropped, dead-code regains precision.
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("db.py"),
        "def save(record):\n    return _write(record)\n\ndef _write(record):\n    return True\n").unwrap();
    std::fs::write(src.join("cache.py"),
        "def save(item):\n    return _store(item)\n\ndef _store(item):\n    return True\n").unwrap();
    std::fs::write(src.join("app.py"),
        "from db import save\n\ndef run():\n    return save({\"id\": 1})\n").unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    // run() must call EXACTLY ONE save — the imported db.save — not fan out.
    let (stdout, _, code) = run_cli(&project, &["callgraph", "run", "--json"]);
    assert_eq!(code, 0, "callgraph run should succeed; {stdout}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let results = v["results"].as_array().expect("results array");
    let save_callees: Vec<&serde_json::Value> =
        results.iter().filter(|r| r["name"] == "save").collect();
    assert_eq!(save_callees.len(), 1,
        "run must call exactly one save (the imported db.save), not fan out to cache.save; got: {stdout}");
    assert_eq!(save_callees[0]["file_path"], "src/db.py",
        "the surviving save edge must be db.save (imported), not cache.save; got: {stdout}");

    // cache.save must have NO caller — `run` imports from db, not cache.
    let (stdout2, _, _) = run_cli(&project, &["callgraph", "save", "--file", "src/cache.py", "--json"]);
    let v2: serde_json::Value = serde_json::from_str(stdout2.trim()).unwrap();
    let cache_results = v2["results"].as_array().cloned().unwrap_or_default();
    assert!(!cache_results.iter().any(|r| r["name"] == "run"),
        "cache.save must have no `run` caller (run imports save from db, not cache); got: {stdout2}");
}

#[test]
fn test_cli_callgraph_no_import_tie_keeps_both() {
    // No-regression guard for the Phase 2d prune: when a bare call ties across
    // same-name same-language nodes with NO disambiguating import edge,
    // refine_ambiguous_targets deliberately keeps BOTH (so Rust scoped-call
    // dead-code precision holds). The import-contradiction prune must NOT fire
    // here — there is no import edge to contradict either target.
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a.rs"), "pub fn thing() -> i32 { 1 }\n").unwrap();
    std::fs::write(src.join("b.rs"), "pub fn thing() -> i32 { 2 }\n").unwrap();
    std::fs::write(src.join("main.rs"),
        "mod a;\nmod b;\nfn main() {\n    let x = thing();\n    println!(\"{}\", x);\n}\n").unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    let (stdout, _, code) = run_cli(&project, &["callgraph", "main", "--json"]);
    assert_eq!(code, 0, "callgraph main should succeed; {stdout}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let results = v["results"].as_array().expect("results array");
    let thing_callees: Vec<&serde_json::Value> =
        results.iter().filter(|r| r["name"] == "thing").collect();
    assert_eq!(thing_callees.len(), 2,
        "with no import to disambiguate, both tied `thing` edges must be kept (no over-prune); got: {stdout}");
}

#[test]
fn test_cli_callgraph_prune_keeps_qualified_call_to_same_name() {
    // Regression for the Phase 2d false-prune guard. A file that BOTH bare-calls
    // an imported `save` (from db) AND qualified-calls `cache.save()` produces two
    // call edges that dedup into NULL-metadata rows — Python extracts
    // `cache.save()` WITHOUT receiver metadata, so it looks identical to a bare
    // fan-out edge. Without the guard, the import-contradiction prune deleted the
    // legitimate run→cache.save edge (worst-direction regression: dropping a real
    // edge). The guard (caller source contains `.save(`) keeps it.
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("db.py"),
        "def save(r):\n    return _w(r)\n\ndef _w(r):\n    return True\n").unwrap();
    std::fs::write(src.join("cache.py"),
        "def save(i):\n    return _s(i)\n\ndef _s(i):\n    return True\n").unwrap();
    // bare imported call to db.save + qualified call to cache.save — both legit.
    std::fs::write(src.join("app.py"),
        "from db import save\nimport cache\n\ndef run():\n    save({\"id\": 1})\n    return cache.save({\"id\": 2})\n").unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    let (stdout, _, code) = run_cli(&project, &["callgraph", "run", "--json"]);
    assert_eq!(code, 0, "callgraph run should succeed; {stdout}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let results = v["results"].as_array().expect("results array");
    let save_files: std::collections::HashSet<&str> = results.iter()
        .filter(|r| r["name"] == "save")
        .filter_map(|r| r["file_path"].as_str())
        .collect();
    assert!(save_files.contains("src/db.py"),
        "the bare imported call must keep run→db.save; got: {stdout}");
    assert!(save_files.contains("src/cache.py"),
        "the qualified cache.save() call must NOT be false-pruned; got: {stdout}");
}

// ============================================================
// stats (clap-migrated, audit #4) — contract lock
// ============================================================

#[test]
fn test_cli_stats_no_data() {
    // Freshly-indexed project has no usage.jsonl yet → handler returns Ok (exit 0).
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["stats"]);
    assert_eq!(code, 0, "stats with no usage data should exit 0; stderr={stderr:?}");
    assert!(stderr.contains("No usage data"), "should explain absence; got: {stderr:?}");
}

#[test]
fn test_cli_stats_json_no_data() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["stats", "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["sessions"], 0);
}

#[test]
fn test_cli_stats_last_valid_parses() {
    let project = setup_indexed_project();
    let (_, _, code) = run_cli(&project, &["stats", "--last", "5"]);
    assert_eq!(code, 0, "valid --last should parse and run");
}

// Flavor-B contract: clap rejects a non-numeric --last with exit 2 (was: warn +
// show-all under the hand parser). Locks the idiomatic parse-error behavior.
#[test]
fn test_cli_stats_invalid_last_errors() {
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["stats", "--last", "abc"]);
    assert_eq!(code, 2, "non-numeric --last must be a clap parse error (exit 2); stderr={stderr:?}");
    assert!(stderr.contains("invalid value") && stderr.contains("abc"),
        "clap should name the bad value; got: {stderr:?}");
}

#[test]
fn test_cli_stats_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["stats", "--help"]);
    assert_eq!(code, 0, "stats --help should exit 0 (clap help)");
    assert!(stdout.contains("Aggregate session metrics") || stdout.contains("--last"),
        "help should describe the command; got: {stdout:?}");
}

#[test]
fn test_cli_stats_unknown_flag_errors() {
    // Flavor-B: clap rejects unknown flags (was: silently ignored).
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["stats", "--bogus"]);
    assert_eq!(code, 2, "unknown flag must error under clap; stderr={stderr:?}");
    assert!(stderr.contains("unexpected") || stderr.contains("--bogus") || stderr.contains("unrecognized"),
        "clap should name the unknown flag; got: {stderr:?}");
}

// Dark-metric visibility: when usage data is present but recommendations.jsonl
// is absent, `stats` must SAY so (the recording hooks aren't active here) rather
// than silently skipping the block — that silence is what hid the dark metric.
#[test]
fn test_cli_stats_recommendations_dark_when_absent() {
    let project = setup_indexed_project();
    // One real session so stats reaches the conversion block (sessions==0 bails).
    std::fs::write(
        project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR).join("usage.jsonl"),
        "{\"ts\":\"2026-06-01T00:00:00Z\",\"v\":\"0.45.4\",\"tools\":{\"get_call_graph\":{\"n\":1,\"ms\":5,\"err\":0,\"max_ms\":5}}}\n",
    ).unwrap();
    // No recommendations.jsonl written → dark.
    let (stdout, _, code) = run_cli(&project, &["stats"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("DARK") && stdout.contains("recommendations.jsonl"),
        "stats must surface the dark conversion metric when recommendations.jsonl is absent; got: {stdout:?}");

    let (jstdout, _, jcode) = run_cli(&project, &["stats", "--json"]);
    assert_eq!(jcode, 0);
    let v: serde_json::Value = serde_json::from_str(jstdout.trim()).unwrap();
    assert_eq!(v["recommendations"]["state"], "absent",
        "JSON stats must mark recommendations.state=absent; got: {jstdout:?}");
}

#[test]
fn test_cli_stats_recommendations_empty_distinct_from_absent() {
    let project = setup_indexed_project();
    let cg = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::write(
        cg.join("usage.jsonl"),
        "{\"ts\":\"2026-06-01T00:00:00Z\",\"v\":\"0.45.4\",\"tools\":{\"get_call_graph\":{\"n\":1,\"ms\":5,\"err\":0,\"max_ms\":5}}}\n",
    ).unwrap();
    // Present but empty → "live but no data", NOT dark.
    std::fs::write(cg.join("recommendations.jsonl"), "").unwrap();
    let (jstdout, _, jcode) = run_cli(&project, &["stats", "--json"]);
    assert_eq!(jcode, 0);
    let v: serde_json::Value = serde_json::from_str(jstdout.trim()).unwrap();
    assert_eq!(v["recommendations"]["state"], "empty",
        "an empty recommendations.jsonl is 'empty', distinct from 'absent'; got: {jstdout:?}");
}

// Deny→use funnel: stats must print the per-session attribution line when usage
// records carry the window-joined `recs` field.
#[test]
fn test_cli_stats_deny_to_use_funnel() {
    let project = setup_indexed_project();
    let cg = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    // Two deny-sessions, one of which called a cg query tool → 1/2 = 50%.
    let s1 = "{\"ts\":\"2026-06-10T10:00:00Z\",\"v\":\"0.45.4\",\"tools\":{\"get_call_graph\":{\"n\":1,\"ms\":5,\"err\":0,\"max_ms\":5}},\"recs\":{\"deny\":1,\"hint\":0}}";
    let s2 = "{\"ts\":\"2026-06-10T11:00:00Z\",\"v\":\"0.45.4\",\"tools\":{},\"recs\":{\"deny\":1,\"hint\":0}}";
    std::fs::write(cg.join("usage.jsonl"), format!("{s1}\n{s2}\n")).unwrap();
    let (stdout, _, code) = run_cli(&project, &["stats"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("Deny→use: 1/2 deny-sessions also called cg = 50%"),
        "stats must print the deny→use funnel; got: {stdout:?}");

    let (jstdout, _, jcode) = run_cli(&project, &["stats", "--json"]);
    assert_eq!(jcode, 0);
    let v: serde_json::Value = serde_json::from_str(jstdout.trim()).unwrap();
    let funnel = &v["recommendations"]["funnel"];
    assert_eq!(funnel["deny_sessions"], 2);
    assert_eq!(funnel["deny_then_cg"], 1);
    assert_eq!(funnel["deny_conversion"], 0.5);
}

// ============================================================
// benchmark (clap-migrated, audit #4) — contract lock
// ============================================================

#[test]
fn test_cli_benchmark_runs() {
    let project = setup_indexed_project();
    let (_, _, code) = run_cli(&project, &["benchmark"]);
    assert_eq!(code, 0, "benchmark should run to completion on the fixture");
}

#[test]
fn test_cli_benchmark_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["benchmark", "--help"]);
    assert_eq!(code, 0, "benchmark --help should exit 0 (clap help)");
    assert!(stdout.contains("Benchmark") || stdout.contains("--json"),
        "help should describe the command; got: {stdout:?}");
}

#[test]
fn test_cli_benchmark_unknown_flag_errors() {
    let project = setup_indexed_project();
    let (_, _, code) = run_cli(&project, &["benchmark", "--bogus"]);
    assert_eq!(code, 2, "unknown flag must error under clap");
}

// ============================================================
// show
// ============================================================

#[test]
fn test_cli_show() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["show", "validateToken"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("validateToken"));
    // Should include code content
    assert!(stdout.contains("token"), "should show code content");
}

#[test]
fn test_cli_show_nonexistent() {
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["show", "nonexistent_fn"]);
    assert_ne!(code, 0, "nonexistent symbol should return non-zero exit code");
    assert!(stderr.contains("Symbol not found"));
}

// Regression: when DB doesn't store `Class.method` as qualified_name (free
// functions, languages where parser omits class prefix), `show Foo.bar` used
// to fail "Symbol not found" even though `callgraph Foo.bar` resolved fine.
// New behavior: fall back to base-name match — consistent with callgraph/impact.
#[test]
fn test_cli_show_qualified_falls_back_to_base_name() {
    let project = setup_indexed_project();
    // `validateToken` is a free function — its qualified_name is just "validateToken",
    // not "Auth.validateToken". Old fallback filter required exact qualified match
    // and silently returned [] when DB had only the base name.
    let (stdout, stderr, code) = run_cli(&project, &["show", "Imaginary.validateToken", "--json"]);
    assert_eq!(code, 0, "qualified-name with no DB match should fall back to base name; stderr={stderr:?}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let arr = v.as_array().unwrap();
    assert!(!arr.is_empty(), "should find validateToken via base-name fallback; got {stdout:?}");
}

#[test]
fn test_cli_show_json() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["show", "validateToken", "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(v.is_array(), "JSON output should be array");
    let arr = v.as_array().unwrap();
    assert!(!arr.is_empty());
    assert!(arr[0]["code_content"].is_string(), "should include code_content field");
}

// clap-migrated (audit #4 Step 5): clap owns --help + unknown-flag rejection. The
// optional positional is gated on --node-id in the handler (exit-1 Usage when both
// absent), and the three --refs spellings stay accepted via hidden clap aliases.
#[test]
fn test_cli_show_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["show", "--help"]);
    assert_eq!(code, 0, "show --help should exit 0 (clap help)");
    assert!(stdout.contains("symbol details") || stdout.contains("--node-id"),
        "help should describe the command; got: {stdout:?}");
}

#[test]
fn test_cli_show_unknown_flag_errors() {
    let project = setup_indexed_project();
    let (_, _, code) = run_cli(&project, &["show", "validateToken", "--bogus"]);
    assert_eq!(code, 2, "unknown flag must error under clap");
}

// The three --refs spellings (--refs / --include-refs / --include-references) must
// stay interchangeable as hidden aliases of one flag.
#[test]
fn test_cli_show_refs_aliases_equivalent() {
    let project = setup_indexed_project();
    let (out_a, _, code_a) = run_cli(&project, &["show", "validateToken", "--refs", "--json"]);
    let (out_b, _, code_b) = run_cli(&project, &["show", "validateToken", "--include-refs", "--json"]);
    let (out_c, _, code_c) = run_cli(&project, &["show", "validateToken", "--include-references", "--json"]);
    assert_eq!(code_a, 0);
    assert_eq!((code_a, code_b, code_c), (0, 0, 0), "all three --refs spellings must succeed");
    assert_eq!(out_a.trim(), out_b.trim(), "--refs and --include-refs must be identical");
    assert_eq!(out_a.trim(), out_c.trim(), "--refs and --include-references must be identical");
}

// ============================================================
// map
// ============================================================

#[test]
fn test_cli_map() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["map"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("Modules:"), "should have modules section");
    assert!(stdout.contains("src"), "should list src module");
}

#[test]
fn test_cli_map_compact() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["map", "--compact"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("Modules:"));
}

#[test]
fn test_cli_map_json() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["map", "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(v["modules"].is_array());
}

// clap-migrated (audit #4): clap owns --help + unknown-flag rejection.
#[test]
fn test_cli_map_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["map", "--help"]);
    assert_eq!(code, 0, "map --help should exit 0 (clap help)");
    assert!(stdout.contains("architecture map") || stdout.contains("--compact"),
        "help should describe the command; got: {stdout:?}");
}

#[test]
fn test_cli_map_unknown_flag_errors() {
    // Flavor-B: clap rejects unknown flags (was: silently ignored by the hand parser).
    let project = setup_indexed_project();
    let (_, _, code) = run_cli(&project, &["map", "--bogus"]);
    assert_eq!(code, 2, "unknown flag must error under clap");
}

// ============================================================
// overview
// ============================================================

#[test]
fn test_cli_overview() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["overview", "src/"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("function:"), "should group by type");
    assert!(stdout.contains("validateToken"));
}

#[test]
fn test_cli_overview_compact() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["overview", "src/", "--compact"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("validateToken"));
    // Compact: no caller counts
    assert!(!stdout.contains("×)"), "compact should not show caller counts");
}

// clap-migrated (audit #4): clap owns --help + unknown-flag rejection; the
// empty-path guard (test_cli_overview_empty_path_errors) is preserved in the
// handler since clap accepts an empty-string positional.
#[test]
fn test_cli_overview_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["overview", "--help"]);
    assert_eq!(code, 0, "overview --help should exit 0 (clap help)");
    assert!(stdout.contains("Module overview") || stdout.contains("PATH"),
        "help should describe the command; got: {stdout:?}");
}

#[test]
fn test_cli_overview_unknown_flag_errors() {
    let project = setup_indexed_project();
    let (_, _, code) = run_cli(&project, &["overview", "src/", "--bogus"]);
    assert_eq!(code, 2, "unknown flag must error under clap");
}

#[test]
fn test_cli_overview_nonexistent_path() {
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["overview", "nonexistent/"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("No symbols found"));
}

// Regression: `.` must normalize to "project root" — same as MCP module_overview.
// Previously CLI only stripped `./`, so `.` produced LIKE pattern `.%` matching nothing.
#[test]
fn test_cli_overview_dot_means_project_root() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["overview", "."]);
    assert_eq!(code, 0, "overview . should succeed; got stdout={stdout:?}");
    assert!(stdout.contains("validateToken"),
        "overview . should list symbols across the project; got: {stdout:?}");
}

// Regression: absolute paths under the project root must normalize to project-relative.
// Indexed `file_path` columns are project-relative, so users pasting absolute paths
// from an IDE previously got "No symbols found" (overview), silent exit-0 "No dead
// code found" (dead-code), or bogus barrel_scan fallback (deps).
#[test]
fn test_cli_overview_absolute_path_under_root() {
    let project = setup_indexed_project();
    let abs = project.path().join("src");
    let (stdout, stderr, code) = run_cli(&project, &["overview", abs.to_str().unwrap()]);
    assert_eq!(code, 0, "absolute path under root should succeed; stderr={stderr:?}");
    assert!(stdout.contains("validateToken"),
        "should list symbols just like `overview src`; got: {stdout:?}");
}

#[test]
fn test_cli_overview_absolute_path_outside_root_errors() {
    let project = setup_indexed_project();
    // Create a sibling dir outside the project for a deterministic "outside" path.
    let outside = TempDir::new().unwrap();
    let (_, stderr, code) = run_cli(&project, &["overview", outside.path().to_str().unwrap()]);
    assert_eq!(code, 1, "absolute path outside root should error");
    assert!(stderr.contains("outside the project root"),
        "stderr should explain the path is outside the project root; got {stderr:?}");
}

#[test]
fn test_cli_deps_absolute_path_under_root() {
    let project = setup_indexed_project();
    let abs = project.path().join("src/api.ts");
    let (stdout, _, code) = run_cli(&project, &["deps", abs.to_str().unwrap(), "--json"]);
    assert_eq!(code, 0);
    // Must surface the relative path in JSON + real depends_on edges (not barrel_scan).
    assert!(stdout.contains("\"file\":\"src/api.ts\""),
        "deps JSON should normalize file to project-relative; got {stdout:?}");
    assert!(!stdout.contains("barrel_scan"),
        "deps must find tracked edges for abs path, not fall back to barrel_scan; got {stdout:?}");
}

#[test]
fn test_cli_dead_code_absolute_path_under_root_matches_relative() {
    let project = setup_indexed_project();
    let (rel_stdout, _, rel_code) = run_cli(&project, &["dead-code", "src"]);
    let abs = project.path().join("src");
    let (abs_stdout, _, abs_code) = run_cli(&project, &["dead-code", abs.to_str().unwrap()]);
    assert_eq!(rel_code, abs_code,
        "abs path under root should match relative behavior exactly");
    assert_eq!(rel_stdout, abs_stdout,
        "abs/rel results must be identical (was: abs silently returned no results)");
}

// Regression (#4): `--ignore` must take a value (be in VALUE_FLAGS), so its value
// is not mistaken for the scan path. Before the fix, `dead-code --ignore <pref> <path>`
// scanned <pref> while `dead-code <path> --ignore <pref>` scanned <path> — same args,
// opposite answer, both exit 0.
#[test]
fn test_cli_dead_code_ignore_before_path_equals_after() {
    let project = setup_indexed_project();
    // Use an ignore prefix that excludes nothing real, so the two orderings must
    // produce the identical (non-trivially-empty when src has dead code) result.
    let (before, _, before_code) =
        run_cli(&project, &["dead-code", "--ignore", "zzz_nonexistent/", "src", "--json"]);
    let (after, _, after_code) =
        run_cli(&project, &["dead-code", "src", "--ignore", "zzz_nonexistent/", "--json"]);
    assert_eq!(before_code, after_code, "exit codes must match regardless of flag order");
    assert_eq!(before.trim(), after.trim(),
        "--ignore before vs after the path must yield identical results");
}

// Regression (#4): a misspelled --type must error loudly, not fall through to a
// literal n.type match that returns zero rows ("No dead code found", exit 0).
#[test]
fn test_cli_dead_code_rejects_misspelled_type() {
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["dead-code", "src", "--type", "fucntion"]);
    assert_ne!(code, 0, "misspelled --type must error, not exit 0 clean; stderr={stderr:?}");
    assert!(stderr.contains("Unknown type filter"),
        "stderr should name the bad type filter; got: {stderr:?}");
}

// clap-migrated (audit #4): clap owns --help + unknown-flag rejection. The
// --node-type/--type alias, repeatable --ignore, and --no-ignore default-clearing
// are preserved by the handler (see ignore_before_path_equals_after / json_empty).
#[test]
fn test_cli_dead_code_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["dead-code", "--help"]);
    assert_eq!(code, 0, "dead-code --help should exit 0 (clap help)");
    assert!(stdout.contains("unused code") || stdout.contains("--ignore"),
        "help should describe the command; got: {stdout:?}");
}

#[test]
fn test_cli_dead_code_unknown_flag_errors() {
    let project = setup_indexed_project();
    let (_, _, code) = run_cli(&project, &["dead-code", "src", "--bogus"]);
    assert_eq!(code, 2, "unknown flag must error under clap");
}

// clap models --node-type and its --type alias as a single arg, so supplying
// BOTH (a contradictory invocation) is a duplicate-arg error (exit 2). This is
// deliberately stricter than the old hand parser, which silently honored
// --node-type and ignored --type — masking a typo'd --type. Locked as an
// intentional flavor-B contract, not an accident (audit #4 verify pass).
#[test]
fn test_cli_dead_code_type_and_node_type_conflict_errors() {
    let project = setup_indexed_project();
    let (_, _, code) =
        run_cli(&project, &["dead-code", "src", "--type", "fn", "--node-type", "class"]);
    assert_eq!(code, 2, "supplying both --type and --node-type must error (clap duplicate-arg)");
}

// The --node-type preferred spelling must work identically to its --type alias.
#[test]
fn test_cli_dead_code_node_type_alias_matches_type() {
    let project = setup_indexed_project();
    let (out_type, _, code_type) =
        run_cli(&project, &["dead-code", "src", "--type", "fn", "--json"]);
    let (out_node, _, code_node) =
        run_cli(&project, &["dead-code", "src", "--node-type", "fn", "--json"]);
    assert_eq!(code_type, code_node, "--type and --node-type must agree on exit code");
    assert_eq!(out_type.trim(), out_node.trim(),
        "--type fn and --node-type fn must yield identical results");
}

// Regression: empty `--json` overview must keep stdout clean (`[]`) and avoid the
// anyhow `Error:` stderr prefix. Exit code stays 1 because the requested path
// matched nothing — mirrors `show --json` / `trace --json` empty contracts.
// Previously `anyhow::bail!` after `println!("[]")` smeared `Error: ...` on stderr,
// breaking log consumers (feedback_cli_json_empty_contract.md).
#[test]
fn test_cli_overview_json_empty_no_anyhow_prefix() {
    let project = setup_indexed_project();
    let (stdout, stderr, code) = run_cli(&project, &["overview", "nonexistent/", "--json"]);
    assert_eq!(code, 1, "JSON empty overview exit code; stderr={stderr:?}");
    assert_eq!(stdout.trim(), "[]", "stdout must be exactly `[]`; got {stdout:?}");
    assert!(!stderr.contains("Error:"),
        "JSON mode must not emit anyhow `Error:` prefix on stderr; got {stderr:?}");
    assert!(stderr.contains("No symbols found"),
        "stderr must still surface the human-readable reason; got {stderr:?}");
}

// ============================================================
// deps
// ============================================================

#[test]
fn test_cli_deps() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["deps", "src/api.ts"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("src/api.ts"), "should show the file");
    assert!(stdout.contains("src/auth.ts"), "api.ts depends on auth.ts");
}

#[test]
fn test_cli_deps_direction() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["deps", "src/auth.ts", "--direction", "incoming"]);
    assert_eq!(code, 0);
    // api.ts imports from auth.ts, so auth.ts has incoming dependency
    assert!(stdout.contains("src/api.ts") || stdout.is_empty() || stdout.contains("Depended by"),
        "should show incoming deps or be empty, got: {}", stdout);
}

#[test]
fn test_cli_deps_invalid_direction() {
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["deps", "src/api.ts", "--direction", "foo"]);
    assert_ne!(code, 0);
    assert!(stderr.contains("must be one of"));
}

#[test]
fn test_cli_deps_json() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["deps", "src/api.ts", "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(v["depends_on"].is_array());
    assert!(v["depended_by"].is_array());
}

// clap-migrated (audit #4): clap owns --help + unknown-flag rejection. --direction
// stays a String validated in-handler, so test_cli_deps_invalid_direction's exact
// "must be one of" + exit-1 contract survives (a clap ValueEnum would change both).
#[test]
fn test_cli_deps_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["deps", "--help"]);
    assert_eq!(code, 0, "deps --help should exit 0 (clap help)");
    assert!(stdout.contains("dependency graph") || stdout.contains("--direction"),
        "help should describe the command; got: {stdout:?}");
}

#[test]
fn test_cli_deps_unknown_flag_errors() {
    let project = setup_indexed_project();
    let (_, _, code) = run_cli(&project, &["deps", "src/api.ts", "--bogus"]);
    assert_eq!(code, 2, "unknown flag must error under clap");
}

// ============================================================
// ast-search
// ============================================================

#[test]
fn test_cli_ast_search_type_filter() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["ast-search", "--type", "fn"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("fn "), "should find functions");
}

#[test]
fn test_cli_ast_search_class_filter() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["ast-search", "--type", "class"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("Logger"), "should find Logger class");
}

#[test]
fn test_cli_ast_search_invalid_type() {
    // Regression: --type INVALID used to print a stderr warning and exit 0
    // with "No results matching filters" because an unknown alias normalizes
    // to an empty Vec which silently filters every node. Must error out so
    // users see the typo instead of believing the index is empty.
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["ast-search", "--type", "INVALID_TYPE"]);
    assert_ne!(code, 0, "invalid --type should fail");
    assert!(stderr.contains("Unknown type filter"), "should explain the typo; got: {stderr}");
}

// clap-migrated (audit #4): clap owns --help + unknown-flag rejection; the
// query-or-filter requirement stays a handler bail (exit 1), and clap accepts an
// empty-string positional so `ast-search ""` still hits that handler check.
#[test]
fn test_cli_ast_search_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["ast-search", "--help"]);
    assert_eq!(code, 0, "ast-search --help should exit 0 (clap help)");
    assert!(stdout.contains("Structured search") || stdout.contains("--returns"),
        "help should describe the command; got: {stdout:?}");
}

#[test]
fn test_cli_ast_search_unknown_flag_errors() {
    let project = setup_indexed_project();
    let (_, _, code) = run_cli(&project, &["ast-search", "--type", "fn", "--bogus"]);
    assert_eq!(code, 2, "unknown flag must error under clap");
}

#[test]
fn test_cli_ast_search_no_query_no_filter_errors() {
    // Neither a query nor any filter → handler bail (exit 1 + Usage), NOT a clap
    // required-arg error: the positional is optional, the requirement is semantic.
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["ast-search"]);
    assert_eq!(code, 1, "no query and no filter must exit 1; stderr={stderr:?}");
    assert!(stderr.contains("Usage:") || stderr.contains("at least one filter"),
        "should explain query-or-filter requirement; got: {stderr:?}");
}

#[test]
fn test_cli_overview_empty_path_errors() {
    // Regression: overview "" used to be silently treated like overview "."
    // (match-all alias), which is almost always a shell-variable substitution
    // bug. Must surface as an error so users see the empty value.
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["overview", ""]);
    assert_ne!(code, 0, "empty path should fail");
    assert!(stderr.contains("must not be empty"), "should explain; got: {stderr}");
}

#[test]
fn test_cli_search_invalid_node_type() {
    // Same regression as ast-search: --node-type INVALID was silently dropped.
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["search", "Logger", "--node-type", "INVALID_TYPE"]);
    assert_ne!(code, 0, "invalid --node-type should fail");
    assert!(stderr.contains("Unknown node-type filter"), "should explain the typo; got: {stderr}");
}

// ============================================================
// trace (no HTTP routes in test project, so test graceful handling)
// ============================================================

#[test]
fn test_cli_trace_no_routes() {
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["trace", "/api/login"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("No routes matching"), "should report no routes found");
}

// clap-migrated (audit #4): clap owns --help + unknown-flag rejection.
#[test]
fn test_cli_trace_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["trace", "--help"]);
    assert_eq!(code, 0, "trace --help should exit 0 (clap help)");
    assert!(stdout.contains("Trace HTTP") || stdout.contains("--no-middleware"),
        "help should describe the command; got: {stdout:?}");
}

// User-approved migration decision (audit #4): --no-middleware is the real flag
// (middleware shown by default); the previously-advertised-but-ignored phantom
// --include-middleware is dropped, so it now errors like any stray flag.
#[test]
fn test_cli_trace_no_middleware_accepted_include_middleware_rejected() {
    let project = setup_indexed_project();
    // --no-middleware is accepted: same no-routes exit 1 as the bare invocation
    // (NOT a clap unknown-flag exit 2).
    let (_, _, code_no) = run_cli(&project, &["trace", "/api/nonexistent", "--no-middleware"]);
    assert_eq!(code_no, 1, "--no-middleware must be accepted (no-routes exit 1, not unknown-flag 2)");
    // --include-middleware is the dropped phantom: clap unknown-flag exit 2.
    let (_, _, code_inc) = run_cli(&project, &["trace", "/api/nonexistent", "--include-middleware"]);
    assert_eq!(code_inc, 2, "dropped phantom --include-middleware must error as unknown flag");
}

// Numeric flags reject a leading-dash value (`--depth -5`): clap reads `-5` as a
// stray token → exit 2. The old hand parser accepted it and clamped to 1 (exit 0).
// Negative depth/limit is nonsensical, so erroring surfaces the typo instead of
// silently coercing — a deliberate, uniform flavor-B contract across every
// migrated numeric flag (search/ast-search --limit, deps/trace --depth). Locked
// here for trace as the representative case (audit #4 Step-3 verify pass).
#[test]
fn test_cli_trace_negative_depth_rejected() {
    let project = setup_indexed_project();
    let (_, _, code) = run_cli(&project, &["trace", "/api/x", "--depth", "-5"]);
    assert_eq!(code, 2, "a negative --depth must error (was: silently clamped to 1)");
}

// ============================================================
// incremental-index
// ============================================================

#[test]
fn test_cli_incremental_index() {
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["incremental-index"]);
    assert_eq!(code, 0);
    assert!(stderr.contains("Incremental index:"), "should show index stats");
}

// clap-migrated (audit #4) contract lock. Flag parsing flipped to clap while the
// git/index guard stays in main(); --quiet still suppresses output (valid path
// above), and clap now owns help + unknown-flag rejection.
#[test]
fn test_cli_incremental_index_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["incremental-index", "--help"]);
    assert_eq!(code, 0, "incremental-index --help should exit 0 (clap help)");
    assert!(stdout.contains("incremental index") || stdout.contains("--quiet"),
        "help should describe the command; got: {stdout:?}");
}

#[test]
fn test_cli_incremental_index_unknown_flag_errors() {
    // Flavor-B: clap rejects unknown flags (was: silently ignored). Parse error
    // exits 2 before the git/index guard or resolve_project_root run.
    let project = setup_indexed_project();
    let (_, _, code) = run_cli(&project, &["incremental-index", "--bogus"]);
    assert_eq!(code, 2, "unknown flag must error under clap");
}

// ============================================================
// reindex (clap-migrated, audit #4) — contract lock (0 prior tests)
// ============================================================

#[test]
fn test_cli_reindex_runs() {
    // Plain `reindex` (no --from-snapshot) resets + re-indexes via cmd_incremental_index.
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["reindex"]);
    assert_eq!(code, 0, "reindex should run to completion; stderr={stderr:?}");
    assert!(stderr.contains("Incremental index:"), "should show index stats; got: {stderr:?}");
}

#[test]
fn test_cli_reindex_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["reindex", "--help"]);
    assert_eq!(code, 0, "reindex --help should exit 0 (clap help)");
    assert!(stdout.contains("snapshot") || stdout.contains("--from-snapshot"),
        "help should describe the command; got: {stdout:?}");
}

#[test]
fn test_cli_reindex_unknown_flag_errors() {
    // Flavor-B: clap rejects unknown flags (was: silently ignored).
    let project = setup_indexed_project();
    let (_, _, code) = run_cli(&project, &["reindex", "--bogus"]);
    assert_eq!(code, 2, "unknown flag must error under clap");
}

// ============================================================
// rebuild-index (§5 hard op — destructive path requires --confirm)
// ============================================================

#[test]
fn test_cli_rebuild_index_requires_confirm() {
    let project = setup_indexed_project();
    let db_path = project.path()
        .join(code_graph_mcp::domain::CODE_GRAPH_DIR)
        .join("index.db");
    assert!(db_path.exists(), "precondition: indexed project has index.db");
    let pre_size = std::fs::metadata(&db_path).unwrap().len();

    // Without --confirm: must bail non-zero AND leave index.db intact.
    let (_, stderr, code) = run_cli(&project, &["rebuild-index"]);
    assert_ne!(code, 0, "rebuild-index without --confirm must fail");
    assert!(stderr.contains("--confirm"), "stderr should demand --confirm, got: {}", stderr);
    assert!(db_path.exists(), "index.db must survive a rejected rebuild-index");
    let post_size = std::fs::metadata(&db_path).unwrap().len();
    assert_eq!(pre_size, post_size, "index.db size must be unchanged");
}

#[test]
fn test_cli_rebuild_index_with_confirm_rebuilds() {
    let project = setup_indexed_project();
    let db_path = project.path()
        .join(code_graph_mcp::domain::CODE_GRAPH_DIR)
        .join("index.db");
    assert!(db_path.exists());

    // With --confirm: drop + re-create index. File should exist post-run and be non-empty.
    let (_, stderr, code) = run_cli(&project, &["rebuild-index", "--confirm"]);
    assert_eq!(code, 0, "rebuild-index --confirm failed: {}", stderr);
    assert!(db_path.exists(), "index.db must be recreated");
    assert!(std::fs::metadata(&db_path).unwrap().len() > 0, "recreated index.db must be non-empty");
}

// clap-migrated (audit #4) contract lock. The --confirm gate stays an exit-1
// anyhow bail (not a clap-required arg — see test_cli_rebuild_index_requires_confirm
// above), while clap now owns help + unknown-flag rejection (exit 2).
#[test]
fn test_cli_rebuild_index_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["rebuild-index", "--help"]);
    assert_eq!(code, 0, "rebuild-index --help should exit 0 (clap help)");
    assert!(stdout.contains("Drop and rebuild") || stdout.contains("--confirm"),
        "help should describe the command; got: {stdout:?}");
}

#[test]
fn test_cli_rebuild_index_unknown_flag_errors() {
    // Flavor-B: unknown flag is a clap parse error (exit 2), evaluated before the
    // --confirm business gate — so --bogus exits 2, not 1.
    let project = setup_indexed_project();
    let (_, _, code) = run_cli(&project, &["rebuild-index", "--bogus"]);
    assert_eq!(code, 2, "unknown flag must error under clap (exit 2, before confirm gate)");
}

// ============================================================
// refs --node-id (P1-1: MCP parity — node_id is authoritative)
// ============================================================

#[test]
fn test_cli_refs_node_id_envelope() {
    let project = setup_indexed_project();
    // First resolve a known symbol to a node_id via search --json
    let (search_out, _, search_code) = run_cli(&project, &["search", "validateToken", "--json", "--limit", "1"]);
    assert_eq!(search_code, 0, "search must succeed");
    let arr: serde_json::Value = serde_json::from_str(search_out.trim()).unwrap();
    let nid = arr[0]["node_id"].as_i64().expect("search result must expose node_id");

    let (out, _, code) = run_cli(&project, &["refs", "--node-id", &nid.to_string(), "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    // Envelope fields match MCP find_references
    assert!(v["symbol"].is_string(), "envelope must include symbol");
    assert!(v["total_references"].is_number(), "envelope must include total_references");
    assert!(v["by_relation"].is_object(), "envelope must include by_relation map");
    assert!(v["references"].is_array(), "envelope must include references array");
}

// Regression: `--relation` must be validated at the CLI layer, before opening the
// index and before symbol resolution — so a bad --relation on a nonexistent symbol
// reports the relation error, not "symbol not found" (which would mask the typo).
#[test]
fn test_cli_refs_invalid_relation_errors_early() {
    let project = setup_indexed_project();
    // Valid symbol, bad relation → relation error.
    let (_, stderr, code) = run_cli(&project, &["refs", "validateToken", "--relation", "bogus"]);
    assert_ne!(code, 0, "bad --relation should error; stderr={stderr:?}");
    assert!(stderr.contains("--relation must be one of"),
        "stderr should explain the valid relation set; got: {stderr:?}");
    // Nonexistent symbol + bad relation → still the RELATION error (validation
    // precedes resolution), not "Symbol not found".
    let (_, stderr2, code2) = run_cli(&project, &["refs", "definitely_absent_xyz", "--relation", "bogus"]);
    assert_ne!(code2, 0);
    assert!(stderr2.contains("--relation must be one of"),
        "relation validation must precede symbol resolution; got: {stderr2:?}");
}

// clap-migrated (audit #4 Step 5): clap owns --help + unknown-flag rejection;
// --relation stays an in-handler String validated before index-open (above).
#[test]
fn test_cli_refs_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["refs", "--help"]);
    assert_eq!(code, 0, "refs --help should exit 0 (clap help)");
    assert!(stdout.contains("references") || stdout.contains("--relation"),
        "help should describe the command; got: {stdout:?}");
}

#[test]
fn test_cli_refs_unknown_flag_errors() {
    let project = setup_indexed_project();
    let (_, _, code) = run_cli(&project, &["refs", "validateToken", "--bogus"]);
    assert_eq!(code, 2, "unknown flag must error under clap");
}

// ============================================================
// trace --json single-object envelope (P1-4)
// ============================================================

#[test]
fn test_cli_trace_json_single_object_envelope_on_empty() {
    let project = setup_indexed_project();
    let (out, _, code) = run_cli(&project, &["trace", "/api/nonexistent", "--json"]);
    assert_ne!(code, 0, "no-match trace still exits non-zero");
    let v: serde_json::Value = serde_json::from_str(out.trim())
        .expect("trace --json must emit a single parseable JSON object, not JSONL");
    assert!(v.is_object(), "envelope must be an object");
    assert!(v["handlers"].is_array(), "envelope must have handlers array");
}

// ============================================================
// ast-search --json envelope (P2-6: {results, count})
// ============================================================

#[test]
fn test_cli_ast_search_json_envelope() {
    let project = setup_indexed_project();
    let (out, _, code) = run_cli(&project, &["ast-search", "--type", "fn", "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert!(v["results"].is_array(), "ast-search --json must wrap in {{results,count}}");
    assert!(v["count"].is_number(), "ast-search --json must include count");
    let count = v["count"].as_u64().unwrap();
    assert_eq!(count, v["results"].as_array().unwrap().len() as u64);
}

// ============================================================
// Edge cases and validation
// ============================================================

#[test]
fn test_cli_version() {
    let project = TempDir::new().unwrap();
    let (stdout, _, code) = run_cli(&project, &["--version"]);
    assert_eq!(code, 0);
    assert!(stdout.starts_with("code-graph-mcp "));
}

#[test]
fn test_cli_help() {
    let project = TempDir::new().unwrap();
    let (stdout, _, code) = run_cli(&project, &["--help"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("COMMANDS:"));
    assert!(stdout.contains("show"));
    assert!(stdout.contains("deps"));
    assert!(stdout.contains("trace"));
    assert!(stdout.contains("similar"));
}

#[test]
fn test_cli_unknown_command() {
    let project = TempDir::new().unwrap();
    let (_, stderr, code) = run_cli(&project, &["foobar"]);
    assert_ne!(code, 0);
    assert!(stderr.contains("Unknown subcommand"));
}

#[test]
fn test_cli_missing_required_arg() {
    let project = setup_indexed_project();
    // callgraph without symbol
    let (_, stderr, code) = run_cli(&project, &["callgraph"]);
    assert_ne!(code, 0);
    assert!(stderr.contains("Usage:"), "should show usage on missing arg");
}

// clap-migrated (audit #4 Step 5, user-approved): a leading-dash numeric value
// like `--depth -5` is now a stray-token error (exit 2), uniformly across every
// migrated numeric flag. The old hand parser read `-5`, parsed it, and clamped to
// 1 (exit 0) — this test asserted that. Flavor-B change: negative depth is
// nonsensical, so erroring surfaces the typo. Mirrors test_cli_trace_negative_depth_rejected.
#[test]
fn test_cli_callgraph_negative_depth_rejected() {
    let project = setup_indexed_project();
    let (_, _, code) = run_cli(&project, &["callgraph", "validateToken", "--depth", "-5"]);
    assert_eq!(code, 2, "negative --depth must error (was: clamped to 1 / exit 0)");
}

// ============================================================
// JSON empty results — must output valid JSON, not plain text
// ============================================================

#[test]
fn test_cli_search_limit_not_shrunk_by_test_filter() {
    // Regression: cmd_search over-fetched (limit*4) ONLY when a language/node-type
    // filter was set, but the post-fetch filter ALWAYS drops <module> and test
    // symbols. So a plain `search foo --limit K` fetched exactly K FTS rows, dropped
    // the test/module ones, and returned fewer than K — even with K+ real matches in
    // the index. MCP semantic_code_search/ast_search over-fetch unconditionally; the
    // CLI must too. Fixture: 9 real matches + 12 test-file matches for one query.
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();

    // 9 real (production) functions sharing the FTS token "gadget". snake_case so
    // the FTS5 tokenizer splits on `_` and "gadget" matches (camelCase would index
    // as one opaque token and never match a sub-word query).
    let mut real = String::new();
    for i in 0..9 {
        real.push_str(&format!("export function find_gadget_real_{i}(): number {{ return {i}; }}\n"));
    }
    std::fs::write(src.join("widgets.ts"), real).unwrap();

    // 12 test-file functions sharing the same token — is_test_symbol drops these
    // via the `.test.ts` path suffix, so they must NOT crowd out the real results.
    let mut testfns = String::new();
    for i in 0..12 {
        testfns.push_str(&format!("export function find_gadget_case_{i}(): number {{ return {i}; }}\n"));
    }
    std::fs::write(src.join("widgets.test.ts"), testfns).unwrap();

    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    // Ask for 6; there are 9 real matches, so we must get exactly 6 — the test-file
    // matches in the top FTS window must be backfilled past, not subtracted.
    let (stdout, _, code) = run_cli(&project, &["search", "gadget", "--limit", "6", "--json"]);
    assert_eq!(code, 0, "search should succeed; stdout: {stdout}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON array");
    let arr = v.as_array().expect("search --json is an array");
    assert_eq!(
        arr.len(),
        6,
        "search --limit 6 with 9 real matches must return 6 (not shrunk by the always-on \
         test/module filter); got {} results: {stdout}",
        arr.len()
    );
    // None of the returned results may come from the .test.ts file.
    for r in arr {
        let fp = r["file_path"].as_str().unwrap_or("");
        assert!(!fp.ends_with(".test.ts"), "test-file symbol leaked into results: {fp}");
    }
}

#[test]
fn test_cli_json_empty_search() {
    let project = setup_indexed_project();
    let (stdout, stderr, code) = run_cli(&project, &["search", "xyznonexistent", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "[]", "JSON search with no results should output []");
    assert!(stderr.contains("No results"), "stderr should still show hint");
}

#[test]
fn test_cli_json_empty_grep() {
    if !has_ripgrep() { eprintln!("skipping: rg not installed"); return; }
    let project = setup_indexed_project();
    let (stdout, stderr, code) = run_cli(&project, &["grep", "xyznonexistent", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "[]", "JSON grep with no results should output []");
    assert!(stderr.contains("No matches"), "stderr should still show hint");
}

#[test]
fn test_cli_json_empty_callgraph() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["callgraph", "xyznonexistent", "--json"]);
    assert_eq!(code, 1);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(v.is_object(), "JSON callgraph error should output JSON object");
}

#[test]
fn test_cli_json_empty_show() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["show", "xyznonexistent", "--json"]);
    assert_eq!(code, 1);
    assert_eq!(stdout.trim(), "[]", "JSON show with no results should output []");
}

#[test]
fn test_cli_json_empty_show_node_id_missing() {
    // Regression: `show --node-id 999999` for a nonexistent ID exited 1 with
    // empty stdout in --json mode, asymmetric with the symbol-not-found path
    // above which already emits []. Both empty-result paths must agree.
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["show", "--node-id", "9999999", "--json"]);
    assert_eq!(code, 1);
    assert_eq!(stdout.trim(), "[]",
        "JSON show with nonexistent --node-id should output [] (matches symbol-not-found path)");
}

#[test]
fn test_cli_json_empty_trace() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["trace", "/api/nonexistent", "--json"]);
    assert_eq!(code, 1);
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .expect("trace --json must output valid JSON even on no-match");
    assert!(v.is_object(), "JSON trace error should output JSON object");
}

#[test]
fn test_cli_json_empty_overview() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["overview", "nonexistent/", "--json"]);
    assert_eq!(code, 1);
    assert_eq!(stdout.trim(), "[]", "JSON overview with no results should output []");
}

#[test]
fn test_cli_json_empty_dead_code() {
    // Regression: dead-code --json with all results filtered by --ignore returned
    // only stderr (no stdout), breaking JSON consumers piping stdout. Must emit `[]`.
    let project = setup_indexed_project();
    let (stdout, stderr, code) = run_cli(&project, &[
        "dead-code",
        "--ignore", "src/",
        "--ignore", "tests/",
        "--json",
    ]);
    assert_eq!(code, 0, "dead-code with no matches should exit 0");
    assert_eq!(stdout.trim(), "[]", "dead-code --json with no results must output []");
    assert!(
        stderr.contains("No dead code"),
        "stderr should still surface the human-readable reason; got: {stderr}",
    );
}

#[test]
fn test_cli_json_empty_similar() {
    // Regression: `similar <existing-symbol>` where vector search yielded no matches
    // wrote only stderr and exited 0 with empty stdout, breaking JSON consumers.
    // Symbol-not-found path already emits []; this guards the no-match path too.
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["similar", "xyznonexistent", "--json"]);
    assert_eq!(code, 1);
    assert_eq!(stdout.trim(), "[]", "JSON similar with unknown symbol should output []");
}

#[test]
fn test_cli_json_empty_deps() {
    // Regression: `deps <unknown-file>` bailed with stderr only, leaving stdout empty.
    // Must emit a JSON error object on stdout for machine consumers.
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["deps", "src/nonexistent_file_xyz.rs", "--json"]);
    assert_eq!(code, 1);
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .expect("deps --json must output valid JSON even on no-match");
    assert!(v.is_object(), "JSON deps error should output JSON object");
    assert_eq!(v["file"], "src/nonexistent_file_xyz.rs");
}

#[test]
fn test_cli_json_empty_refs() {
    // Regression: `refs <unknown-symbol> --json` returned a bare `[]`, but the
    // success case returns an object {symbol,total_references,by_relation,references}.
    // Object-success commands must emit an object on the empty/error path too so a
    // single consumer parser handles both (matches callgraph/trace/deps). refs was
    // the outlier — `.references` access broke on not-found.
    let project = setup_indexed_project();
    // All three not-found branches must emit the same parseable object envelope:
    // bare symbol, --file (symbol absent from that file), and --node-id (missing id).
    // Previously the --file and --node-id branches bailed via anyhow with EMPTY
    // stdout under --json; only the bare-symbol branch emitted (a wrong `[]`).
    let cases: [&[&str]; 3] = [
        &["refs", "xyznonexistent", "--json"],
        &["refs", "validateToken", "--file", "src/utils.ts", "--json"],
        &["refs", "--node-id", "99999999", "--json"],
    ];
    for args in cases {
        let (stdout, _, code) = run_cli(&project, args);
        assert_eq!(code, 1, "{args:?} should exit 1");
        let v: serde_json::Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|_| panic!("{args:?}: refs --json must output valid JSON even on not-found; got: {stdout:?}"));
        assert!(v.is_object(), "{args:?}: refs --json not-found should be an object, not a bare array; got: {stdout}");
        assert!(v["references"].is_array(), "{args:?}: envelope must include references array");
        assert!(v["by_relation"].is_object(), "{args:?}: envelope must include by_relation map");
    }
}

#[test]
fn test_cli_json_empty_similar_existing_symbol() {
    // Regression: `similar <existing-symbol> --json` against an index with no
    // generated embeddings (vec extension present, embedded_count == 0) hit the
    // "No embeddings found" path, which exited 1 with EMPTY stdout — breaking JSON
    // consumers piping stdout. Every --json exit path must emit parseable stdout.
    // Feature-agnostic: with embed-model + embeddings it returns a results array;
    // without, []. Both are valid JSON — the bug was an empty string.
    let project = setup_indexed_project();
    let (stdout, _, _code) = run_cli(&project, &["similar", "validateToken", "--json"]);
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(stdout.trim());
    assert!(
        parsed.is_ok(),
        "similar --json for an existing symbol must emit parseable JSON on stdout, got: {stdout:?}"
    );
}

#[test]
fn test_cli_similar_node_id_missing_accurate_and_json() {
    // Regression: `similar --node-id <missing>` skipped existence validation, so a
    // missing id fell through to the embedded_count==0 guard and reported a
    // MISLEADING "No embeddings found" instead of "not found". The check now runs
    // up-front (embedding-independent → reachable in the default no-embed build).
    // Must: exit 1, emit parseable JSON on stdout (empty-JSON contract), and the
    // stderr must name the real cause — not the embeddings red herring.
    let project = setup_indexed_project();
    let (stdout, stderr, code) = run_cli(&project, &["similar", "--node-id", "99999999", "--json"]);
    assert_eq!(code, 1);
    serde_json::from_str::<serde_json::Value>(stdout.trim())
        .unwrap_or_else(|_| panic!("similar --node-id missing --json must emit valid JSON; got: {stdout:?}"));
    assert!(
        stderr.contains("not found"),
        "stderr should state the node_id was not found; got: {stderr}"
    );
    assert!(
        !stderr.contains("No embeddings"),
        "missing node_id must NOT be misreported as an embeddings problem; got: {stderr}"
    );
}

#[test]
fn test_cli_similar_digit_positional_suggests_node_id() {
    // Regression: `similar 1010` (digits as positional) used to print a confusing
    // "Symbol not found: 1010" instead of nudging the user toward `--node-id 1010`.
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["similar", "9999"]);
    assert_eq!(code, 1);
    assert!(
        stderr.contains("--node-id 9999"),
        "all-digit positional should suggest --node-id flag; got stderr: {stderr}"
    );
}

// clap-migrated (audit #4 Step 5): `similar` was the last hand-parsed command (the
// plan's step breakdown omitted it); migrating it decommissioned the whole hand
// parser. clap owns --help + unknown-flag rejection.
#[test]
fn test_cli_similar_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["similar", "--help"]);
    assert_eq!(code, 0, "similar --help should exit 0 (clap help)");
    assert!(stdout.contains("similar code") || stdout.contains("--top-k"),
        "help should describe the command; got: {stdout:?}");
}

#[test]
fn test_cli_similar_unknown_flag_errors() {
    let project = setup_indexed_project();
    let (_, _, code) = run_cli(&project, &["similar", "validateToken", "--bogus"]);
    assert_eq!(code, 2, "unknown flag must error under clap");
}

#[test]
fn test_cli_callgraph_requested_depth_preserved() {
    // Regression: CLI used to pre-clamp `--depth` to (1, 20). Server caps to
    // CALL_GRAPH_MAX_DEPTH=10 internally and exposes `requested_max_depth` so
    // callers can see when truncation happened. Pre-clamping to 20 silently
    // rewrote the user's request to 20 in the JSON, defeating the truth signal.
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(
        &project,
        &["callgraph", "validateToken", "--depth", "99", "--json"],
    );
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        v["requested_max_depth"].as_i64(),
        Some(99),
        "requested_max_depth must echo user's value (99), got {}",
        v["requested_max_depth"]
    );
    let eff = v["effective_max_depth"].as_i64().unwrap();
    assert!(eff <= 10, "effective should be capped at CALL_GRAPH_MAX_DEPTH=10, got {eff}");
    assert!(eff < 99, "effective ({eff}) must be visibly less than requested (99)");
}

#[test]
fn test_cli_callgraph_json_includes_parent_id() {
    // Regression: depth>1 callgraph used to render depth-N nodes flat-indented under
    // the last depth-(N-1) sibling. Tree rendering needs `parent_id` on each row.
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(
        &project,
        &["callgraph", "validateToken", "--depth", "2", "--json"],
    );
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let results = v["results"].as_array().expect("results is array");
    let with_parent = results.iter().filter(|r| !r["parent_id"].is_null()).count();
    let depth_gt_zero = results.iter().filter(|r| r["depth"].as_i64().unwrap_or(0) > 0).count();
    assert!(
        with_parent > 0 && with_parent == depth_gt_zero,
        "every non-root row must carry parent_id; with_parent={with_parent} depth>0={depth_gt_zero}"
    );
}
