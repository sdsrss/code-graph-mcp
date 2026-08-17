//! End-to-end MCP protocol tests via stdio JSON-RPC.
//!
//! These tests spawn `code-graph-mcp serve` as a subprocess, talk to it
//! through stdin/stdout, and assert on the live JSON-RPC responses.
//! Cover the fix points that unit tests can't reach:
//!   - prod-first sort ordering survives serde_json round-trip and
//!     centralized_compress truncation (R1/R2 fixes)
//!   - SQL caller_count filtering produces the same shape MCP clients see (R4/R5)
//!   - find_references explanatory error for test-only symbols (A fix)

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;

fn binary_path() -> String {
    env!("CARGO_BIN_EXE_code-graph-mcp").to_string()
}

/// Build a fixture project with one target function plus enough callers
/// (mix of prod, inline test, tests/ dir, benches/) to force compression
/// truncation and stress the prod-first sort.
fn setup_fixture_project() -> TempDir {
    let project = TempDir::new().unwrap();

    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let tests_dir = project.path().join("tests");
    std::fs::create_dir_all(&tests_dir).unwrap();
    let benches_dir = project.path().join("benches");
    std::fs::create_dir_all(&benches_dir).unwrap();

    // Target with 3 prod callers in src/cli.rs
    std::fs::write(src.join("target.rs"), "pub fn target_fn() -> i32 { 42 }\n").unwrap();
    std::fs::write(
        src.join("lib.rs"),
        "pub mod target;\npub mod cli;\npub mod inline_tests;\n",
    )
    .unwrap();
    std::fs::write(
        src.join("cli.rs"),
        r#"use crate::target::target_fn;
pub fn prod_caller_a() -> i32 { target_fn() }
pub fn prod_caller_b() -> i32 { target_fn() + 1 }
pub fn prod_caller_c() -> i32 { target_fn() + 2 }
"#,
    )
    .unwrap();

    // 25 inline tests in src/inline_tests.rs (trigger compression > 20-element cap)
    let mut inline = String::from("use crate::target::target_fn;\n");
    for i in 0..25 {
        inline.push_str(&format!(
            "#[cfg(test)]\n#[test]\nfn test_inline_{i:02}_calls_target() {{ assert_eq!(target_fn(), 42); }}\n"
        ));
    }
    std::fs::write(src.join("inline_tests.rs"), inline).unwrap();

    // 5 integration tests in tests/integration.rs
    let mut integ = String::new();
    for i in 0..5 {
        integ.push_str(&format!(
            "#[test]\nfn test_integ_{i}_calls_target() {{ assert_eq!(fixture_lib::target::target_fn(), 42); }}\n"
        ));
    }
    std::fs::write(tests_dir.join("integration.rs"), integ).unwrap();

    // 1 bench
    std::fs::write(
        benches_dir.join("bench_target.rs"),
        "fn bench_target() { let _ = fixture_lib::target::target_fn(); }\n",
    )
    .unwrap();

    // Cargo.toml so the indexer picks the right language root
    std::fs::write(
        project.path().join("Cargo.toml"),
        r#"[package]
name = "fixture_lib"
version = "0.0.0"
edition = "2021"

[lib]
path = "src/lib.rs"
"#,
    )
    .unwrap();

    // Index in-process (faster + deterministic than letting the spawned
    // server do it on first call).
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db_path = db_dir.join("index.db");
    let db = code_graph_mcp::storage::db::Database::open(&db_path).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    project
}

/// Fixture with two `ambiguous` by-name fan-out cases for the v0.76 confidence
/// floor disclosure, mirroring the cli_e2e fixtures but indexed for the stdio
/// server. Cargo.toml is present so `serve` serves the full tool catalog rather
/// than the 0-tool stub a bare cwd gets (see mcp_non_project_cwd_serves_zero_tool_stub).
///   - Rust: `main` bare-calls `thing()`, defined in BOTH a.rs and b.rs → both
///     call edges are `ambiguous` (drives `get_call_graph` ambiguous_edges_hidden).
///   - Python: `save` defined in BOTH db.py and cache.py; `run` calls the imported
///     db.save → that caller edge is `ambiguous` (drives `get_ast_node
///     include_impact` ambiguous_callers_excluded).
fn setup_ambiguous_fanout_project() -> TempDir {
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();

    // Rust: ambiguous callee fan-out.
    std::fs::write(src.join("a.rs"), "pub fn thing() -> i32 { 1 }\n").unwrap();
    std::fs::write(src.join("b.rs"), "pub fn thing() -> i32 { 2 }\n").unwrap();
    std::fs::write(
        src.join("main.rs"),
        "mod a;\nmod b;\nfn main() {\n    let x = thing();\n    println!(\"{}\", x);\n}\n",
    )
    .unwrap();

    // Python: ambiguous caller fan-out. `run` calls `save` WITHOUT importing
    // either def, so the by-name edge is genuinely ambiguous (2 tied candidates).
    // An explicit `from db import save` would corroborate the binding → inferred,
    // not folded (resolve::confidence::classify_import_corroborated_duplicate_stays_visible).
    std::fs::write(src.join("db.py"), "def save(r):\n    return True\n").unwrap();
    std::fs::write(src.join("cache.py"), "def save(i):\n    return True\n").unwrap();
    std::fs::write(
        src.join("app.py"),
        "def run():\n    return save({\"id\": 1})\n",
    )
    .unwrap();

    // Project marker so the spawned server serves the full catalog.
    std::fs::write(
        project.path().join("Cargo.toml"),
        "[package]\nname = \"fixture_lib\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .unwrap();

    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    project
}

struct McpClient {
    child: Child,
    next_id: i64,
    reader: BufReader<std::process::ChildStdout>,
    init_response: Value,
}

impl McpClient {
    fn spawn(project_root: &std::path::Path) -> Self {
        let mut child = Command::new(binary_path())
            .arg("serve")
            .current_dir(project_root)
            // Disable the embed-model auto-download: under `cargo test --features
            // embed-model` each spawned server would otherwise background-fetch the
            // ~90 MB model on a cache-less runner (slow + flaky). A cached model is
            // still loaded, so embed tests that need real weights behave the same.
            .env("CODE_GRAPH_DISABLE_MODEL_DOWNLOAD", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn mcp server");
        let stdout = child.stdout.take().expect("stdout piped");
        let reader = BufReader::new(stdout);
        let mut client = Self {
            child,
            next_id: 1,
            reader,
            init_response: Value::Null,
        };

        // Initialize handshake — required before tools/list or tools/call
        let init = client.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "stdio-test", "version": "0.0.0"},
            }),
            Duration::from_secs(15),
        );
        assert!(
            init.get("result").is_some(),
            "initialize failed: {:?}",
            init
        );
        client.init_response = init;
        client
    }

    fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let stdin = self.child.stdin.as_mut().expect("stdin piped");
        writeln!(stdin, "{}", req).expect("write request");
        stdin.flush().expect("flush stdin");

        let start = Instant::now();
        loop {
            if start.elapsed() > timeout {
                panic!("MCP request {} timed out after {:?}", method, timeout);
            }
            let mut line = String::new();
            let n = self.reader.read_line(&mut line).expect("read line");
            if n == 0 {
                panic!("MCP server closed stdout before response to {}", method);
            }
            let line_trim = line.trim();
            if line_trim.is_empty() {
                continue;
            }
            let resp: Value = match serde_json::from_str(line_trim) {
                Ok(v) => v,
                Err(_) => continue, // skip non-JSON lines (shouldn't happen on stdout, but be defensive)
            };
            // Filter notifications (no id) and other-id responses
            if resp.get("id").and_then(|i| i.as_i64()) == Some(id) {
                return resp;
            }
        }
    }

    fn call_tool(&mut self, name: &str, args: Value) -> Value {
        self.request(
            "tools/call",
            json!({"name": name, "arguments": args}),
            Duration::from_secs(30),
        )
    }

    /// Fire-and-forget JSON-RPC notification (no id, no response expected).
    #[cfg_attr(not(feature = "embed-model"), allow(dead_code))]
    fn notify(&mut self, method: &str, params: Value) {
        let req = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let stdin = self.child.stdin.as_mut().expect("stdin piped");
        writeln!(stdin, "{}", req).expect("write notification");
        stdin.flush().expect("flush notification");
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// MCP wraps tool results as `{result: {content: [{type: "text", text: <json-string>}]}}`.
/// Pull out the inner JSON.
fn extract_tool_payload(resp: &Value) -> Value {
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("expected result.content[0].text string in: {}", resp));
    serde_json::from_str(text).unwrap_or_else(|e| panic!("tool text not JSON ({}): {}", e, text))
}

