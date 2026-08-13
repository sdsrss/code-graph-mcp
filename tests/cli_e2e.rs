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

    std::fs::write(
        src.join("auth.ts"),
        r#"
import jwt from 'jsonwebtoken';

export function validateToken(token: string): boolean {
    const decoded = jwt.verify(token, process.env.SECRET);
    return decoded !== null;
}

export function hashPassword(password: string): string {
    return password; // stub
}
"#,
    )
    .unwrap();

    std::fs::write(
        src.join("api.ts"),
        r#"
import { validateToken } from './auth';

export function handleLogin(req: Request, res: Response) {
    const user = validateToken(req.headers.authorization);
    if (!user) { res.status(401); return; }
    res.json({ userId: user.id });
}

export function handleLogout(req: Request, res: Response) {
    res.json({ ok: true });
}
"#,
    )
    .unwrap();

    std::fs::write(
        src.join("utils.ts"),
        r#"
export function formatDate(date: Date): string {
    return date.toISOString();
}

export class Logger {
    log(msg: string) {
        console.log(msg);
    }
}
"#,
    )
    .unwrap();

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
    run_cli_env(project, args, &[])
}

/// Like `run_cli` but with extra environment variables.
fn run_cli_env(project: &TempDir, args: &[&str], envs: &[(&str, &str)]) -> (String, String, i32) {
    let mut cmd = Command::new(binary_path());
    cmd.current_dir(project.path()).args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let output = cmd.output().expect("failed to run binary");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let code = output.status.code().unwrap_or(-1);
    (stdout, stderr, code)
}

/// Like `run_cli` but with the shell cwd inside a subdirectory of the project
/// (the persistent-shell shape: the agent `cd`'d into a module and stayed there).
fn run_cli_from(project: &TempDir, subdir: &str, args: &[&str]) -> (String, String, i32) {
    let mut cmd = Command::new(binary_path());
    cmd.current_dir(project.path().join(subdir)).args(args);
    let output = cmd.output().expect("failed to run binary");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let code = output.status.code().unwrap_or(-1);
    (stdout, stderr, code)
}

/// Temp project where src/api.ts and src/auth.test.ts both depend on src/auth.ts.
/// auth.test.ts is co-located so the proven `./auth` import resolves, and `.test.ts`
/// makes it a test file via is_test_path.
fn setup_affected_project() -> TempDir {
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();

    std::fs::write(
        src.join("auth.ts"),
        r#"
export function validateToken(token: string): boolean {
    return token.length > 0;
}
"#,
    )
    .unwrap();

    std::fs::write(
        src.join("api.ts"),
        r#"
import { validateToken } from './auth';
export function handleLogin(token: string): boolean {
    return validateToken(token);
}
"#,
    )
    .unwrap();

    std::fs::write(
        src.join("auth.test.ts"),
        r#"
import { validateToken } from './auth';
export function testValidate(): void {
    validateToken('x');
}
"#,
    )
    .unwrap();

    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    project
}

/// Issue #36's layout: a C# monorepo whose xUnit suites live in
/// `src/Tests/<Project>/<Area>/<Name>Tests.cs`. `affected` reported
/// "0 test file(s) to re-run" for a change to a symbol the test calls directly,
/// because `is_test_path` only knew JS/Rust/Go conventions — a silent false
/// negative in the one output a CI or pre-commit hook acts on.
#[test]
fn test_cli_affected_finds_csharp_xunit_tests() {
    let project = TempDir::new().unwrap();
    let lib = project.path().join("src/Libraries/Core");
    let tests = project.path().join("src/Tests/WebApi.Tests/Authorization");
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::create_dir_all(&tests).unwrap();

    std::fs::write(
        lib.join("Enumerations.cs"),
        r#"
namespace DEQ.Core {
    public class Visibility {
        public bool CanView(VisibilityTier minimumTier) { return true; }
    }
}
"#,
    )
    .unwrap();
    std::fs::write(
        tests.join("ResponsibleEntityAuthorizationServiceTests.cs"),
        r#"
namespace DEQ.Tests {
    public class ResponsibleEntityAuthorizationServiceTests {
        public void CanView_returns_false_for_anonymous() {
            var result = new Visibility();
            result.CanView(VisibilityTier.Authorized);
        }
    }
}
"#,
    )
    .unwrap();

    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();

    let (stdout, _, code) = run_cli(
        &project,
        &["affected", "src/Libraries/Core/Enumerations.cs", "--json"],
    );
    assert_eq!(code, 0, "stdout: {stdout}");
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let tests_listed = v["tests"].as_array().unwrap();
    assert!(
        tests_listed.iter().any(|t| t
            .as_str()
            .unwrap()
            .ends_with("ResponsibleEntityAuthorizationServiceTests.cs")),
        "xUnit test file must be reported as a test to re-run, got: {stdout}"
    );
    // …and it must be flagged is_test in the blast radius, not just listed there.
    let flagged = v["affected_files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"].as_str().unwrap().contains("Tests.cs"));
    if let Some(f) = flagged {
        assert_eq!(f["is_test"], serde_json::json!(true), "got: {stdout}");
    }
}

/// Maven/Gradle + xUnit side by side. `OrderCases.java` is deliberately named
/// WITHOUT a `Test` stem: it is a test only because it sits under `src/test/`,
/// so it is the only file here that exercises the directory-segment leg on its
/// own. Without it, disabling that leg leaves this test green — the stem leg
/// covers for it — and the test proves less than it appears to.
fn setup_polyglot_affected_project() -> TempDir {
    let project = TempDir::new().unwrap();
    let write = |rel: &str, body: &str| {
        let p = project.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
    };

    write(
        "src/main/java/com/acme/OrderService.java",
        "package com.acme;\npublic class OrderService {\n    public int total(int qty, int price) { return qty * price; }\n}\n",
    );
    write(
        "src/test/java/com/acme/OrderServiceTest.java",
        "package com.acme;\npublic class OrderServiceTest {\n    public void totalMultiplies() {\n        OrderService svc = new OrderService();\n        svc.total(2, 3);\n    }\n}\n",
    );
    write(
        "src/test/java/com/acme/OrderCases.java",
        "package com.acme;\npublic class OrderCases {\n    public void twoTimesThree() {\n        OrderService svc = new OrderService();\n        svc.total(2, 3);\n    }\n}\n",
    );
    write(
        "src/Acme.Api/OrderController.cs",
        "namespace Acme.Api;\npublic class OrderController {\n    public int Create(int id) { return id; }\n}\n",
    );
    write(
        "src/Tests/Acme.Api/OrderControllerTests.cs",
        "namespace Acme.Api;\npublic class OrderControllerTests {\n    public void CreateReturnsId() {\n        var c = new OrderController();\n        c.Create(1);\n    }\n}\n",
    );

    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    project
}

/// Issue #36 end to end on the JVM layout: `affected` on a Maven production file
/// must name its covering test instead of printing "0 test file(s) to re-run".
#[test]
fn test_cli_affected_finds_jvm_and_dotnet_tests() {
    let project = setup_polyglot_affected_project();

    for (changed, expected_test) in [
        // Stem leg (`…Test.java`)…
        (
            "src/main/java/com/acme/OrderService.java",
            "src/test/java/com/acme/OrderServiceTest.java",
        ),
        // …and the directory-segment leg on its own.
        (
            "src/main/java/com/acme/OrderService.java",
            "src/test/java/com/acme/OrderCases.java",
        ),
        (
            "src/Acme.Api/OrderController.cs",
            "src/Tests/Acme.Api/OrderControllerTests.cs",
        ),
    ] {
        let (stdout, _, code) = run_cli(&project, &["affected", changed, "--json"]);
        assert_eq!(code, 0, "stdout: {stdout}");
        let v: serde_json::Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("invalid json: {e}; raw: {stdout}"));
        let tests: Vec<String> = v["tests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().to_string())
            .collect();
        assert!(
            tests.contains(&expected_test.to_string()),
            "changing {changed} must re-run {expected_test}; got tests={tests:?} raw={stdout}"
        );
    }
}

/// The same classification on the surface a CI integration actually reads: the
/// human-readable count line, which is the literal sentence from the bug report.
#[test]
fn test_cli_affected_text_count_line_counts_jvm_and_dotnet_tests() {
    let project = setup_polyglot_affected_project();
    let (stdout, _, code) = run_cli(
        &project,
        &[
            "affected",
            "src/main/java/com/acme/OrderService.java",
            "src/Acme.Api/OrderController.cs",
        ],
    );
    assert_eq!(code, 0, "stdout: {stdout}");
    assert!(
        !stdout.contains("0 test file(s) to re-run"),
        "regression: JVM/.NET tests went undetected; raw: {stdout}"
    );
    // Scope the assertion to the "to re-run" block. A bare `stdout.contains(path)`
    // is NOT enough: each of these files also appears in the "Full blast radius"
    // listing below it, so the loose form stays green even with the
    // classification legs disabled.
    let rerun_block: Vec<&str> = stdout
        .lines()
        .skip_while(|l| !l.contains("test file(s) to re-run:"))
        .skip(1)
        .take_while(|l| !l.starts_with("Full blast radius:"))
        .map(|l| l.trim())
        .collect();
    for expected in [
        "src/test/java/com/acme/OrderServiceTest.java", // stem leg
        "src/test/java/com/acme/OrderCases.java",       // segment leg alone
        "src/Tests/Acme.Api/OrderControllerTests.cs",   // .NET, capitalized segment
    ] {
        assert!(
            rerun_block.contains(&expected),
            "{expected} must be listed as a test to re-run; re-run block was \
             {rerun_block:?}; raw: {stdout}"
        );
    }
}

/// Issue #36, second half: the blast radius was a flat path-sorted dump, so the
/// depth-1 dependents worth inspecting were buried among depth-8..10 transitive
/// hits. Text output now groups by proximity and caps the list — and says how
/// many it withheld, so a truncated list can never read as "that's everything".
/// `--json` stays uncapped and ungrouped for scripted consumers.
#[test]
fn test_cli_affected_groups_blast_radius_by_depth() {
    let project = setup_affected_project();
    let (stdout, _, code) = run_cli(&project, &["affected", "src/auth.ts"]);
    assert_eq!(code, 0, "stdout: {stdout}");
    assert!(
        stdout.contains("depth 1 ("),
        "blast radius must be grouped by depth, got:\n{stdout}"
    );
    // The grouped form indents members under their depth header rather than
    // suffixing "(depth N)" onto every line.
    assert!(
        !stdout.contains("(depth 1)"),
        "old flat per-line depth suffix must be gone, got:\n{stdout}"
    );

    let (json_out, _, _) = run_cli(&project, &["affected", "src/auth.ts", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&json_out).unwrap();
    assert!(
        v["affected_files"][0]["depth"].is_number(),
        "--json must keep the flat per-file depth field: {json_out}"
    );
}

/// A depth-group header must describe the listing printed under it. With
/// `AFFECTED_DISPLAY_CAP = 40` and a wide fan-in, `depth 1 (60 file(s)):`
/// stood above 40 paths — the count and the list disagreed, and the only
/// correction (`… 20 more at depth 1-N`) is attributed to the whole depth range
/// rather than to this group, so neither a reader nor a script scraping the
/// header could reconcile them.
#[test]
fn test_cli_affected_truncated_depth_header_reports_shown_and_total() {
    const FANIN: usize = 60; // > AFFECTED_DISPLAY_CAP (40)
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("core.ts"),
        "export function coreFn(x: number): number { return x + 1; }\n",
    )
    .unwrap();
    for i in 0..FANIN {
        std::fs::write(
            src.join(format!("dep_{i:02}.ts")),
            format!(
                "import {{ coreFn }} from './core';\n\
                 export function use_{i}(v: number): number {{ return coreFn(v); }}\n"
            ),
        )
        .unwrap();
    }
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();

    let (stdout, _, code) = run_cli(&project, &["affected", "src/core.ts"]);
    assert_eq!(code, 0, "stdout: {stdout}");

    // Locate the depth-1 header and count the paths actually listed under it.
    let header = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("depth 1 ("))
        .unwrap_or_else(|| panic!("no depth 1 header in:\n{stdout}"));
    let listed = stdout
        .lines()
        .skip_while(|l| !l.trim_start().starts_with("depth 1 ("))
        .skip(1)
        .take_while(|l| l.starts_with("    ") && !l.trim_start().starts_with("depth "))
        .count();

    assert!(
        listed < FANIN,
        "test premise: the cap must truncate this group (listed {listed} of {FANIN})\n{stdout}"
    );
    assert!(
        header.contains(&format!("{} of ", listed)),
        "a truncated group header must state how many of the group are shown; \
         got {header:?} above {listed} listed path(s)\n{stdout}"
    );
    assert!(
        header.contains(&format!("of {} file(s) shown", FANIN)),
        "a truncated group header must still state the group total; got {header:?}\n{stdout}"
    );

    // Un-truncated groups keep the plain form.
    let (small_out, _, _) = run_cli(&setup_affected_project(), &["affected", "src/auth.ts"]);
    assert!(
        small_out.contains("depth 1 (") && !small_out.contains(" of "),
        "an un-truncated group must keep the plain `depth N (M file(s)):` form, got:\n{small_out}"
    );
}

#[test]
fn test_cli_affected_json_core() {
    let project = setup_affected_project();
    let (stdout, _, code) = run_cli(&project, &["affected", "src/auth.ts", "--json"]);
    assert_eq!(code, 0, "stdout: {stdout}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("invalid json: {e}; raw: {stdout}"));

    let tests: Vec<String> = v["tests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect();
    assert!(
        tests.contains(&"src/auth.test.ts".to_string()),
        "auth.test.ts must be a test to re-run; got {tests:?}"
    );

    let affected: Vec<String> = v["affected_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["path"].as_str().unwrap().to_string())
        .collect();
    assert!(
        affected.contains(&"src/api.ts".to_string()),
        "api.ts depends on auth.ts and must be in blast radius; got {affected:?}"
    );
    assert_eq!(v["not_indexed"].as_array().unwrap().len(), 0);
}

#[test]
fn test_cli_impact_json_lists_test_callers() {
    // Edit-time covering-test targeting: `impact --json` must surface the test
    // callers' identities (name + file), not just the `tests_affected` count, so a
    // hook can build a runnable test command. setup_affected_project's
    // src/auth.test.ts::testValidate calls validateToken (a test caller); src/api.ts
    // calls it from prod.
    let project = setup_affected_project();
    let (stdout, _, code) = run_cli(
        &project,
        &["impact", "validateToken", "--file", "src/auth.ts", "--json"],
    );
    assert_eq!(code, 0, "stdout: {stdout}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("invalid json: {e}; raw: {stdout}"));

    assert_eq!(
        v["tests_affected"].as_u64().unwrap(),
        1,
        "exactly one test caller (testValidate); raw: {stdout}"
    );
    let test_callers = v["test_callers"]
        .as_array()
        .unwrap_or_else(|| panic!("test_callers must be a JSON array; raw: {stdout}"));
    let names: Vec<&str> = test_callers
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"testValidate"),
        "testValidate must be listed as a covering test; got {names:?}"
    );
    let tv = test_callers
        .iter()
        .find(|c| c["name"] == "testValidate")
        .unwrap();
    assert_eq!(
        tv["file"].as_str().unwrap(),
        "src/auth.test.ts",
        "the test caller carries its file — needed to build the test command"
    );
}

#[test]
fn test_cli_affected_not_indexed_json_envelope() {
    // json-empty contract: unknown input still yields a valid same-shape envelope.
    let project = setup_affected_project();
    let (stdout, _, code) = run_cli(&project, &["affected", "src/ghost.ts", "--json"]);
    assert_eq!(code, 0, "stdout: {stdout}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    assert_eq!(v["tests"].as_array().unwrap().len(), 0);
    assert_eq!(v["affected_files"].as_array().unwrap().len(), 0);
    let ni: Vec<String> = v["not_indexed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect();
    assert_eq!(ni, vec!["src/ghost.ts".to_string()]);
}

#[test]
fn test_cli_affected_bare_invocation_hints() {
    // Misleading-feedback guard: a bare `affected` (no positional files, no
    // --stdin) has no input, so it prints "0 test file(s) to re-run" — which a
    // user who simply forgot the argument could read as "no tests needed". The
    // command does not auto-diff git, so it must point at the intended pipe.
    // stdout stays unchanged; the guidance goes to stderr.
    let project = setup_affected_project();
    let (stdout, stderr, code) = run_cli(&project, &["affected"]);
    assert_eq!(code, 0, "bare affected should exit 0; stdout: {stdout}");
    assert!(
        stderr.contains("No files given") && stderr.contains("--stdin"),
        "bare affected must hint at how to supply input; stderr: {stderr}"
    );
    // stdout still reports the empty result (shape unchanged).
    assert!(
        stdout.contains("0 test file(s) to re-run"),
        "stdout: {stdout}"
    );
}

#[test]
fn test_cli_affected_empty_stdin_pipe_stays_silent() {
    // Gating check: an explicit --stdin pipe that happens to be empty (a clean
    // `git diff`) used the command correctly and found no changes — it must NOT
    // get the "No files given" hint (that would be wrong/annoying for a working
    // pipe). Only the bare no-input invocation is hinted.
    use std::io::Write;
    use std::process::{Command, Stdio};
    let project = setup_affected_project();
    let mut child = Command::new(binary_path())
        .current_dir(project.path())
        .args(["affected", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"").unwrap(); // empty pipe
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("No files given"),
        "empty --stdin pipe must stay silent (it was used correctly); stderr: {stderr}"
    );
}

#[test]
fn test_cli_affected_stdin_matches_positional() {
    let project = setup_affected_project();
    // Pipe the path via stdin instead of positional.
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new(binary_path())
        .current_dir(project.path())
        .args(["affected", "--stdin", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"src/auth.ts\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    let tests: Vec<String> = v["tests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect();
    assert!(
        tests.contains(&"src/auth.test.ts".to_string()),
        "got {tests:?}"
    );
}

#[test]
fn test_cli_affected_changed_test_file_is_self_included() {
    // Changing a test file → that test file is in the re-run set.
    let project = setup_affected_project();
    let (stdout, _, code) = run_cli(&project, &["affected", "src/auth.test.ts", "--json"]);
    assert_eq!(code, 0, "stdout: {stdout}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    let tests: Vec<String> = v["tests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect();
    assert!(
        tests.contains(&"src/auth.test.ts".to_string()),
        "a changed test file must re-run itself; got {tests:?}"
    );
}

#[test]
fn test_cli_affected_dot_input_no_pollution() {
    // F2: `affected .` normalizes to "" → must pollute neither changed nor not_indexed.
    let project = setup_affected_project();
    let (stdout, _, code) = run_cli(&project, &["affected", ".", "--json"]);
    assert_eq!(code, 0, "stdout: {stdout}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    assert_eq!(
        v["changed"].as_array().unwrap().len(),
        0,
        "`.` must not be a changed file"
    );
    assert_eq!(
        v["not_indexed"].as_array().unwrap().len(),
        0,
        "`.` must not be reported not_indexed"
    );
}

#[test]
fn test_cli_affected_nonexistent_test_path_not_in_tests() {
    // F3: a nonexistent test-path input goes to not_indexed only, never the tests set.
    let project = setup_affected_project();
    let (stdout, _, code) = run_cli(&project, &["affected", "src/ghost.test.ts", "--json"]);
    assert_eq!(code, 0, "stdout: {stdout}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    let tests: Vec<String> = v["tests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect();
    let ni: Vec<String> = v["not_indexed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect();
    assert!(
        ni.contains(&"src/ghost.test.ts".to_string()),
        "nonexistent input → not_indexed; got {ni:?}"
    );
    assert!(
        !tests.contains(&"src/ghost.test.ts".to_string()),
        "nonexistent test must NOT be in the re-run set; got {tests:?}"
    );
}

#[test]
fn test_cli_affected_blast_radius_disjoint_from_changed() {
    // F4: a changed file must never appear in affected_files. api.ts imports auth.ts;
    // changing BOTH must not list api.ts (a changed file) as 'affected'.
    let project = setup_affected_project();
    let (stdout, _, code) = run_cli(
        &project,
        &["affected", "src/auth.ts", "src/api.ts", "--json"],
    );
    assert_eq!(code, 0, "stdout: {stdout}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    let affected: Vec<String> = v["affected_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["path"].as_str().unwrap().to_string())
        .collect();
    let changed: Vec<String> = v["changed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect();
    for c in &changed {
        assert!(
            !affected.contains(c),
            "changed file {c} must not be in affected_files; affected={affected:?}"
        );
    }
}

#[test]
fn test_cli_health_check_resolution_block() {
    let project = setup_indexed_project(); // TS fixture: api.ts → auth.ts validateToken
    let (stdout, _, code) = run_cli(&project, &["health-check", "--json"]);
    assert_eq!(code, 0, "stdout: {stdout}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    let res = &v["resolution"];
    assert!(res.is_object(), "resolution block must be present; got {v}");
    assert!(res["pending_unresolved_calls"].is_number());
    // Key-agnostic: ≥1 resolved call edge across all languages (handleLogin→validateToken).
    let total_calls: i64 = res["edges_by_language"]
        .as_object()
        .unwrap()
        .values()
        .filter_map(|rels| rels["calls"].as_i64())
        .sum();
    assert!(
        total_calls >= 1,
        "expected ≥1 resolved call edge across languages; got {res}"
    );
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
    let internal_tokens = [
        "audit #",
        "clap-migrat",
        "resolved_format",
        "plan §",
        "issue #",
    ];
    for cmd in [
        "stats",
        "benchmark",
        "incremental-index",
        "reindex",
        "rebuild-index",
        "health-check",
        "map",
        "tour",
        "grep",
        "overview",
        "dead-code",
        "search",
        "ast-search",
        "deps",
        "trace",
        "snapshot",
        "callgraph",
        "impact",
        "show",
        "refs",
        "similar",
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

// Regression guard: the top-level `print_help()` (in main.rs) is a hand-maintained
// string that can silently drift from the `fn main` dispatch match. `tour` shipped
// as a first-class command (own clap `--help`, JSON output, a dispatch arm, and an
// entry in doc_cli_alignment::clap_commands) yet was never added to the COMMANDS
// list, so `code-graph-mcp --help` never mentioned it — undiscoverable. This asserts
// every user-facing subcommand appears in the top-level help. Keep the list in sync
// with the match arms in `fn main`; MCP-name aliases (get_call_graph, project_map, …)
// are intentionally not listed and excluded here.
#[test]
fn test_cli_top_level_help_lists_all_commands() {
    let project = TempDir::new().unwrap();
    let (stdout, _, code) = run_cli(&project, &["--help"]);
    assert_eq!(code, 0, "--help should exit 0");
    for cmd in [
        "serve",
        "grep",
        "search",
        "ast-search",
        "callgraph",
        "impact",
        "affected",
        "show",
        "map",
        "tour",
        "overview",
        "deps",
        "trace",
        "similar",
        "refs",
        "dead-code",
        "centrality",
        "cycles",
        "surprising",
        "report",
        "incremental-index",
        "rebuild-index",
        "reindex",
        "health-check",
        "doctor",
        "benchmark",
        "stats",
        "outcome",
        "adopt",
        "unadopt",
        "snapshot",
    ] {
        assert!(
            stdout.contains(&format!("\n    {cmd} ")),
            "`code-graph-mcp --help` COMMANDS list is missing `{cmd}` — print_help() in \
             main.rs drifted from the dispatch. Full help:\n{stdout}"
        );
    }
}

// JS-dispatched subcommands (doctor/adopt/unadopt) bypass clap, so before the
// `--help` interception in main.rs, `doctor --help` RAN doctor — rewriting
// ~/.claude/settings.json — instead of printing usage. `--help`/`-h` must be
// side-effect-free. Asserts each prints usage and exits 0, and that doctor's
// help neither runs the diagnostic (no `🔍` run header) nor hides --check-only.
#[test]
fn test_cli_js_subcommands_help_is_side_effect_free() {
    let project = TempDir::new().unwrap();
    for cmd in ["doctor", "adopt", "unadopt"] {
        for help_flag in ["--help", "-h"] {
            let (stdout, _, code) = run_cli(&project, &[cmd, help_flag]);
            assert_eq!(
                code, 0,
                "{cmd} {help_flag} should exit 0; got {code}\n{stdout}"
            );
            assert!(
                stdout.contains("USAGE"),
                "{cmd} {help_flag} should print usage, not run the command; got:\n{stdout}"
            );
            // The doctor diagnostic run prints a `🔍` header; help must not.
            assert!(
                !stdout.contains('\u{1f50d}'),
                "{cmd} {help_flag} ran the command instead of showing help; got:\n{stdout}"
            );
        }
    }
    // doctor help must advertise the diagnose-only escape hatch.
    let (stdout, _, _) = run_cli(&project, &["doctor", "--help"]);
    assert!(
        stdout.contains("--check-only"),
        "doctor --help must document --check-only; got:\n{stdout}"
    );
}

/// The published binary must not swallow an unrecognized `doctor` flag.
///
/// `doctor`'s default mode REPAIRS — it rewrites `~/.claude/settings.json`. The
/// dispatch used to filter argv down to the single literal `--check-only`, so a
/// typo like `--check-onlyy` was dropped and doctor.js was invoked with an empty
/// argv, which parses as "no flags" and takes the repair path. The user asked for
/// the read-only mode and got the writing one.
///
/// This is the THIRD entry point onto the same parsing: `doctor.js` and
/// `lifecycle.js doctor` were fixed first, and this one — the surface installed
/// via npx / `cargo install` / the plugin — kept the old behavior, invisible to
/// the JS tests because they only drive the two JS entry points.
#[test]
fn test_cli_doctor_rejects_an_unknown_flag_instead_of_repairing() {
    let project = TempDir::new().unwrap();

    // HOME and CLAUDE_CONFIG_DIR MUST be sandboxed here, and this is the only
    // cli_e2e test for which that is true.
    //
    // Its RED state is "doctor performed the repairs" — the very regression it
    // guards. Repairs rewrite ~/.claude/settings.json, re-register the
    // statusline, populate ~/.cache/code-graph (including the auto-update binary
    // pin) and shell out to npm. Inheriting the ambient HOME meant that a
    // mutation run to prove this guard live did all of that to the developer's
    // real config — measured, and it is what the verification recorded in
    // a1c94f8 actually did. On CI the same run would rewrite the runner's home.
    //
    // CLAUDE_CONFIG_DIR is set too, not just HOME: claude-config.js honours it
    // ahead of os.homedir(), so redirecting HOME alone leaves an escape hatch
    // for anyone who has it exported.
    //
    // It must point at `<home>/.claude`, NOT at `<home>`. `settingsPath()` is
    // `claudeHome()/settings.json`, so pointing it at `<home>` made the closing
    // assertion below watch `<home>/.claude/settings.json` — a path nothing in
    // the program can ever create. The guard read as live and was inert: the
    // mutation it claims to catch (a repair pass running here) would have
    // written `<home>/settings.json` and the assertion would still have passed.
    let home = TempDir::new().unwrap();
    let claude_home = home.path().join(".claude");
    let sandbox_env: Vec<(&str, &str)> = vec![
        ("HOME", home.path().to_str().unwrap()),
        ("CLAUDE_CONFIG_DIR", claude_home.to_str().unwrap()),
    ];

    // `doctor` is JS-dispatched: main.rs spawns claude-plugin/scripts/doctor.js
    // relative to the executable. Under a redirected CARGO_TARGET_DIR that path
    // does not resolve, and the failure surfaces as "must name the offending
    // token" while the real cause sits in stderr. Point the resolver at the repo
    // so the test measures the guard rather than the layout.
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut env = sandbox_env.clone();
    env.push(("_FIND_BINARY_ROOT", repo_root.to_str().unwrap()));

    for typo in ["--check-onlyy", "--checkonly", "--check_only", "--dry-run"] {
        let (stdout, stderr, code) = run_cli_env(&project, &["doctor", typo], &env);
        assert!(
            !stderr.contains("doctor.js not found"),
            "doctor.js was unreachable, so this run proved nothing about flag \
             validation.\nstderr: {stderr}"
        );
        assert_ne!(
            code, 0,
            "`doctor {typo}` must not exit 0 — it was silently dropped and the              repair pass ran.
stdout: {stdout}
stderr: {stderr}"
        );
        assert!(
            stderr.contains("unknown argument"),
            "`doctor {typo}` must name the offending token.
stdout: {stdout}
stderr: {stderr}"
        );
        // The diagnostic run prints a magnifying-glass header; a rejected flag
        // must not have reached it.
        assert!(
            !stdout.contains('\u{1f50d}'),
            "`doctor {typo}` ran the diagnostic (and therefore the repairs) \
             instead of refusing.\nstdout: {stdout}"
        );
    }

    // Negative control: the real flag still works and is still read-only.
    let (stdout, _, _) = run_cli_env(&project, &["doctor", "--check-only"], &env);
    assert!(
        stdout.contains('\u{1f50d}') || stdout.contains("issue"),
        "`doctor --check-only` must still run the diagnostic; got:\n{stdout}"
    );

    // And nothing landed in the sandboxed home — if a future edit lets a repair
    // run here, this is what catches it before it reaches a real machine. The
    // path is derived from `claude_home` (the same value handed to the child)
    // rather than spelled out, so the two can no longer drift apart silently.
    for artifact in [
        "settings.json",
        "statusline-providers.json",
        "plugins/installed_plugins.json",
    ] {
        assert!(
            !claude_home.join(artifact).exists(),
            "a rejected flag (and --check-only) must not write {artifact}; found one at {}",
            claude_home.join(artifact).display()
        );
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
    // F12: text health-check surfaces the Resolution line (mirrors the --json block).
    assert!(
        stdout.contains("Resolution:"),
        "text health-check must show Resolution line: {}",
        stdout
    );
}

#[test]
fn test_cli_health_check_files_excludes_external_pseudo_file() {
    // Regression: the user-facing `files` count must report real source files
    // only, not the synthetic `<external>` pseudo-file (the unresolved-import
    // bucket). a.js imports a builtin (`crypto`) so `<external>` is created; with
    // 2 real files the count must be 2, not 3. The fix is in the shared
    // get_index_status, so `report` and the MCP get_index_status tool inherit it.
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a.js"),
        "const crypto = require('crypto');\nfunction fa() { return crypto.randomUUID(); }\nmodule.exports = { fa };\n").unwrap();
    std::fs::write(src.join("b.js"),
        "const { fa } = require('./a');\nfunction fb() { return fa(); }\nmodule.exports = { fb };\n").unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    {
        let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
        code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
        // Self-check: the fixture must actually create the <external> pseudo-file,
        // else this test would pass even with the bug present.
        let total: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        let real: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM files WHERE path != '<external>'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(real, 2, "fixture has 2 real source files");
        assert_eq!(
            total, 3,
            "fixture must create the <external> pseudo-file to exercise the bug; total={total}"
        );
    }
    let (stdout, _, code) = run_cli(&project, &["health-check", "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    assert_eq!(
        v["files"].as_i64().unwrap(),
        2,
        "health-check `files` must exclude the <external> pseudo-file; got: {stdout}"
    );
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
    assert_ne!(
        code, 0,
        "unhealthy should exit non-zero, stderr: {}",
        stderr
    );
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
    assert_eq!(
        v_flag["healthy"], v_fmt["healthy"],
        "--format json must mirror --json"
    );
    assert_eq!(v_flag["nodes"], v_fmt["nodes"]);
}

#[test]
fn test_cli_health_check_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["health-check", "--help"]);
    assert_eq!(code, 0, "health-check --help should exit 0 (clap help)");
    assert!(
        stdout.contains("index status") || stdout.contains("--format"),
        "help should describe the command; got: {stdout:?}"
    );
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
    assert!(
        stdout.contains("validateToken"),
        "should find validateToken, got: {}",
        stdout
    );
}

#[test]
fn test_cli_search_no_results() {
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["search", "xyznonexistent"]);
    assert_eq!(code, 0);
    assert!(
        stderr.contains("No results"),
        "should show no results message"
    );
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
    let (stdout, _, code) = run_cli(
        &project,
        &["search", "validate", "--language", "typescript"],
    );
    assert_eq!(code, 0);
    assert!(stdout.contains("validateToken"));
}

#[test]
fn test_cli_search_filter_removed_all_explains_why() {
    // Regression: an over-selective language filter that removes EVERY query match must
    // explain that candidates matched but were filtered out — not a bare "no results".
    // Guards the post-fetch under-fetch fix: vec0/FTS can't pre-filter on joined
    // language, so the drop happens after fetch and is invisible without this signal.
    // v0.99.1 (roadmap §1.1): stdout now carries a self-describing object (NOT `[]`)
    // so the disclosure survives `2>/dev/null`; stderr keeps the human message.
    let project = setup_indexed_project(); // TS fixture: validateToken in api.ts/auth.ts
    let (stdout, stderr, code) = run_cli(
        &project,
        &["search", "validateToken", "--language", "python", "--json"],
    );
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["results"], serde_json::json!([]));
    assert!(
        v["filtered_out"].as_u64().unwrap_or(0) >= 1,
        "in-band disclosure; got {stdout:?}"
    );
    assert!(
        stderr.contains("removed by the active filter"),
        "stderr must explain the filter emptied the results; got: {stderr:?}"
    );
}

#[test]
fn test_cli_search_unknown_language_rejected() {
    // Regression: an unknown/mistyped --language must fail loudly at entry (parity
    // with --node-type), naming the bad value and the valid set — NOT be silently
    // swallowed and reported as a too-narrow "removed by the active filter" (which
    // wrongly implies the language is valid but the query too specific).
    let project = setup_indexed_project();
    let (_stdout, stderr, code) = run_cli(
        &project,
        &["search", "validateToken", "--language", "pyton"],
    );
    assert_ne!(
        code, 0,
        "unknown language must exit nonzero; stderr: {stderr:?}"
    );
    assert!(
        stderr.contains("Unknown language filter") && stderr.contains("pyton"),
        "stderr must name the bad language and list the valid set; got: {stderr:?}"
    );
}

#[test]
fn test_cli_search_language_case_insensitive() {
    // A known language in mixed case must be accepted (not rejected by the new
    // entry validation) and still match. NOTE: the CLI's downstream filter was
    // already `eq_ignore_ascii_case`, so this does not by itself guard the
    // validation change — it guards that `canonical_language` stays
    // case-insensitive. The load-bearing case-normalization is on the MCP side
    // (mcp::server::tests::test_semantic_search_language_case_insensitive), whose
    // downstream filter is case-sensitive.
    let project = setup_indexed_project();
    let (stdout, _stderr, code) = run_cli(
        &project,
        &["search", "validate", "--language", "TypeScript"],
    );
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
            assert!(
                !line.contains("(token:"),
                "compact should not include params, got: {}",
                line
            );
        }
    }
}

#[test]
fn test_cli_search_limit() {
    let project = setup_indexed_project();
    let (stdout, _, _) = run_cli(&project, &["search", "function", "--limit", "2"]);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert!(
        lines.len() <= 2,
        "should respect --limit, got {} lines",
        lines.len()
    );
}

// clap-migrated (audit #4): clap owns --help + unknown-flag rejection; --top-k is
// a hidden alias of --limit; the non-empty query guard is preserved in the handler.
#[test]
fn test_cli_search_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["search", "--help"]);
    assert_eq!(code, 0, "search --help should exit 0 (clap help)");
    assert!(
        stdout.contains("FTS5") || stdout.contains("QUERY"),
        "help should describe the command; got: {stdout:?}"
    );
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
    let (out_limit, _, code_limit) =
        run_cli(&project, &["search", "function", "--limit", "2", "--json"]);
    let (out_topk, _, code_topk) =
        run_cli(&project, &["search", "function", "--top-k", "2", "--json"]);
    assert_eq!(
        code_limit, code_topk,
        "--top-k must mirror --limit exit code"
    );
    assert_eq!(
        out_limit.trim(),
        out_topk.trim(),
        "--top-k 2 must equal --limit 2"
    );
}

// ============================================================
// grep (requires ripgrep `rg` binary)
// ============================================================

fn has_ripgrep() -> bool {
    let present = Command::new("rg").arg("--version").output().is_ok();
    // On CI an absent rg must REDDEN the run, not silently skip: 43 grep
    // tests gate on this helper, and every runner image ships without
    // ripgrep — the whole cmd_grep surface (largest CLI handler) ran with
    // zero executed coverage on all OS legs AND the release gate while
    // reporting green (audit 2026-08-02 P1-8). The workflows now install
    // rg; this assert keeps the skip from going dark again if a job loses
    // that step. Same pattern as tests/predicate_parity.rs.
    assert!(
        present || std::env::var_os("CI").is_none(),
        "ripgrep must be installed on CI (the grep suite would silently skip)"
    );
    present
}

#[test]
fn test_cli_grep() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["grep", "validateToken"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("validateToken"), "should find matches");
    assert!(stdout.contains("→"), "should include AST context arrows");
}

#[test]
fn test_cli_grep_no_matches() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    // grep-parity: no match exits 1 (was 0 pre-v0.50) so scripts can branch
    // on match presence like with real grep.
    let (_, stderr, code) = run_cli(&project, &["grep", "xyznonexistent"]);
    assert_eq!(code, 1, "no match must exit 1 (grep parity)");
    assert!(
        stderr.contains("No matches"),
        "should show no matches message"
    );
}

#[test]
fn test_cli_grep_invalid_regex_exits_two() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    // Unescaped `(` is an invalid regex — ripgrep exits 2. grep-parity: error
    // paths exit 2 (grep's "trouble" code), distinct from no-match (1).
    let (_, stderr, code) = run_cli(&project, &["grep", "res.json("]);
    assert_eq!(
        code, 2,
        "invalid regex must exit 2 (grep parity), got stderr: {stderr}"
    );
    assert!(
        stderr.contains("ripgrep error") || stderr.to_lowercase().contains("regex"),
        "should surface the ripgrep error, got: {stderr}"
    );
}

#[test]
fn test_cli_grep_partial_results_on_missing_path() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    // GNU grep parity: an unreadable/missing path is an error (exit 2), but
    // matches from the readable paths still print — rg itself emits the hits
    // from `src` and exits 2 on `no_such_dir`. The exit-2 branch used to
    // discard rg's stdout wholesale, so a multi-path grep with one bad path
    // silently dropped every result.
    let (stdout, stderr, code) =
        run_cli(&project, &["grep", "validateToken", "src", "no_such_dir"]);
    assert_eq!(
        code, 2,
        "path error must exit 2 (grep parity), got stderr: {stderr}"
    );
    assert!(
        stdout.contains("validateToken"),
        "partial results from the valid path must print, got stdout: {stdout:?} stderr: {stderr:?}"
    );
    assert!(
        stderr.contains("No such file") || stderr.contains("ripgrep error"),
        "the path error must be surfaced on stderr, got: {stderr:?}"
    );
}

#[test]
fn test_cli_grep_partial_results_count_mode() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    // Same partial-results contract for -c (the observed field failure shape:
    // `grep "pat" scripts parse -c` where `parse` did not exist → exit 2 with
    // all counts dropped).
    let (stdout, stderr, code) = run_cli(
        &project,
        &["grep", "validateToken", "src", "no_such_dir", "-c"],
    );
    assert_eq!(
        code, 2,
        "path error must exit 2 (grep parity), got stderr: {stderr}"
    );
    assert!(
        stdout.contains("src/auth.ts:") && stdout.contains("src/api.ts:"),
        "counts from the valid path must print, got stdout: {stdout:?}"
    );
}

#[test]
fn test_cli_grep_with_path() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["grep", "validateToken", "src/auth.ts"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("validateToken"));
}

