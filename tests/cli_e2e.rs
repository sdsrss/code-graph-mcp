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
fn test_cli_grep_with_path() {
    if !has_ripgrep() { eprintln!("skipping: rg not installed"); return; }
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["grep", "validateToken", "src/auth.ts"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("validateToken"));
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

#[test]
fn test_cli_callgraph_json() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["callgraph", "validateToken", "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(v["results"].is_array());
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

#[test]
fn test_cli_overview_nonexistent_path() {
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["overview", "nonexistent/"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("No symbols found"));
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

#[test]
fn test_cli_depth_clamping() {
    let project = setup_indexed_project();
    // Negative depth should be clamped to 1 (not error)
    let (stdout, _, code) = run_cli(&project, &["callgraph", "validateToken", "--depth", "-5"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("validateToken"), "should still work with clamped depth");
}

// ============================================================
// JSON empty results — must output valid JSON, not plain text
// ============================================================

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