// =============================================================================
// Tests
// =============================================================================

/// P0.1: `run_serve` must serve a 0-tool stub in a non-project cwd (no
/// .git/manifest), mirroring the JS launcher gate (mcp-launcher.js). It must
/// NOT create `.code-graph/` in the throwaway dir. Closes the parallel path
/// the v0.33.0 launcher gate left open for direct-binary invocations.
#[test]
fn mcp_non_project_cwd_serves_zero_tool_stub() {
    let bare = TempDir::new().unwrap(); // no Cargo.toml / .git / package.json
    let mut client = McpClient::spawn(bare.path());

    // initialize response must identify the non-project stub
    let name = client.init_response["result"]["serverInfo"]["name"]
        .as_str()
        .unwrap_or("");
    assert!(
        name.contains("stub"),
        "expected non-project stub serverInfo, got: {}",
        client.init_response
    );

    // tools/list must be empty
    let resp = client.request("tools/list", json!({}), Duration::from_secs(10));
    let tools = resp["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools not an array: {}", resp));
    assert!(
        tools.is_empty(),
        "non-project stub must serve 0 tools, got {}",
        tools.len()
    );

    // unknown method → -32601 stub error
    let err = client.request(
        "tools/call",
        json!({"name": "get_call_graph"}),
        Duration::from_secs(10),
    );
    assert_eq!(
        err["error"]["code"].as_i64(),
        Some(-32601),
        "stub must reject tool calls: {}",
        err
    );

    // and no index must have been created in the throwaway dir
    assert!(
        !bare.path().join(".code-graph").exists(),
        "stub must not create .code-graph/ in a non-project cwd"
    );
}

/// Positive control: CODE_GRAPH_FORCE_PLUGIN_MCP=1 overrides the gate, so even
/// a bare dir gets the full server (non-empty tool catalog).
#[test]
fn mcp_force_plugin_mcp_overrides_non_project_gate() {
    let bare = TempDir::new().unwrap();
    let mut child = Command::new(binary_path())
        .arg("serve")
        .current_dir(bare.path())
        .env("CODE_GRAPH_FORCE_PLUGIN_MCP", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mcp server");
    let stdout = child.stdout.take().expect("stdout piped");
    let mut reader = BufReader::new(stdout);
    let stdin = child.stdin.as_mut().expect("stdin piped");

    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2024-11-05","capabilities":{{}},"clientInfo":{{"name":"t","version":"0"}}}}}}"#).unwrap();
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{{}}}}"#
    )
    .unwrap();
    stdin.flush().unwrap();

    let start = Instant::now();
    let mut tools_len = None;
    while start.elapsed() < Duration::from_secs(20) {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap() == 0 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            if v.get("id").and_then(|i| i.as_i64()) == Some(2) {
                tools_len = v["result"]["tools"].as_array().map(|a| a.len());
                break;
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        tools_len.unwrap_or(0) > 0,
        "FORCE_PLUGIN_MCP=1 must serve the full tool catalog even in a bare dir, got {:?}",
        tools_len
    );
}

/// R1 fix: get_ast_node called_by must put prod callers first when test-heavy.
/// Without the sort, post-truncation `called_by` would be all-test (the bug).
#[test]
fn mcp_get_ast_node_called_by_prod_first_under_truncation() {
    let project = setup_fixture_project();
    let mut client = McpClient::spawn(project.path());

    let resp = client.call_tool(
        "get_ast_node",
        json!({
            "symbol_name": "target_fn",
            "include_references": true,
            "include_tests": true,
            "compact": true,
        }),
    );

    let body = extract_tool_payload(&resp);
    let called_by = body["called_by"]
        .as_array()
        .unwrap_or_else(|| panic!("called_by is not an array: {}", body));

    assert!(
        !called_by.is_empty(),
        "called_by must have entries (target has 3 prod + many test callers)"
    );

    // Look at the first 3 entries — these should be the 3 prod callers
    // (post-sort, prod come first; tests at tail).
    let first_three_names: Vec<&str> = called_by
        .iter()
        .take(3)
        .filter_map(|x| x["name"].as_str())
        .collect();
    let prod_count = first_three_names
        .iter()
        .filter(|n| n.starts_with("prod_caller_"))
        .count();
    assert!(
        prod_count >= 2,
        "first 3 of called_by should include >=2 prod_caller_*; got {:?} (full body: {})",
        first_three_names,
        body
    );
}

/// R2 fix: find_references default include_tests=true must put prod first.
#[test]
fn mcp_find_references_default_prod_first() {
    let project = setup_fixture_project();
    let mut client = McpClient::spawn(project.path());

    let resp = client.call_tool(
        "find_references",
        json!({
            "symbol_name": "target_fn",
            "compact": true,
        }),
    );

    let body = extract_tool_payload(&resp);
    let refs = body["references"]
        .as_array()
        .unwrap_or_else(|| panic!("references not array: {}", body));
    assert!(!refs.is_empty(), "references must have entries");

    let first_three_names: Vec<&str> = refs
        .iter()
        .take(3)
        .filter_map(|x| x["name"].as_str())
        .collect();
    let prod_count = first_three_names
        .iter()
        .filter(|n| n.starts_with("prod_caller_"))
        .count();
    assert!(
        prod_count >= 2,
        "first 3 of references should include >=2 prod_caller_*; got {:?}",
        first_three_names
    );
}