#[test]
fn test_cli_grep_root_relative_path_from_subdir_rebases() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    // Field failure 2026-07-24: the shell sat in claude-plugin/scripts while the
    // path was quoted repo-root-relative (the deny-hook displays paths that way),
    // so cwd.join doubled the prefix — rg got <root>/<sub>/<sub>/… and exited 2
    // with a cryptic "No such file". cwd-missing + root-existing is unambiguous:
    // rebase against the project root and note it on stderr.
    let (stdout, stderr, code) =
        run_cli_from(&project, "src", &["grep", "validateToken", "src/auth.ts"]);
    assert_eq!(
        code, 0,
        "root-relative path from a subdir must rebase to the project root, stderr: {stderr:?}"
    );
    assert!(
        stdout.contains("validateToken"),
        "matches must print, got: {stdout:?}"
    );
    assert!(
        stderr.contains("project root"),
        "the rebase must be surfaced on stderr, got: {stderr:?}"
    );
}

#[test]
fn test_cli_overview_root_relative_path_from_subdir_rebases() {
    let project = setup_indexed_project();
    // Same near-miss class as the grep test above, through normalize_user_path
    // (covers overview/callgraph/impact/show/deps/refs/dead_code/tour/affected):
    // `overview src` from inside src/ used to look up the doubled "src/src" and
    // report "No symbols found under: src" — echoing the perfectly valid path
    // the caller typed.
    let (stdout, stderr, code) = run_cli_from(&project, "src", &["overview", "src"]);
    assert_eq!(code, 0, "stderr: {stderr:?}");
    assert!(
        stdout.contains("validateToken"),
        "overview must list the module's symbols after the rebase, got: {stdout:?}"
    );
    assert!(
        stderr.contains("project root"),
        "the rebase must be surfaced on stderr, got: {stderr:?}"
    );
}

#[test]
fn test_cli_grep_cwd_anchored_path_never_rebases() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    // `./x` (like `.` and `../x`) is explicitly cwd-anchored: `./src/auth.ts`
    // from inside src/ means src/src/auth.ts, which doesn't exist — that's the
    // caller's error to see, not a near-miss to silently repair. Both rebase
    // sites (cmd_grep here, normalize_user_path_from in the unit tests) follow
    // the same rule; audit 2026-07-24 caught cmd_grep rebasing these.
    let (_stdout, stderr, code) =
        run_cli_from(&project, "src", &["grep", "validateToken", "./src/auth.ts"]);
    assert_eq!(
        code, 2,
        "cwd-anchored miss must surface as an rg error, stderr: {stderr:?}"
    );
    assert!(
        !stderr.contains("project root"),
        "cwd-anchored paths must not rebase, got: {stderr:?}"
    );
}

#[test]
fn test_cli_grep_cwd_relative_path_from_subdir_unchanged() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    // grep/rg parity guard: a path that DOES exist under the shell cwd keeps its
    // cwd-relative reading — the rebase fires only on the cwd-missing +
    // root-existing near-miss, never when both readings exist.
    let (stdout, stderr, code) =
        run_cli_from(&project, "src", &["grep", "validateToken", "auth.ts"]);
    assert_eq!(
        code, 0,
        "cwd-relative path must keep working, stderr: {stderr:?}"
    );
    assert!(
        stdout.contains("validateToken"),
        "matches must print, got: {stdout:?}"
    );
    assert!(
        !stderr.contains("project root"),
        "no rebase note when the cwd-relative path exists, got: {stderr:?}"
    );
}

// clap-migrated (audit #4): clap owns --help + unknown-flag rejection; the
// non-empty pattern guard (exit 1 + Usage) is preserved in the handler because
// clap accepts an empty-string positional.
#[test]
fn test_cli_grep_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["grep", "--help"]);
    assert_eq!(code, 0, "grep --help should exit 0 (clap help)");
    assert!(
        stdout.contains("AST-context grep") || stdout.contains("PATTERN"),
        "help should describe the command; got: {stdout:?}"
    );
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
    assert_eq!(
        code, 2,
        "empty pattern is a usage error: exit 2 (grep parity); stderr={stderr:?}"
    );
    assert!(
        stderr.contains("Usage:"),
        "should show usage on empty pattern; got: {stderr:?}"
    );
}

#[test]
fn test_cli_grep_leading_dash_pattern() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    std::fs::write(
        project.path().join("BUILD.md"),
        "Use cargo build --no-default-features for the small binary.\n",
    )
    .unwrap();
    // A pattern starting with `-` must be searchable without clap or rg
    // swallowing it as a flag (rg invocation needs the `--` separator).
    let (stdout, stderr, code) = run_cli(&project, &["grep", "--no-default-features"]);
    assert_eq!(code, 0, "leading-dash pattern must work; stderr={stderr}");
    assert!(
        stdout.contains("BUILD.md"),
        "should find the hit, got: {stdout}"
    );
    // The clap-suggested `--` escape form must work too.
    let (stdout2, _, code2) = run_cli(&project, &["grep", "--", "--no-default-features"]);
    assert_eq!(code2, 0);
    assert!(stdout2.contains("BUILD.md"));
}

#[test]
fn test_cli_grep_noop_muscle_memory_flags() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    // "drop-in grep" parity: -n/-r/-R/-H are accepted no-ops (line numbers,
    // recursion, and filenames are already the default output here), NOT
    // swallowed as the pattern by allow_hyphen_values. Pre-fix, `grep -n "pat"`
    // bound `-n` as the pattern and pushed "pat" into the path list → rg errored
    // with "No such file or directory: pat" at exit 2 — silently training the
    // model to abandon the tool the whole project exists to steer it toward.
    for flag in [
        "-n",
        "--line-number",
        "-r",
        "-R",
        "--recursive",
        "-H",
        "--with-filename",
    ] {
        let (stdout, stderr, code) = run_cli(&project, &["grep", flag, "validateToken"]);
        assert_eq!(
            code, 0,
            "grep {flag} <pat> must bind the pattern, not swallow the flag; stderr={stderr}"
        );
        assert!(
            stdout.contains("validateToken"),
            "grep {flag}: should find matches, got: {stdout}"
        );
    }
    // -n combined with an explicit path (the most common real invocation).
    let (stdout, _, code) = run_cli(&project, &["grep", "-n", "validateToken", "src/auth.ts"]);
    assert_eq!(code, 0, "grep -n <pat> <path> must work");
    assert!(stdout.contains("validateToken"));
}

#[test]
fn test_cli_grep_attached_context_forms() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    // Drop-in grep parity: the attached numeric context forms `-A2`/`-B1`/`-C2`
    // (real grep and ripgrep both accept them) must parse as context, NOT get
    // bound as the search pattern by the pattern positional's allow_hyphen_values.
    // Pre-fix, `grep -A2 pat path` set pattern="-A2" and pushed "pat" into the
    // path list → rg "No such file or directory: pat", exit 2 — a cryptic failure
    // on one of grep's most common invocations.
    let (stdout, stderr, code) =
        run_cli(&project, &["grep", "-A2", "validateToken", "src/auth.ts"]);
    assert_eq!(
        code, 0,
        "attached -A2 must parse as after-context; stderr={stderr}"
    );
    assert!(
        stdout.contains("validateToken"),
        "should find the match, got: {stdout}"
    );
    // After-context lines render with a `-` line separator (rg/grep style).
    assert!(
        stdout.contains("src/auth.ts-"),
        "after-context lines should appear for -A2, got: {stdout}"
    );
    // -C2 and -B1 attached forms likewise bind the pattern, not the flag.
    let (_, se_c, code_c) = run_cli(&project, &["grep", "-C2", "validateToken", "src/auth.ts"]);
    assert_eq!(code_c, 0, "attached -C2 must work; stderr={se_c}");
    let (_, se_b, code_b) = run_cli(&project, &["grep", "-B1", "validateToken", "src/auth.ts"]);
    assert_eq!(code_b, 0, "attached -B1 must work; stderr={se_b}");
    // Bundled boolean short(s) + trailing attached context. grep and ripgrep both
    // accept `-nA2`; the value flag is always last in a bundle. `-n` is advertised
    // here as a no-op parity flag, so `grep -nA2 pat` is a high-probability
    // muscle-memory form that must not regress to the cryptic rg path error.
    let (so_n, se_n, code_n) = run_cli(&project, &["grep", "-nA2", "validateToken", "src/auth.ts"]);
    assert_eq!(
        code_n, 0,
        "bundled -nA2 must parse as -n + after-context; stderr={se_n}"
    );
    assert!(
        so_n.contains("validateToken"),
        "should find the match, got: {so_n}"
    );
    assert!(
        so_n.contains("src/auth.ts-"),
        "after-context lines should appear for -nA2, got: {so_n}"
    );
    // -niA2: the bundled -i must still take effect (case-insensitive match).
    let (so_ni, se_ni, code_ni) =
        run_cli(&project, &["grep", "-niA2", "VALIDATETOKEN", "src/auth.ts"]);
    assert_eq!(
        code_ni, 0,
        "bundled -niA2 (with -i) must work; stderr={se_ni}"
    );
    assert!(
        so_ni.contains("validateToken"),
        "-niA2 should match case-insensitively, got: {so_ni}"
    );
    // The `--` escape still lets a literal "-A2" be searched as a pattern
    // (normalization must stop at the `--` separator).
    std::fs::write(project.path().join("DASH.md"), "the -A2 flag here\n").unwrap();
    let (stdout_lit, _, code_lit) = run_cli(&project, &["grep", "--", "-A2"]);
    assert_eq!(
        code_lit, 0,
        "literal -A2 via -- must still search as a pattern"
    );
    assert!(
        stdout_lit.contains("DASH.md"),
        "should find literal -A2, got: {stdout_lit}"
    );
}

#[test]
fn test_cli_grep_fixed_strings_literal() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    // -F treats the pattern literally — `res.json(` stops being a regex error.
    let (stdout, _, code) = run_cli(&project, &["grep", "-F", "res.json(", "src/api.ts"]);
    assert_eq!(
        code, 0,
        "-F literal search must succeed on regex-hostile pattern"
    );
    assert!(
        stdout.contains("res.json("),
        "should find literal hits, got: {stdout}"
    );
}

#[test]
fn test_cli_grep_ignore_case() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    let (_, _, code_sensitive) = run_cli(&project, &["grep", "VALIDATETOKEN"]);
    assert_eq!(code_sensitive, 1, "case-sensitive by default (grep parity)");
    let (stdout, _, code) = run_cli(&project, &["grep", "-i", "VALIDATETOKEN"]);
    assert_eq!(code, 0, "-i must match case-insensitively");
    assert!(stdout.contains("validateToken"));
}

#[test]
fn test_cli_grep_word_regexp() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    // `validate` appears only inside `validateToken` — -w must not match it.
    let (_, _, code) = run_cli(&project, &["grep", "-w", "validate"]);
    assert_eq!(code, 1, "-w must not match partial words");
    let (stdout, _, code2) = run_cli(&project, &["grep", "-w", "validateToken"]);
    assert_eq!(code2, 0);
    assert!(stdout.contains("validateToken"));
}

#[test]
fn test_cli_grep_multi_path() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(
        &project,
        &["grep", "validateToken", "src/auth.ts", "src/api.ts"],
    );
    assert_eq!(
        code, 0,
        "multiple path arguments must be accepted (grep parity)"
    );
    assert!(
        stdout.contains("src/auth.ts"),
        "hit from first path, got: {stdout}"
    );
    assert!(
        stdout.contains("src/api.ts"),
        "hit from second path, got: {stdout}"
    );
}

#[test]
fn test_cli_grep_max_count_truncation_note() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    let many = "needle\n".repeat(5);
    std::fs::write(project.path().join("many.txt"), &many).unwrap();
    // Cap hit → must say so on stderr instead of silently truncating.
    let (stdout, stderr, code) = run_cli(
        &project,
        &["grep", "needle", "many.txt", "--max-count", "2"],
    );
    assert_eq!(code, 0);
    assert_eq!(
        stdout.matches("needle").count(),
        2,
        "cap applies, got: {stdout}"
    );
    assert!(
        stderr.contains("--max-count") || stderr.contains("truncat"),
        "truncation must be surfaced on stderr, got: {stderr:?}"
    );
    // --max-count 0 lifts the cap entirely.
    let (stdout_all, stderr_all, code_all) = run_cli(
        &project,
        &["grep", "needle", "many.txt", "--max-count", "0"],
    );
    assert_eq!(code_all, 0);
    assert_eq!(
        stdout_all.matches("needle").count(),
        5,
        "0 = unlimited, got: {stdout_all}"
    );
    assert!(
        !stderr_all.contains("truncat"),
        "no cap → no truncation note"
    );
}

#[test]
fn test_cli_grep_max_count_short_flag() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    std::fs::write(project.path().join("many.txt"), "needle\n".repeat(5)).unwrap();
    // grep/ripgrep short `-m` is the `--max-count` alias. Separated `-m 2` and
    // attached `-m2` must both cap — pre-fix `-m`/`-m2` had no short and were
    // bound as the pattern via allow_hyphen_values → cryptic "No such file".
    let forms: [&[&str]; 2] = [
        &["grep", "-m", "2", "needle", "many.txt"],
        &["grep", "-m2", "needle", "many.txt"],
    ];
    for argv in forms {
        let (stdout, stderr, code) = run_cli(&project, argv);
        assert_eq!(code, 0, "{argv:?} must succeed; stderr={stderr}");
        assert_eq!(
            stdout.matches("needle").count(),
            2,
            "{argv:?}: -m caps at 2, got: {stdout}"
        );
    }
    // -m0 lifts the cap (attached zero).
    let (stdout0, _, code0) = run_cli(&project, &["grep", "-m0", "needle", "many.txt"]);
    assert_eq!(code0, 0);
    assert_eq!(
        stdout0.matches("needle").count(),
        5,
        "-m0 = unlimited, got: {stdout0}"
    );
}

#[test]
fn test_cli_grep_unsupported_flag_clear_error() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    // Unsupported common grep shorts (-v invert, -c count, -o only-matching) must
    // fail with a clear "unsupported flag" message + exit 2, NOT bind as the
    // pattern and emit a cryptic "No such file: <next-arg>" (allow_hyphen_values
    // trap, same class as the -A2/-n parity fixes).
    for bad in ["-v", "-o", "-e"] {
        let (_, stderr, code) = run_cli(&project, &["grep", bad, "validateToken"]);
        assert_eq!(code, 2, "{bad} must exit 2; stderr={stderr}");
        assert!(
            stderr.contains("unsupported flag"),
            "{bad}: clear message expected, got: {stderr}"
        );
        assert!(
            !stderr.contains("No such file"),
            "{bad}: must not leak cryptic rg error, got: {stderr}"
        );
    }
    // --json keeps the empty-array contract even on this usage error.
    let (stdout, _, code) = run_cli(&project, &["grep", "-v", "validateToken", "--json"]);
    assert_eq!(code, 2);
    assert_eq!(
        stdout.trim(),
        "[]",
        "--json unsupported-flag bail must emit []"
    );
    // The `--` escape still lets a literal "-v" be searched (parity preserved).
    std::fs::write(project.path().join("DASHV.md"), "the -v flag here\n").unwrap();
    let (stdout2, _, code2) = run_cli(&project, &["grep", "--", "-v"]);
    assert_eq!(code2, 0, "literal -v via -- must search; got code {code2}");
    assert!(
        stdout2.contains("DASHV.md"),
        "should find literal -v, got: {stdout2}"
    );
}

#[test]
fn test_cli_grep_json_truncated_marker() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    std::fs::write(project.path().join("many.txt"), "needle\n".repeat(5)).unwrap();
    // A `--json` consumer can't see the stderr truncation note, so each match in
    // a file that hit the per-file cap carries `"truncated": true`.
    let (stdout, _, code) = run_cli(
        &project,
        &["grep", "needle", "many.txt", "--max-count", "2", "--json"],
    );
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON array");
    let arr = v.as_array().expect("grep --json is an array");
    assert_eq!(arr.len(), 2, "cap applies in JSON too, got: {stdout}");
    assert!(
        arr.iter()
            .all(|e| e["truncated"] == serde_json::json!(true)),
        "each capped-file entry must be marked truncated, got: {stdout}"
    );
    // Uncapped search carries no truncated marker.
    let (stdout2, _, code2) = run_cli(
        &project,
        &["grep", "needle", "many.txt", "--max-count", "0", "--json"],
    );
    assert_eq!(code2, 0);
    let v2: serde_json::Value = serde_json::from_str(stdout2.trim()).unwrap();
    assert!(
        v2.as_array()
            .unwrap()
            .iter()
            .all(|e| e.get("truncated").is_none()),
        "no cap → no truncated marker, got: {stdout2}"
    );
    // v0.79: the JSON `text` value carries no trailing newline (strip is applied
    // uniformly, truncated or not).
    assert!(
        v2[0]["text"].as_str().is_some_and(|t| !t.ends_with('\n')),
        "json text must not carry a trailing newline, got: {stdout2}"
    );
}

#[test]
fn test_cli_grep_type_filter() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    std::fs::write(project.path().join("probe.rs"), "fn x() { NEEDLE }\n").unwrap();
    std::fs::write(project.path().join("probe.md"), "doc NEEDLE here\n").unwrap();
    // -t rust restricts to ripgrep's `rust` type (*.rs), excluding the md hit.
    let (stdout, stderr, code) = run_cli(&project, &["grep", "-t", "rust", "NEEDLE"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(
        stdout.contains("probe.rs"),
        "should hit the rust file, got: {stdout}"
    );
    assert!(
        !stdout.contains("probe.md"),
        "must exclude non-rust, got: {stdout}"
    );
    // An unknown type is an rg error (exit 2), surfaced not swallowed.
    let (_, _, code_bad) = run_cli(&project, &["grep", "-t", "nosuchtype", "NEEDLE"]);
    assert_eq!(code_bad, 2, "unknown --type must error");
}

#[test]
fn test_cli_grep_glob_filter() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    std::fs::write(project.path().join("probe.rs"), "fn x() { NEEDLE }\n").unwrap();
    std::fs::write(project.path().join("probe.md"), "doc NEEDLE here\n").unwrap();
    // include-glob: only *.md
    let (inc, _, code) = run_cli(&project, &["grep", "-g", "*.md", "NEEDLE"]);
    assert_eq!(code, 0);
    assert!(
        inc.contains("probe.md") && !inc.contains("probe.rs"),
        "include glob, got: {inc}"
    );
    // exclude-glob: drop *.md
    let (exc, _, code2) = run_cli(&project, &["grep", "-g", "!*.md", "NEEDLE"]);
    assert_eq!(code2, 0);
    assert!(
        exc.contains("probe.rs") && !exc.contains("probe.md"),
        "exclude glob, got: {exc}"
    );
}

#[test]
fn test_cli_grep_count_mode() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    std::fs::write(project.path().join("many.txt"), "needle\n".repeat(150)).unwrap();
    // -c prints file:count and is exhaustive — it ignores the default per-file
    // cap of 100 (150 > 100), unlike content mode.
    let (stdout, _, code) = run_cli(&project, &["grep", "-c", "needle", "many.txt"]);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("many.txt") && stdout.trim().ends_with(":150"),
        "exhaustive count expected, got: {stdout}"
    );
    // --json count shape: [{file, count}]
    let (j, _, cj) = run_cli(&project, &["grep", "-c", "needle", "many.txt", "--json"]);
    assert_eq!(cj, 0);
    let v: serde_json::Value = serde_json::from_str(j.trim()).unwrap();
    assert_eq!(v[0]["file"], "many.txt");
    assert_eq!(v[0]["count"], 150);
    // no match on a NAMED file → exit 1 + zero row (GNU `grep -c` prints a
    // count for every named file, including 0 — pre-fix this was `[]`/silence)
    let (je, _, ce) = run_cli(
        &project,
        &["grep", "-c", "zzz_nothing", "many.txt", "--json"],
    );
    assert_eq!(ce, 1);
    let ve: serde_json::Value = serde_json::from_str(je.trim()).unwrap();
    assert_eq!(ve[0]["file"], "many.txt");
    assert_eq!(ve[0]["count"], 0);
}

#[test]
fn test_cli_grep_count_zero_named_file() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    // GNU parity: `grep -c pat file` with zero matches prints `file:0` on
    // STDOUT and exits 1 — not stderr-only silence. Field failure shape:
    // `grep "pat" file.py -c 2>/dev/null` showed literally nothing.
    let (stdout, stderr, code) =
        run_cli(&project, &["grep", "xyznonexistent", "src/auth.ts", "-c"]);
    assert_eq!(code, 1, "all-zero counts still exit 1 (grep parity)");
    assert!(
        stdout.contains("src/auth.ts:0"),
        "named file must get a zero row on stdout, got stdout: {stdout:?}"
    );
    assert!(
        stderr.contains("No matches"),
        "stderr note stays, got: {stderr:?}"
    );
}

#[test]
fn test_cli_grep_count_zero_fills_nonmatching_named_files() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    // hashPassword exists only in auth.ts: the matching file keeps its count,
    // the non-matching NAMED file gets a 0 row, and the run exits 0 (matches
    // exist). GNU prints zeros for every named file; rg lists matching only.
    let (stdout, _, code) = run_cli(
        &project,
        &["grep", "hashPassword", "src/auth.ts", "src/api.ts", "-c"],
    );
    assert_eq!(code, 0, "matches exist → exit 0");
    assert!(
        stdout.contains("src/auth.ts:") && !stdout.contains("src/auth.ts:0"),
        "matching file keeps its real count, got: {stdout:?}"
    );
    assert!(
        stdout.contains("src/api.ts:0"),
        "non-matching named file must get a zero row, got: {stdout:?}"
    );
}

#[test]
fn test_cli_grep_count_zero_dir_arg_no_zero_rows() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    // Deliberate GNU deviation: dir / repo-wide args do NOT enumerate zero rows
    // (GNU -rc prints `path:0` for every scanned file — repo-scale noise).
    let (stdout, stderr, code) = run_cli(&project, &["grep", "xyznonexistent", "src", "-c"]);
    assert_eq!(code, 1);
    assert_eq!(
        stdout.trim(),
        "",
        "dir args stay silent on stdout, got: {stdout:?}"
    );
    assert!(stderr.contains("No matches"), "got: {stderr:?}");
}

#[test]
fn test_cli_grep_bre_escape_hint_on_zero_hits() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    // `\|` is alternation in GNU BRE but a LITERAL pipe in ripgrep's Rust regex
    // dialect — the habit pattern zero-hits silently and an LLM consumer
    // concludes "no such code". The no-match path must disclose the dialect.
    let (_, stderr, code) = run_cli(&project, &["grep", r"validateToken\|hashPassword"]);
    assert_eq!(
        code, 1,
        "literal 'validateToken|hashPassword' matches nothing"
    );
    assert!(
        stderr.contains("BRE") && stderr.contains(r"\|"),
        "zero-hit + BRE-style escape must emit the dialect hint, got: {stderr:?}"
    );
    // -c mode shares the hint path
    let (_, stderr_c, _) = run_cli(
        &project,
        &["grep", r"validateToken\|hashPassword", "src/auth.ts", "-c"],
    );
    assert!(
        stderr_c.contains("BRE"),
        "hint must fire in -c mode too, got: {stderr_c:?}"
    );
    // no escapes → no hint
    let (_, stderr_plain, _) = run_cli(&project, &["grep", "xyznonexistent"]);
    assert!(
        !stderr_plain.contains("BRE"),
        "plain zero-hit must not hint, got: {stderr_plain:?}"
    );
    // -F: backslashes are genuinely literal — never hint
    let (_, stderr_f, code_f) = run_cli(&project, &["grep", "-F", r"validateToken\|hashPassword"]);
    assert_eq!(code_f, 1);
    assert!(
        !stderr_f.contains("BRE"),
        "-F must suppress the hint, got: {stderr_f:?}"
    );
}

#[test]
fn test_cli_grep_max_columns() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    let long = format!("NEEDLE_{}", "x".repeat(1000)); // 1007 chars
    std::fs::write(project.path().join("long.txt"), format!("{long}\n")).unwrap();
    // Default -M 512: the 1007-char line is truncated with a marker.
    let (stdout, _, code) = run_cli(&project, &["grep", "NEEDLE", "long.txt"]);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("NEEDLE"),
        "match still shown, got len {}",
        stdout.len()
    );
    assert!(
        stdout.contains("[+") && stdout.contains("chars]"),
        "truncation marker, got len {}",
        stdout.len()
    );
    let maxlen = stdout.lines().map(|l| l.chars().count()).max().unwrap_or(0);
    assert!(
        maxlen < 1007,
        "emitted line must be capped, got max {maxlen}"
    );
    // -M 0 disables the cap — full line present.
    let (full, _, codef) = run_cli(&project, &["grep", "-M", "0", "NEEDLE", "long.txt"]);
    assert_eq!(codef, 0);
    assert!(full.contains(&long), "-M 0 must show the full line");
    // --json marks the omitted char count.
    let (jc, _, cj) = run_cli(&project, &["grep", "NEEDLE", "long.txt", "--json"]);
    assert_eq!(cj, 0);
    let v: serde_json::Value = serde_json::from_str(jc.trim()).unwrap();
    assert!(
        v[0]["line_truncated"].as_u64().unwrap_or(0) > 0,
        "line_truncated expected, got: {jc}"
    );
}

#[test]
fn test_cli_grep_files_with_matches() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    // -l: file names only, no line numbers, no AST arrows (grep parity).
    let (stdout, _, code) = run_cli(&project, &["grep", "-l", "validateToken"]);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("src/auth.ts") && stdout.contains("src/api.ts"),
        "both matching files listed, got: {stdout}"
    );
    assert!(
        !stdout.contains(':'),
        "-l output must be bare file paths, got: {stdout}"
    );
    assert!(!stdout.contains('→'), "-l output must have no AST arrows");
    // no match → exit 1, like grep.
    let (_, _, code2) = run_cli(&project, &["grep", "-l", "zzz_nothing"]);
    assert_eq!(code2, 1);
}

#[test]
fn test_cli_grep_deterministic_sorted_order() {
    // Regression: ripgrep parallelizes the file walk and emitted results in
    // worker-completion order, so multi-file grep shuffled every run (observed up
    // to 8/8 distinct). `--sort path` must force a stable ascending-path order.
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    // Files whose creation order deliberately differs from sorted order, all
    // sharing one unique token so only these files match.
    let names = [
        "zebra.txt",
        "mango.txt",
        "apple.txt",
        "kiwi.txt",
        "cherry.txt",
        "lime.txt",
        "banana.txt",
        "orange.txt",
    ];
    for n in names {
        std::fs::write(project.path().join(n), "GREPSORTMARKER\n").unwrap();
    }
    let (stdout, _, code) = run_cli(&project, &["grep", "-l", "GREPSORTMARKER"]);
    assert_eq!(code, 0);
    let got: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    let mut want = got.clone();
    want.sort_unstable();
    assert_eq!(
        got, want,
        "grep -l output must be ascending-path sorted; got: {got:?}"
    );
    // Multi-path input passed in NON-sorted arg order must still come back globally
    // sorted. (rg's `--sort path` only orders within each root and preserves arg-group
    // order, so this case guards that we post-sort the merged result set instead.)
    let (mp_out, _, mp_code) = run_cli(
        &project,
        &[
            "grep",
            "-l",
            "GREPSORTMARKER",
            "zebra.txt",
            "apple.txt",
            "mango.txt",
        ],
    );
    assert_eq!(mp_code, 0);
    let mp: Vec<&str> = mp_out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        mp,
        vec!["apple.txt", "mango.txt", "zebra.txt"],
        "multi-path grep -l must be globally sorted, not arg order; got: {mp:?}"
    );
    // Byte-identical across repeated runs (the determinism guarantee).
    let mut seen = std::collections::HashSet::new();
    for _ in 0..6 {
        let (s, _, _) = run_cli(&project, &["grep", "-l", "GREPSORTMARKER"]);
        seen.insert(s);
    }
    assert_eq!(
        seen.len(),
        1,
        "grep -l must be byte-identical across runs; got {} distinct",
        seen.len()
    );
}

#[test]
fn test_cli_grep_dedup_overlapping_paths() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    // A file passed twice — or overlapping dir args like `grep pat . src` — makes
    // rg scan it once per path instance and emit every match twice; the global
    // sort then makes the duplicates adjacent. Each mode must collapse the
    // exact-identical rows so an accidental path overlap doesn't double the output
    // (and its AST arrows / token cost). Real grep/rg duplicate here; this
    // AST-context grep deliberately deduplicates.

    // Content mode: the doubled-path run must equal the single-path run byte-for-byte
    // (same sorted+deduped set, same AST arrows).
    let (single, _, c1) = run_cli(&project, &["grep", "validateToken", "src/auth.ts"]);
    assert_eq!(c1, 0);
    let (double, _, c2) = run_cli(
        &project,
        &["grep", "validateToken", "src/auth.ts", "src/auth.ts"],
    );
    assert_eq!(c2, 0);
    assert_eq!(
        double, single,
        "duplicate path arg must not double content-mode output; got:\n{double}"
    );
    assert!(double.contains("validateToken"));

    // -l mode: overlapping dir args must list each matching file exactly once.
    let (l_out, _, cl) = run_cli(&project, &["grep", "-l", "validateToken", "src", "src"]);
    assert_eq!(cl, 0);
    assert_eq!(
        l_out.matches("src/auth.ts").count(),
        1,
        "-l must list a file once under overlapping paths, got: {l_out}"
    );

    // -c mode: a duplicate path must not emit two `path:N` rows for one file.
    let (c_out, _, cc) = run_cli(
        &project,
        &["grep", "-c", "validateToken", "src/api.ts", "src/api.ts"],
    );
    assert_eq!(cc, 0);
    assert_eq!(
        c_out.matches("src/api.ts:").count(),
        1,
        "-c must emit one row per file under duplicate paths, got: {c_out}"
    );
}

#[test]
fn test_cli_grep_files_with_matches_json() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["grep", "-l", "validateToken", "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let arr = v.as_array().expect("-l --json must be a JSON array");
    assert!(
        arr.iter().all(|e| e.is_string()),
        "-l --json entries are path strings"
    );
    assert!(
        arr.iter().any(|e| e.as_str() == Some("src/auth.ts")),
        "got: {stdout}"
    );
    // empty contract preserved
    let (stdout2, _, code2) = run_cli(&project, &["grep", "-l", "zzz_nothing", "--json"]);
    assert_eq!(code2, 1);
    assert_eq!(stdout2.trim(), "[]");
}

#[test]
fn test_cli_grep_context_lines() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    std::fs::write(
        project.path().join("ctx.txt"),
        "line1\nline2\nNEEDLE_A\nline4\nline5\nline6\nline7\nNEEDLE_B\nline9\n",
    )
    .unwrap();
    let (stdout, _, code) = run_cli(&project, &["grep", "-C", "1", "NEEDLE_", "ctx.txt"]);
    assert_eq!(code, 0);
    // grep-style: matches use `:`, context lines use `-`, gaps separated by `--`.
    assert!(
        stdout.contains("ctx.txt:3  NEEDLE_A"),
        "match line with colon, got: {stdout}"
    );
    assert!(
        stdout.contains("ctx.txt-2  line2"),
        "before-context with dash, got: {stdout}"
    );
    assert!(
        stdout.contains("ctx.txt-4  line4"),
        "after-context with dash, got: {stdout}"
    );
    assert!(
        stdout.contains("\n--\n"),
        "non-contiguous groups separated by --, got: {stdout}"
    );
    assert!(
        !stdout.contains("line6"),
        "-C 1 must not pull distant lines, got: {stdout}"
    );
}

#[test]
fn test_cli_grep_after_before_context() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    std::fs::write(project.path().join("ab.txt"), "before\nNEEDLE\nafter\n").unwrap();
    let (stdout, _, code) = run_cli(&project, &["grep", "-A", "1", "NEEDLE", "ab.txt"]);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("ab.txt-3  after") && !stdout.contains("before"),
        "-A 1 shows only the after line, got: {stdout}"
    );
    let (stdout2, _, code2) = run_cli(&project, &["grep", "-B", "1", "NEEDLE", "ab.txt"]);
    assert_eq!(code2, 0);
    assert!(
        stdout2.contains("ab.txt-1  before") && !stdout2.contains("after"),
        "-B 1 shows only the before line, got: {stdout2}"
    );
}

#[test]
fn test_cli_grep_context_ast_arrow_only_on_matches() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    // validateToken sits inside a fn in the indexed fixture; context lines
    // around it must not each get their own AST arrow (annotation spam).
    let (stdout, _, code) = run_cli(
        &project,
        &["grep", "-C", "1", "decoded !== null", "src/auth.ts"],
    );
    assert_eq!(code, 0);
    let arrows = stdout.matches('→').count();
    assert_eq!(
        arrows, 1,
        "exactly one arrow (the match line), got: {stdout}"
    );
}

/// Extract the start line from the first AST annotation `(lines N-M)`.
fn first_annotation_start(stdout: &str) -> Option<u64> {
    let idx = stdout.find("(lines ")?;
    let rest = &stdout[idx + "(lines ".len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

#[test]
fn test_cli_grep_annotation_resyncs_after_edit() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    let (out1, _, code1) = run_cli(&project, &["grep", "decoded !== null", "src/auth.ts"]);
    assert_eq!(code1, 0);
    let start1 = first_annotation_start(&out1).expect("baseline annotation present");

    // Shift every fn down by 5 lines; without query-time freshness the
    // annotation would keep the pre-edit boundaries (the stale-arrow bug).
    let p = project.path().join("src/auth.ts");
    let content = std::fs::read_to_string(&p).unwrap();
    std::fs::write(
        &p,
        format!("// pad\n// pad\n// pad\n// pad\n// pad\n{content}"),
    )
    .unwrap();

    let (out2, _, code2) = run_cli(&project, &["grep", "decoded !== null", "src/auth.ts"]);
    assert_eq!(code2, 0);
    let start2 = first_annotation_start(&out2).expect("post-edit annotation present");
    assert_eq!(
        start2,
        start1 + 5,
        "annotation must use post-edit fn boundaries (lazy resync), got: {out2}"
    );
    assert!(
        !out2.contains("[stale]"),
        "synced annotation must not carry a stale marker"
    );
}

#[test]
fn test_cli_grep_stale_marker_when_sync_budget_exhausted() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    // Dirty the file, then forbid syncing (budget 0): the annotation must
    // still appear but carry an honest [stale] marker + a stderr hint.
    let p = project.path().join("src/auth.ts");
    let content = std::fs::read_to_string(&p).unwrap();
    std::fs::write(&p, format!("// pad\n{content}")).unwrap();

    let (stdout, stderr, code) = run_cli_env(
        &project,
        &["grep", "decoded !== null", "src/auth.ts"],
        &[("CODE_GRAPH_GREP_SYNC_BUDGET", "0")],
    );
    assert_eq!(code, 0);
    assert!(
        stdout.contains("[stale]"),
        "dirty + no budget → stale marker, got: {stdout}"
    );
    assert!(
        stderr.contains("incremental-index"),
        "stderr must point at the fix, got: {stderr:?}"
    );

    // JSON shape: container carries "stale": true.
    let (json_out, _, _) = run_cli_env(
        &project,
        &["grep", "decoded !== null", "src/auth.ts", "--json"],
        &[("CODE_GRAPH_GREP_SYNC_BUDGET", "0")],
    );
    let v: serde_json::Value = serde_json::from_str(json_out.trim()).unwrap();
    let entry = &v.as_array().unwrap()[0];
    assert_eq!(
        entry["container"]["stale"],
        serde_json::json!(true),
        "JSON container must flag staleness, got: {json_out}"
    );
}

/// `show` reads start_line/end_line straight from the index. Without query-time
/// freshness (parity with `grep`'s lazy resync + the MCP tools' ensure_file_fresh_opt),
/// an edit made after the last index leaves `show` reporting the pre-edit line
/// numbers — the "sed to a `show` line and land off by the inserted-line count" bug.
#[test]
fn test_cli_show_resyncs_after_edit() {
    let project = setup_indexed_project();
    let start = |out: &str| -> i64 {
        let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        v.as_array().unwrap()[0]["start_line"].as_i64().unwrap()
    };

    // hashPassword is defined only in src/auth.ts (no cross-file refs), so the
    // bare-name resolve is unambiguous.
    let (out1, _, code1) = run_cli(&project, &["show", "hashPassword", "--json"]);
    assert_eq!(code1, 0);
    let start1 = start(&out1);

    // Shift every symbol down by 5 lines.
    let p = project.path().join("src/auth.ts");
    let content = std::fs::read_to_string(&p).unwrap();
    std::fs::write(
        &p,
        format!("// pad\n// pad\n// pad\n// pad\n// pad\n{content}"),
    )
    .unwrap();

    let (out2, _, code2) = run_cli(&project, &["show", "hashPassword", "--json"]);
    assert_eq!(code2, 0);
    let start2 = start(&out2);
    assert_eq!(start2, start1 + 5,
        "show must report post-edit line numbers (lazy resync), got start1={start1} start2={start2}: {out2}");
}

// --- CLI freshness parity (MED-2): show/grep already resynced; refs/overview/
// search/ast-search/trace/similar/impact/dead-code now share the same lazy
// resync (refresh_files_if_stale). Each test uses CODE_GRAPH_RESYNC_BUDGET=0 as an
// in-test negative control: budget 0 disables the reindex, reproducing the pre-fix
// stale output — proving the resync (not some other effect) is what freshens the
// line numbers. Direct e2e coverage: refs, overview, search, dead-code, impact +
// the partial-disclosure path. trace (needs a route fixture) and similar (needs
// embeddings, unreachable in the no-default build) ride the shared helper only.

/// Pull the `start_line` of a named entry out of a JSON array of `{name,start_line,…}`.
fn json_start_line(out: &str, name: &str) -> i64 {
    let v: serde_json::Value = serde_json::from_str(out.trim())
        .unwrap_or_else(|e| panic!("invalid json: {e}; raw: {out}"));
    v.as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == name)
        .unwrap_or_else(|| panic!("{name} not found in: {out}"))["start_line"]
        .as_i64()
        .unwrap()
}

fn prepend_pad(project: &TempDir, rel: &str, lines: usize) {
    let p = project.path().join(rel);
    let content = std::fs::read_to_string(&p).unwrap();
    let pad = "// pad\n".repeat(lines);
    std::fs::write(&p, format!("{pad}{content}")).unwrap();
}

#[test]
fn test_cli_refs_resyncs_after_edit() {
    let project = setup_indexed_project();
    // handleLogin (src/api.ts) calls validateToken, so it is an incoming ref.
    let ref_start = |out: &str| -> i64 {
        let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        v["references"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == "handleLogin")
            .unwrap_or_else(|| panic!("handleLogin ref missing: {out}"))["start_line"]
            .as_i64()
            .unwrap()
    };
    let (out1, _, code1) = run_cli(&project, &["refs", "validateToken", "--json"]);
    assert_eq!(code1, 0, "{out1}");
    let s1 = ref_start(&out1);

    prepend_pad(&project, "src/api.ts", 5);

    // RED control: budget 0 → no resync → pre-edit line number.
    let (red, _, _) = run_cli_env(
        &project,
        &["refs", "validateToken", "--json"],
        &[("CODE_GRAPH_RESYNC_BUDGET", "0")],
    );
    assert_eq!(
        ref_start(&red),
        s1,
        "budget 0 must stay stale (proves the resync is load-bearing): {red}"
    );

    // GREEN: default budget → post-edit line number.
    let (out2, _, code2) = run_cli(&project, &["refs", "validateToken", "--json"]);
    assert_eq!(code2, 0);
    assert_eq!(
        ref_start(&out2),
        s1 + 5,
        "refs must report post-edit line numbers (lazy resync): {out2}"
    );
}

#[test]
fn test_cli_overview_resyncs_after_edit() {
    let project = setup_indexed_project();
    let (out1, _, code1) = run_cli(&project, &["overview", "src", "--json"]);
    assert_eq!(code1, 0, "{out1}");
    let s1 = json_start_line(&out1, "hashPassword");

    prepend_pad(&project, "src/auth.ts", 5);

    let (red, _, _) = run_cli_env(
        &project,
        &["overview", "src", "--json"],
        &[("CODE_GRAPH_RESYNC_BUDGET", "0")],
    );
    assert_eq!(
        json_start_line(&red, "hashPassword"),
        s1,
        "budget 0 must stay stale: {red}"
    );

    let (out2, _, code2) = run_cli(&project, &["overview", "src", "--json"]);
    assert_eq!(code2, 0);
    assert_eq!(
        json_start_line(&out2, "hashPassword"),
        s1 + 5,
        "overview must report post-edit line numbers: {out2}"
    );
}

#[test]
fn test_cli_search_resyncs_after_edit() {
    let project = setup_indexed_project();
    let (out1, _, code1) = run_cli(&project, &["search", "hashPassword", "--json"]);
    assert_eq!(code1, 0, "{out1}");
    let s1 = json_start_line(&out1, "hashPassword");

    prepend_pad(&project, "src/auth.ts", 5);

    let (red, _, _) = run_cli_env(
        &project,
        &["search", "hashPassword", "--json"],
        &[("CODE_GRAPH_RESYNC_BUDGET", "0")],
    );
    assert_eq!(
        json_start_line(&red, "hashPassword"),
        s1,
        "budget 0 must stay stale: {red}"
    );

    let (out2, _, code2) = run_cli(&project, &["search", "hashPassword", "--json"]);
    assert_eq!(code2, 0);
    assert_eq!(
        json_start_line(&out2, "hashPassword"),
        s1 + 5,
        "search must report post-edit line numbers: {out2}"
    );
}

#[test]
fn test_cli_dead_code_resyncs_after_edit() {
    let project = setup_indexed_project();
    // hashPassword is exported and unused → an exported-unused candidate.
    let (out1, _, code1) = run_cli(&project, &["dead-code", "--min-lines", "1", "--json"]);
    assert_eq!(code1, 0, "{out1}");
    let s1 = json_start_line(&out1, "hashPassword");

    prepend_pad(&project, "src/auth.ts", 5);

    let (red, _, _) = run_cli_env(
        &project,
        &["dead-code", "--min-lines", "1", "--json"],
        &[("CODE_GRAPH_RESYNC_BUDGET", "0")],
    );
    assert_eq!(
        json_start_line(&red, "hashPassword"),
        s1,
        "budget 0 must stay stale: {red}"
    );

    let (out2, _, code2) = run_cli(&project, &["dead-code", "--min-lines", "1", "--json"]);
    assert_eq!(code2, 0);
    assert_eq!(
        json_start_line(&out2, "hashPassword"),
        s1 + 5,
        "dead-code must report post-edit line numbers: {out2}"
    );
}

/// impact prints no line numbers, so freshness is observable as the caller SET:
/// adding a second caller in an existing caller file must be picked up after resync.
#[test]
fn test_cli_impact_resyncs_after_edit() {
    let project = setup_indexed_project();
    let total = |out: &str| -> i64 {
        let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        v["total_callers"].as_i64().unwrap()
    };
    let (out1, _, code1) = run_cli(&project, &["impact", "validateToken", "--json"]);
    assert_eq!(code1, 0, "{out1}");
    assert_eq!(
        total(&out1),
        1,
        "baseline: only handleLogin calls validateToken: {out1}"
    );

    // Append a second caller to src/api.ts (already a caller file → in the refresh set).
    let p = project.path().join("src/api.ts");
    let content = std::fs::read_to_string(&p).unwrap();
    std::fs::write(&p, format!(
        "{content}\nexport function handleRefresh(req: Request) {{\n    return validateToken(req.headers.authorization);\n}}\n"
    )).unwrap();

    let (red, _, _) = run_cli_env(
        &project,
        &["impact", "validateToken", "--json"],
        &[("CODE_GRAPH_RESYNC_BUDGET", "0")],
    );
    assert_eq!(
        total(&red),
        1,
        "budget 0 must stay stale (one caller): {red}"
    );

    let (out2, _, code2) = run_cli(&project, &["impact", "validateToken", "--json"]);
    assert_eq!(code2, 0);
    assert_eq!(
        total(&out2),
        2,
        "impact must reflect the added caller after resync: {out2}"
    );
}

/// Partial refresh (budget exhausted) must disclose on stderr only, never in the
/// stdout JSON contract; a fully-fresh run must stay silent.
#[test]
fn test_cli_resync_partial_discloses_on_stderr() {
    let project = setup_indexed_project();
    let (_, stderr_fresh, _) = run_cli(&project, &["overview", "src", "--json"]);
    assert!(
        !stderr_fresh.contains("changed since indexing"),
        "no disclosure when everything is fresh: {stderr_fresh:?}"
    );

    prepend_pad(&project, "src/auth.ts", 1);

    let (stdout, stderr, code) = run_cli_env(
        &project,
        &["overview", "src", "--json"],
        &[("CODE_GRAPH_RESYNC_BUDGET", "0")],
    );
    assert_eq!(code, 0);
    assert!(
        stderr.contains("changed since indexing"),
        "partial refresh must disclose on stderr: {stderr:?}"
    );
    assert!(
        stderr.contains("incremental-index"),
        "disclosure must point at the fix: {stderr:?}"
    );
    serde_json::from_str::<serde_json::Value>(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must stay clean JSON: {e}; raw: {stdout}"));
    assert!(
        !stdout.contains("changed since indexing"),
        "note must not pollute stdout: {stdout}"
    );
}

fn has_git() -> bool {
    Command::new("git").arg("--version").output().is_ok()
}

#[test]
fn test_cli_grep_tracked_but_gitignored() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    if !has_git() {
        eprintln!("skipping: git not installed");
        return;
    }
    let project = setup_indexed_project();
    let root = project.path();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(root.join("docs/notes.md"), "tracked_needle lives here\n").unwrap();
    std::fs::write(root.join("docs/scratch.md"), "scratch_needle lives here\n").unwrap();
    std::fs::write(root.join(".gitignore"), "docs/\n.code-graph/\n").unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "add",
        "-f",
        "docs/notes.md",
    ]);

    // git-grep parity: a tracked file stays searchable even when its directory
    // is gitignored (rg alone skips it — the audited blind spot).
    let (stdout, stderr, code) = run_cli(&project, &["grep", "tracked_needle"]);
    assert_eq!(
        code, 0,
        "tracked-but-gitignored file must be found; stderr={stderr}"
    );
    assert!(stdout.contains("docs/notes.md"), "got: {stdout}");

    // Untracked + ignored stays invisible (matches git grep semantics).
    let (_, _, code2) = run_cli(&project, &["grep", "scratch_needle"]);
    assert_eq!(code2, 1, "untracked ignored file must stay skipped");
}