/// R4/R5 fix + A fix: caller_count is prod-only and find_references on a
/// test-only symbol returns an explanatory error (not "not found").
#[test]
fn mcp_caller_count_prod_only_and_test_symbol_error_explains() {
    let project = setup_fixture_project();
    let mut client = McpClient::spawn(project.path());

    // module_overview src — target_fn must have caller_count == 3 (prod-only),
    // not 31 (3 prod + 25 inline test + 5 tests/ + 1 bench, target reachable).
    let overview = client.call_tool(
        "module_overview",
        json!({
            "path": "src",
            "compact": true,
        }),
    );
    let body = extract_tool_payload(&overview);
    let active = body["active"].as_array().expect("active array");
    let target = active
        .iter()
        .find(|e| e["name"].as_str() == Some("target_fn"))
        .unwrap_or_else(|| panic!("target_fn missing from active exports: {}", body));
    let caller_count = target["caller_count"].as_i64().expect("caller_count i64");
    assert_eq!(
        caller_count, 3,
        "caller_count must be 3 prod-only (3 prod_caller_* in src/cli.rs), \
         not include test/bench sources; got {}",
        caller_count
    );

    // A fix: find_references on a test-only symbol should error with
    // "exists but all matches are in test/bench paths" rather than the old
    // misleading "not found".
    let resp = client.call_tool(
        "find_references",
        json!({
            "symbol_name": "test_inline_00_calls_target",
        }),
    );
    // Tool errors come back either as JSON-RPC error or as result.isError=true with text.
    let err_text = resp
        .get("error")
        .and_then(|e| e["message"].as_str())
        .or_else(|| {
            if resp["result"]["isError"].as_bool() == Some(true) {
                resp["result"]["content"][0]["text"].as_str()
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("expected error response, got: {}", resp));

    assert!(
        err_text.contains("test/bench paths") || err_text.contains("bypass the test filter"),
        "error must explain the test filter; got: {}",
        err_text
    );
}

/// Regression: enum-valued direction/deps_direction args must be validated at the
/// tool entry. Previously, `get_call_graph` echoed a bogus direction back through
/// the ambiguity-resolution path (two errors for one mistake), `dependency_graph`
/// only rejected after index-freshness checks ran, and `module_overview` silently
/// swallowed bogus `deps_direction` into a `dependencies_unavailable` field.
#[test]
fn mcp_enum_args_validated_at_tool_entry() {
    let project = setup_fixture_project();
    let mut client = McpClient::spawn(project.path());

    let tool_err = |resp: &Value| -> String {
        if resp["result"]["isError"].as_bool() == Some(true) {
            resp["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or("")
                .to_string()
        } else {
            panic!("expected isError=true, got: {}", resp);
        }
    };

    // get_call_graph direction enum
    let r = client.call_tool(
        "get_call_graph",
        json!({
            "symbol_name": "target_fn", "direction": "sideways",
        }),
    );
    assert!(
        tool_err(&r).contains("direction must be one of: callers, callees, both"),
        "get_call_graph should reject bad direction at entry; got: {}",
        tool_err(&r)
    );

    // dependency_graph direction enum
    let r = client.call_tool(
        "dependency_graph",
        json!({
            "file_path": "src/lib.rs", "direction": "upside_down",
        }),
    );
    assert!(
        tool_err(&r).contains("direction must be one of: outgoing, incoming, both"),
        "dependency_graph should reject bad direction at entry; got: {}",
        tool_err(&r)
    );

    // module_overview deps_direction enum (this was silently swallowed before)
    let r = client.call_tool(
        "module_overview",
        json!({
            "path": "src/lib.rs", "include_deps": true, "deps_direction": "upside_down",
        }),
    );
    assert!(
        tool_err(&r).contains("deps_direction must be one of"),
        "module_overview should reject bad deps_direction at entry; got: {}",
        tool_err(&r)
    );

    // module_overview deps_direction must be validated UNCONDITIONALLY — even
    // without include_deps and for a directory path. Before the fix the check was
    // gated inside `if include_deps { if path-is-file {...} }`, so this path never
    // validated and returned a normal OK overview, hiding the typo.
    let r = client.call_tool(
        "module_overview",
        json!({
            "path": "src", "deps_direction": "upside_down",
        }),
    );
    assert!(
        tool_err(&r).contains("deps_direction must be one of"),
        "module_overview must reject bad deps_direction even without include_deps; got: {}",
        tool_err(&r)
    );

    // find_references relation enum typo
    let r = client.call_tool(
        "find_references",
        json!({
            "symbol_name": "target_fn", "relation": "call",
        }),
    );
    assert!(
        tool_err(&r).contains("Unknown relation filter"),
        "find_references should reject bad relation at entry; got: {}",
        tool_err(&r)
    );
}

/// Contract: `impact_analysis` was removed as an MCP tool — it was the lone orphaned
/// folded handler (no advertised tool delegated to it; `get_ast_node include_impact`
/// has its own compact summary). Full impact now lives on the CLI (`impact --json`),
/// compact impact via `get_ast_node include_impact`. Calling the legacy name must
/// error cleanly, not silently dispatch — pins the removal so a future re-add is a
/// conscious decision.
#[test]
fn mcp_impact_analysis_tool_is_removed() {
    let project = setup_fixture_project();
    let mut client = McpClient::spawn(project.path());
    let resp = client.call_tool("impact_analysis", json!({ "symbol_name": "target_fn" }));
    assert_eq!(
        resp["result"]["isError"].as_bool(),
        Some(true),
        "the removed impact_analysis tool name must return isError; got: {resp}"
    );
    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("Unknown tool"),
        "calling the removed impact_analysis name must report Unknown tool; got: {text}"
    );
}

/// Regression: `relation` must be validated BEFORE symbol resolution, so a bogus
/// relation on a nonexistent symbol reports the relation error — not the
/// "symbol not found" error that would otherwise mask the real typo.
#[test]
fn mcp_find_references_invalid_relation_precedes_resolution() {
    let project = setup_fixture_project();
    let mut client = McpClient::spawn(project.path());
    let r = client.call_tool(
        "find_references",
        json!({
            "symbol_name": "definitely_absent_symbol_xyz", "relation": "bogus",
        }),
    );
    let text = if r["result"]["isError"].as_bool() == Some(true) {
        r["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string()
    } else {
        panic!("expected isError=true, got: {}", r);
    };
    assert!(
        text.contains("Unknown relation filter"),
        "relation must be validated before symbol resolution; got: '{}'",
        text
    );
}

/// Regression (#4): find_dead_code must reject an unknown node_type loudly rather
/// than returning a false-clean empty result (a literal `n.type = :x` → 0 rows).
#[test]
fn mcp_find_dead_code_rejects_unknown_node_type() {
    let project = setup_fixture_project();
    let mut client = McpClient::spawn(project.path());
    let r = client.call_tool("find_dead_code", json!({ "node_type": "fucntion" }));
    let text = if r["result"]["isError"].as_bool() == Some(true) {
        r["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string()
    } else {
        panic!("expected isError=true, got: {}", r);
    };
    assert!(
        text.contains("Unknown type filter"),
        "find_dead_code must reject unknown node_type; got: '{}'",
        text
    );
    // Argument-order pin, matching the CLI sites: the MCP surface shipped the
    // same swapped pair (v0.118.0 pre-tag review), and a prefix-only assertion
    // could not see it.
    assert!(
        text.contains("'fucntion'"),
        "the caller's bad value must be the quoted one; got: '{}'",
        text
    );
    assert!(
        text.contains(&format!(
            "Valid: {}",
            code_graph_mcp::domain::TYPE_FILTER_HELP
        )),
        "the vocabulary must follow `Valid:`; got: '{}'",
        text
    );
}

/// Regression: an "edit-only" session that issues NO code-graph tool call must
/// still get its index embedded. The embedding backfill used to be kicked off
/// only by `consume_startup_index_result()`, which runs on an incoming MCP
/// message (i.e. a tool call). With no tool call the finished startup index's
/// vectors were stranded — the daagu "2% vec, never moves" symptom. The fix
/// drives the backfill from the startup-index thread itself, so the handshake
/// alone is enough.
#[cfg(feature = "embed-model")]
#[test]
fn mcp_startup_embeds_without_any_tool_call() {
    use code_graph_mcp::storage::db::Database;
    use code_graph_mcp::storage::queries::count_nodes_with_vectors;

    // Coverage note: CI's `embed-check` job now runs `cargo test --features embed-model`,
    // but with CODE_GRAPH_DISABLE_MODEL_DOWNLOAD=1 (set by McpClient::spawn AND the job),
    // so the server never auto-fetches weights. This test therefore still runs only
    // where the model is ALREADY cached (a local `cargo test --features embed-model`);
    // in CI it skips. It needs real weights to observe embedding; skip loudly when
    // absent rather than false-fail.
    if code_graph_mcp::embedding::model::EmbeddingModel::load()
        .ok()
        .flatten()
        .is_none()
    {
        eprintln!("[skip] embedding model weights unavailable; cannot observe backfill");
        return;
    }

    let project = setup_fixture_project();
    let db_path = project
        .path()
        .join(code_graph_mcp::domain::CODE_GRAPH_DIR)
        .join("index.db");

    // Precondition: the in-process index (built with model=None) has embeddable
    // nodes but zero vectors. Open with vec so node_vectors exists for polling.
    {
        let db = Database::open_with_vec(&db_path).unwrap();
        let (with_vectors, total) = count_nodes_with_vectors(db.conn()).unwrap();
        assert!(
            total > 0,
            "fixture must have embeddable nodes (got total={total})"
        );
        assert_eq!(
            with_vectors, 0,
            "fixture must start with 0 vectors (got {with_vectors})"
        );
    }

    // Drive ONLY the lifecycle handshake: initialize (in spawn) + the initialized
    // notification. Never send a tools/call.
    let mut client = McpClient::spawn(project.path());
    client.notify("notifications/initialized", json!({}));

    // The backfill runs asynchronously in the startup-index thread. Poll the
    // vector count until it climbs above zero.
    let mut embedded = 0i64;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(60) {
        std::thread::sleep(Duration::from_millis(500));
        if let Ok(db) = Database::open_with_vec(&db_path) {
            if let Ok((with_vectors, _)) = count_nodes_with_vectors(db.conn()) {
                embedded = with_vectors;
                if embedded > 0 {
                    break;
                }
            }
        }
    }

    assert!(
        embedded > 0,
        "startup index must embed nodes with NO tool call; got {embedded} vectors after 60s"
    );
}

/// The server backfills embeddings only on startup or on an MCP tool call
/// (`ensure_indexed`). A session that uses code-graph purely through the PreToolUse
/// CLI hooks adds nodes via `ensure_file_indexed` (model=None) with NO tool call, and
/// — with the watcher off — nothing embeds them: they strand at <100% vector coverage
/// until restart (the mem "99% vec, never finishes" symptom). The periodic backfill
/// driver must drain such out-of-band additions on its own, with no tool call at all.
#[cfg(feature = "embed-model")]
#[test]
fn mcp_periodic_backfill_embeds_out_of_band_nodes() {
    use code_graph_mcp::storage::db::Database;
    use code_graph_mcp::storage::queries::{count_nodes_with_vectors, count_unembedded_nodes};

    if code_graph_mcp::embedding::model::EmbeddingModel::load()
        .ok()
        .flatten()
        .is_none()
    {
        eprintln!("[skip] embedding model weights unavailable; cannot observe backfill");
        return;
    }

    let project = setup_fixture_project();
    let project_root = project.path().to_path_buf();
    let db_path = project_root
        .join(code_graph_mcp::domain::CODE_GRAPH_DIR)
        .join("index.db");

    // Bring the server up and let the startup backfill settle, so the periodic driver's
    // floor converges on the un-embeddable residue. Send ONLY the lifecycle handshake.
    let mut client = McpClient::spawn(project.path());
    client.notify("notifications/initialized", json!({}));

    // Wait for the startup backfill to FULLY DRAIN (every embeddable node vectored,
    // `count_unembedded_nodes == 0`) before adding the out-of-band node. A "count stopped
    // changing" heuristic is unreliable — model inference is bursty, so the startup loop
    // can stall for seconds mid-drain and look settled while embeddable nodes remain.
    // Draining to exactly zero is the unambiguous "startup backfill is done" signal, so
    // afterwards ONLY a fresh trigger (the periodic driver — no tool call is ever sent)
    // can embed the node we insert. NOTE: this assumes `setup_fixture_project`'s sources
    // produce NO un-embeddable residue (every node with a context_string embeds). If a
    // future fixture symbol breaks that, this loop times out and the assert below fires —
    // a real signal to revisit the fixture, not a flake.
    let mut base_vectors = 0i64;
    let settled_unembedded = 0i64;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(90) {
        std::thread::sleep(Duration::from_millis(1000));
        let db = match Database::open_with_vec(&db_path) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let (with_vectors, _) = count_nodes_with_vectors(db.conn()).unwrap_or((0, 0));
        let unembedded = count_unembedded_nodes(db.conn()).unwrap_or(i64::MAX);
        if with_vectors > 0 && unembedded == 0 {
            base_vectors = with_vectors;
            break;
        }
    }
    assert!(
        base_vectors > 0,
        "startup must fully drain the initial embeddings before the out-of-band test"
    );

    // Add an embeddable node OUT OF BAND, by DIRECT DB insert and NO filesystem write —
    // this is the stranded state a CLI/hook `ensure_file_indexed` (model=None) leaves
    // behind: a node with a `context_string` and no vector, with no pending tool call to
    // drain it. Inserting via the DB (not a file write) also keeps the server's file
    // watcher entirely out of the picture, so ONLY a DB-polling embedder can pick it up.
    {
        use code_graph_mcp::storage::queries::{insert_node, upsert_file, FileRecord, NodeRecord};
        let db = Database::open_with_vec(&db_path).unwrap();
        let fid = upsert_file(
            db.conn(),
            &FileRecord {
                path: "src/out_of_band.rs".into(),
                blake3_hash: "oob-hash".into(),
                last_modified: 1,
                language: Some("rust".into()),
            },
        )
        .unwrap();
        insert_node(db.conn(), &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "periodically_backfilled_fn".into(),
            qualified_name: None, start_line: 1, end_line: 3,
            code_content: "pub fn periodically_backfilled_fn() -> i32 { 1234 }".into(),
            signature: None, doc_comment: None,
            context_string: Some(
                "rust function periodically_backfilled_fn — pub fn periodically_backfilled_fn() -> i32 { 1234 }".into()),
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();
    }
    // Precondition: the new node is present but UNembedded — it raised the unembedded
    // count above the settled residue.
    {
        let db = Database::open_with_vec(&db_path).unwrap();
        let unembedded = count_unembedded_nodes(db.conn()).unwrap();
        assert!(
            unembedded > settled_unembedded,
            "out-of-band insert must add an unembedded node (settled={settled_unembedded}, now={unembedded})"
        );
    }

    // No tool call is ever sent. The periodic driver alone must embed the new node.
    let mut now_vectors = base_vectors;
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(500));
        if let Ok(db) = Database::open_with_vec(&db_path) {
            if let Ok((with_vectors, _)) = count_nodes_with_vectors(db.conn()) {
                now_vectors = with_vectors;
                if now_vectors > base_vectors {
                    break;
                }
            }
        }
    }
    assert!(
        now_vectors > base_vectors,
        "periodic backfill must embed out-of-band nodes with NO tool call; \
         base={base_vectors}, after={now_vectors}"
    );
}

/// Wire-layer contract for the v0.76 confidence floor (closes the previously
/// deferred MCP stdio assertion). The disclosure field NAMES must survive the
/// JSON-RPC round-trip on the advertised tools an agent actually consumes, and
/// the `min_confidence` opt-in must make them disappear. The query layer is
/// already covered by unit tests + cli_e2e, but a field present in a Rust struct
/// yet dropped from serde output would pass those and fail only a real client —
/// so this pins the serialized names over the wire (field presence != serialization).
#[test]
fn mcp_confidence_floor_disclosure_over_wire() {
    let project = setup_ambiguous_fanout_project();
    let mut client = McpClient::spawn(project.path());

    let thing_callees = |b: &Value| {
        b["callees"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|c| c["name"].as_str() == Some("thing"))
                    .count()
            })
            .unwrap_or(0)
    };

    // get_call_graph: ambiguous callee fan-out hidden + disclosed by default.
    // file_path pins the seed so the all-suppressed result routes straight to
    // format_call_graph_response (not the fuzzy-resolve fallback that can return
    // a bare object without the disclosure).
    let body = extract_tool_payload(&client.call_tool(
        "get_call_graph",
        json!({
            "symbol_name": "main", "file_path": "src/main.rs", "direction": "callees",
        }),
    ));
    assert_eq!(
        body["ambiguous_edges_hidden"].as_u64(), Some(2),
        "get_call_graph default floor must disclose the 2 hidden ambiguous `thing` edges over the wire; got: {body}"
    );
    assert_eq!(
        thing_callees(&body),
        0,
        "default floor must hide the ambiguous `thing` fan-out from callees; got: {body}"
    );

    // Opt-in restores the edges and drops the disclosure field.
    let body = extract_tool_payload(&client.call_tool(
        "get_call_graph",
        json!({
            "symbol_name": "main", "file_path": "src/main.rs", "direction": "callees",
            "min_confidence": "ambiguous",
        }),
    ));
    assert!(
        body.get("ambiguous_edges_hidden").is_none(),
        "nothing is suppressed at the ambiguous floor; got: {body}"
    );
    assert_eq!(
        thing_callees(&body),
        2,
        "min_confidence:\"ambiguous\" must show both tied `thing` edges over the wire; got: {body}"
    );

    // get_ast_node include_impact: ambiguous caller folded + disclosed by default.
    // file_path disambiguates the seed to the db.py `save` def.
    let body = extract_tool_payload(&client.call_tool(
        "get_ast_node",
        json!({
            "symbol_name": "save", "file_path": "src/db.py",
            "include_impact": true, "compact": true,
        }),
    ));
    assert_eq!(body["impact"]["ambiguous_callers_excluded"].as_u64(), Some(1),
        "get_ast_node include_impact must disclose the 1 folded ambiguous caller over the wire; got: {body}");
    assert_eq!(
        body["impact"]["direct_callers"].as_u64(),
        Some(0),
        "the ambiguous caller is folded out of the default risk count; got: {body}"
    );

    // Opt-in counts the caller and drops the disclosure field.
    let body = extract_tool_payload(&client.call_tool(
        "get_ast_node",
        json!({
            "symbol_name": "save", "file_path": "src/db.py",
            "include_impact": true, "compact": true, "min_confidence": "ambiguous",
        }),
    ));
    assert!(
        body["impact"].get("ambiguous_callers_excluded").is_none(),
        "nothing excluded at the ambiguous floor; got: {body}"
    );
    assert_eq!(
        body["impact"]["direct_callers"].as_u64(),
        Some(1),
        "min_confidence:\"ambiguous\" must count the ambiguous caller over the wire; got: {body}"
    );
}

/// Read stdout lines until one carries JSON-RPC `id == id`, or the timeout /
/// EOF is hit. Returns `None` on EOF (server closed stdout — i.e. the session
/// died) or timeout, so callers can assert survival without panicking.
fn read_json_id(
    reader: &mut BufReader<std::process::ChildStdout>,
    id: i64,
    timeout: Duration,
) -> Option<Value> {
    let start = Instant::now();
    loop {
        if start.elapsed() > timeout {
            return None;
        }
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return None, // EOF: stdout closed → serve loop exited
            Ok(_) => {}
            Err(_) => return None,
        }
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(t) {
            if v.get("id").and_then(|i| i.as_i64()) == Some(id) {
                return Some(v);
            }
        }
    }
}