/// Issue #34: the supplement (tracked files rg's walk skips) used to be appended
/// to ONE argv, capped at 500 entries. On Windows that argv blew past the 32 KB
/// command-line limit (`os error 206`) and the cap silently dropped the rest —
/// `grep` reporting "no matches" for a file that had one. The supplement is now
/// split into argv-sized batches; every tracked file is searched.
///
/// Driven through `CODE_GRAPH_RG_ARGV_BUDGET` so the batch boundary is crossed
/// with a handful of files instead of a real 32 KB command line.
#[test]
fn test_cli_grep_supplement_batches_across_argv_budget() {
    if !has_ripgrep() || !has_git() {
        eprintln!("skipping: rg or git not installed");
        return;
    }
    let project = setup_indexed_project();
    let root = project.path();
    std::fs::create_dir_all(root.join("vendored")).unwrap();
    std::fs::write(root.join(".gitignore"), "vendored/\n.code-graph/\n").unwrap();

    // 24 force-tracked files inside a gitignored dir → all 24 reach rg only via
    // the supplement, and a 40-char budget fits ~1 path per batch.
    const N: usize = 24;
    let mut add_args: Vec<String> = vec!["add".into(), "-f".into()];
    for i in 0..N {
        let rel = format!("vendored/file_{i:02}.md");
        std::fs::write(root.join(&rel), format!("batched_needle number {i}\n")).unwrap();
        add_args.push(rel);
    }
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"]);
    let add_refs: Vec<&str> = add_args.iter().map(|s| s.as_str()).collect();
    git(&add_refs);

    let (stdout, stderr, code) = run_cli_env(
        &project,
        &["grep", "batched_needle"],
        &[("CODE_GRAPH_RG_ARGV_BUDGET", "40")],
    );
    assert_eq!(
        code, 0,
        "batched supplement grep must match; stderr={stderr}"
    );
    for i in 0..N {
        assert!(
            stdout.contains(&format!("vendored/file_{i:02}.md")),
            "file_{i:02}.md missing from batched results — a batch was dropped.\n{stdout}"
        );
    }
    // Each file appears exactly once: a file must not be scanned by both the
    // walk and a supplement batch (the duplicate-output half of issue #34).
    assert_eq!(
        stdout.matches("batched_needle number 7").count(),
        1,
        "each match must be emitted once, got:\n{stdout}"
    );
}

#[test]
fn test_cli_grep_supplement_respects_filters() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    if !has_git() {
        eprintln!("skipping: git not installed");
        return;
    }
    let project = setup_indexed_project();
    let root = project.path();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    // Markdown in a gitignored dir, force-tracked → reaches grep via the git-grep
    // SUPPLEMENT (appended as an explicit rg arg), NOT the rg walk. rg does not
    // apply --type/--glob to explicit args, so without our supplement re-filter
    // this markdown would leak past -t rust / -g '!*.md'.
    std::fs::write(root.join("docs/leak.md"), "LEAKPAT in markdown\n").unwrap();
    // A normal rust file the rg walk finds (untracked-but-not-ignored).
    std::fs::write(root.join("probe.rs"), "fn p() { /* LEAKPAT */ }\n").unwrap();
    std::fs::write(root.join(".gitignore"), "docs/\n.code-graph/\n").unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "add",
        "-f",
        "docs/leak.md",
    ]);

    // Baseline: no filter → supplement brings in the gitignored-but-tracked md.
    let (base, _, bc) = run_cli(&project, &["grep", "LEAKPAT"]);
    assert_eq!(bc, 0);
    assert!(
        base.contains("docs/leak.md") && base.contains("probe.rs"),
        "no filter must find both walk + supplement, got: {base}"
    );

    // -t rust must NOT leak the markdown supplement; the rust walk file stays.
    let (t, _, tc) = run_cli(&project, &["grep", "-t", "rust", "LEAKPAT"]);
    assert_eq!(tc, 0, "rust file still matches");
    assert!(t.contains("probe.rs"), "rust walk file kept, got: {t}");
    assert!(
        !t.contains("leak.md"),
        "-t rust must filter the md supplement, got: {t}"
    );

    // -g '!*.md' must exclude the markdown supplement too.
    let (g, _, _gc) = run_cli(&project, &["grep", "-g", "!*.md", "LEAKPAT"]);
    assert!(
        g.contains("probe.rs") && !g.contains("leak.md"),
        "-g '!*.md' must exclude the md supplement, got: {g}"
    );

    // -c -t rust must not count the markdown supplement file.
    let (c, _, _cc) = run_cli(&project, &["grep", "-c", "-t", "rust", "LEAKPAT"]);
    assert!(
        !c.contains("leak.md"),
        "-c -t rust must not count the md supplement, got: {c}"
    );
}

#[test]
fn test_cli_grep_gitignore_negation_divergence() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    if !has_git() {
        eprintln!("skipping: git not installed");
        return;
    }
    let project = setup_indexed_project();
    let root = project.path();
    // daagu-shape divergence: a bare dir-name pattern (`keep/`, matches at any
    // depth) with a specific-path whitelist (`!sub/keep/`). git evaluates the
    // negation and treats sub/keep/a.txt as NOT ignored (plain `git add`
    // works), but rg 14.x prunes the directory during the walk before the
    // negation applies, so the file never gets searched — the tracked\walked
    // supplement must restore it (feedback_srcpath_abs_path_blindspot,
    // previously unfixed).
    std::fs::create_dir_all(root.join("sub/keep")).unwrap();
    std::fs::write(root.join("sub/keep/a.txt"), "negation_needle here\n").unwrap();
    std::fs::write(root.join(".gitignore"), "keep/\n!sub/keep/\n.code-graph/\n").unwrap();
    // A tracked hidden file is the third blind-spot class (rg skips hidden).
    std::fs::write(root.join(".hidden-config.md"), "hidden_needle here\n").unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["add", "sub/keep/a.txt", ".hidden-config.md"]);

    let (stdout, stderr, code) = run_cli(&project, &["grep", "negation_needle"]);
    assert_eq!(
        code, 0,
        "whitelisted-but-pruned tracked file must be found; stderr={stderr}"
    );
    assert!(stdout.contains("sub/keep/a.txt"), "got: {stdout}");

    let (stdout2, _, code2) = run_cli(&project, &["grep", "hidden_needle"]);
    assert_eq!(code2, 0, "tracked hidden file must be found");
    assert!(stdout2.contains(".hidden-config.md"), "got: {stdout2}");

    // Path-scoped: supplement must honor the path restriction.
    let (_, _, code3) = run_cli(&project, &["grep", "negation_needle", "src/"]);
    assert_eq!(
        code3, 1,
        "supplement must not leak outside the requested path scope"
    );
}

#[test]
fn test_cli_grep_sigpipe_graceful() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    use std::io::Read;
    let project = setup_indexed_project();
    // >64 KiB of matches so the writer outlives the pipe buffer after the
    // reader hangs up — forces EPIPE mid-stream.
    let many = "needle line that is long enough to fill the pipe buffer quickly\n".repeat(3000);
    std::fs::write(project.path().join("many.txt"), &many).unwrap();
    let mut child = Command::new(binary_path())
        .current_dir(project.path())
        .args(["grep", "needle", "many.txt", "--max-count", "0"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Read a token amount, then hang up while the writer still has data.
    let mut stdout = child.stdout.take().unwrap();
    let mut buf = [0u8; 512];
    let _ = stdout.read(&mut buf).unwrap();
    drop(stdout);
    let status = child.wait().unwrap();
    let mut err = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    assert!(
        !err.contains("Broken pipe"),
        "EPIPE must be handled silently like grep, got stderr: {err:?}"
    );
    assert_eq!(
        status.code(),
        Some(0),
        "early reader hangup is not an error"
    );
}

// The grep contract above (EPIPE → silent, exit 0) must hold for EVERY command,
// not just grep/stats: `health-check | head` used to panic in `println!` and
// `map | head` used to print `Error: Broken pipe (os error 32)` via the anyhow
// return path. Pre-closing the pipe's read end before spawn makes the child's
// FIRST stdout write hit EPIPE deterministically — no output-size or timing
// dependence, so small-output commands like health-check are testable too.
#[cfg(unix)]
#[test]
fn test_cli_sigpipe_graceful_non_grep_commands() {
    let project = setup_indexed_project();
    for cmd in [["health-check"], ["map"]] {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader); // read end closed before the child ever writes
        let child = Command::new(binary_path())
            .current_dir(project.path())
            .args(cmd)
            .stdout(writer)
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let out = child.wait_with_output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            !err.contains("Broken pipe") && !err.contains("panicked"),
            "{cmd:?}: EPIPE must be silent like grep, got stderr: {err:?}"
        );
        assert_eq!(
            out.status.code(),
            Some(0),
            "{cmd:?}: early reader hangup is not an error"
        );
    }
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
    assert!(
        stdout.contains("handleLogin"),
        "should show caller handleLogin, got: {}",
        stdout
    );
}

#[test]
fn test_cli_callgraph_compact() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["callgraph", "validateToken", "--compact"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("validateToken"));
    // Compact: no [function] type annotation
    assert!(
        !stdout.contains("[function]"),
        "compact should not have type annotation"
    );
}

#[test]
fn test_cli_callgraph_direction() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(
        &project,
        &["callgraph", "handleLogin", "--direction", "callees"],
    );
    assert_eq!(code, 0);
    assert!(
        stdout.contains("validateToken"),
        "handleLogin should call validateToken"
    );
}

#[test]
fn test_cli_callgraph_nonexistent() {
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["callgraph", "nonexistent_fn"]);
    assert_ne!(
        code, 0,
        "nonexistent symbol should return non-zero exit code"
    );
    assert!(
        stderr.contains("No call graph results"),
        "should report not found"
    );
}

// Regression: `--direction` must be validated at the CLI layer (like cmd_deps does).
// Without early validation a typo only surfaced after ambiguity resolution: user
// got "Ambiguous symbol" first, retried with --file, then was told "invalid direction" —
// two error messages for one mistake.
#[test]
fn test_cli_callgraph_invalid_direction_errors_early() {
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(
        &project,
        &["callgraph", "validateToken", "--direction", "bogus"],
    );
    assert_eq!(code, 1, "bad --direction should error; stderr={stderr:?}");
    assert!(
        stderr.contains("--direction must be one of"),
        "stderr should explain the valid set; got: {stderr:?}"
    );
}

/// R10: enum filters accept case variants (parity with --node-type / --min-confidence
/// / --language, which already normalize case). Valid UPPERCASE must be accepted, and
/// cross-vocab must still be rejected case-insensitively.
#[test]
fn test_cli_enum_filters_accept_case_variants() {
    let project = setup_indexed_project();
    // Uppercase valid direction → accepted (not the case-sensitive rejection).
    let (_, stderr, code) = run_cli(
        &project,
        &["callgraph", "validateToken", "--direction", "BOTH"],
    );
    assert_eq!(
        code, 0,
        "uppercase --direction BOTH should be accepted; stderr={stderr:?}"
    );
    assert!(
        !stderr.contains("--direction must be one of"),
        "BOTH must not hit the case-sensitive rejection; got: {stderr:?}"
    );
    // Uppercase valid relation → accepted.
    let (_, stderr, _) = run_cli(&project, &["refs", "validateToken", "--relation", "CALLS"]);
    assert!(
        !stderr.contains("--relation must be one of"),
        "CALLS must not hit the case-sensitive rejection; got: {stderr:?}"
    );
    // Cross-vocab is STILL rejected, case-insensitively (a deps word on callgraph).
    let (_, stderr, code) = run_cli(
        &project,
        &["callgraph", "validateToken", "--direction", "OUTGOING"],
    );
    assert_eq!(
        code, 1,
        "cross-vocab --direction OUTGOING must still error; stderr={stderr:?}"
    );
    assert!(
        stderr.contains("--direction must be one of: callers, callees, both"),
        "cross-vocab must still be rejected; got: {stderr:?}"
    );
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
    assert!(
        stdout.contains("call graph") || stdout.contains("--direction"),
        "help should describe the command; got: {stdout:?}"
    );
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
    assert_ne!(
        code, 0,
        "nonexistent symbol should return non-zero exit code"
    );
    assert!(
        stderr.contains("Symbol not found"),
        "should report symbol not found"
    );
}

#[test]
fn test_cli_impact_change_type_remove() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(
        &project,
        &["impact", "validateToken", "--change-type", "remove"],
    );
    assert_eq!(code, 0);
    assert!(stdout.contains("Risk:"));
}

#[test]
fn test_cli_impact_invalid_change_type() {
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(
        &project,
        &["impact", "validateToken", "--change-type", "invalid"],
    );
    assert_ne!(code, 0, "invalid change-type should fail");
    assert!(
        stderr.contains("must be one of"),
        "should show valid options"
    );
}

#[test]
fn test_cli_impact_json() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["impact", "validateToken", "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(v["risk"].is_string());
    assert!(v["symbol"].is_string());
    // value_references mirrors the MCP impact tool (CLI/MCP parity) — must be present.
    assert!(
        v["value_references"].is_number(),
        "CLI impact json must expose value_references like MCP"
    );
}

#[test]
fn test_cli_impact_json_reports_value_references() {
    // Migrated from the MCP `test_r15_impact_reports_value_references` (removed with
    // the standalone impact_analysis MCP tool): `impact` is the canonical full-impact
    // surface, so a fn passed as a callback must surface value_references >= 1 there —
    // rename / signature-change risk must include callback coupling.
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("app.rs"),
        "pub fn caller() { register(handler); }\nfn register<F>(_f: F) {}\nfn handler() {}\n",
    )
    .unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    let (stdout, _, code) = run_cli(&project, &["impact", "handler", "--json"]);
    assert_eq!(code, 0, "impact handler should succeed; got: {stdout}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(
        v["value_references"].as_u64().unwrap_or(0) >= 1,
        "CLI impact(handler) must report value_references >= 1 (callback coupling); got: {stdout}"
    );
    // The referencer (`caller`) references but never CALLS `handler`, so it must NOT
    // inflate the direct-caller count — the calls-vs-references separation (migrated
    // from the MCP test_r14). Only a value reference exists, so direct_callers == 0.
    assert_eq!(
        v["direct_callers"].as_u64(),
        Some(0),
        "a callback referencer must not count as a direct CALLER; got: {stdout}"
    );
}

#[test]
fn test_cli_impact_json_struct_returns_unknown_risk() {
    // Migrated from the MCP `test_impact_analysis_struct_*` tests (removed with the
    // standalone impact_analysis MCP tool): a non-function symbol (struct) with no
    // call-graph callers must get UNKNOWN risk (not LOW) plus a non-function warning,
    // so "0 callers" on a type isn't misread as safe to change.
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("models.rs"),
        "pub struct UserModel { pub id: i64 }\n",
    )
    .unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    let (stdout, _, code) = run_cli(&project, &["impact", "UserModel", "--json"]);
    assert_eq!(code, 0, "impact UserModel should succeed; got: {stdout}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        v["risk"].as_str(),
        Some("UNKNOWN"),
        "non-function struct with no callers must be UNKNOWN risk, not LOW; got: {stdout}"
    );
    assert!(
        v["warning"].is_string(),
        "non-function impact must carry a warning steering to refs/find_references; got: {stdout}"
    );
}

#[test]
fn test_cli_impact_json_route_handler_affected_routes() {
    // Migrated from the MCP `test_e2e_impact_on_inline_route_handler` (removed with the
    // standalone impact_analysis tool): an inline (arrow) route handler materializes as
    // a function node and flows through impact as a route-carrying caller of what it
    // calls, so impact on that callee reports a caller + affected_routes >= 1.
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("server.js"),
        "function logAccess(msg) { return msg; }\napp.get(\"/widgets\", (req, res) => { logAccess(\"hit\"); res.json([]); });\n").unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    let (stdout, _, code) = run_cli(&project, &["impact", "logAccess", "--json"]);
    assert_eq!(code, 0, "impact logAccess should succeed; got: {stdout}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(
        v["direct_callers"].as_u64().unwrap_or(0) >= 1,
        "the inline route handler must count as a direct caller of logAccess; got: {stdout}"
    );
    assert!(
        v["affected_routes"].as_u64().unwrap_or(0) >= 1,
        "the materialized handler carries a route, so affected_routes must be >= 1; got: {stdout}"
    );
}

// clap-migrated (audit #4 Step 5): clap owns --help + unknown-flag rejection;
// --change-type stays an in-handler String so invalid-change-type is exit-1 (above).
#[test]
fn test_cli_impact_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["impact", "--help"]);
    assert_eq!(code, 0, "impact --help should exit 0 (clap help)");
    assert!(
        stdout.contains("Impact analysis") || stdout.contains("--change-type"),
        "help should describe the command; got: {stdout:?}"
    );
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
    std::fs::write(
        project.path().join("lib.rs"),
        r#"
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
"#,
    )
    .unwrap();
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
    assert_eq!(
        code, 1,
        "same-file overload `new` must error, not silently merge; stderr={stderr:?}"
    );
    assert!(
        stderr.contains("Ambiguous symbol 'new'"),
        "should report ambiguity; got: {stderr:?}"
    );
    // The guidance must be accurate for same-file overloads: file_path can't
    // split them, so point at the node_id-capable tools instead.
    assert!(
        stderr.contains("same file") && stderr.contains("node-id"),
        "same-file message must mention 'same file' + a node-id path; got: {stderr:?}"
    );
}

#[test]
fn test_cli_callgraph_same_file_overload_is_ambiguous_json() {
    let project = setup_same_file_overload_project();
    let (stdout, _, code) = run_cli(&project, &["callgraph", "new", "--json"]);
    assert_eq!(code, 1, "same-file overload must error in --json mode too");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(
        v["error"].as_str().unwrap_or("").contains("Ambiguous"),
        "json error field should report ambiguity; got: {stdout}"
    );
    let sugg = v["suggestions"].as_array().expect("suggestions array");
    assert!(
        sugg.len() >= 2,
        "expected ≥2 node_id suggestions; got: {stdout}"
    );
    for s in sugg {
        assert!(
            s["node_id"].as_i64().is_some(),
            "suggestion needs node_id: {s}"
        );
        assert!(
            s["start_line"].as_i64().is_some(),
            "suggestion needs start_line: {s}"
        );
    }
}

#[test]
fn test_cli_impact_same_file_overload_is_ambiguous() {
    let project = setup_same_file_overload_project();
    let (_, stderr, code) = run_cli(&project, &["impact", "new"]);
    assert_eq!(
        code, 1,
        "same-file overload `new` must error in impact, not merge callers; stderr={stderr:?}"
    );
    assert!(
        stderr.contains("Ambiguous symbol 'new'"),
        "should report ambiguity; got: {stderr:?}"
    );
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
    std::fs::write(
        src.join("db.py"),
        "def save(record):\n    return _write(record)\n\ndef _write(record):\n    return True\n",
    )
    .unwrap();
    std::fs::write(
        src.join("cache.py"),
        "def save(item):\n    return _store(item)\n\ndef _store(item):\n    return True\n",
    )
    .unwrap();
    std::fs::write(
        src.join("app.py"),
        "from db import save\n\ndef run():\n    return save({\"id\": 1})\n",
    )
    .unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    // run() must call EXACTLY ONE save — the imported db.save — not fan out.
    // The surviving edge is `ambiguous` (target name `save` has 2 same-language
    // defs), so probe with --min-confidence ambiguous: the default floor
    // (inferred) hides the by-name class this resolution-layer test asserts on.
    let (stdout, _, code) = run_cli(
        &project,
        &[
            "callgraph",
            "run",
            "--min-confidence",
            "ambiguous",
            "--json",
        ],
    );
    assert_eq!(code, 0, "callgraph run should succeed; {stdout}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let results = v["results"].as_array().expect("results array");
    let save_callees: Vec<&serde_json::Value> =
        results.iter().filter(|r| r["name"] == "save").collect();
    assert_eq!(save_callees.len(), 1,
        "run must call exactly one save (the imported db.save), not fan out to cache.save; got: {stdout}");
    assert_eq!(
        save_callees[0]["file_path"], "src/db.py",
        "the surviving save edge must be db.save (imported), not cache.save; got: {stdout}"
    );

    // cache.save must have NO caller — `run` imports from db, not cache.
    let (stdout2, _, _) = run_cli(
        &project,
        &[
            "callgraph",
            "save",
            "--file",
            "src/cache.py",
            "--min-confidence",
            "ambiguous",
            "--json",
        ],
    );
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
    std::fs::write(
        src.join("main.rs"),
        "mod a;\nmod b;\nfn main() {\n    let x = thing();\n    println!(\"{}\", x);\n}\n",
    )
    .unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    // Tied `thing` edges are `ambiguous` (2 same-language defs); probe with
    // --min-confidence ambiguous since the default floor hides that class.
    let (stdout, _, code) = run_cli(
        &project,
        &[
            "callgraph",
            "main",
            "--min-confidence",
            "ambiguous",
            "--json",
        ],
    );
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
    std::fs::write(
        src.join("db.py"),
        "def save(r):\n    return _w(r)\n\ndef _w(r):\n    return True\n",
    )
    .unwrap();
    std::fs::write(
        src.join("cache.py"),
        "def save(i):\n    return _s(i)\n\ndef _s(i):\n    return True\n",
    )
    .unwrap();
    // bare imported call to db.save + qualified call to cache.save — both legit.
    std::fs::write(src.join("app.py"),
        "from db import save\nimport cache\n\ndef run():\n    save({\"id\": 1})\n    return cache.save({\"id\": 2})\n").unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    // Both surviving edges are `ambiguous` (`save` has 2 same-language defs);
    // probe with --min-confidence ambiguous since the default floor hides them.
    let (stdout, _, code) = run_cli(
        &project,
        &[
            "callgraph",
            "run",
            "--min-confidence",
            "ambiguous",
            "--json",
        ],
    );
    assert_eq!(code, 0, "callgraph run should succeed; {stdout}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let results = v["results"].as_array().expect("results array");
    let save_files: std::collections::HashSet<&str> = results
        .iter()
        .filter(|r| r["name"] == "save")
        .filter_map(|r| r["file_path"].as_str())
        .collect();
    assert!(
        save_files.contains("src/db.py"),
        "the bare imported call must keep run→db.save; got: {stdout}"
    );
    assert!(
        save_files.contains("src/cache.py"),
        "the qualified cache.save() call must NOT be false-pruned; got: {stdout}"
    );
}

#[test]
fn test_cli_callgraph_hides_ambiguous_fanout_by_default() {
    // New default (v0.76): the confidence floor `inferred` hides the `ambiguous`
    // by-name fan-out from callgraph output so the agent isn't fed phantom edges
    // (a method name shared by many defs resolving to all of them). Same fixture
    // as the tie test: `main` calls `thing()` with two `thing` defs, so both call
    // edges are ambiguous. Default view: neither shown, suppressed count
    // disclosed. --min-confidence ambiguous: both restored.
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a.rs"), "pub fn thing() -> i32 { 1 }\n").unwrap();
    std::fs::write(src.join("b.rs"), "pub fn thing() -> i32 { 2 }\n").unwrap();
    std::fs::write(
        src.join("main.rs"),
        "mod a;\nmod b;\nfn main() {\n    let x = thing();\n    println!(\"{}\", x);\n}\n",
    )
    .unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    // Default floor: ambiguous `thing` edges hidden, disclosure present.
    let (stdout, _, code) = run_cli(&project, &["callgraph", "main", "--json"]);
    assert_eq!(code, 0, "callgraph main should succeed; {stdout}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let results = v["results"].as_array().expect("results array");
    assert!(
        !results.iter().any(|r| r["name"] == "thing"),
        "default floor must hide the ambiguous `thing` fan-out; got: {stdout}"
    );
    assert_eq!(
        v["ambiguous_edges_hidden"].as_u64(),
        Some(2),
        "default view must disclose the 2 hidden ambiguous edges; got: {stdout}"
    );

    // Opt-in: --min-confidence ambiguous restores both edges, nothing suppressed.
    let (stdout2, _, _) = run_cli(
        &project,
        &[
            "callgraph",
            "main",
            "--min-confidence",
            "ambiguous",
            "--json",
        ],
    );
    let v2: serde_json::Value = serde_json::from_str(stdout2.trim()).unwrap();
    let shown = v2["results"]
        .as_array()
        .expect("results array")
        .iter()
        .filter(|r| r["name"] == "thing")
        .count();
    assert_eq!(
        shown, 2,
        "--min-confidence ambiguous must show both tied edges; got: {stdout2}"
    );
    assert!(
        v2.get("ambiguous_edges_hidden").is_none(),
        "nothing is suppressed at the ambiguous floor; got: {stdout2}"
    );
}

#[test]
fn test_cli_impact_folds_ambiguous_callers_but_discloses() {
    // New default (v0.76): impact folds the ambiguous by-name caller fan-out out
    // of the risk count, but DISCLOSES the excluded count so a folded real caller
    // never silently under-states risk (unlike callgraph, an ambiguous caller may
    // be a true dependency). Fixture: `save` is defined in two files and `run`
    // calls it WITHOUT importing either def, so the by-name edge is genuinely
    // ambiguous (2 tied candidates). (An explicit `from db import save` would
    // corroborate the binding → `inferred`, NOT folded — that path is covered by
    // resolve::confidence::classify_import_corroborated_duplicate_stays_visible.)
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("db.py"), "def save(r):\n    return True\n").unwrap();
    std::fs::write(src.join("cache.py"), "def save(i):\n    return True\n").unwrap();
    std::fs::write(
        src.join("app.py"),
        "def run():\n    return save({\"id\": 1})\n",
    )
    .unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    // Default floor: the ambiguous caller `run` is folded out of the count, but
    // the exclusion is disclosed. --file disambiguates so the exact-name guard
    // doesn't fire.
    let (stdout, _, code) = run_cli(
        &project,
        &["impact", "save", "--file", "src/db.py", "--json"],
    );
    assert_eq!(code, 0, "impact should succeed; {stdout}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        v["total_callers"].as_u64(),
        Some(0),
        "ambiguous caller folded out of the risk count by default; got: {stdout}"
    );
    assert_eq!(
        v["ambiguous_callers_excluded"].as_u64(),
        Some(1),
        "the folded caller must be disclosed, not silently dropped; got: {stdout}"
    );

    // Opt-in: --min-confidence ambiguous counts the ambiguous caller.
    let (stdout2, _, _) = run_cli(
        &project,
        &[
            "impact",
            "save",
            "--file",
            "src/db.py",
            "--min-confidence",
            "ambiguous",
            "--json",
        ],
    );
    let v2: serde_json::Value = serde_json::from_str(stdout2.trim()).unwrap();
    assert_eq!(
        v2["total_callers"].as_u64(),
        Some(1),
        "--min-confidence ambiguous counts the ambiguous caller; got: {stdout2}"
    );
    assert!(
        v2.get("ambiguous_callers_excluded").is_none(),
        "nothing excluded at the ambiguous floor; got: {stdout2}"
    );
}

#[test]
fn test_cli_impact_discloses_transitive_ambiguous_callers() {
    // Regression for the frontier-disclosure fix (v0.76.1): a uniquely-named
    // target `uniq_target` has a clean (inferred) direct caller `amb`, so
    // SEED-DIRECT excluded == 0. But `amb` is ambiguously named (2 defs), so its
    // own caller `caller_b` is folded at the default floor. The disclosure must
    // count `caller_b` (frontier-wide), else a uniquely-named symbol under-states
    // risk with ZERO hint — the silent under-statement the reviewer flagged.
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("core.py"), "def uniq_target():\n    return 1\n").unwrap();
    // `amb` (ambiguous — 2 defs) is the clean inferred direct caller of uniq_target.
    std::fs::write(
        src.join("a.py"),
        "from core import uniq_target\n\ndef amb():\n    return uniq_target()\n",
    )
    .unwrap();
    std::fs::write(src.join("a2.py"), "def amb():\n    return 2\n").unwrap();
    // `caller_b` calls amb bare (no import) — a TRANSITIVE caller folded because
    // amb is ambiguous. (Importing amb here would corroborate the edge → inferred,
    // so it would no longer be folded; keep it un-imported to exercise the fold.)
    std::fs::write(src.join("b.py"), "def caller_b():\n    return amb()\n").unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    let (stdout, _, code) = run_cli(&project, &["impact", "uniq_target", "--json"]);
    assert_eq!(code, 0, "impact should succeed; {stdout}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        v["total_callers"].as_u64(),
        Some(1),
        "inferred direct caller kept, transitive ambiguous one folded; got: {stdout}"
    );
    assert!(v["ambiguous_callers_excluded"].as_u64().unwrap_or(0) >= 1,
        "the folded TRANSITIVE ambiguous caller must be disclosed (seed-direct excluded is 0 here); got: {stdout}");
}

// ============================================================
// stats (clap-migrated, audit #4) — contract lock
// ============================================================

#[test]
fn test_cli_stats_no_data() {
    // Freshly-indexed project has no usage.jsonl yet → handler returns Ok (exit 0).
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["stats"]);
    assert_eq!(
        code, 0,
        "stats with no usage data should exit 0; stderr={stderr:?}"
    );
    assert!(
        stderr.contains("No usage data"),
        "should explain absence; got: {stderr:?}"
    );
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
    assert_eq!(
        code, 2,
        "non-numeric --last must be a clap parse error (exit 2); stderr={stderr:?}"
    );
    assert!(
        stderr.contains("invalid value") && stderr.contains("abc"),
        "clap should name the bad value; got: {stderr:?}"
    );
}

#[test]
fn test_cli_stats_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["stats", "--help"]);
    assert_eq!(code, 0, "stats --help should exit 0 (clap help)");
    assert!(
        stdout.contains("Aggregate session metrics") || stdout.contains("--last"),
        "help should describe the command; got: {stdout:?}"
    );
}

#[test]
fn test_cli_stats_sigpipe_graceful() {
    // Regression: `stats | head` (early reader hangup) must NOT panic on EPIPE.
    // Before the sout! macro, cmd_stats used raw println! which panics on a
    // broken pipe -> SIGABRT (exit 134). Contract mirrors grep's
    // test_cli_grep_sigpipe_graceful: silent, exit 0.
    use std::io::Read;
    let project = setup_indexed_project();
    // Synthesize a usage.jsonl whose table is far larger than the 64 KiB pipe
    // buffer (one row per tool) so the writer outlives the reader hangup and
    // hits EPIPE mid-stream rather than fitting entirely in the buffer.
    let mut line = String::from("{\"tools\":{");
    for i in 0..3000 {
        if i > 0 {
            line.push(',');
        }
        line.push_str(&format!(
            "\"qa_tool_{i:04}\":{{\"n\":1,\"ms\":1,\"err\":0,\"max_ms\":1}}"
        ));
    }
    line.push_str("}}\n");
    let cg = project.path().join(".code-graph");
    std::fs::create_dir_all(&cg).unwrap();
    std::fs::write(cg.join("usage.jsonl"), &line).unwrap();

    let mut child = Command::new(binary_path())
        .current_dir(project.path())
        .args(["stats"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Read a token amount, then hang up while the writer still has data.
    let mut stdout = child.stdout.take().unwrap();
    let mut buf = [0u8; 512];
    let _ = stdout.read(&mut buf).unwrap();
    drop(stdout);
    let status = child.wait().unwrap();
    let mut err = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    assert!(
        !err.contains("panicked") && !err.contains("Broken pipe"),
        "EPIPE must be handled silently like grep, got stderr: {err:?}"
    );
    assert_eq!(
        status.code(),
        Some(0),
        "early reader hangup is not an error (was SIGABRT/134 before the fix); status={status:?}"
    );
}

#[test]
fn test_cli_stats_unknown_flag_errors() {
    // Flavor-B: clap rejects unknown flags (was: silently ignored).
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["stats", "--bogus"]);
    assert_eq!(
        code, 2,
        "unknown flag must error under clap; stderr={stderr:?}"
    );
    assert!(
        stderr.contains("unexpected")
            || stderr.contains("--bogus")
            || stderr.contains("unrecognized"),
        "clap should name the unknown flag; got: {stderr:?}"
    );
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
    assert_eq!(
        v["recommendations"]["state"], "absent",
        "JSON stats must mark recommendations.state=absent; got: {jstdout:?}"
    );
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
    assert_eq!(
        v["recommendations"]["state"], "empty",
        "an empty recommendations.jsonl is 'empty', distinct from 'absent'; got: {jstdout:?}"
    );
}

// P1-6: the misleading "Conversion (proxy)" headline (tool_calls / recs = two
// independent populations) is renamed to an honest volume label, and folded/
// hidden tool names from older sessions are flagged so the table doesn't
// commingle them with the live tools/list surface (domain::LIVE_MCP_TOOLS).
#[test]
fn test_cli_stats_marks_legacy_tool_names_and_volume_label() {
    let project = setup_indexed_project();
    let cg = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    // One session mixing a live tool (get_call_graph) with a folded one
    // (read_snippet, merged into get_ast_node) + a recommendation so the volume
    // line prints.
    std::fs::write(
        cg.join("usage.jsonl"),
        "{\"ts\":\"2026-06-01T00:00:00Z\",\"v\":\"0.45.4\",\"tools\":{\"get_call_graph\":{\"n\":2,\"ms\":5,\"err\":0,\"max_ms\":5},\"read_snippet\":{\"n\":3,\"ms\":4,\"err\":0,\"max_ms\":4}}}\n",
    ).unwrap();
    std::fs::write(
        cg.join("recommendations.jsonl"),
        "{\"hook\":\"pre-grep-guide\",\"action\":\"deny\"}\n",
    )
    .unwrap();

    let (stdout, _, code) = run_cli(&project, &["stats"]);
    assert_eq!(code, 0, "stats should run; stdout={stdout}");
    assert!(
        stdout.contains("read_snippet †"),
        "folded tool name must be flagged legacy; got: {stdout}"
    );
    assert!(
        stdout.contains("† not in the current tools/list surface"),
        "legacy footnote must be present; got: {stdout}"
    );
    assert!(
        !stdout.contains("get_call_graph †"),
        "a live tool must NOT be flagged legacy; got: {stdout}"
    );
    assert!(
        stdout.contains("Tool-call volume:") && stdout.contains("not conversion"),
        "the volume ratio must not be labeled 'Conversion'; got: {stdout}"
    );
    assert!(
        !stdout.contains("Conversion (proxy)"),
        "the old misleading 'Conversion (proxy)' label must be gone; got: {stdout}"
    );
}

#[test]
fn test_cli_stats_json_renames_conversion_and_lists_live_tools() {
    let project = setup_indexed_project();
    let cg = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::write(
        cg.join("usage.jsonl"),
        "{\"ts\":\"2026-06-01T00:00:00Z\",\"v\":\"0.45.4\",\"tools\":{\"get_call_graph\":{\"n\":1,\"ms\":5,\"err\":0,\"max_ms\":5}}}\n",
    ).unwrap();
    std::fs::write(
        cg.join("recommendations.jsonl"),
        "{\"hook\":\"pre-grep-guide\",\"action\":\"deny\"}\n",
    )
    .unwrap();
    let (jstdout, _, jcode) = run_cli(&project, &["stats", "--json"]);
    assert_eq!(jcode, 0);
    let v: serde_json::Value = serde_json::from_str(jstdout.trim()).unwrap();
    assert!(
        v["recommendations"]["conversion_ratio"].is_null(),
        "the misleading conversion_ratio field must be renamed; got: {jstdout}"
    );
    assert!(
        v["recommendations"]["tool_calls_per_rec"].is_number(),
        "tool_calls_per_rec must replace it; got: {jstdout}"
    );
    assert!(
        v["live_tools"]
            .as_array()
            .is_some_and(|a| a.iter().any(|t| t == "get_call_graph")),
        "live_tools must surface the current tools/list set; got: {jstdout}"
    );
}

// Deny→use funnel: stats must print the per-session attribution line when usage
// records carry the window-joined `recs` field.
#[test]
fn test_cli_stats_deny_to_use_funnel() {
    let project = setup_indexed_project();
    let cg = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    // Three deny-sessions: one converted via MCP cg tool, one via CLI query
    // (recs.cli_use, v0.49), one not at all → any-use 2/3 = 67%.
    let s1 = "{\"ts\":\"2026-06-10T10:00:00Z\",\"v\":\"0.45.4\",\"tools\":{\"get_call_graph\":{\"n\":1,\"ms\":5,\"err\":0,\"max_ms\":5}},\"recs\":{\"deny\":1,\"hint\":0}}";
    let s2 = "{\"ts\":\"2026-06-10T11:00:00Z\",\"v\":\"0.45.4\",\"tools\":{},\"recs\":{\"deny\":1,\"hint\":0}}";
    let s3 = "{\"ts\":\"2026-06-10T12:00:00Z\",\"v\":\"0.49.0\",\"tools\":{},\"recs\":{\"deny\":2,\"hint\":1,\"cli_use\":3}}";
    std::fs::write(cg.join("usage.jsonl"), format!("{s1}\n{s2}\n{s3}\n")).unwrap();
    let (stdout, _, code) = run_cli(&project, &["stats"]);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("Deny→use: 2/3 deny-sessions used cg = 67% (mcp 1, cli 1)"),
        "stats must print the deny→use funnel with mcp/cli legs; got: {stdout:?}"
    );

    let (jstdout, _, jcode) = run_cli(&project, &["stats", "--json"]);
    assert_eq!(jcode, 0);
    let v: serde_json::Value = serde_json::from_str(jstdout.trim()).unwrap();
    let funnel = &v["recommendations"]["funnel"];
    assert_eq!(funnel["deny_sessions"], 3);
    assert_eq!(funnel["deny_then_cg"], 1);
    assert_eq!(funnel["deny_then_cli"], 1);
    assert_eq!(funnel["deny_then_use"], 2);
    assert_eq!(funnel["deny_conversion"], 0.67);
}

// P0a (v0.49): a session with ZERO tool calls but in-window recommendation
// traffic must still flush a usage record — otherwise the funnel denominator
// only ever contains converted sessions (2026-06-12 daagu: 53 recs, 0 records).
#[test]
fn test_cli_stats_zero_tool_session_with_recs_counts_in_funnel() {
    let project = setup_indexed_project();
    let cg = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    // What flush now writes for such a session: empty tools, recs present.
    let s = "{\"ts\":\"2026-06-12T22:00:00Z\",\"v\":\"0.49.0\",\"tools\":{},\"recs\":{\"deny\":1,\"hint\":5,\"bypass\":2}}";
    std::fs::write(cg.join("usage.jsonl"), format!("{s}\n")).unwrap();
    let (jstdout, _, jcode) = run_cli(&project, &["stats", "--json"]);
    assert_eq!(jcode, 0);
    let v: serde_json::Value = serde_json::from_str(jstdout.trim()).unwrap();
    assert_eq!(v["sessions"], 1, "0-tool session must appear in stats");
    let funnel = &v["recommendations"]["funnel"];
    assert_eq!(funnel["deny_sessions"], 1);
    assert_eq!(funnel["deny_then_use"], 0);
    assert_eq!(
        funnel["deny_conversion"], 0.0,
        "0% conversion must be observable, not absent"
    );
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
    assert!(
        stdout.contains("Benchmark") || stdout.contains("--json"),
        "help should describe the command; got: {stdout:?}"
    );
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
    assert_ne!(
        code, 0,
        "nonexistent symbol should return non-zero exit code"
    );
    assert!(stderr.contains("Symbol not found"));
}

// health-check's "pending" arm used to print "no download has been attempted on
// this machine" even when model weights were already on disk (the npm plugin
// installs them without writing the binary's download marker) — contradicting
// the filesystem and telling the user to fix a problem they don't have.
#[cfg(feature = "embed-model")]
#[test]
fn test_cli_health_check_pending_reports_present_model_files() {
    let project = setup_indexed_project();
    let model_dir = TempDir::new().unwrap();
    // find_models_dir only stats model.safetensors — a stub is enough for the
    // presence probe (nothing loads weights on this path: 0 vectors → pending).
    std::fs::write(model_dir.path().join("model.safetensors"), b"stub").unwrap();
    let (stdout, _, code) = run_cli_env(
        &project,
        &["health-check"],
        &[("CODE_GRAPH_MODEL_DIR", model_dir.path().to_str().unwrap())],
    );
    assert_eq!(code, 0);
    assert!(
        stdout.contains("model files present"),
        "pending arm must acknowledge on-disk model files, got: {stdout}"
    );
    assert!(
        !stdout.contains("no download has been attempted"),
        "must not claim no-download when weights exist on disk, got: {stdout}"
    );
}

// An embed-model binary that finds 0 embeddings used to advise "build with
// --features embed-model" — a rebuild the user already has. The remedy line
// must match the running binary's features (here: just start the MCP server).
#[cfg(feature = "embed-model")]
#[test]
fn test_cli_similar_no_embeddings_remedy_matches_binary_features() {
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["similar", "validateToken"]);
    assert_eq!(code, 1);
    assert!(
        stderr.contains("No embeddings found"),
        "expected the empty-embeddings path, got: {stderr}"
    );
    assert!(
        !stderr.contains("--features embed-model"),
        "embed-model build must not advise rebuilding with embed-model, got: {stderr}"
    );
    assert!(
        stderr.contains("start the MCP server"),
        "remedy must point at running the server, got: {stderr}"
    );
}

// A symbol ADDED after the last index has no indexed file for query-time
// freshness to refresh, so `show` misses it — the miss must at least tell the
// user the index may be stale instead of a bare "Symbol not found" that reads
// as "doesn't exist" (a fresh `incremental-index` then makes it visible).
#[test]
fn test_cli_show_miss_hints_stale_index_for_new_symbol() {
    let project = setup_indexed_project();
    std::fs::write(
        project.path().join("src").join("fresh.ts"),
        "export function brandNewFn() { return 1; }\n",
    )
    .unwrap();
    let (_, stderr, code) = run_cli(&project, &["show", "brandNewFn"]);
    assert_ne!(code, 0);
    assert!(
        stderr.contains("incremental-index"),
        "miss without fuzzy candidates must hint at reindexing, got: {stderr}"
    );
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
    assert_eq!(
        code, 0,
        "qualified-name with no DB match should fall back to base name; stderr={stderr:?}"
    );
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let arr = v.as_array().unwrap();
    assert!(
        !arr.is_empty(),
        "should find validateToken via base-name fallback; got {stdout:?}"
    );
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
    assert!(
        arr[0]["code_content"].is_string(),
        "should include code_content field"
    );
}

// Regression: `show --impact --json` must disclose how many test callers were
// excluded from the prod risk count (`test_callers_filtered`), matching MCP
// get_ast_node's impact object. Without it a CLI consumer sees direct_callers but
// not that N test callers also exercise the symbol.
#[test]
fn test_cli_show_impact_discloses_filtered_test_callers() {
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("math.ts"),
        r#"
export function addNumbers(a: number, b: number): number {
    return a + b;
}
"#,
    )
    .unwrap();
    // `.test.ts` → is_test; its call to addNumbers is a test caller (excluded from
    // the prod risk count, but counted in test_callers_filtered).
    std::fs::write(
        src.join("math.test.ts"),
        r#"
import { addNumbers } from './math';
test('adds', () => {
    addNumbers(1, 2);
});
"#,
    )
    .unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();

    let (stdout, stderr, code) = run_cli(&project, &["show", "addNumbers", "--impact", "--json"]);
    assert_eq!(
        code, 0,
        "show --impact --json should succeed; stderr={stderr:?}"
    );
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let impact = &parsed[0]["impact"];
    assert!(impact["test_callers_filtered"].as_u64().unwrap_or(0) >= 1,
        "show --impact --json must disclose filtered test callers (parity with MCP get_ast_node): got {impact}");
}

// clap-migrated (audit #4 Step 5): clap owns --help + unknown-flag rejection. The
// optional positional is gated on --node-id in the handler (exit-1 Usage when both
// absent), and the three --refs spellings stay accepted via hidden clap aliases.
#[test]
fn test_cli_show_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["show", "--help"]);
    assert_eq!(code, 0, "show --help should exit 0 (clap help)");
    assert!(
        stdout.contains("symbol details") || stdout.contains("--node-id"),
        "help should describe the command; got: {stdout:?}"
    );
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
    let (out_b, _, code_b) = run_cli(
        &project,
        &["show", "validateToken", "--include-refs", "--json"],
    );
    let (out_c, _, code_c) = run_cli(
        &project,
        &["show", "validateToken", "--include-references", "--json"],
    );
    assert_eq!(code_a, 0);
    assert_eq!(
        (code_a, code_b, code_c),
        (0, 0, 0),
        "all three --refs spellings must succeed"
    );
    assert_eq!(
        out_a.trim(),
        out_b.trim(),
        "--refs and --include-refs must be identical"
    );
    assert_eq!(
        out_a.trim(),
        out_c.trim(),
        "--refs and --include-references must be identical"
    );
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

// key_symbols lists exported constants (`export const db`), so the module
// header's symbol total must count them too — it used to say "1 symbols" for a
// module whose key-symbol line printed two names (1 fn + 1 const).
#[test]
fn test_cli_map_symbol_count_includes_constants() {
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("store.ts"),
        "export const db = { q: 1 };\nexport function openDb() { return db; }\n",
    )
    .unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();

    let (stdout, _, code) = run_cli(&project, &["map"]);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("2 symbols"),
        "1 fn + 1 exported const must total 2 symbols, got: {stdout}"
    );

    let (json_out, _, code) = run_cli(&project, &["map", "--json"]);
    assert_eq!(code, 0);
    let parsed: serde_json::Value = serde_json::from_str(json_out.trim()).unwrap();
    let module = &parsed["modules"][0];
    assert_eq!(
        module["constants"].as_i64(),
        Some(1),
        "map --json must expose the constants bucket, got: {module}"
    );
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
    assert!(
        stdout.contains("architecture map") || stdout.contains("--compact"),
        "help should describe the command; got: {stdout:?}"
    );
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
    assert!(
        !stdout.contains("×)"),
        "compact should not show caller counts"
    );
}

// Regression: two exported classes in one file that share a method name must be
// distinguishable in output. `overview --json` carries a `qualified_name`
// (Animal.render / Widget.render) so they don't both surface as a bare `render`.
#[test]
fn test_cli_overview_json_disambiguates_same_named_methods() {
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("widgets.ts"),
        r#"
export class Animal {
    render(): string { return "animal"; }
}

export class Widget {
    render(): string { return "widget"; }
}
"#,
    )
    .unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();

    let (stdout, stderr, code) = run_cli(&project, &["overview", "src/", "--json"]);
    assert_eq!(code, 0, "overview --json should succeed; stderr={stderr:?}");
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("overview --json must be valid JSON ({e}); got: {stdout:?}"));
    let qns: Vec<&str> = parsed
        .as_array()
        .expect("overview --json is an array")
        .iter()
        .filter_map(|e| e.get("qualified_name").and_then(|v| v.as_str()))
        .collect();
    assert!(
        qns.contains(&"Animal.render"),
        "Animal.render disambiguated: {qns:?} in {stdout}"
    );
    assert!(
        qns.contains(&"Widget.render"),
        "Widget.render disambiguated: {qns:?} in {stdout}"
    );
}

// clap-migrated (audit #4): clap owns --help + unknown-flag rejection; the
// empty-path guard (test_cli_overview_empty_path_errors) is preserved in the
// handler since clap accepts an empty-string positional.
#[test]
fn test_cli_overview_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["overview", "--help"]);
    assert_eq!(code, 0, "overview --help should exit 0 (clap help)");
    assert!(
        stdout.contains("Module overview") || stdout.contains("PATH"),
        "help should describe the command; got: {stdout:?}"
    );
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
    assert!(
        stdout.contains("validateToken"),
        "overview . should list symbols across the project; got: {stdout:?}"
    );
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
    assert_eq!(
        code, 0,
        "absolute path under root should succeed; stderr={stderr:?}"
    );
    assert!(
        stdout.contains("validateToken"),
        "should list symbols just like `overview src`; got: {stdout:?}"
    );
}

#[test]
fn test_cli_overview_absolute_path_outside_root_errors() {
    let project = setup_indexed_project();
    // Create a sibling dir outside the project for a deterministic "outside" path.
    let outside = TempDir::new().unwrap();
    let (_, stderr, code) = run_cli(&project, &["overview", outside.path().to_str().unwrap()]);
    assert_eq!(code, 1, "absolute path outside root should error");
    assert!(
        stderr.contains("outside the project root"),
        "stderr should explain the path is outside the project root; got {stderr:?}"
    );
}

#[test]
fn test_cli_deps_absolute_path_under_root() {
    let project = setup_indexed_project();
    let abs = project.path().join("src/api.ts");
    let (stdout, _, code) = run_cli(&project, &["deps", abs.to_str().unwrap(), "--json"]);
    assert_eq!(code, 0);
    // Must surface the relative path in JSON + real depends_on edges (not barrel_scan).
    assert!(
        stdout.contains("\"file\":\"src/api.ts\""),
        "deps JSON should normalize file to project-relative; got {stdout:?}"
    );
    assert!(
        !stdout.contains("barrel_scan"),
        "deps must find tracked edges for abs path, not fall back to barrel_scan; got {stdout:?}"
    );
}

#[test]
fn test_cli_dead_code_absolute_path_under_root_matches_relative() {
    let project = setup_indexed_project();
    let (rel_stdout, _, rel_code) = run_cli(&project, &["dead-code", "src"]);
    let abs = project.path().join("src");
    let (abs_stdout, _, abs_code) = run_cli(&project, &["dead-code", abs.to_str().unwrap()]);
    assert_eq!(
        rel_code, abs_code,
        "abs path under root should match relative behavior exactly"
    );
    assert_eq!(
        rel_stdout, abs_stdout,
        "abs/rel results must be identical (was: abs silently returned no results)"
    );
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
    let (before, _, before_code) = run_cli(
        &project,
        &["dead-code", "--ignore", "zzz_nonexistent/", "src", "--json"],
    );
    let (after, _, after_code) = run_cli(
        &project,
        &["dead-code", "src", "--ignore", "zzz_nonexistent/", "--json"],
    );
    assert_eq!(
        before_code, after_code,
        "exit codes must match regardless of flag order"
    );
    assert_eq!(
        before.trim(),
        after.trim(),
        "--ignore before vs after the path must yield identical results"
    );
}

// Regression (#4): a misspelled --type must error loudly, not fall through to a
// literal n.type match that returns zero rows ("No dead code found", exit 0).
#[test]
fn test_cli_dead_code_rejects_misspelled_type() {
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["dead-code", "src", "--type", "fucntion"]);
    assert_ne!(
        code, 0,
        "misspelled --type must error, not exit 0 clean; stderr={stderr:?}"
    );
    assert!(
        stderr.contains("Unknown type filter"),
        "stderr should name the bad type filter; got: {stderr:?}"
    );
}

// clap-migrated (audit #4): clap owns --help + unknown-flag rejection. The
// --node-type/--type alias, repeatable --ignore, and --no-ignore default-clearing
// are preserved by the handler (see ignore_before_path_equals_after / json_empty).
#[test]
fn test_cli_dead_code_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["dead-code", "--help"]);
    assert_eq!(code, 0, "dead-code --help should exit 0 (clap help)");
    assert!(
        stdout.contains("unused code") || stdout.contains("--ignore"),
        "help should describe the command; got: {stdout:?}"
    );
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
    let (_, _, code) = run_cli(
        &project,
        &["dead-code", "src", "--type", "fn", "--node-type", "class"],
    );
    assert_eq!(
        code, 2,
        "supplying both --type and --node-type must error (clap duplicate-arg)"
    );
}

// The --node-type preferred spelling must work identically to its --type alias.
#[test]
fn test_cli_dead_code_node_type_alias_matches_type() {
    let project = setup_indexed_project();
    let (out_type, _, code_type) =
        run_cli(&project, &["dead-code", "src", "--type", "fn", "--json"]);
    let (out_node, _, code_node) = run_cli(
        &project,
        &["dead-code", "src", "--node-type", "fn", "--json"],
    );
    assert_eq!(
        code_type, code_node,
        "--type and --node-type must agree on exit code"
    );
    assert_eq!(
        out_type.trim(),
        out_node.trim(),
        "--type fn and --node-type fn must yield identical results"
    );
}

// Regression (real-user QA): at the default `--min-lines 3`, `dead-code` printed a
// bare "No dead code found." even when short (<3-line) orphans existed — a
// false-clean signal, worst for the primary consumer (an LLM that won't think to
// widen the threshold). The empty message must now name that shorter symbols are
// hidden and how to see them. Real indexed project (not a fixture) so it drives the
// actual find_dead_code producer.
#[test]
fn test_cli_dead_code_hints_at_symbols_below_min_lines() {
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    // `orphan1` is a 2-line dead function (below default min-lines 3). `used` has a
    // caller so it isn't dead; no function is BOTH >=3 lines AND dead, so the default
    // run is empty and must take the hint path (not the non-empty listing path).
    std::fs::write(src.join("m.py"),
        "def orphan1():\n    return 1\n\ndef used():\n    return 2\n\ndef caller():\n    return used()\n").unwrap();
    // Index via the library (like setup_indexed_project) — a bare TempDir has no
    // `.git`, and the `incremental-index` CLI skips indexing without a git anchor.
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    // Default min-lines 3: the short orphan is hidden → must HINT, not false-clean.
    let (_, stderr, code) = run_cli(&project, &["dead-code"]);
    assert_eq!(code, 0, "empty dead-code exits 0; stderr={stderr}");
    assert!(
        stderr.contains("below the threshold") && stderr.contains("--min-lines 1"),
        "empty dead-code at default min-lines must hint at hidden short symbols; got: {stderr}"
    );

    // At min-lines 1 the short orphan surfaces (proving the hint wasn't crying wolf).
    let (stdout1, _, code1) = run_cli(&project, &["dead-code", "--min-lines", "1"]);
    assert_eq!(code1, 0);
    assert!(
        stdout1.contains("orphan1"),
        "min-lines 1 must surface the short orphan; got: {stdout1}"
    );
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
    // v0.99.1: the miss emits a self-describing error object (roadmap §1.3), not `[]`.
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        v["error"], "No symbols found",
        "in-band error field; got {stdout:?}"
    );
    assert!(
        !stderr.contains("Error:"),
        "JSON mode must not emit anyhow `Error:` prefix on stderr; got {stderr:?}"
    );
    assert!(
        stderr.contains("No symbols found"),
        "stderr must still surface the human-readable reason; got {stderr:?}"
    );
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
    // api.ts imports exactly one symbol (validateToken) from auth.ts — the
    // count must pluralize ("1 symbol", not "1 symbols").
    assert!(
        stdout.contains("(1 symbol)") && !stdout.contains("1 symbols"),
        "single-symbol dep must render '1 symbol', got: {stdout}"
    );
}

#[test]
fn test_cli_deps_direction() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(
        &project,
        &["deps", "src/auth.ts", "--direction", "incoming"],
    );
    assert_eq!(code, 0);
    // api.ts imports from auth.ts, so auth.ts has incoming dependency
    assert!(
        stdout.contains("src/api.ts") || stdout.is_empty() || stdout.contains("Depended by"),
        "should show incoming deps or be empty, got: {}",
        stdout
    );
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
    assert!(
        stdout.contains("dependency graph") || stdout.contains("--direction"),
        "help should describe the command; got: {stdout:?}"
    );
}

#[test]
fn test_cli_deps_unknown_flag_errors() {
    let project = setup_indexed_project();
    let (_, _, code) = run_cli(&project, &["deps", "src/api.ts", "--bogus"]);
    assert_eq!(code, 2, "unknown flag must error under clap");
}

#[test]
fn test_cli_deps_directory_points_to_overview() {
    // Regression: `deps <dir>` reported "File not found" (is_file() is false for a
    // directory) — misleading, since the directory plainly exists. It must instead
    // say it's a directory and point at `overview`.
    let project = setup_indexed_project(); // contains a src/ directory
    let (_out, err, code) = run_cli(&project, &["deps", "src"]);
    assert_ne!(code, 0, "deps on a directory is an error");
    assert!(
        err.contains("directory"),
        "deps on a directory must say it's a directory; got stderr={err:?}"
    );
    assert!(
        err.contains("overview"),
        "deps on a directory must point at `overview`; got stderr={err:?}"
    );
    // --json must still honor the empty-contract: a JSON object with a dir error.
    let (out, _err, code2) = run_cli(&project, &["deps", "src", "--json"]);
    assert_ne!(code2, 0);
    let v: serde_json::Value =
        serde_json::from_str(out.trim()).expect("deps --json on a directory must emit valid JSON");
    assert!(
        v["error"]
            .as_str()
            .unwrap_or("")
            .to_lowercase()
            .contains("director"),
        "deps --json directory error should mention directory; got {v:?}"
    );
}

/// A project with a call chain `top`/`side` → `middle` → `bottom`, so `middle`
/// lies on the shortest paths (top→bottom, side→bottom) and is a real betweenness
/// chokepoint. Used to prove `centrality --limit 0` surfaces it instead of falsely
/// claiming the graph has none.
fn setup_centrality_project() -> TempDir {
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("chain.ts"),
        r#"
export function bottom(): number { return 1; }
export function middle(): number { return bottom(); }
export function top(): number { return middle(); }
export function side(): number { return middle(); }
"#,
    )
    .unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    project
}

#[test]
fn test_cli_centrality_limit_zero_not_misleading() {
    // Regression: `centrality --limit 0` returned an empty ranking and printed
    // "No chokepoints found (graph has no multi-hop call paths)", falsely blaming
    // the graph when the user merely asked for zero rows. --limit 0 must clamp to 1
    // (mirrors cmd_callgraph's depth.max(1)).
    let project = setup_centrality_project();
    // Sanity: a real chokepoint exists, so limit 1 lists it on stdout.
    let (out1, _err1, code1) = run_cli(&project, &["centrality", "--limit", "1"]);
    assert_eq!(code1, 0);
    assert!(
        out1.contains("chokepoint"),
        "fixture must surface a chokepoint at limit 1; got {out1:?}"
    );
    // With a chokepoint present, limit 0 must NOT claim there are none.
    let (out0, err0, code0) = run_cli(&project, &["centrality", "--limit", "0"]);
    assert_eq!(code0, 0);
    assert!(
        !err0.contains("no multi-hop call paths"),
        "centrality --limit 0 must not claim the graph has no chokepoints; stderr={err0:?}"
    );
    assert_eq!(
        out0, out1,
        "--limit 0 must be clamped to 1 (identical output to --limit 1)"
    );
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

// Regression (real-user QA): ast-search leaked the <module>/<external> placeholder
// nodes and test symbols into structural results — `ast-search <extern-name>` printed
// an `<external>:0-0` stub and a `<module>` file node alongside the real symbol, and
// `ast-search --type function` listed test_ functions — unlike `search`/`similar`.
// Both the query path and the filter-only path now skip the triad.
#[test]
fn test_cli_ast_search_excludes_module_external_and_test() {
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(project.path().join("tests")).unwrap();
    // real_fn imports an external module (distinctlibxyz → <external> node); the file
    // itself yields a <module> node; the tests/ file adds a test_ function whose NAME
    // contains the query term so FTS surfaces it (and it must be filtered as a test).
    std::fs::write(
        src.join("m.py"),
        "import distinctlibxyz\n\ndef real_fn():\n    return distinctlibxyz.go()\n",
    )
    .unwrap();
    std::fs::write(
        project.path().join("tests/test_m.py"),
        "def test_distinctlibxyz_marker():\n    assert real_fn() is not None\n",
    )
    .unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    // Query path: searching the external name must NOT surface <external>/<module>/test.
    let (stdout, _, code) = run_cli(&project, &["ast-search", "distinctlibxyz"]);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("real_fn"),
        "the real symbol must appear; got: {stdout}"
    );
    assert!(
        !stdout.contains("<external>") && !stdout.contains("<module>"),
        "ast-search must not leak <module>/<external> placeholder nodes; got: {stdout}"
    );
    assert!(
        !stdout.contains("test_distinctlibxyz_marker"),
        "ast-search must not leak test symbols (query path); got: {stdout}"
    );

    // Filter-only path: --type function must exclude the test_ function too.
    let (stdout2, _, code2) = run_cli(&project, &["ast-search", "--type", "function"]);
    assert_eq!(code2, 0);
    assert!(
        stdout2.contains("real_fn"),
        "prod fn must appear under --type function; got: {stdout2}"
    );
    assert!(
        !stdout2.contains("test_distinctlibxyz_marker"),
        "ast-search --type function must exclude test_ functions; got: {stdout2}"
    );
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
    assert!(
        stderr.contains("Unknown type filter"),
        "should explain the typo; got: {stderr}"
    );
}