/// H3 regression: a single oversized message whose byte length exceeds
/// MAX_MESSAGE_SIZE (10 MiB) and whose truncation boundary lands in the middle
/// of a multi-byte UTF-8 char must NOT tear down the long-lived serve session.
///
/// 10 MiB (10485760) is not a multiple of 3, so `take(MAX).read_line` over a
/// stream of 3-byte CJP chars truncates mid-character. The old code used
/// `read_line`, which UTF-8-validates: it returned `Err(InvalidData)`, and the
/// `?` propagated OUTSIDE the per-request `catch_unwind`, killing the whole loop
/// — a single 10 MB+ CJK request became a session-level DoS. The fix reads raw
/// bytes (`read_until`) + lossily decodes, so the oversized line is rejected
/// with a JSON-RPC error and the session survives to answer the next request.
#[test]
fn mcp_oversized_multibyte_message_does_not_kill_session() {
    let project = setup_fixture_project();
    let mut child = Command::new(binary_path())
        .arg("serve")
        .current_dir(project.path())
        .env("CODE_GRAPH_DISABLE_MODEL_DOWNLOAD", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mcp server");
    let stdout = child.stdout.take().expect("stdout piped");
    let mut reader = BufReader::new(stdout);
    let mut stdin = child.stdin.take().expect("stdin piped");

    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2024-11-05","capabilities":{{}},"clientInfo":{{"name":"t","version":"0"}}}}}}"#).unwrap();
    stdin.flush().unwrap();
    assert!(
        read_json_id(&mut reader, 1, Duration::from_secs(15)).is_some(),
        "initialize must respond before the oversized-message probe"
    );

    // 12 MB of a 3-byte CJK char, no embedded newline: forces the mid-character
    // truncation at the 10 MiB `take` boundary. Tolerate EPIPE — under the OLD
    // (buggy) binary the server dies mid-read and this write breaks; that broken
    // pipe IS the failure the assertion below catches.
    let big = "好".repeat(4_000_000);
    let _ = stdin.write_all(big.as_bytes());
    let _ = stdin.write_all(b"\n");
    let _ = stdin.flush();

    // The next VALID request must still be answered.
    let _ = writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{{}}}}"#
    );
    let _ = stdin.flush();

    let got = read_json_id(&mut reader, 2, Duration::from_secs(20));
    let _ = child.kill();
    let _ = child.wait();

    let resp = got.expect(
        "serve was killed by the oversized multibyte message: no id:2 response (H3 regression)",
    );
    let tools = resp["result"]["tools"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    assert!(
        tools > 0,
        "follow-up tools/list must return the catalog after the oversized message; got: {resp}"
    );
}

/// LOW-drain regression: a line LARGER than 2×MAX_MESSAGE_SIZE (10 MiB) must be
/// FULLY drained after the oversized rejection. The old drain did a single
/// `take(MAX).read_until('\n')`, which consumes at most one MAX-sized chunk — so
/// the tail of a >2×MAX line was left in the stream and misparsed as a bogus
/// next message, producing a SPURIOUS second error response and desyncing the
/// stream. The fix loops the drain until the terminating newline (or EOF), so
/// arbitrarily large lines are consumed whole: exactly ONE error response for
/// the oversized line, and the following valid request is still answered.
#[test]
fn mcp_oversized_line_beyond_2x_max_drains_fully_no_spurious_error() {
    let project = setup_fixture_project();
    let mut child = Command::new(binary_path())
        .arg("serve")
        .current_dir(project.path())
        .env("CODE_GRAPH_DISABLE_MODEL_DOWNLOAD", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mcp server");
    let stdout = child.stdout.take().expect("stdout piped");
    let mut reader = BufReader::new(stdout);
    let mut stdin = child.stdin.take().expect("stdin piped");

    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2024-11-05","capabilities":{{}},"clientInfo":{{"name":"t","version":"0"}}}}}}"#).unwrap();
    stdin.flush().unwrap();
    assert!(
        read_json_id(&mut reader, 1, Duration::from_secs(15)).is_some(),
        "initialize must respond before the oversized-drain probe"
    );

    // 21 MiB single-byte line (> 2×MAX = 20 MiB), no embedded newline. Under the
    // OLD single-shot drain, ~1 MiB of tail survives past the 2× MAX boundary and
    // is misparsed as a second (invalid-JSON) message → a spurious parse error.
    // ASCII byte on purpose: multibyte-truncation survival is the sibling test's
    // concern; here we isolate the drain-length bug. Build with a repeated byte
    // vec (no per-char string push). Tolerate EPIPE if a buggy build dies.
    let big = vec![b'x'; 21 * 1024 * 1024];
    let _ = stdin.write_all(&big);
    let _ = stdin.write_all(b"\n");
    let _ = stdin.flush();

    // Valid follow-up request.
    let _ = writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{{}}}}"#
    );
    let _ = stdin.flush();

    // Read every line until the id:2 response (or timeout/EOF), counting how many
    // carry a JSON-RPC `error`. The stream is ordered, so any spurious tail-parse
    // error is emitted BEFORE the id:2 response. Exactly one error (the oversized
    // reject) is expected; two means the >2×MAX tail leaked as a bogus message.
    let mut error_count = 0usize;
    let mut tools_ok = false;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(25) {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF: session died
            Ok(_) => {}
            Err(_) => break,
        }
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(t) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("error").is_some() {
            error_count += 1;
        }
        if v.get("id").and_then(|i| i.as_i64()) == Some(2) {
            tools_ok = v["result"]["tools"]
                .as_array()
                .map(|a| !a.is_empty())
                .unwrap_or(false);
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();

    assert!(
        tools_ok,
        "valid tools/list after a >2×MAX line must be answered (session survived and stream not desynced)"
    );
    assert_eq!(
        error_count, 1,
        "exactly ONE error response expected for the oversized line; {error_count} means the >2×MAX tail was misparsed as a spurious message (drain not looped)"
    );
}