// clap-migrated (audit #4): clap owns --help + unknown-flag rejection; the
// query-or-filter requirement stays a handler bail (exit 1), and clap accepts an
// empty-string positional so `ast-search ""` still hits that handler check.
#[test]
fn test_cli_ast_search_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["ast-search", "--help"]);
    assert_eq!(code, 0, "ast-search --help should exit 0 (clap help)");
    assert!(
        stdout.contains("Structured search") || stdout.contains("--returns"),
        "help should describe the command; got: {stdout:?}"
    );
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
    assert_eq!(
        code, 1,
        "no query and no filter must exit 1; stderr={stderr:?}"
    );
    assert!(
        stderr.contains("Usage:") || stderr.contains("at least one filter"),
        "should explain query-or-filter requirement; got: {stderr:?}"
    );
}

#[test]
fn test_cli_overview_empty_path_errors() {
    // Regression: overview "" used to be silently treated like overview "."
    // (match-all alias), which is almost always a shell-variable substitution
    // bug. Must surface as an error so users see the empty value.
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["overview", ""]);
    assert_ne!(code, 0, "empty path should fail");
    assert!(
        stderr.contains("must not be empty"),
        "should explain; got: {stderr}"
    );
}

#[test]
fn test_cli_search_invalid_node_type() {
    // Same regression as ast-search: --node-type INVALID was silently dropped.
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(
        &project,
        &["search", "Logger", "--node-type", "INVALID_TYPE"],
    );
    assert_ne!(code, 0, "invalid --node-type should fail");
    assert!(
        stderr.contains("Unknown node-type filter"),
        "should explain the typo; got: {stderr}"
    );
}

// ============================================================
// trace (no HTTP routes in test project, so test graceful handling)
// ============================================================

#[test]
fn test_cli_trace_no_routes() {
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["trace", "/api/login"]);
    assert_eq!(code, 1);
    assert!(
        stderr.contains("No routes matching"),
        "should report no routes found"
    );
    // Empty trace must disclose the framework-coverage limit (a Rust/Java project has
    // real routes the extractor never sees) so a bare miss doesn't read as "no such
    // route". Mirrors the richer MCP trace message.
    assert!(
        stderr.contains("not yet extracted") && stderr.contains("Flask"),
        "empty trace must disclose which frameworks route-extraction covers; got: {stderr}"
    );
}

#[test]
fn test_cli_trace_filters_test_symbols_by_default() {
    // Parity with the MCP trace_http_chain tool, which filters is_test_symbol out of
    // the call chain (server/tools/advanced.rs). The CLI trace chain used to show test
    // callees; it now hides them by default and exposes --include-tests to opt back in.
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("server.ts"),
        r#"
const app = express();
function realWork() { return 1; }
function test_helper() { return 2; }
app.get('/widgets', (req, res) => {
    realWork();
    test_helper();
    res.json([]);
});
"#,
    )
    .unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();

    // --no-middleware isolates the recursive call chain (the surface MCP filters);
    // the one-hop "downstream" list stays unfiltered on BOTH surfaces, so excluding
    // it here keeps the assertion focused on the chain parity.
    // Default: the test-named callee is hidden from the chain; the prod one shows.
    let (out, _, code) = run_cli(&project, &["trace", "GET /widgets", "--no-middleware"]);
    assert_eq!(code, 0, "trace should resolve the route; got code {code}");
    assert!(
        out.contains("realWork"),
        "prod callee must appear; got:\n{out}"
    );
    assert!(
        !out.contains("test_helper"),
        "test callee must be hidden by default; got:\n{out}"
    );

    // --include-tests shows both.
    let (out2, _, code2) = run_cli(
        &project,
        &[
            "trace",
            "GET /widgets",
            "--no-middleware",
            "--include-tests",
        ],
    );
    assert_eq!(code2, 0);
    assert!(
        out2.contains("realWork") && out2.contains("test_helper"),
        "--include-tests must show test callees; got:\n{out2}"
    );
}

#[test]
fn test_cli_worktree_reads_main_checkout_index() {
    // D#106 / roadmap §2.2 (Rust read-side of the v0.99.0 JS worktree fix):
    // query commands run inside a linked git worktree with no own index must
    // fall back to the MAIN checkout's index instead of erroring "No index
    // found" (and instead of cold-building a duplicate). Write side is
    // unchanged; a worktree's OWN index still wins (open() checks it first).
    if !has_git() {
        eprintln!("skipping: git not installed");
        return;
    }
    let root = TempDir::new().unwrap();
    let main = root.path().join("main");
    let src = main.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("auth.ts"),
        "export function hashPassword(p: string): string { return p; }\n",
    )
    .unwrap();
    let git = |args: &[&str], cwd: &std::path::Path| {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"], &main);
    git(&["add", "."], &main);
    git(&["commit", "-qm", "init"], &main);
    // Index the MAIN checkout.
    let db_dir = main.join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, &main, None, None).unwrap();
    drop(db);
    // Linked worktree (its `.git` is a FILE pointing at main/.git/worktrees/<n>).
    let wt = root.path().join("wt");
    git(
        &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "feat"],
        &main,
    );
    assert!(
        wt.join(".git").is_file(),
        "fixture must be a linked worktree"
    );

    let out = Command::new(binary_path())
        .args(["search", "hashPassword", "--json"])
        .current_dir(&wt)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(0),
        "worktree query must fall back to the main index; stderr: {stderr}"
    );
    assert!(
        stdout.contains("hashPassword"),
        "results must come from the main checkout's index; got stdout: {stdout} stderr: {stderr}"
    );
}

#[test]
fn test_cli_deps_namespace_import_and_star_barrel() {
    // v51 (roadmap §2.3): `import * as ns from './m'` and `export * from './m'`
    // now bind a module-level imports edge to the resolved file's <module> node,
    // so namespace-only and star-barrel dependencies show in deps; the ns alias
    // also feeds ns_module_map, binding `ns.fmt()` member calls cross-file.
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("util.ts"),
        "export function fmt(x: string): string { return x.trim(); }\n",
    )
    .unwrap();
    std::fs::write(src.join("barrel.ts"), "export * from './util';\n").unwrap();
    std::fs::write(
        src.join("app.ts"),
        "import * as u from './util';\nexport function run(): string { return u.fmt(' hi '); }\n",
    )
    .unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    let (barrel_deps, _, code1) = run_cli(&project, &["deps", "src/barrel.ts"]);
    assert_eq!(
        code1, 0,
        "star barrel must have dep edges now; got:\n{barrel_deps}"
    );
    assert!(
        barrel_deps.contains("util.ts"),
        "export * from './util' must show util.ts as a dependency; got:\n{barrel_deps}"
    );

    let (app_deps, _, code2) = run_cli(&project, &["deps", "src/app.ts"]);
    assert_eq!(code2, 0);
    assert!(
        app_deps.contains("util.ts"),
        "import * as u from './util' must show util.ts as a dependency; got:\n{app_deps}"
    );

    // Member-call binding through the ESM namespace: run → fmt cross-file.
    let (cg, _, code3) = run_cli(&project, &["callgraph", "fmt", "--direction", "callers"]);
    assert_eq!(code3, 0, "fmt must have callers; got:\n{cg}");
    assert!(
        cg.contains("run"),
        "u.fmt() must bind to util.ts fmt (ns_module_map via ns_import); got:\n{cg}"
    );
}

#[test]
fn test_cli_trace_axum_routes_end_to_end() {
    // axum route extraction (v51, roadmap §2.1): builder-chain routes are
    // traceable end-to-end, including the cross-file named-handler case (the
    // [route-imported-handler] class that bit Express at IDX v29 — the handler
    // fn lives in another file and resolves via the routes_to recovery path).
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("handlers.rs"),
        r#"
pub async fn list_users() -> String {
    fetch_all()
}

fn fetch_all() -> String { String::new() }
"#,
    )
    .unwrap();
    std::fs::write(
        src.join("main.rs"),
        r#"
use axum::{routing::get, Router};
use crate::handlers::list_users;

async fn health() -> &'static str { "ok" }

fn app() -> Router {
    Router::new()
        .nest("/api", Router::new().route("/users", get(list_users)))
        .route("/health", get(health))
}
"#,
    )
    .unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();

    // Same-file handler.
    let (out, _, code) = run_cli(&project, &["trace", "GET /health"]);
    assert_eq!(code, 0, "axum same-file route must resolve; got:\n{out}");
    assert!(out.contains("health"), "handler must be named; got:\n{out}");

    // Cross-file handler behind an inline nest prefix, callees traced.
    let (out2, _, code2) = run_cli(&project, &["trace", "GET /api/users"]);
    assert_eq!(
        code2, 0,
        "nested cross-file axum route must resolve; got:\n{out2}"
    );
    assert!(
        out2.contains("list_users"),
        "cross-file handler; got:\n{out2}"
    );
    assert!(
        out2.contains("fetch_all"),
        "handler callees must chain; got:\n{out2}"
    );
}

#[test]
fn test_cli_trace_hides_ambiguous_fanout_by_default() {
    // v0.77: trace inherits the v0.76 confidence floor (was deliberately left at
    // rank-0 show-all). A route handler that makes an ambiguous by-name call (one
    // name resolving to many same-language defs) used to splatter every tied edge
    // into BOTH the recursive call_chain AND the one-hop downstream list. The
    // default floor `inferred` now hides that fan-out on both surfaces, discloses
    // the count via `ambiguous_edges_hidden`, and --min-confidence ambiguous
    // restores every edge. Mirrors test_cli_callgraph_hides_ambiguous_fanout_by_default.
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    // Two same-name `thing` defs in different files, not imported into server.ts, so
    // the handler's bare `thing()` call resolves ambiguously to both (the fan-out class).
    std::fs::write(src.join("a.ts"), "export function thing() { return 1; }\n").unwrap();
    std::fs::write(src.join("b.ts"), "export function thing() { return 2; }\n").unwrap();
    std::fs::write(
        src.join("server.ts"),
        r#"
const app = express();
function widgetsHandler(req, res) {
    thing();
    res.json([]);
}
app.get('/widgets', widgetsHandler);
"#,
    )
    .unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    // Default floor: ambiguous `thing` fan-out hidden from BOTH the chain and the
    // one-hop downstream list; the count is disclosed at the top level.
    let (stdout, _, code) = run_cli(&project, &["trace", "GET /widgets", "--json"]);
    assert_eq!(
        code, 0,
        "trace should resolve the route; got code {code}: {stdout}"
    );
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let handler = &v["handlers"][0];
    let chain = handler["call_chain"].as_array().expect("call_chain array");
    assert!(
        !chain.iter().any(|n| n["name"] == "thing"),
        "default floor must hide the ambiguous `thing` fan-out from the chain; got: {stdout}"
    );
    let downstream = handler["downstream_calls"]
        .as_array()
        .expect("downstream_calls array");
    assert!(!downstream.iter().any(|n| n == "thing"),
        "default floor must hide the ambiguous fan-out from the one-hop downstream list too; got: {stdout}");
    assert_eq!(
        v["ambiguous_edges_hidden"].as_u64(),
        Some(2),
        "default view must disclose the 2 hidden ambiguous edges; got: {stdout}"
    );

    // Opt-in: --min-confidence ambiguous restores the fan-out on the chain, and the
    // disclosure field disappears (nothing suppressed at rank 0).
    let (stdout2, _, code2) = run_cli(
        &project,
        &[
            "trace",
            "GET /widgets",
            "--min-confidence",
            "ambiguous",
            "--json",
        ],
    );
    assert_eq!(
        code2, 0,
        "trace --min-confidence ambiguous should succeed; got: {stdout2}"
    );
    let v2: serde_json::Value = serde_json::from_str(stdout2.trim()).unwrap();
    let shown = v2["handlers"][0]["call_chain"]
        .as_array()
        .expect("call_chain array")
        .iter()
        .filter(|n| n["name"] == "thing")
        .count();
    assert_eq!(
        shown, 2,
        "--min-confidence ambiguous must show both tied `thing` edges in the chain; got: {stdout2}"
    );
    assert!(
        v2.get("ambiguous_edges_hidden").is_none(),
        "nothing is suppressed at the ambiguous floor; got: {stdout2}"
    );
}

// clap-migrated (audit #4): clap owns --help + unknown-flag rejection.
#[test]
fn test_cli_trace_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["trace", "--help"]);
    assert_eq!(code, 0, "trace --help should exit 0 (clap help)");
    assert!(
        stdout.contains("Trace HTTP") || stdout.contains("--no-middleware"),
        "help should describe the command; got: {stdout:?}"
    );
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
    assert_eq!(
        code_no, 1,
        "--no-middleware must be accepted (no-routes exit 1, not unknown-flag 2)"
    );
    // --include-middleware is the dropped phantom: clap unknown-flag exit 2.
    let (_, _, code_inc) = run_cli(
        &project,
        &["trace", "/api/nonexistent", "--include-middleware"],
    );
    assert_eq!(
        code_inc, 2,
        "dropped phantom --include-middleware must error as unknown flag"
    );
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
    assert_eq!(
        code, 2,
        "a negative --depth must error (was: silently clamped to 1)"
    );
}

// ============================================================
// incremental-index
// ============================================================

#[test]
fn test_cli_incremental_index() {
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["incremental-index"]);
    assert_eq!(code, 0);
    assert!(
        stderr.contains("Incremental index:"),
        "should show index stats"
    );
}

// Regression guard for feedback_tracing_invisible_in_cli: CLI subcommands now
// install a stderr tracing subscriber (previously only `serve` did), so indexer
// warn!/info! is visible. The "[incremental]" line is a tracing::info! (distinct
// from the always-printed "Incremental index:" eprintln), so it surfaces only
// when a subscriber is installed AND the level allows it.
#[test]
fn test_cli_incremental_index_tracing_subscriber_installed() {
    // RUST_LOG=info → the tracing-only "[incremental]" summary reaches stderr.
    let project = setup_indexed_project();
    std::fs::write(
        project.path().join("src/auth.ts"),
        "export function validateToken(t: string): boolean { return t.length > 0; }\nexport function freshlyAdded() { return 42; }\n",
    ).unwrap();
    let (_, stderr, code) = run_cli_env(&project, &["incremental-index"], &[("RUST_LOG", "info")]);
    assert_eq!(code, 0, "incremental-index should succeed; stderr={stderr}");
    assert!(
        stderr.contains("[incremental]"),
        "RUST_LOG=info must surface the indexer's tracing output on the CLI path \
         (proves the subscriber is installed); got stderr: {stderr:?}"
    );

    // Negative control: at "warn" level the info-level "[incremental]" line is
    // filtered out, while the non-tracing "Incremental index:" eprintln still
    // prints — proving the assertion above is level-gated by the subscriber, not
    // an always-present string. RUST_LOG is set explicitly (not left to ambient
    // env) so the control is deterministic on a shell/CI that exports RUST_LOG.
    let project2 = setup_indexed_project();
    std::fs::write(
        project2.path().join("src/auth.ts"),
        "export function validateToken(t: string): boolean { return t.length > 0; }\nexport function freshlyAdded2() { return 7; }\n",
    ).unwrap();
    let (_, stderr2, code2) =
        run_cli_env(&project2, &["incremental-index"], &[("RUST_LOG", "warn")]);
    assert_eq!(code2, 0);
    assert!(
        !stderr2.contains("[incremental]"),
        "at default warn level the info [incremental] line must be filtered out; got: {stderr2:?}"
    );
    assert!(
        stderr2.contains("Incremental index:"),
        "the non-tracing eprintln summary still prints at warn level; got: {stderr2:?}"
    );
}

// clap-migrated (audit #4) contract lock. Flag parsing flipped to clap while the
// git/index guard stays in main(); --quiet still suppresses output (valid path
// above), and clap now owns help + unknown-flag rejection.
#[test]
fn test_cli_incremental_index_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["incremental-index", "--help"]);
    assert_eq!(
        code, 0,
        "incremental-index --help should exit 0 (clap help)"
    );
    assert!(
        stdout.contains("incremental index") || stdout.contains("--quiet"),
        "help should describe the command; got: {stdout:?}"
    );
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
    // Plain `reindex` (no --from-snapshot) is an incremental refresh via
    // cmd_incremental_index — it does NOT drop the index (only --from-snapshot
    // does). rebuild-index is the unconditional rebuild. Asserting the
    // "Incremental index:" banner pins that contract against the help text.
    let project = setup_indexed_project();
    let (_, stderr, code) = run_cli(&project, &["reindex"]);
    assert_eq!(
        code, 0,
        "reindex should run to completion; stderr={stderr:?}"
    );
    assert!(
        stderr.contains("Incremental index:"),
        "should show index stats; got: {stderr:?}"
    );
}

#[test]
fn test_cli_reindex_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["reindex", "--help"]);
    assert_eq!(code, 0, "reindex --help should exit 0 (clap help)");
    assert!(
        stdout.contains("snapshot") || stdout.contains("--from-snapshot"),
        "help should describe the command; got: {stdout:?}"
    );
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
    let db_path = project
        .path()
        .join(code_graph_mcp::domain::CODE_GRAPH_DIR)
        .join("index.db");
    assert!(
        db_path.exists(),
        "precondition: indexed project has index.db"
    );
    let pre_size = std::fs::metadata(&db_path).unwrap().len();

    // Without --confirm: must bail non-zero AND leave index.db intact.
    let (_, stderr, code) = run_cli(&project, &["rebuild-index"]);
    assert_ne!(code, 0, "rebuild-index without --confirm must fail");
    assert!(
        stderr.contains("--confirm"),
        "stderr should demand --confirm, got: {}",
        stderr
    );
    assert!(
        db_path.exists(),
        "index.db must survive a rejected rebuild-index"
    );
    let post_size = std::fs::metadata(&db_path).unwrap().len();
    assert_eq!(pre_size, post_size, "index.db size must be unchanged");
}

#[test]
fn test_cli_rebuild_index_with_confirm_rebuilds() {
    let project = setup_indexed_project();
    let db_path = project
        .path()
        .join(code_graph_mcp::domain::CODE_GRAPH_DIR)
        .join("index.db");
    assert!(db_path.exists());

    // With --confirm: drop + re-create index. File should exist post-run and be non-empty.
    let (_, stderr, code) = run_cli(&project, &["rebuild-index", "--confirm"]);
    assert_eq!(code, 0, "rebuild-index --confirm failed: {}", stderr);
    assert!(db_path.exists(), "index.db must be recreated");
    assert!(
        std::fs::metadata(&db_path).unwrap().len() > 0,
        "recreated index.db must be non-empty"
    );
}

// The atomic rebuild builds into `index.db.rebuild-<pid>` then renames it over
// index.db (so concurrent readers never see the empty mid-rebuild window). After
// a successful rebuild no temp file may survive, a stale temp from a
// previously-killed rebuild must be cleaned, and the index stays queryable.
#[test]
fn test_cli_rebuild_index_atomic_leaves_no_temp() {
    let project = setup_indexed_project();
    let cg_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    let db_path = cg_dir.join("index.db");
    assert!(db_path.exists());

    // Plant a stale temp file as if a prior rebuild was killed mid-flight.
    std::fs::write(cg_dir.join("index.db.rebuild-99999"), b"garbage").unwrap();

    let (_, stderr, code) = run_cli(&project, &["rebuild-index", "--confirm"]);
    assert_eq!(code, 0, "rebuild-index --confirm failed: {}", stderr);
    assert!(
        db_path.exists() && std::fs::metadata(&db_path).unwrap().len() > 0,
        "index.db must be a non-empty rebuilt file"
    );

    let leftovers: Vec<String> = std::fs::read_dir(&cg_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("index.db.rebuild-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "rebuild left temp files behind: {:?}",
        leftovers
    );

    // Index still answers after the atomic swap.
    let (_, _, hc_code) = run_cli(&project, &["health-check"]);
    assert_eq!(hc_code, 0, "health-check must succeed after atomic rebuild");
}

// clap-migrated (audit #4) contract lock. The --confirm gate stays an exit-1
// anyhow bail (not a clap-required arg — see test_cli_rebuild_index_requires_confirm
// above), while clap now owns help + unknown-flag rejection (exit 2).
#[test]
fn test_cli_rebuild_index_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["rebuild-index", "--help"]);
    assert_eq!(code, 0, "rebuild-index --help should exit 0 (clap help)");
    assert!(
        stdout.contains("Drop and rebuild") || stdout.contains("--confirm"),
        "help should describe the command; got: {stdout:?}"
    );
}

#[test]
fn test_cli_rebuild_index_unknown_flag_errors() {
    // Flavor-B: unknown flag is a clap parse error (exit 2), evaluated before the
    // --confirm business gate — so --bogus exits 2, not 1.
    let project = setup_indexed_project();
    let (_, _, code) = run_cli(&project, &["rebuild-index", "--bogus"]);
    assert_eq!(
        code, 2,
        "unknown flag must error under clap (exit 2, before confirm gate)"
    );
}

// ============================================================
// refs --node-id (P1-1: MCP parity — node_id is authoritative)
// ============================================================

#[test]
fn test_cli_refs_node_id_envelope() {
    let project = setup_indexed_project();
    // First resolve a known symbol to a node_id via search --json
    let (search_out, _, search_code) = run_cli(
        &project,
        &["search", "validateToken", "--json", "--limit", "1"],
    );
    assert_eq!(search_code, 0, "search must succeed");
    let arr: serde_json::Value = serde_json::from_str(search_out.trim()).unwrap();
    let nid = arr[0]["node_id"]
        .as_i64()
        .expect("search result must expose node_id");

    let (out, _, code) = run_cli(&project, &["refs", "--node-id", &nid.to_string(), "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    // Envelope fields match MCP find_references
    assert!(v["symbol"].is_string(), "envelope must include symbol");
    assert!(
        v["total_references"].is_number(),
        "envelope must include total_references"
    );
    assert!(
        v["by_relation"].is_object(),
        "envelope must include by_relation map"
    );
    assert!(
        v["references"].is_array(),
        "envelope must include references array"
    );
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
    assert!(
        stderr.contains("--relation must be one of"),
        "stderr should explain the valid relation set; got: {stderr:?}"
    );
    // Nonexistent symbol + bad relation → still the RELATION error (validation
    // precedes resolution), not "Symbol not found".
    let (_, stderr2, code2) = run_cli(
        &project,
        &["refs", "definitely_absent_xyz", "--relation", "bogus"],
    );
    assert_ne!(code2, 0);
    assert!(
        stderr2.contains("--relation must be one of"),
        "relation validation must precede symbol resolution; got: {stderr2:?}"
    );
}

// clap-migrated (audit #4 Step 5): clap owns --help + unknown-flag rejection;
// --relation stays an in-handler String validated before index-open (above).
#[test]
fn test_cli_refs_help_exits_zero() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["refs", "--help"]);
    assert_eq!(code, 0, "refs --help should exit 0 (clap help)");
    assert!(
        stdout.contains("references") || stdout.contains("--relation"),
        "help should describe the command; got: {stdout:?}"
    );
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
    assert!(
        v["handlers"].is_array(),
        "envelope must have handlers array"
    );
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
    assert!(
        v["results"].is_array(),
        "ast-search --json must wrap in {{results,count}}"
    );
    assert!(
        v["count"].is_number(),
        "ast-search --json must include count"
    );
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
    // --json is NOT accepted by the index commands (clap: unexpected argument),
    // doctor (unknown flag), adopt or unadopt — the help must not overclaim.
    assert!(
        !stdout.contains("available on all commands"),
        "--json help line overclaims: doctor/adopt/unadopt/index commands reject it"
    );
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
    assert!(
        stderr.contains("Usage:"),
        "should show usage on missing arg"
    );
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
    assert_eq!(
        code, 2,
        "negative --depth must error (was: clamped to 1 / exit 0)"
    );
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
        real.push_str(&format!(
            "export function find_gadget_real_{i}(): number {{ return {i}; }}\n"
        ));
    }
    std::fs::write(src.join("widgets.ts"), real).unwrap();

    // 12 test-file functions sharing the same token — is_test_symbol drops these
    // via the `.test.ts` path suffix, so they must NOT crowd out the real results.
    let mut testfns = String::new();
    for i in 0..12 {
        testfns.push_str(&format!(
            "export function find_gadget_case_{i}(): number {{ return {i}; }}\n"
        ));
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
        assert!(
            !fp.ends_with(".test.ts"),
            "test-file symbol leaked into results: {fp}"
        );
    }
}

#[test]
fn test_cli_json_empty_search() {
    let project = setup_indexed_project();
    let (stdout, stderr, code) = run_cli(&project, &["search", "xyznonexistent", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(
        stdout.trim(),
        "[]",
        "JSON search with no results should output []"
    );
    assert!(
        stderr.contains("No results"),
        "stderr should still show hint"
    );
}

#[test]
fn test_cli_json_empty_grep() {
    if !has_ripgrep() {
        eprintln!("skipping: rg not installed");
        return;
    }
    let project = setup_indexed_project();
    let (stdout, stderr, code) = run_cli(&project, &["grep", "xyznonexistent", "--json"]);
    assert_eq!(
        code, 1,
        "no match exits 1 (grep parity) while keeping the JSON contract"
    );
    assert_eq!(
        stdout.trim(),
        "[]",
        "JSON grep with no results should output []"
    );
    assert!(
        stderr.contains("No matches"),
        "stderr should still show hint"
    );
}

#[test]
fn test_cli_json_empty_callgraph() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["callgraph", "xyznonexistent", "--json"]);
    assert_eq!(code, 1);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(
        v.is_object(),
        "JSON callgraph error should output JSON object"
    );
    // v0.99.1 (roadmap §1.3): the miss is self-describing in-band, not a bare
    // {"results":[]} indistinguishable from an edge-less symbol under 2>/dev/null.
    assert!(
        v["error"]
            .as_str()
            .unwrap_or("")
            .contains("No call graph results"),
        "in-band error field; got {stdout:?}"
    );
    assert_eq!(v["results"], serde_json::json!([]));
}

#[test]
fn test_cli_json_empty_show() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["show", "xyznonexistent", "--json"]);
    assert_eq!(code, 1);
    // v0.99.1 (roadmap §1.3): self-describing error object with the fuzzy
    // candidates in-band (they were stderr-only, invisible under 2>/dev/null).
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        v["error"], "Symbol not found",
        "in-band error; got {stdout:?}"
    );
    assert_eq!(v["symbol"], "xyznonexistent");
    assert!(
        v["candidates"].is_array(),
        "candidates array must be present (may be empty)"
    );
}

#[test]
fn test_cli_show_json_miss_carries_fuzzy_candidates() {
    // A near-miss symbol must surface its "Did you mean" candidates IN the JSON
    // error object, not only on stderr (roadmap §1.3).
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["show", "validateTokenz", "--json"]);
    assert_eq!(code, 1);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let cands = v["candidates"].as_array().unwrap();
    assert!(
        cands.iter().any(|c| c["name"] == "validateToken"),
        "fuzzy candidate validateToken must be in-band; got {stdout:?}"
    );
}

#[test]
fn test_cli_json_empty_show_node_id_missing() {
    // Regression: `show --node-id 999999` for a nonexistent ID exited 1 with
    // empty stdout in --json mode. Both miss paths must agree on the
    // self-describing error-object contract (roadmap §1.3).
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["show", "--node-id", "9999999", "--json"]);
    assert_eq!(code, 1);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        v["error"], "Node ID not found",
        "in-band error; got {stdout:?}"
    );
    assert_eq!(v["node_id"], 9999999);
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
fn test_cli_trace_no_route_clean_error() {
    // Regression: a no-match route must report the clean `[code-graph] …` stderr +
    // exit 1 used by refs/impact/show — NOT anyhow's double-prefixed
    // `Error: [code-graph] No routes matching`.
    let project = setup_indexed_project();
    let (_stdout, stderr, code) = run_cli(&project, &["trace", "/api/definitely-nope"]);
    assert_eq!(code, 1);
    assert!(
        stderr.contains("[code-graph] No routes matching"),
        "stderr must carry the house-style prefix; got: {stderr:?}"
    );
    assert!(
        !stderr.contains("Error: [code-graph]"),
        "must not double-prefix with anyhow's `Error:`; got: {stderr:?}"
    );
}