/// Express-style route fixture whose handler makes an ambiguous by-name call, so
/// the trace chain + one-hop downstream list both carry the `ambiguous` fan-out.
fn setup_route_ambiguous_fanout_project() -> TempDir {
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();

    // Two same-name `thing` defs, not imported into server.ts, so the handler's
    // bare `thing()` call resolves ambiguously to both (the fan-out class).
    std::fs::write(src.join("a.ts"), "export function thing() { return 1; }\n").unwrap();
    std::fs::write(src.join("b.ts"), "export function thing() { return 2; }\n").unwrap();
    std::fs::write(src.join("server.ts"), "\nconst app = express();\nfunction widgetsHandler(req, res) {\n    thing();\n    res.json([]);\n}\napp.get('/widgets', widgetsHandler);\n").unwrap();

    // Project marker so the spawned server resolves the root (language-agnostic).
    std::fs::write(
        project.path().join("Cargo.toml"),
        "[package]\nname = \"fixture_lib\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .unwrap();

    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    project
}

/// Wire-layer contract for the v0.77 trace confidence floor. `get_call_graph` in
/// `route_path` mode dispatches to the HTTP-chain tracer, which (pre-v0.77) ran at
/// rank-0 show-all and ignored the already-advertised `min_confidence` arg. This
/// pins, over the real JSON-RPC round-trip, that the default floor hides the
/// ambiguous downstream fan-out from the trace chain and discloses the count, and
/// that `min_confidence:"ambiguous"` restores it. Sibling to
/// `mcp_confidence_floor_disclosure_over_wire` for the route surface.
#[test]
fn mcp_trace_confidence_floor_over_wire() {
    let project = setup_route_ambiguous_fanout_project();
    let mut client = McpClient::spawn(project.path());

    let chain_things = |b: &Value| {
        b["handlers"][0]["call_chain"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|n| n["name"].as_str() == Some("thing"))
                    .count()
            })
            .unwrap_or(0)
    };

    // Default floor: ambiguous `thing` fan-out hidden from the trace chain + disclosed.
    let body = extract_tool_payload(&client.call_tool(
        "get_call_graph",
        json!({
            "route_path": "GET /widgets",
        }),
    ));
    assert_eq!(
        chain_things(&body),
        0,
        "trace default floor must hide the ambiguous `thing` fan-out over the wire; got: {body}"
    );
    assert_eq!(
        body["ambiguous_edges_hidden"].as_u64(),
        Some(2),
        "trace default floor must disclose the 2 hidden ambiguous edges over the wire; got: {body}"
    );

    // Opt-in restores the fan-out and drops the disclosure field.
    let body = extract_tool_payload(&client.call_tool(
        "get_call_graph",
        json!({
            "route_path": "GET /widgets", "min_confidence": "ambiguous",
        }),
    ));
    assert_eq!(
        chain_things(&body),
        2,
        "min_confidence:\"ambiguous\" must show both tied `thing` edges over the wire; got: {body}"
    );
    assert!(
        body.get("ambiguous_edges_hidden").is_none(),
        "nothing is suppressed at the ambiguous floor; got: {body}"
    );
}

/// Contract audit 2026-07-27: the two `find_references` surfaces disagreed about
/// an import-only name, and the MCP one lied about why.
///
/// `<external>` sentinels are excluded from *resolution* on purpose, so
/// `resolve_fuzzy` returns NotFound for a name that exists only as an import.
/// The NotFound arm then re-queried unfiltered, found the sentinel, and reported
/// it as a "test/bench path" the caller could "bypass the test filter" on —
/// `<external>` is neither a path on disk nor subject to any test filter, and
/// the advice it gives an LLM client cannot work. Meanwhile the CLI `refs`
/// answered normally, so CHANGELOG's "find_references / refs now answer for
/// imported std names" was true of exactly half of what it named.
#[test]
fn mcp_find_references_answers_for_an_import_only_name() {
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("lib.rs"), "pub mod a;\npub mod b;\n").unwrap();
    // HashMap exists ONLY as an import — no project symbol by that name.
    std::fs::write(
        src.join("a.rs"),
        "use std::collections::HashMap;\npub fn build() -> HashMap<u8, u8> { HashMap::new() }\n",
    )
    .unwrap();
    // A project symbol whose name is ALSO imported from std in the same file.
    // What it pins is project-symbol PREFERENCE, not the `<external>` exclusion
    // — see the long note at the assertions below for why it cannot be the
    // latter and which test is.
    //
    // Two earlier versions of it were INERT, for two different reasons, and the
    // second is the subtle one:
    //   1. the fixture defined `take_local`, so there was no project `take` to
    //      prefer and the assertion could not fail;
    //   2. `use`-import sentinels are stored with node type `module`, and the
    //      by-name queries already carry `AND n.type != 'module'` — so the
    //      `<external>` predicate the control is supposed to exercise was a
    //      no-op for them. Deleting it changed nothing and the test still passed.
    //
    // A third version added an `impl Debug for S` fixture in `src/c.rs` on the
    // theory that a `trait`-typed IMPLEMENTS sentinel escapes the type filter and
    // would therefore exercise the exclusion. Round 7 measured it: the payload is
    // byte-identical either way, because find_references answers from edge rows.
    // Both the fixture and this comment's claim about it were deleted — the
    // sentence that used to sit here ("`impl Debug for S` is what makes it live")
    // described a file that no longer exists, which is the same invitation to
    // trust an inert control, just relocated into prose.
    std::fs::write(
        src.join("b.rs"),
        "use std::mem::take;\npub fn take() -> u8 { 7 }\npub fn helper() -> u8 { take() }\n",
    )
    .unwrap();
    std::fs::write(
        project.path().join("Cargo.toml"),
        "[package]\nname = \"ext_fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n",
    )
    .unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    let mut client = McpClient::spawn(project.path());
    let resp = client.call_tool("find_references", json!({ "symbol_name": "HashMap" }));

    let err_text = resp
        .get("error")
        .and_then(|e| e["message"].as_str())
        .or_else(|| {
            if resp["result"]["isError"].as_bool() == Some(true) {
                resp["result"]["content"][0]["text"].as_str()
            } else {
                None
            }
        });

    // Whatever it does, it must never describe `<external>` as a test/bench path.
    if let Some(text) = err_text {
        assert!(
            !text.contains("test/bench paths"),
            "`<external>` is not a test/bench path and there is no filter to \
             bypass; this message sends an LLM client down an impossible \
             recovery. Got: {text}"
        );
    }

    // And per CHANGELOG it should answer with the import edges that bind it.
    let body = extract_tool_payload(&resp);
    let refs = body["references"]
        .as_array()
        .unwrap_or_else(|| panic!("expected references for an import-only name, got: {body}"));
    assert!(
        !refs.is_empty(),
        "find_references on an import-only name must return its `imports` rows \
         (the CLI `refs` surface already does): {body}"
    );

    // A name that exists BOTH as a project symbol and as a std import must still
    // resolve to the project symbol.
    //
    // Honest scope, after three attempts to make this a negative control for the
    // `<external>` exclusion and three mutation runs proving it was not one: it
    // is NOT that control and cannot be. The by-name fuzzy path already carries
    // `AND n.type != 'module'`, and a sentinel is typed non-`module` only when no
    // project symbol shares its name — precisely the case with nothing to
    // discriminate. Deleting `EXCLUDE_EXTERNAL_BY_NAME` or neutering
    // `is_selectable_definition` leaves this test green.
    //
    // The live guard for that exclusion is
    // `show_does_not_resolve_a_name_that_exists_only_as_an_import` in
    // tests/reader_nondestructive.rs, which drives the binary at the surface
    // where the defect was observed. Round-7 correction: it goes red under the
    // SQL mutation only — `is_selectable_definition` sits BEHIND that guard, so
    // no reachable input exercises it end-to-end, and it is unit-tested directly
    // in src/resolve.rs instead. What the assertions below DO cover is
    // project-symbol preference, which is worth pinning on its own.
    let resp = client.call_tool("find_references", json!({ "symbol_name": "take" }));
    let body = extract_tool_payload(&resp);
    let refs = body["references"]
        .as_array()
        .unwrap_or_else(|| panic!("expected references for a project `take`, got: {body}"));
    assert!(
        refs.iter()
            .any(|r| r["file_path"].as_str() == Some("src/b.rs")),
        "`take` must resolve to the project fn in src/b.rs, not to the std import \
         sentinel: {body}"
    );
    assert!(
        !refs
            .iter()
            .any(|r| r["file_path"].as_str() == Some("<external>")),
        "a name with a real project definition must not resolve to `<external>`: {body}"
    );

    // An earlier version added a `Debug` trait-sentinel block here labelled "the
    // live half of the control". Round 7 instrumented it: the payload is
    // byte-identical with and without the exclusion, because find_references
    // answers from EDGE rows, which never enter the by-name lookups the
    // exclusion filters — as `EXCLUDE_EXTERNAL_BY_NAME`'s own doc comment states.
    // It was the FOURTH inert control in this effort, so it is deleted rather
    // than relabelled: an inert block invites the next reader to trust it.
}

/// Review 2026-07-28: `module_overview`'s "obviously outside the root" guard
/// keyed on a colon at BYTE 1 with no separator requirement — the over-broad
/// predicate `src/cli.rs` copied and then fixed on its own side, leaving the two
/// surfaces disagreeing about the same name. `a:b.rs` is a legal POSIX filename;
/// `src/cli.rs`'s own unit test now asserts it must survive normalization, while
/// this entry refused it with "must be relative to the project root" — a
/// factually false answer about a file sitting in the root.
#[test]
fn mcp_module_overview_rejects_drive_roots_but_not_colon_filenames() {
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("lib.rs"), "pub fn anchor() -> u8 { 1 }\n").unwrap();

    // `:` is legal in a POSIX filename but forbidden by NTFS (it introduces an
    // alternate data stream), so the fixture — not just the expectation — is
    // Unix-only. On Windows the drive-root half below still runs.
    let colon_file = "a:b.rs";
    if !cfg!(windows) {
        std::fs::write(
            project.path().join(colon_file),
            "pub fn colon_named() -> u8 { 2 }\n",
        )
        .unwrap();
    }

    // Without a project marker the server comes up in non-project stub mode and
    // answers every tool call with "method not found", which would have made the
    // rejection assertions below pass for the wrong reason.
    std::fs::write(
        project.path().join("Cargo.toml"),
        "[package]\nname = \"colon_fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n",
    )
    .unwrap();

    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    let mut client = McpClient::spawn(project.path());

    let refused = |resp: &Value| -> bool {
        resp["result"]["isError"].as_bool() == Some(true)
            && resp["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or("")
                .contains("must be relative to the project root")
    };

    // Still refused: real drive roots and UNC roots, which no index key can name.
    for bad in ["C:/repo", r"D:\repo\src", "c:", r"\\server\share\src"] {
        let resp = client.call_tool("module_overview", json!({ "path": bad }));
        assert!(
            refused(&resp),
            "{bad:?} must still be refused as an absolute path; got: {resp}"
        );
    }

    // Not refused: ordinary relative paths whose second byte happens to be `:`.
    if !cfg!(windows) {
        let resp = client.call_tool("module_overview", json!({ "path": colon_file }));
        assert!(
            !refused(&resp),
            "{colon_file:?} is a real file in the project root — refusing it as \
             'outside the project root' is a false answer; got: {resp}"
        );
    }
}