#[test]
fn test_cli_similar_accepts_limit_alias() {
    // Regression: `similar --limit N` must be accepted as an alias of `--top-k`,
    // not rejected by clap with a cryptic "unexpected argument '--limit'" (exit 2).
    // Parses to results (embed build) or the no-vector notice (no-embed build) —
    // both exit 0 — so the guard is: NOT a clap parse error.
    let project = setup_indexed_project();
    let (_stdout, stderr, code) = run_cli(&project, &["similar", "validateToken", "--limit", "3"]);
    assert_ne!(
        code, 2,
        "clap must accept --limit as an alias; stderr: {stderr:?}"
    );
    assert!(
        !stderr.contains("unexpected argument"),
        "--limit must not be an unexpected argument; stderr: {stderr:?}"
    );
}

#[test]
fn test_cli_json_empty_overview() {
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["overview", "nonexistent/", "--json"]);
    assert_eq!(code, 1);
    // v0.99.1 (roadmap §1.3): self-describing error object instead of a bare `[]`
    // indistinguishable from an empty-but-indexed dir under 2>/dev/null.
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        v["error"], "No symbols found",
        "in-band error; got {stdout:?}"
    );
    assert_eq!(v["path"], "nonexistent/");
}

#[test]
fn test_cli_json_empty_dead_code() {
    // Regression: dead-code --json with all results filtered by --ignore returned
    // only stderr (no stdout), breaking JSON consumers piping stdout. v0.99.1
    // (roadmap §1.2): when --ignore actually suppressed candidates, the empty
    // output is a self-describing object carrying ignored_count in-band.
    let project = setup_indexed_project();
    // --min-lines 1 so the fixture's short dead symbols land in the candidate set
    // and are then ignore-suppressed (ignored_count is counted at the ACTIVE
    // min-lines; at the default 3 the short fixture symbols never reach it).
    let (stdout, stderr, code) = run_cli(
        &project,
        &[
            "dead-code",
            "--min-lines",
            "1",
            "--ignore",
            "src/",
            "--ignore",
            "tests/",
            "--json",
        ],
    );
    assert_eq!(code, 0, "dead-code with no matches should exit 0");
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .expect("dead-code --json must emit valid JSON on the ignored-empty path");
    assert_eq!(v["results"], serde_json::json!([]));
    assert!(
        v["ignored_count"].as_u64().unwrap_or(0) >= 1,
        "ignored_count must disclose the suppressed candidates; got {stdout:?}"
    );
    assert!(
        stderr.contains("No dead code"),
        "stderr should still surface the human-readable reason; got: {stderr}",
    );
}

#[test]
fn test_cli_json_empty_similar() {
    // Regression: `similar <existing-symbol>` where vector search yielded no matches
    // wrote only stderr and exited 0 with empty stdout, breaking JSON consumers.
    //
    // Audit 2026-07-27 P2-14: the fix emitted a bare `[]`, and THIS TEST froze
    // that — `similar` was the last exit-1 miss in the CLI still answering with
    // an array while `impact`, `callgraph`, `trace` and `deps` all answer with
    // `{error, symbol}`. A bare `[]` on exit 1 is indistinguishable from a
    // successful empty result once stderr is dropped, which is the failure the
    // three-tier contract exists to prevent — so the assertion that was supposed
    // to enforce the contract was pinning its violation.
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["similar", "xyznonexistent", "--json"]);
    assert_eq!(code, 1);
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .expect("similar --json must output valid JSON on a miss");
    assert!(
        v.is_object(),
        "an exit-1 miss must be an error OBJECT, not a bare array: {stdout}"
    );
    assert_eq!(v["symbol"], "xyznonexistent");
    assert_eq!(v["error"], "Symbol not found");
}

/// The other two `similar` exit-1 misses take the same shape, and the
/// capability-missing case discloses rather than claiming emptiness.
#[test]
fn test_cli_json_similar_misses_all_carry_a_reason() {
    let project = setup_indexed_project();

    // --node-id that does not exist.
    let (stdout, _, code) = run_cli(&project, &["similar", "--node-id", "999999", "--json"]);
    assert_eq!(code, 1, "unknown node_id must exit 1; got:\n{stdout}");
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("similar --node-id --json must emit valid JSON");
    assert!(v.is_object(), "expected an error object, got: {stdout}");
    assert_eq!(v["error"], "node_id not found");
    assert_eq!(v["node_id"], 999999);

    // Existing symbol, but the build/index cannot answer: either sqlite-vec is
    // absent (exit 0, disclosure object) or no embeddings exist (exit 1, error
    // object). Which one depends on the feature set, so assert what BOTH must
    // carry — a machine-readable reason, never a bare `[]`.
    let (stdout, _, _) = run_cli(&project, &["similar", "validateToken", "--json"]);
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .expect("similar --json must emit valid JSON for an existing symbol");
    if v.is_array() {
        // embed-model build with embeddings present: a real result array.
        return;
    }
    assert!(
        v.get("error").is_some() || v.get("unavailable").is_some(),
        "an empty similar answer must say why it is empty: {stdout}"
    );
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
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|_| {
            panic!(
                "{args:?}: refs --json must output valid JSON even on not-found; got: {stdout:?}"
            )
        });
        assert!(
            v.is_object(),
            "{args:?}: refs --json not-found should be an object, not a bare array; got: {stdout}"
        );
        assert!(
            v["references"].is_array(),
            "{args:?}: envelope must include references array"
        );
        assert!(
            v["by_relation"].is_object(),
            "{args:?}: envelope must include by_relation map"
        );
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
fn test_cli_json_empty_impact() {
    // impact on an unknown symbol emits the same {error,symbol} object the
    // success path's consumer can parse (matches callgraph/trace/deps), exit 1.
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["impact", "xyznonexistent", "--json"]);
    assert_eq!(code, 1);
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .expect("impact --json must output valid JSON even on not-found");
    assert!(
        v.is_object(),
        "JSON impact not-found should be an object; got: {stdout}"
    );
    assert_eq!(
        v["symbol"], "xyznonexistent",
        "envelope echoes the queried symbol"
    );
}

#[test]
fn test_cli_json_empty_ast_search() {
    // ast-search with a no-match query is a healthy SUCCESS (exit 0); --json must
    // still emit the same {results,count} envelope as the populated path.
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["ast-search", "xyznonexistent", "--json"]);
    assert_eq!(
        code, 0,
        "ast-search empty is a structural query success, not an error"
    );
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .expect("ast-search --json must output valid JSON even on no-match");
    assert!(
        v.is_object(),
        "ast-search --json empty should be the {{results,count}} envelope"
    );
    assert!(
        v["results"].is_array(),
        "envelope must include results array"
    );
    assert_eq!(v["count"], 0, "empty result count is 0");
}

#[test]
fn test_cli_search_filter_emptied_discloses() {
    // v0.99.1 (roadmap §1.1 HIGH): when the query HAD hits but the language
    // filter removed them all, the JSON must be a self-describing object —
    // previously a bare `[]`, byte-identical to a true zero-hit under
    // `2>/dev/null`. Text mode gets a stdout line for the same reason.
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(
        &project,
        &["search", "validateToken", "--language", "python", "--json"],
    );
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["results"], serde_json::json!([]));
    assert!(
        v["filtered_out"].as_u64().unwrap_or(0) >= 1,
        "filtered_out must disclose the removed candidates; got {stdout:?}"
    );
    assert!(
        v["filter"].as_str().unwrap_or("").contains("python"),
        "active filter must be named; got {stdout:?}"
    );

    // Text mode: the disclosure reaches stdout (not only stderr).
    let (t_stdout, _, t_code) = run_cli(
        &project,
        &["search", "validateToken", "--language", "python"],
    );
    assert_eq!(t_code, 0);
    assert!(
        t_stdout.contains("removed by the active filter"),
        "text mode must disclose on stdout; got {t_stdout:?}"
    );

    // Negative control: a true zero-hit (no filter) keeps the plain `[]`.
    let (z_stdout, _, _) = run_cli(&project, &["search", "xyznonexistent", "--json"]);
    assert_eq!(z_stdout.trim(), "[]");
}

#[test]
fn test_cli_ast_search_filter_emptied_discloses() {
    // Same disclosure for ast-search's structural filters (§1.1): query hits +
    // an over-selective --returns → envelope carries filtered_out + filter.
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(
        &project,
        &[
            "ast-search",
            "validateToken",
            "--returns",
            "zzznope",
            "--json",
        ],
    );
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["results"], serde_json::json!([]));
    assert_eq!(v["count"], 0);
    assert!(
        v["filtered_out"].as_u64().unwrap_or(0) >= 1,
        "filtered_out must disclose the removed candidates; got {stdout:?}"
    );
    assert!(
        v["filter"]
            .as_str()
            .unwrap_or("")
            .contains("returns: zzznope"),
        "active filter must be named; got {stdout:?}"
    );

    // Negative control: a no-hit query (nothing filtered) keeps the bare envelope.
    let (z_stdout, _, _) = run_cli(&project, &["ast-search", "xyznonexistent", "--json"]);
    let z: serde_json::Value = serde_json::from_str(z_stdout.trim()).unwrap();
    assert!(
        z.get("filtered_out").is_none(),
        "no disclosure fields on a true zero-hit"
    );
}

#[test]
fn test_cli_dead_code_below_threshold_json_discloses() {
    // §1.2: the threshold-hidden empty case must be self-describing in JSON —
    // mirrors test_cli_dead_code_hints_at_symbols_below_min_lines (stderr) but
    // pins the `--json 2>/dev/null` surface an LLM consumer actually reads.
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("m.py"),
        "def orphan1():\n    return 1\n\ndef used():\n    return 2\n\ndef caller():\n    return used()\n").unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    let (stdout, _, code) = run_cli(&project, &["dead-code", "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["results"], serde_json::json!([]));
    assert!(
        v["below_threshold_count"].as_u64().unwrap_or(0) >= 1,
        "below_threshold_count must disclose hidden short symbols; got {stdout:?}"
    );
    assert_eq!(v["min_lines"], 3, "the active threshold must be named");

    // Text mode: the rerun hint reaches stdout too.
    let (t_stdout, _, _) = run_cli(&project, &["dead-code"]);
    assert!(
        t_stdout.contains("--min-lines 1"),
        "text mode must put the rerun hint on stdout; got {t_stdout:?}"
    );
}

#[test]
fn test_cli_cycles_truncation_discloses() {
    // §1.5: `--limit` used to shrink the printed "(N found)" to the truncated
    // length with no marker. Two independent 2-file cycles + --limit 1 must
    // disclose the real total on both surfaces; without truncation the JSON
    // keeps its plain array shape.
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("a.ts"),
        "import { b } from './b';\nexport function a(): number { return b(); }\n",
    )
    .unwrap();
    std::fs::write(
        src.join("b.ts"),
        "import { a } from './a';\nexport function b(): number { return a(); }\n",
    )
    .unwrap();
    std::fs::write(
        src.join("c.ts"),
        "import { d } from './d';\nexport function c(): number { return d(); }\n",
    )
    .unwrap();
    std::fs::write(
        src.join("d.ts"),
        "import { c } from './c';\nexport function d(): number { return c(); }\n",
    )
    .unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    let (stdout, _, code) = run_cli(&project, &["cycles", "--limit", "1", "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        v["truncated"], true,
        "truncation must be disclosed; got {stdout:?}"
    );
    assert_eq!(v["total_found"], 2);
    assert_eq!(v["results"].as_array().unwrap().len(), 1);

    let (t_stdout, _, _) = run_cli(&project, &["cycles", "--limit", "1"]);
    assert!(
        t_stdout.contains("showing 1 of 2"),
        "text mode must print the pre-truncation total; got {t_stdout:?}"
    );

    // Untruncated: plain array shape, both cycles present.
    let (full, _, _) = run_cli(&project, &["cycles", "--json"]);
    let fv: serde_json::Value = serde_json::from_str(full.trim()).unwrap();
    assert_eq!(
        fv.as_array().map(|a| a.len()),
        Some(2),
        "untruncated cycles keeps the plain array; got {full:?}"
    );
}

#[test]
fn test_cli_ast_search_freshness_partial_in_json() {
    // §1.4: a partial freshness resync was stderr-only — invisible under
    // `--json 2>/dev/null`. Object-shaped outputs must carry freshness_partial.
    // RESYNC_BUDGET=0 forces the partial (skipped_over_budget) path.
    let project = setup_indexed_project();
    let (fresh, _, _) = run_cli(&project, &["ast-search", "hashPassword", "--json"]);
    let fv: serde_json::Value = serde_json::from_str(fresh.trim()).unwrap();
    assert!(
        fv.get("freshness_partial").is_none(),
        "fully-fresh run must not carry the marker; got {fresh:?}"
    );

    prepend_pad(&project, "src/auth.ts", 1);

    let (stale, _, code) = run_cli_env(
        &project,
        &["ast-search", "hashPassword", "--json"],
        &[("CODE_GRAPH_RESYNC_BUDGET", "0")],
    );
    assert_eq!(code, 0);
    let sv: serde_json::Value = serde_json::from_str(stale.trim()).unwrap();
    assert_eq!(
        sv["freshness_partial"], true,
        "partial resync must be disclosed in-band; got {stale:?}"
    );
}

#[test]
fn test_cli_json_empty_centrality() {
    // A single trivial file has no multi-hop call paths → no chokepoints. Empty
    // centrality is a healthy success (exit 0) and --json must emit `[]`.
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("solo.ts"),
        "export function alone(): number { return 1; }\n",
    )
    .unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();

    let (stdout, _, code) = run_cli(&project, &["centrality", "--json"]);
    assert_eq!(code, 0, "no chokepoints is success, not an error");
    assert_eq!(
        stdout.trim(),
        "[]",
        "empty centrality must emit [] per the JSON-empty contract"
    );
}

#[test]
fn test_cli_json_empty_map() {
    // An empty project (no indexed source) still yields the same-shape map object
    // envelope on stdout, not a bare bail to stderr.
    let project = TempDir::new().unwrap();
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();

    let (stdout, _, code) = run_cli(&project, &["map", "--json"]);
    assert_eq!(code, 0, "map exits 0 even for an empty project");
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .expect("map --json must output valid JSON for an empty project");
    assert!(
        v.is_object(),
        "map --json is an object envelope; got: {stdout}"
    );
    assert!(
        v["modules"].is_array(),
        "envelope must include modules array"
    );
    assert_eq!(
        v["modules"].as_array().unwrap().len(),
        0,
        "empty project has no modules"
    );
    assert!(
        v["hot_functions"].is_array(),
        "envelope must include hot_functions array"
    );
}

#[test]
fn test_cli_json_empty_affected() {
    // A changed file that isn't in the index exercises the empty blast-radius path:
    // same-shape object with empty changed/tests/affected_files and the raw input
    // echoed in not_indexed. Exits 0 (structural analysis, not a lookup).
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(
        &project,
        &["affected", "src/nonexistent_file_xyz.rs", "--json"],
    );
    assert_eq!(
        code, 0,
        "affected exits 0 even when no input file is indexed"
    );
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .expect("affected --json must output valid JSON even when nothing is affected");
    assert!(
        v.is_object(),
        "affected --json is an object envelope; got: {stdout}"
    );
    assert!(
        v["changed"].is_array() && v["changed"].as_array().unwrap().is_empty(),
        "no indexed changed files"
    );
    assert!(v["tests"].is_array(), "envelope must include tests array");
    assert!(
        v["affected_files"].is_array(),
        "envelope must include affected_files array"
    );
    let not_indexed: Vec<&str> = v["not_indexed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert_eq!(
        not_indexed,
        ["src/nonexistent_file_xyz.rs"],
        "raw input echoed in not_indexed"
    );
}

#[test]
fn test_index_counts_parse_error_files() {
    // Observability: a file whose tree-sitter parse recovers from a syntax error
    // (ERROR/MISSING nodes, tree still returned) must be counted in
    // stats.files_with_parse_errors — extraction proceeds best-effort but symbols
    // may be dropped, so the count makes that risk visible without a schema change.
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    // Unterminated params + body and stray tokens force ERROR nodes in the tree.
    std::fs::write(
        src.join("broken.ts"),
        "export function broken( { const x = ;;; @@@ return\n",
    )
    .unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    let result =
        code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    assert!(
        result.stats.files_with_parse_errors >= 1,
        "a syntax-error file must be counted; got {}",
        result.stats.files_with_parse_errors
    );
}