/// Audit 2026-07-27 (incremental) Δ1: `get_ast_node` was the one of five tools
/// that did not treat an empty `file_path` as absent.
///
/// `trace`, `dependency_graph`, `find_similar_code`, `ast_search` and
/// `get_call_graph` all carry `.filter(|s| !s.trim().is_empty())`, and this same
/// function applies it to `symbol_name` forty lines below. Without it,
/// `{symbol_name: "target_fn", file_path: ""}` took the by-file branch and came
/// back "File '' not found" — a hard error for a request that names a real
/// symbol. An LLM client that fills every declared field with a placeholder hits
/// this on its first call.
#[test]
fn mcp_get_ast_node_treats_an_empty_file_path_as_absent() {
    let project = setup_fixture_project();
    let mut client = McpClient::spawn(project.path());

    let by_name = client.call_tool("get_ast_node", json!({ "symbol_name": "target_fn" }));

    for blank in ["", "   ", "\t"] {
        let resp = client.call_tool(
            "get_ast_node",
            json!({ "symbol_name": "target_fn", "file_path": blank }),
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            resp["result"]["isError"].as_bool() != Some(true),
            "file_path={blank:?} must behave like absent, not error: {text}"
        );
        assert!(
            !text.contains("File '' not found") && !text.contains("not found"),
            "file_path={blank:?} still took the by-file branch: {text}"
        );
        assert_eq!(
            extract_tool_payload(&resp),
            extract_tool_payload(&by_name),
            "file_path={blank:?} must return exactly what omitting it returns"
        );
    }

    // Negative control: a non-blank path that really is absent must STILL error.
    // Otherwise "treat blank as absent" could have been implemented by dropping
    // the by-file branch entirely and this test would not notice.
    let missing = client.call_tool(
        "get_ast_node",
        json!({ "symbol_name": "target_fn", "file_path": "src/no_such_file_xyz.rs" }),
    );
    let missing_text = missing["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    assert!(
        missing["result"]["isError"].as_bool() == Some(true) || missing_text.contains("not found"),
        "a real but absent file_path must still be reported: {missing_text}"
    );
}

/// A doubled separator in an MCP `file_path` used to WRITE a duplicate into the
/// index, not merely miss.
///
/// `tools::normalize_path_arg` unified `\` to `/` but left `//` alone, so
/// `src//a.ts` reached the freshness path as a key the index had never heard of
/// — and that path's answer to "unknown file" is to index it. Measured before
/// the fix: `files` went from `package.json | src/a.ts` to
/// `package.json | src//a.ts | src/a.ts`, and one `alpha` became two nodes, each
/// reporting a different `file_path` for the same source line.
///
/// The CLI-side fix for the same input (`dead-code src//` reporting a false
/// clean) was first written into `cli::normalize_user_path`, which left this
/// surface untouched — and this was the failing direction that mutates. The
/// collapse now lives in `merkle::normalize_rel_str_on`, the crate's single
/// separator-normalizing implementation.
#[test]
fn mcp_doubled_separator_in_file_path_does_not_duplicate_the_index() {
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a.ts"), "export function alpha(){ return 1; }\n").unwrap();
    std::fs::write(
        project.path().join("package.json"),
        "{\"name\":\"p\",\"version\":\"1.0.0\"}",
    )
    .unwrap();

    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db_path = db_dir.join("index.db");
    let db = code_graph_mcp::storage::db::Database::open(&db_path).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    let file_rows = |label: &str| -> Vec<String> {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let mut stmt = conn
            .prepare("SELECT path FROM files ORDER BY path")
            .unwrap();
        let rows: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert!(
            rows.iter().all(|p| !p.contains("//")),
            "{label}: a `//` key reached the files table: {rows:?}"
        );
        rows
    };
    let before = file_rows("before");

    let mut client = McpClient::spawn(project.path());
    let resp = client.call_tool(
        "get_ast_node",
        json!({ "symbol_name": "alpha", "file_path": "src//a.ts" }),
    );
    let body = extract_tool_payload(&resp);

    // It must answer, and answer with the CANONICAL path — echoing `src//a.ts`
    // back is how the duplicate row was visible from outside.
    let text = serde_json::to_string(&body).unwrap();
    assert!(
        text.contains("src/a.ts") && !text.contains("src//a.ts"),
        "the response must carry the canonical key: {text}"
    );

    drop(client);
    assert_eq!(
        file_rows("after"),
        before,
        "a doubled separator must not add a files row"
    );

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let alphas: i64 = conn
        .query_row("SELECT COUNT(*) FROM nodes WHERE name = 'alpha'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(alphas, 1, "one source symbol must stay one node");
}

/// MCP `ast_search` and CLI `ast-search` must return the SAME SYMBOLS for the
/// same query — data parity, not shape parity (shapes legitimately differ per
/// surface).
///
/// They did not: both fetched a fixed `limit * 4` FTS rows and filtered in
/// Rust, but only the MCP copy had a `name LIKE '%query%'` fallback for the
/// "FTS rank drowned the type" case. On a type-filtered query whose matches
/// ranked below the cut, MCP answered from the fallback while the CLI reported
/// zero — opposite answers, same index (audit 2026-08-16 P1-8). Both now call
/// `crate::search::ast_query::run`.
#[test]
fn mcp_and_cli_ast_search_agree_on_a_type_filtered_query() {
    // 40 `node_*` functions outrank 8 `Node*` structs in BM25, so the structs
    // sit below the old 20-row (limit 5 × 4) candidate cut.
    let project = TempDir::new().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let mut walk = String::new();
    for i in 0..40 {
        walk.push_str(&format!(
            "pub fn node_walk_{i:02}(node: &NodeRef) -> u32 {{\n    let node_id = node.node_id();\n    let node_depth = node.node_depth();\n    node_id + node_depth + node_id\n}}\n"
        ));
    }
    std::fs::write(src.join("walk.rs"), walk).unwrap();
    let mut types = String::new();
    for name in [
        "NodeAlpha",
        "NodeBravo",
        "NodeCharlie",
        "NodeDelta",
        "NodeEcho",
        "NodeFoxtrot",
        "NodeGolf",
        "NodeHotel",
    ] {
        types.push_str(&format!("pub struct {name} {{\n    pub id: u32,\n}}\n"));
    }
    std::fs::write(src.join("types.rs"), types).unwrap();
    std::fs::write(src.join("lib.rs"), "pub mod walk;\npub mod types;\n").unwrap();
    std::fs::write(
        project.path().join("Cargo.toml"),
        "[package]\nname = \"fixture_lib\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    drop(db);

    let names_of = |v: &Value| -> Vec<String> {
        let mut n: Vec<String> = v["results"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .map(|r| r["name"].as_str().unwrap_or("").to_string())
            .collect();
        n.sort();
        n
    };

    let mut client = McpClient::spawn(project.path());
    let mcp_body = extract_tool_payload(&client.call_tool(
        "ast_search",
        json!({"query": "node", "type": "struct", "limit": 5}),
    ));
    drop(client);

    let out = Command::new(binary_path())
        .current_dir(project.path())
        .args([
            "ast-search",
            "node",
            "--type",
            "struct",
            "--limit",
            "5",
            "--json",
        ])
        .output()
        .expect("run cli");
    let cli_body: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "cli --json must emit JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });

    let mcp_names = names_of(&mcp_body);
    let cli_names = names_of(&cli_body);
    assert_eq!(
        mcp_names.len(),
        5,
        "MCP must return 5 of the 8 matching structs: {mcp_body}"
    );
    assert_eq!(
        mcp_names, cli_names,
        "the two surfaces must name the same symbols.\nMCP: {mcp_body}\nCLI: {cli_body}"
    );
    // And both must disclose that the answer was cut, with the same count.
    assert_eq!(
        mcp_body["matched_total"], cli_body["matched_total"],
        "matched_total must agree: MCP {mcp_body} CLI {cli_body}"
    );
    assert_eq!(mcp_body["truncated"], json!(true), "{mcp_body}");
    assert_eq!(cli_body["truncated"], json!(true), "{cli_body}");
}