#[test]
fn test_index_clean_project_zero_parse_errors() {
    // Negative control: the all-clean TypeScript fixture must report zero files
    // with parse errors (guards against the counter firing on valid syntax).
    let project = setup_indexed_project();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    let result =
        code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    assert_eq!(
        result.stats.files_with_parse_errors, 0,
        "clean fixture must report zero parse errors; got {}",
        result.stats.files_with_parse_errors
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
    serde_json::from_str::<serde_json::Value>(stdout.trim()).unwrap_or_else(|_| {
        panic!("similar --node-id missing --json must emit valid JSON; got: {stdout:?}")
    });
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
    assert!(
        stdout.contains("similar code") || stdout.contains("--top-k"),
        "help should describe the command; got: {stdout:?}"
    );
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
    assert!(
        eff <= 10,
        "effective should be capped at CALL_GRAPH_MAX_DEPTH=10, got {eff}"
    );
    assert!(
        eff < 99,
        "effective ({eff}) must be visibly less than requested (99)"
    );
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
    let depth_gt_zero = results
        .iter()
        .filter(|r| r["depth"].as_i64().unwrap_or(0) > 0)
        .count();
    assert!(
        with_parent > 0 && with_parent == depth_gt_zero,
        "every non-root row must carry parent_id; with_parent={with_parent} depth>0={depth_gt_zero}"
    );
}

// --- tour subcommand ---

/// Three modules in distinct directories with a clean dependency chain:
/// `src/api` → `src/store` → `src/core`. Exercises cross-module ordering
/// (single-dir fixtures collapse to one module).
fn setup_tour_project() -> TempDir {
    let project = TempDir::new().unwrap();
    let mk = |dir: &str, file: &str, body: &str| {
        let d = project.path().join(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(file), body).unwrap();
    };
    // NB: export *functions*, not `const`s — top-level const exports are not
    // extracted as symbol nodes, so importing them produces no REL_IMPORTS edge
    // and the cross-module dependency (the whole point here) would silently vanish.
    mk(
        "src/core",
        "util.ts",
        "export function clampLen(x: string): number { return x.length; }\n",
    );
    mk("src/store", "store.ts",
        "import { clampLen } from '../core/util';\nexport function saveItem(x: string): boolean { return clampLen(x) < 10; }\n");
    mk("src/api", "handlers.ts",
        "import { saveItem } from '../store/store';\nexport function handleSave(x: string): boolean { return saveItem(x); }\n");

    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    project
}

fn tour_paths(v: &serde_json::Value) -> Vec<String> {
    v["reading_order"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn test_cli_tour_orders_prerequisites_first() {
    // api → store → core, so the reading order must be core, then store, then api.
    let project = setup_tour_project();
    let (stdout, _, code) = run_cli(&project, &["tour", "--json"]);
    assert_eq!(code, 0, "stdout: {stdout}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json envelope");
    let paths = tour_paths(&v);
    let pos = |p: &str| {
        paths
            .iter()
            .position(|x| x == p)
            .unwrap_or_else(|| panic!("module {p} missing from {paths:?}"))
    };
    assert!(
        pos("src/core") < pos("src/store"),
        "core before store; got {paths:?}"
    );
    assert!(
        pos("src/store") < pos("src/api"),
        "store before api; got {paths:?}"
    );
}

#[test]
fn test_cli_tour_json_shape() {
    let project = setup_tour_project();
    let (stdout, _, code) = run_cli(&project, &["tour", "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json envelope");
    let arr = v["reading_order"]
        .as_array()
        .expect("reading_order is an array");
    assert!(!arr.is_empty());
    let core = arr.iter().find(|e| e["path"] == "src/core").unwrap();
    assert_eq!(
        core["role"], "foundational",
        "core imports nothing in-scope"
    );
    assert_eq!(core["depended_on_by"], 1, "store imports core");
    assert!(core["depends_on"].as_array().unwrap().is_empty());
    assert!(core["in_cycle"].is_boolean());
    assert!(core["key_symbols"].is_array());
    // The dependent module records its in-scope import.
    let store = arr.iter().find(|e| e["path"] == "src/store").unwrap();
    let store_deps: Vec<&str> = store["depends_on"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert_eq!(store_deps, ["src/core"], "store imports core");
}

#[test]
fn test_cli_tour_path_scope() {
    // Scoping to a subtree filters out modules outside it.
    let project = setup_tour_project();
    let (stdout, _, code) = run_cli(&project, &["tour", "src/store", "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    let paths = tour_paths(&v);
    assert_eq!(
        paths,
        ["src/store"],
        "only the scoped module remains; got {paths:?}"
    );
}

#[test]
fn test_cli_json_empty_tour() {
    // cli_json_empty contract: a scope matching no modules still yields the
    // same-shape object envelope on stdout (not a bare bail to stderr).
    let project = setup_tour_project();
    let (stdout, _, code) = run_cli(&project, &["tour", "zznonexistent/", "--json"]);
    assert_eq!(
        code, 0,
        "empty tour exits 0 (structural overview, not a lookup)"
    );
    assert_eq!(
        stdout.trim(),
        r#"{"reading_order":[]}"#,
        "empty result must be the same-shape envelope"
    );
}

#[test]
fn test_cli_cycles_detects_circular_imports() {
    // a.ts and b.ts import each other → a file-level circular import dependency.
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("a.ts"),
        r#"
import { fromB } from './b';
export function fromA(): number { return fromB() + 1; }
"#,
    )
    .unwrap();
    std::fs::write(
        src.join("b.ts"),
        r#"
import { fromA } from './a';
export function fromB(): number { return 2; }
export function alsoB(): number { return fromA(); }
"#,
    )
    .unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();

    let (stdout, stderr, code) = run_cli(&project, &["cycles", "--json"]);
    assert_eq!(code, 0, "cycles exits 0; stdout={stdout} stderr={stderr}");
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("cycles --json must emit valid JSON");
    let arr = v.as_array().expect("cycles --json is an array");
    assert_eq!(
        arr.len(),
        1,
        "exactly one import cycle expected; got {stdout}"
    );
    let files: Vec<&str> = arr[0]["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect();
    assert_eq!(files, ["src/a.ts", "src/b.ts"], "the cycle is a.ts ↔ b.ts");
}

#[test]
fn test_cli_json_empty_cycles() {
    // The standard fixture has only a one-way dep (api → auth), so no cycle.
    // No cycles is a healthy SUCCESS (exit 0), and --json must still emit `[]`.
    let project = setup_indexed_project();
    let (stdout, _, code) = run_cli(&project, &["cycles", "--json"]);
    assert_eq!(code, 0, "no circular imports is success, not an error");
    assert_eq!(
        stdout.trim(),
        "[]",
        "empty cycles must emit [] per the JSON-empty contract"
    );
}

#[test]
fn test_cli_surprising_detects_cross_module_call() {
    // doWork (src/) calls helper (lib/) → a cross-module call edge: surprising.
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    let lib = project.path().join("lib");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::write(
        lib.join("b.ts"),
        "export function helper(): number { return 42; }\n",
    )
    .unwrap();
    std::fs::write(
        src.join("a.ts"),
        r#"
import { helper } from '../lib/b';
export function doWork(): number { return helper(); }
"#,
    )
    .unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();

    let (stdout, stderr, code) = run_cli(&project, &["surprising", "--json"]);
    assert_eq!(
        code, 0,
        "surprising exits 0; stdout={stdout} stderr={stderr}"
    );
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("surprising --json must emit valid JSON");
    let arr = v.as_array().expect("surprising --json is an array");
    assert!(
        arr.iter()
            .any(|c| c["source"].as_str() == Some("doWork")
                && c["target"].as_str() == Some("helper")),
        "should surface the cross-module doWork → helper call; got {stdout}"
    );
}

#[test]
fn test_cli_json_empty_surprising() {
    // A single file with no cross-file calls → no surprising connections.
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("solo.ts"),
        "export function alone(): number { return 1; }\n",
    )
    .unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();

    let (stdout, _, code) = run_cli(&project, &["surprising", "--json"]);
    assert_eq!(
        code, 0,
        "no surprising connections is success, not an error"
    );
    assert_eq!(
        stdout.trim(),
        "[]",
        "empty must emit [] per the JSON-empty contract"
    );
}

#[test]
fn test_cli_report_aggregates_all_sections() {
    // a.ts <-> b.ts mutually import + call → both an import cycle and a surprising edge.
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("a.ts"),
        r#"
import { fromB } from './b';
export function fromA(): number { return fromB(); }
"#,
    )
    .unwrap();
    std::fs::write(
        src.join("b.ts"),
        r#"
import { fromA } from './a';
export function fromB(): number { return fromA(); }
"#,
    )
    .unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();

    let (stdout, stderr, code) = run_cli(&project, &["report", "--json"]);
    assert_eq!(code, 0, "report exits 0; stdout={stdout} stderr={stderr}");
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("report --json must emit a valid JSON object");

    // Object envelope with a populated summary and every section present as an array.
    let summary = v.get("summary").expect("report has a summary");
    assert!(
        summary["files"].as_i64().unwrap() >= 2,
        "summary counts files"
    );
    assert!(
        summary["confidence"].is_object(),
        "summary has a confidence breakdown"
    );
    for key in [
        "hot_functions",
        "chokepoints",
        "import_cycles",
        "surprising_connections",
        "dead_code",
    ] {
        assert!(
            v.get(key).map(|x| x.is_array()).unwrap_or(false),
            "section `{key}` is present as an array"
        );
    }
    // The mutual import is a real cycle, so that section must be non-empty.
    assert!(
        !v["import_cycles"].as_array().unwrap().is_empty(),
        "should report the a.ts <-> b.ts import cycle; got {stdout}"
    );
}

/// Audit 2026-07-27 P2-15: `dead-code <path-with-nothing-indexed> --json`
/// answered `[]` with exit 0 — a clean bill of health for a path the index has
/// never heard of. `overview` answers the same input with an error object and
/// exit 1, and the two surfaces disagreed about the identical failure.
///
/// The path is in-root and well-formed, so `normalize_user_path` passes it
/// through by design; nothing before the query can tell that it names no
/// indexed file. Under `--json 2>/dev/null` the old answer is byte-identical to
/// "this directory genuinely has no dead code", which is the shape an LLM client
/// acts on.
#[test]
fn test_cli_json_dead_code_unindexed_path_discloses() {
    let project = setup_indexed_project();

    let (stdout, _, code) = run_cli(&project, &["dead-code", "src/no_such_dir_xyz", "--json"]);
    assert_eq!(
        code, 1,
        "an unindexed path filter must not exit 0; got:\n{stdout}"
    );
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .expect("stdout must stay valid JSON on the disclosure path");
    assert!(v.is_object(), "expected an error object, got: {v}");
    assert_eq!(v["path"], "src/no_such_dir_xyz");
    assert!(
        v["error"]
            .as_str()
            .unwrap_or("")
            .contains("No indexed files"),
        "the error must say WHY, not just that it is empty: {v}"
    );

    // Same shape as the surface it was diverging from.
    let (ov_stdout, _, ov_code) = run_cli(&project, &["overview", "src/no_such_dir_xyz", "--json"]);
    assert_eq!(
        ov_code, code,
        "dead-code and overview must agree on exit code"
    );
    let ov: serde_json::Value = serde_json::from_str(ov_stdout.trim()).unwrap();
    assert!(ov.is_object() && ov.get("error").is_some());

    // Negative controls: every spelling of a path that IS indexed must still take
    // the true-empty path — `[]`, exit 0. Without these, "disclose harder" passes
    // by turning clean runs into errors, which is what the first version did:
    // it used only bare `src` here, and that is the one spelling of the four that
    // worked. `.` normalizes to `""` and a tab-completed `src/` keeps its
    // trailing slash; neither equals a stored path nor prefixes one with `/`, so
    // both were reported as "no indexed files" on a repo that is simply clean —
    // breaking anything gating CI on the exit code the day it goes green.
    //
    // `--min-lines 999` forces the report EMPTY so the probe is actually reached.
    // Without it this fixture returns candidates for these paths, the probe never
    // runs, and the loop passes no matter what the probe does — measured: with
    // the normalization deleted, the scratch repro exits 1 for `.` and `src/`
    // while this loop stayed green.
    for ok_path in [".", "src", "src/", "./src", "src//", "./src//"] {
        let (clean_stdout, _, clean_code) = run_cli(
            &project,
            &["dead-code", ok_path, "--min-lines", "999", "--json"],
        );
        assert_eq!(
            clean_code, 0,
            "`dead-code {ok_path}` names indexed files and must keep the \
             true-empty contract; got:\n{clean_stdout}"
        );
        let clean: serde_json::Value = serde_json::from_str(clean_stdout.trim())
            .unwrap_or_else(|e| panic!("`dead-code {ok_path} --json` emitted invalid JSON: {e}"));
        assert!(
            clean.get("error").is_none(),
            "`dead-code {ok_path}` must not be reported as an error: {clean}"
        );
    }

    // ...and a trailing slash must not smuggle an unindexed path past the probe
    // either — the trim is normalization, not a bypass.
    let (slash_stdout, _, slash_code) =
        run_cli(&project, &["dead-code", "src/no_such_dir_xyz/", "--json"]);
    assert_eq!(
        slash_code, 1,
        "trailing slash must not bypass the probe: {slash_stdout}"
    );

    // The FALSE CLEAN itself, which the exit-code checks above cannot see: with
    // candidates present, every spelling of the same directory must return the
    // same ones. `src//` used to return `[]` at exit 0 while `src` returned real
    // dead code — the probe trimmed a TRAILING slash for its own comparison
    // while the query kept the untrimmed filter, so the disclosure never fired
    // and the empty result read as clean. Asserting only on exit codes leaves
    // that invisible: the probe's own trim makes the exit code right while the
    // answer stays wrong. Measured — with the collapse in
    // `merkle::normalize_rel_str_on` disabled, the exit-code loop above stays
    // green and this block goes red.
    let results_for = |p: &str| -> serde_json::Value {
        let (out, _, _) = run_cli(&project, &["dead-code", p, "--min-lines", "1", "--json"]);
        serde_json::from_str(out.trim())
            .unwrap_or_else(|e| panic!("`dead-code {p} --min-lines 1 --json` invalid JSON: {e}"))
    };
    let canonical = results_for("src");
    for spelling in ["src/", "src//", "./src//", "src///"] {
        assert_eq!(
            results_for(spelling),
            canonical,
            "`dead-code {spelling}` must return exactly what `dead-code src` returns"
        );
    }
}

/// Audit 2026-07-27 P2-3 / review follow-up: `project_map`'s two inline copies of
/// the test-classification rule fell a fix behind the shared one.
///
/// `hot_functions` judges SOURCE rows with `domain::prod_source_filter_and()` and
/// TARGET rows with what used to be a hand-written copy against its own `n`/`f`
/// aliases. When the shared rule moved to anchored, extension-pinned,
/// case-sensitive GLOB, the copy kept unanchored, any-extension,
/// case-INsensitive LIKE — so inside one query the two sides disagreed about
/// what a test is, and symbols that `callgraph` happily lists were silently
/// missing from the map.
///
/// The two shapes below are the ones that fell through, both production per
/// `is_test_symbol`:
///   * `Test_Signup` — `LIKE 'test\_%'` is ASCII-case-insensitive; `starts_with`
///     is not;
///   * a symbol in `src/a_test.ts` — `.ts` is not in `INFIX_TEST_EXTS`, so
///     `is_test_path` calls it production, but `LIKE '%_test.%'` matched any
///     extension.
#[test]
fn test_cli_map_hot_functions_agree_with_is_test_symbol() {
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("main.ts"),
        "export function plainHelper(): number { return 1; }\n\
         export function Test_Signup(): number { return 2; }\n\
         export function callAll(): number { return plainHelper() + Test_Signup() + helperFromUnderscoreTest(); }\n\
         export function callAgain(): number { return plainHelper() + Test_Signup() + helperFromUnderscoreTest(); }\n",
    )
    .unwrap();
    std::fs::write(
        src.join("a_test.ts"),
        "export function helperFromUnderscoreTest(): number { return 3; }\n",
    )
    .unwrap();
    std::fs::write(
        project.path().join("package.json"),
        "{\"name\":\"p\",\"version\":\"1.0.0\"}",
    )
    .unwrap();
    std::fs::create_dir_all(project.path().join(".git")).unwrap();

    let (_, _, _) = run_cli(&project, &["incremental-index"]);
    let (stdout, _, code) = run_cli(&project, &["map", "--json"]);
    assert_eq!(code, 0, "map --json failed:\n{stdout}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("map --json");
    let names: Vec<&str> = v["hot_functions"]
        .as_array()
        .expect("hot_functions array")
        .iter()
        .filter_map(|h| h["name"].as_str())
        .collect();

    // Control: never in doubt on either rule. If this one is missing the fixture
    // failed to index and the assertions below would pass for the wrong reason.
    assert!(
        names.contains(&"plainHelper"),
        "fixture did not index — no hot function at all: {names:?}"
    );
    for expected in ["Test_Signup", "helperFromUnderscoreTest"] {
        assert!(
            names.contains(&expected),
            "`{expected}` is production per `is_test_symbol` but project_map \
             dropped it — the target-side filter has drifted from \
             `domain::prod_filter_and`: {names:?}"
        );
    }
}

#[test]
fn test_cli_worktree_reader_never_writes_worktree_content_into_main_index() {
    // Audit 2026-08-02 P1-1: read commands in a linked worktree resolve the
    // MAIN checkout's index (cc655aa read-side fallback) but the freshness
    // check hashed WORKTREE files against it — divergent branch content read
    // as "stale" and ensure_file_indexed wrote the worktree's version into
    // the main index (hash swapped, lines shifted, files absent on the branch
    // CASCADE-deleted). The sibling test above uses a same-commit worktree,
    // so every hash matches and that write path is unreachable by
    // construction; this one DIVERGES the branch first and pins that a read
    // command leaves the main index byte-identical.
    if !has_git() {
        eprintln!("skipping: git not installed");
        return;
    }
    let root = TempDir::new().unwrap();
    let main = root.path().join("main");
    let src = main.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("auth.ts"),
        "export function hashPassword(p: string): string { return p; }\n",
    )
    .unwrap();
    std::fs::write(
        src.join("other.ts"),
        "export function sideHelper(): number { return 1; }\n",
    )
    .unwrap();
    let git = |args: &[&str], cwd: &std::path::Path| {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"], &main);
    git(&["add", "."], &main);
    git(&["commit", "-qm", "init"], &main);
    let db_dir = main.join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, &main, None, None).unwrap();
    let baseline: Vec<(String, String)> = {
        let mut stmt = db
            .conn()
            .prepare("SELECT path, blake3_hash FROM files ORDER BY path")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    drop(db);
    assert!(
        baseline.iter().any(|(p, _)| p == "src/other.ts"),
        "precondition: other.ts indexed"
    );

    // Diverge the worktree: edit auth.ts (lines shift) and DELETE other.ts.
    let wt = root.path().join("wt");
    git(
        &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "feat"],
        &main,
    );
    std::fs::write(
        wt.join("src/auth.ts"),
        "// divergent branch content\n// shifting every line\n\n\n\nexport function hashPassword(p: string): string { return p + p; }\n",
    )
    .unwrap();
    std::fs::remove_file(wt.join("src/other.ts")).unwrap();

    // Two reads from the worktree (the reproduction needed two: the first
    // marked stale, the second surfaced the corrupted state).
    for _ in 0..2 {
        let out = Command::new(binary_path())
            .args(["show", "hashPassword"])
            .current_dir(&wt)
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "worktree read must succeed; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // The MAIN index must be untouched: same file set, same hashes.
    let db2 = code_graph_mcp::storage::db::Database::open_nondestructive(&db_dir.join("index.db"))
        .unwrap();
    let after: Vec<(String, String)> = {
        let mut stmt = db2
            .conn()
            .prepare("SELECT path, blake3_hash FROM files ORDER BY path")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    assert_eq!(
        baseline, after,
        "a READ command from a divergent worktree mutated the main checkout's index"
    );
}

#[test]
fn test_cli_json_error_no_index_emits_error_object() {
    // Audit 2026-08-02 P1-7: with --json, a pre-handler bail (here: no index)
    // used to leave stdout at 0 bytes with exit 1 — a machine consumer got a
    // JSON parse failure on the single most common error path. The contract's
    // third tier is an {"error": ...} object on stdout + exit 1.
    let project = TempDir::new().unwrap();
    for cmd in [
        vec!["show", "foo"],
        vec!["overview", "src"],
        vec!["callgraph", "x"],
        vec!["refs", "y"],
        vec!["map"],
        vec!["dead-code"],
        vec!["cycles"],
        vec!["impact", "z"],
    ] {
        let mut args = cmd.clone();
        args.push("--json");
        let (out, _err, code) = run_cli(&project, &args);
        assert_eq!(code, 1, "{cmd:?} --json must exit 1 without an index");
        let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap_or_else(|_| {
            panic!("{cmd:?} --json must emit parseable JSON on stdout, got: {out:?}")
        });
        assert!(
            v.get("error").is_some(),
            "{cmd:?} --json must carry an error key; got: {v}"
        );
    }
}

#[test]
fn test_cli_json_error_path_outside_root_emits_error_object() {
    // Same tier-3 contract, out-of-root leg (the 8 path-taking commands used
    // to bail before any JSON-aware code ran).
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a.ts"), "export function alpha() {}\n").unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);
    for args in [
        vec!["overview", "/etc", "--json"],
        vec!["deps", "/etc/passwd", "--json"],
        vec!["tour", "../..", "--json"],
    ] {
        let (out, _err, code) = run_cli(&project, &args);
        assert_eq!(code, 1, "{args:?} must exit 1");
        let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap_or_else(|_| {
            panic!("{args:?} must emit parseable JSON on stdout, got: {out:?}")
        });
        assert!(v.get("error").is_some(), "{args:?} error key; got: {v}");
    }
}

// --- health-check integrity + version-staleness parity (audit DB-1 / DB-3) ---

/// Build a tiny indexed project and return it. Separate from
/// `setup_indexed_project` so these tests own a minimal, fast fixture.
fn setup_tiny_indexed_project() -> TempDir {
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("a.ts"),
        "export function alpha(): number { return 1; }\nexport function beta(): number { return alpha(); }\n",
    )
    .unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);
    project
}

/// Rewind `application_id` (where INDEX_VERSION lives) so a reader open reports
/// `index_version_stale`, exactly as it would for an index built by an older
/// extractor generation. Must be the LAST write: a later indexer open restamps it.
fn make_index_version_stale(project: &TempDir) {
    let db_path = project
        .path()
        .join(code_graph_mcp::domain::CODE_GRAPH_DIR)
        .join("index.db");
    let db = code_graph_mcp::storage::db::Database::open_nondestructive(&db_path).unwrap();
    db.conn()
        .execute_batch(&format!(
            "PRAGMA application_id = {};",
            code_graph_mcp::domain::INDEX_VERSION - 1
        ))
        .unwrap();
    drop(db);
}

/// DB-1: `healthy` used to be `schema_ok && nodes>0 && files>0` — no integrity
/// signal at all. Both output faces must now carry the three probes.
#[test]
fn test_cli_health_check_reports_integrity_in_json_face() {
    let project = setup_tiny_indexed_project();
    let (out, err, code) = run_cli(&project, &["health-check", "--json"]);
    assert_eq!(code, 0, "healthy index; stderr: {err}");
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    let integ = v
        .get("integrity")
        .unwrap_or_else(|| panic!("health-check --json must carry integrity; got: {v}"));
    assert_eq!(
        integ.get("quick_check").and_then(|q| q.as_str()),
        Some("ok"),
        "a freshly built index must pass quick_check; got: {integ}"
    );
    assert_eq!(
        integ.get("fts_drift").and_then(|d| d.as_i64()),
        Some(0),
        "nodes and the FTS index must agree on a fresh build; got: {integ}"
    );
    // Structure-only builds have no vec table → null, never a bogus 0 count.
    let orphans = integ.get("orphan_vectors").unwrap();
    assert!(
        orphans.is_null() || orphans.as_i64() == Some(0),
        "orphan_vectors must be 0 or unavailable on a fresh index; got: {integ}"
    );
}

#[test]
fn test_cli_health_check_reports_integrity_in_text_face() {
    let project = setup_tiny_indexed_project();
    let (out, err, code) = run_cli(&project, &["health-check"]);
    assert_eq!(code, 0, "healthy index; stderr: {err}");
    assert!(
        out.contains("Integrity: quick_check ok"),
        "the human face must state the integrity verdict too; got:\n{out}"
    );
    assert!(
        out.contains("FTS drift 0"),
        "FTS drift must be visible in the text face; got:\n{out}"
    );
}

/// DB-3: the JSON face has reported `issue: "…rebuild pending"` for a
/// version-lagging index all along while the text face printed a bare `OK:` —
/// the same command answering a script and a human differently. Assert BOTH.
#[test]
fn test_cli_health_check_index_version_stale_appears_in_both_faces() {
    let project = setup_tiny_indexed_project();
    make_index_version_stale(&project);

    let (json_out, _e, json_code) = run_cli(&project, &["health-check", "--json"]);
    let v: serde_json::Value = serde_json::from_str(json_out.trim()).unwrap();
    assert_eq!(
        v.get("index_version_stale").and_then(|s| s.as_bool()),
        Some(true),
        "fixture precondition: the index must read as version-stale; got: {v}"
    );
    assert!(
        v.get("issue")
            .and_then(|i| i.as_str())
            .unwrap_or_default()
            .contains("rebuild pending"),
        "JSON face must keep naming the owed rebuild; got: {v}"
    );
    // Version staleness is not an unhealthy index — the data is usable.
    assert_eq!(json_code, 0, "a stale-but-usable index stays exit 0");

    let (text_out, text_err, text_code) = run_cli(&project, &["health-check"]);
    assert_eq!(text_code, 0);
    let text = format!("{text_out}{text_err}");
    assert!(
        text.contains("Index version: STALE"),
        "the text face must report the SAME stale verdict as the JSON face \
         (DB-3: it said only `OK:`); got:\n{text}"
    );
    assert!(
        text.contains("reindex"),
        "the stale line must name the fix; got:\n{text}"
    );
}

/// The discriminating case for DB-1: an index whose PAGES no longer read back.
/// Before the integrity probe, this exact database reported `"healthy": true`
/// with exit 0 — `healthy` only ever meant "right schema, non-empty".
///
/// Fixture note: page 4 (offset 4096*3) of this fixture is a live table b-tree
/// page. That is deliberate and narrow — corrupting the whole file instead would
/// break `get_index_status` first and never reach the probe, while corrupting
/// page 1 would make SQLite refuse to open at all. If a schema change relocates
/// the page this test fails loudly rather than silently passing.
#[test]
fn test_cli_health_check_flags_page_level_corruption() {
    use std::io::{Seek, SeekFrom, Write};
    let project = setup_tiny_indexed_project();
    let db_path = project
        .path()
        .join(code_graph_mcp::domain::CODE_GRAPH_DIR)
        .join("index.db");

    const CORRUPT_AT: u64 = 4096 * 3;
    let len = std::fs::metadata(&db_path).unwrap().len();
    assert!(
        len > CORRUPT_AT + 300,
        "fixture precondition: the index must be big enough to hold page 4 (got {len} bytes)"
    );
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .open(&db_path)
        .unwrap();
    f.seek(SeekFrom::Start(CORRUPT_AT)).unwrap();
    f.write_all(&[0xAB; 300]).unwrap();
    drop(f);

    let (out, err, code) = run_cli(&project, &["health-check", "--json"]);
    let v: serde_json::Value = serde_json::from_str(out.trim())
        .unwrap_or_else(|_| panic!("must still emit JSON on a corrupt index; got {out:?} {err:?}"));
    let quick = v
        .pointer("/integrity/quick_check")
        .and_then(|q| q.as_str())
        .unwrap_or("");
    assert!(
        quick != "ok" && !quick.is_empty(),
        "quick_check must report the damaged page; got integrity: {}",
        v.get("integrity").unwrap()
    );
    assert_eq!(
        v.get("healthy").and_then(|h| h.as_bool()),
        Some(false),
        "a database whose pages do not read back is not healthy; got: {v}"
    );
    assert!(
        v.get("issue")
            .and_then(|i| i.as_str())
            .unwrap_or_default()
            .contains("integrity check failed"),
        "the issue must name corruption so the caller knows to rebuild; got: {v}"
    );
    assert_eq!(code, 1, "unhealthy must exit 1");

    // The read-only poll must not have destroyed the evidence.
    assert!(
        db_path.exists(),
        "health-check is a reader — it must report corruption, not delete the index"
    );

    // Text face reaches the SAME verdict (the DB-3 class of bug is two faces
    // disagreeing; a new check must not reintroduce it).
    let (t_out, t_err, t_code) = run_cli(&project, &["health-check"]);
    let text = format!("{t_out}{t_err}");
    assert!(
        text.contains("integrity check failed"),
        "text face must report corruption too; got:\n{text}"
    );
    assert_eq!(t_code, 1);
}

/// Proves the FTS drift probe is DISCRIMINATING. The obvious spelling —
/// `COUNT(*) FROM nodes` vs `COUNT(*) FROM nodes_fts` — cannot fail: `nodes_fts`
/// is an external-content table (`content='nodes'`), so counting it reads
/// through to `nodes` and returns the same number no matter what the FTS index
/// actually holds. This test drops one document from the FTS5 shadow table,
/// leaving `nodes` untouched: the vacuous spelling still reports drift 0, the
/// shipped one reports 1.
#[test]
fn test_cli_health_check_fts_drift_detects_a_lost_fts_document() {
    let project = setup_tiny_indexed_project();
    let db_path = project
        .path()
        .join(code_graph_mcp::domain::CODE_GRAPH_DIR)
        .join("index.db");
    {
        let db = code_graph_mcp::storage::db::Database::open_nondestructive(&db_path).unwrap();
        db.conn()
            .execute_batch(
                "DELETE FROM nodes_fts_docsize WHERE id = (SELECT MIN(id) FROM nodes_fts_docsize);",
            )
            .unwrap();
    }

    let (out, err, _code) = run_cli(&project, &["health-check", "--json"]);
    let v: serde_json::Value =
        serde_json::from_str(out.trim()).unwrap_or_else(|_| panic!("got {out:?} {err:?}"));
    assert_eq!(
        v.pointer("/integrity/fts_drift").and_then(|d| d.as_i64()),
        Some(1),
        "one node indexed by `nodes` but missing from the FTS index must read as \
         drift 1 — search silently misses that symbol; got: {}",
        v.get("integrity").unwrap()
    );
    let (t_out, _t_err, _c) = run_cli(&project, &["health-check"]);
    assert!(
        t_out.contains("FTS drift 1"),
        "the text face must carry the same number; got:\n{t_out}"
    );
}

// --- report freshness (audit FRS-5) + CLI .gitignore (DB-4) ---

/// `report` prints dead-code `file:line` straight from the index and had NO
/// query-time refresh, while its standalone sibling `dead-code` has had one
/// since the shared resync landed. Editing above a dead symbol therefore made
/// `report` cite a line the symbol no longer occupies — with no disclosure.
#[test]
fn test_cli_report_refreshes_dead_code_line_numbers_after_an_edit() {
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    // `orphan` is called by nobody and spans >3 lines, so find_dead_code's
    // min_lines=3 floor (hard-coded in cmd_report) keeps it.
    let orphan =
        "export function orphan(): number {\n  const a = 1;\n  const b = 2;\n  return a + b;\n}\n";
    std::fs::write(src.join("a.ts"), orphan).unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    let line_of = |out: &str| -> i64 {
        let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        v["dead_code"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["name"] == "orphan")
            .unwrap_or_else(|| panic!("orphan must be reported as dead code; got: {v}"))["line"]
            .as_i64()
            .unwrap()
    };

    let (before, _e, code) = run_cli(&project, &["report", "--json"]);
    assert_eq!(code, 0);
    let before_line = line_of(&before);
    assert_eq!(before_line, 1, "fixture: orphan starts at line 1");

    // Push the symbol down by 3 lines WITHOUT reindexing — the "edited, then
    // immediately queried" shape the resync exists for.
    std::fs::write(src.join("a.ts"), format!("// x\n// y\n// z\n{orphan}")).unwrap();

    let (after, _e2, code2) = run_cli(&project, &["report", "--json"]);
    assert_eq!(code2, 0);
    assert_eq!(
        line_of(&after),
        before_line + 3,
        "report must re-index the edited file before printing its line numbers \
         (FRS-5); got:\n{after}"
    );
}

/// DB-4: writing `.code-graph/` to `.gitignore` lived only in the MCP server's
/// `from_project_root`, so a pure-CLI user (hook-driven indexing, server never
/// started) got an untracked 100 MB index that `git add -A` would commit.
#[test]
fn test_cli_incremental_index_gitignores_the_index_dir() {
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a.ts"), "export function alpha() {}\n").unwrap();
    // main.rs skips indexing a root with neither .git nor an existing index.
    std::fs::create_dir_all(project.path().join(".git")).unwrap();

    let (_o, err, code) = run_cli(&project, &["incremental-index", "--quiet", "--no-embed"]);
    assert_eq!(code, 0, "stderr: {err}");
    let gitignore = project.path().join(".gitignore");
    let content = std::fs::read_to_string(&gitignore)
        .unwrap_or_else(|e| panic!("CLI indexing must create .gitignore: {e}"));
    assert!(
        content
            .lines()
            .any(|l| l.trim().trim_end_matches('/') == ".code-graph"),
        "the index dir must be ignored; got: {content:?}"
    );

    // Idempotent: a second run must not append a duplicate line.
    let (_o2, _e2, code2) = run_cli(&project, &["incremental-index", "--quiet", "--no-embed"]);
    assert_eq!(code2, 0);
    let content2 = std::fs::read_to_string(&gitignore).unwrap();
    assert_eq!(content, content2, "second run must not re-append");
}

// --- worktree read-side root split (audit FRS-4, sibling of 5eb80c6) ---

/// The write-side worktree fix (5eb80c6) routed all 9 refresh call sites through
/// `ctx.project_root`, but `show --context-lines` still sliced source bytes out
/// of the RAW `project_root` while its line numbers came from the main
/// checkout's index — so the moment the branch's content diverged, `show`
/// printed whatever happened to sit on those lines in the worktree.
///
/// The existing worktree e2e (`..._falls_back_to_main_index`) cannot see this:
/// its worktree is on the same commit, so both roots hold identical bytes. This
/// one forks the content on purpose.
#[test]
fn test_cli_show_context_lines_reads_the_indexed_checkout_not_the_worktree() {
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("skipping: git not installed");
        return;
    }
    let root = TempDir::new().unwrap();
    let main = root.path().join("main");
    let src = main.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("auth.ts"),
        "export function hashPassword(p: string): string { return MAIN_MARKER(p); }\n",
    )
    .unwrap();
    let git = |args: &[&str], cwd: &std::path::Path| {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"], &main);
    git(&["add", "."], &main);
    git(&["commit", "-qm", "init"], &main);

    // Index the MAIN checkout only — the worktree gets no index of its own, so
    // reads fall back to this one (effective_read_root).
    let db_dir = main.join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, &main, None, None).unwrap();
    drop(db);

    let wt = root.path().join("wt");
    git(
        &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "feat"],
        &main,
    );
    assert!(
        wt.join(".git").is_file(),
        "fixture must be a LINKED worktree"
    );
    // THE FORK: same path, different bytes, and hashPassword no longer sits on
    // line 1. Line 1 in the worktree is now unrelated content.
    std::fs::write(
        wt.join("src/auth.ts"),
        "// worktree-only line A\n// worktree-only line B\n\
         export function hashPassword(p: string): string { return WORKTREE_MARKER(p); }\n",
    )
    .unwrap();

    // --context-lines MUST be > 0: at 0, cmd_show prints the index's stored
    // code_content and never calls read_source_context, so the divergence this
    // test exists for is unreachable (the first draft used 0 and stayed green
    // against the unfixed code).
    let out = Command::new(binary_path())
        .args(["show", "hashPassword", "--context-lines", "1", "--json"])
        .current_dir(&wt)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("show --json must parse; got {stdout:?} {stderr:?}"));
    let code_content = v[0]["code_content"].as_str().unwrap_or_default();

    assert!(
        code_content.contains("MAIN_MARKER"),
        "start_line comes from the MAIN checkout's index, so the bytes must come \
         from the main checkout too (FRS-4); got code_content: {code_content:?}"
    );
    assert!(
        !code_content.contains("worktree-only line"),
        "slicing the WORKTREE's bytes at the main index's line numbers prints \
         unrelated lines — the exact defect; got: {code_content:?}"
    );

    // Parallel path: the human arm reads source through the same helper and was
    // changed in the same way, so it needs its own assertion — a green JSON arm
    // says nothing about it.
    let text_out = Command::new(binary_path())
        .args(["show", "hashPassword", "--context-lines", "1"])
        .current_dir(&wt)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&text_out.stdout).to_string();
    assert!(
        text.contains("MAIN_MARKER") && !text.contains("worktree-only line"),
        "the text arm must read the indexed checkout too; got:\n{text}"
    );
}

/// `deps`' barrel fallback is the sibling reader: it echoes source lines WITH
/// line numbers, from the same raw root.
#[test]
fn test_cli_deps_barrel_scan_reads_the_indexed_checkout_not_the_worktree() {
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("skipping: git not installed");
        return;
    }
    let root = TempDir::new().unwrap();
    let main = root.path().join("main");
    let src = main.join("src");
    std::fs::create_dir_all(&src).unwrap();
    // A Rust-style barrel: `pub mod` lines with no tracked dep edges, which is
    // what drives cmd_deps into scan_barrel_patterns.
    std::fs::write(src.join("mod.rs"), "pub mod main_only;\n").unwrap();
    std::fs::write(src.join("main_only.rs"), "pub fn f() {}\n").unwrap();
    let git = |args: &[&str], cwd: &std::path::Path| {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"], &main);
    git(&["add", "."], &main);
    git(&["commit", "-qm", "init"], &main);
    let db_dir = main.join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, &main, None, None).unwrap();
    drop(db);

    let wt = root.path().join("wt");
    git(
        &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "feat"],
        &main,
    );
    std::fs::write(wt.join("src/mod.rs"), "pub mod worktree_only;\n").unwrap();

    let out = Command::new(binary_path())
        .args(["deps", "src/mod.rs", "--json"])
        .current_dir(&wt)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Only assert the divergence when the barrel-scan fallback actually fired —
    // if the graph gained tracked edges for this file the branch is not reached,
    // and asserting on it would be testing a path that did not run.
    if stdout.contains("barrel_scan") {
        assert!(
            stdout.contains("main_only"),
            "barrel_scan must echo the checkout the index describes (FRS-4); got:\n{stdout}"
        );
        assert!(
            !stdout.contains("worktree_only"),
            "echoing the WORKTREE's lines under the main index's numbering is the \
             defect; got:\n{stdout}"
        );
    } else {
        assert!(
            stdout.contains("main_only") && !stdout.contains("worktree_only"),
            "deps answered from the graph; it must still describe the indexed \
             checkout; got:\n{stdout} {stderr}"
        );
    }

    // Third reader in the same command: the "does this file exist?" probe that
    // picks between the "not a barrel/import file" and "File not found"
    // diagnoses. Delete the file from the WORKTREE only — it still exists in the
    // checkout the index describes, so the honest answer is the former.
    std::fs::remove_file(wt.join("src/main_only.rs")).unwrap();
    let out2 = Command::new(binary_path())
        .args(["deps", "src/main_only.rs", "--json"])
        .current_dir(&wt)
        .output()
        .unwrap();
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout2.trim())
        .unwrap_or_else(|_| panic!("deps --json must parse; got {stdout2:?}"));
    let err_msg = v.get("error").and_then(|e| e.as_str()).unwrap_or_default();
    assert!(
        !err_msg.contains("File not found"),
        "the file exists in the indexed checkout — judging existence against the \
         worktree reports a phantom missing file (FRS-4); got: {v}"
    );
}

/// The size gate must be VISIBLE, not silent, and `--deep` must defeat it —
/// otherwise a large index quietly loses the integrity signal with no way to
/// ask for it. `CODE_GRAPH_INTEGRITY_MAX_BYTES` stands in for a 128 MB index.
#[test]
fn test_cli_health_check_size_gate_is_disclosed_and_deep_overrides_it() {
    let project = setup_tiny_indexed_project();

    let (skipped, _e, _c) = run_cli_env(
        &project,
        &["health-check", "--json"],
        &[("CODE_GRAPH_INTEGRITY_MAX_BYTES", "1")],
    );
    let v: serde_json::Value = serde_json::from_str(skipped.trim()).unwrap();
    assert_eq!(
        v.pointer("/integrity/quick_check").and_then(|q| q.as_str()),
        Some("skipped_large"),
        "over the ceiling the skip must be stated, not blank; got: {v}"
    );
    assert_eq!(
        v.get("healthy").and_then(|h| h.as_bool()),
        Some(true),
        "a SKIPPED check is not a failed check — it must not flip healthy; got: {v}"
    );

    let (deep, _e2, _c2) = run_cli_env(
        &project,
        &["health-check", "--json", "--deep"],
        &[("CODE_GRAPH_INTEGRITY_MAX_BYTES", "1")],
    );
    let v2: serde_json::Value = serde_json::from_str(deep.trim()).unwrap();
    assert_eq!(
        v2.pointer("/integrity/quick_check")
            .and_then(|q| q.as_str()),
        Some("ok"),
        "--deep must run the pragma regardless of the ceiling; got: {v2}"
    );
}
