//! P1 #4 — an empty caller result discloses the dynamic-dispatch sites that
//! name the symbol, on every surface that can answer "who calls it":
//! CLI `callgraph` / `impact` / `refs` (text + `--json`) and MCP
//! `get_call_graph` / `find_references` / `get_ast_node include_impact`.
//!
//! The fixture has one symbol of each kind:
//! * `on_save` — reached only through `getattr(ctrl, "on_save")` (one site;
//!   a comment and a test file mention it too, and must not count);
//! * `fire` — registered as a callback on 7 lines (exercises the 5-site cap);
//! * `dead_helper` — nothing names it (the explicit "none found" line);
//! * `direct_callee` — an ordinary static callee: its outputs are pinned
//!   byte-for-byte to what the binary printed before this change.

mod common;

use std::process::Command;

use tempfile::TempDir;

const CONTROLLER_PY: &str = r#"class Controller:
    def on_save(self, doc):
        return doc


def route(ctrl, doc):
    # getattr(ctrl, "on_save") in a comment is not a site
    return getattr(ctrl, "on_save")(doc)


def dead_helper():
    return 1


def direct_callee():
    return 2


def caller():
    return direct_callee()
"#;

const BUS_JS: &str = "export function fire(evt) { return evt; }
bus.on('a', fire);
bus.on('b', fire);
bus.on('c', fire);
bus.on('d', fire);
bus.on('e', fire);
bus.on('f', fire);
bus.on('g', fire);
";

const TEST_PY: &str = "def test_dispatch(c):\n    getattr(c, \"on_save\")(1)\n";

fn write_fixture(root: &std::path::Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("tests")).unwrap();
    std::fs::write(root.join("src/controller.py"), CONTROLLER_PY).unwrap();
    std::fs::write(root.join("src/bus.js"), BUS_JS).unwrap();
    std::fs::write(root.join("tests/test_controller.py"), TEST_PY).unwrap();
}

fn indexed_fixture() -> TempDir {
    let project = TempDir::new().unwrap();
    write_fixture(project.path());
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, project.path(), None, None).unwrap();
    project
}

fn cli(project: &TempDir, args: &[&str]) -> (String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_code-graph-mcp"))
        .current_dir(project.path())
        .args(args)
        .output()
        .expect("run binary");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

fn cli_json(project: &TempDir, args: &[&str]) -> serde_json::Value {
    let (out, code) = cli(project, args);
    assert_eq!(code, 0, "{args:?} exited {code}: {out}");
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("{args:?}: {e}\n{out}"))
}

const ON_SAVE_BLOCK: &str = "  1 dynamic-dispatch site(s) name 'on_save' (not graph edges):
    src/controller.py:8  reflection (getattr)
    next: code-graph-mcp grep -w -F on_save
";

#[test]
fn cli_text_appends_the_block_after_each_empty_result() {
    let p = indexed_fixture();
    let (out, code) = cli(&p, &["callgraph", "on_save"]);
    assert_eq!(code, 0);
    assert_eq!(out, format!("on_save (src/controller.py)\n{ON_SAVE_BLOCK}"));

    let (out, _) = cli(&p, &["refs", "on_save"]);
    assert_eq!(
        out,
        format!("No references found for 'on_save'.\n{ON_SAVE_BLOCK}")
    );

    // `on_save` is called only through `getattr`: no resolved caller, so the
    // risk is UNKNOWN, never a LOW that would endorse the change.
    let (out, _) = cli(&p, &["impact", "on_save"]);
    assert_eq!(
        out,
        format!(
            "Impact: on_save — Risk: UNKNOWN\n  (warning: {})\n  0 direct, 0 callers total, 0 files, 0 routes (0 tests affected)\n{ON_SAVE_BLOCK}",
            code_graph_mcp::domain::NO_CALLERS_IMPACT_WARNING
        )
    );
}

fn assert_on_save_boundaries(b: &serde_json::Value, surface: &str) {
    assert_eq!(b["total"], 1, "{surface}: {b}");
    let sites = b["sites"].as_array().unwrap();
    assert_eq!(sites.len(), 1, "{surface}: {b}");
    assert_eq!(sites[0]["file_path"], "src/controller.py", "{surface}");
    assert_eq!(sites[0]["line"], 8, "{surface}");
    assert_eq!(sites[0]["shape"], "reflection", "{surface}");
    assert_eq!(sites[0]["via"], "getattr", "{surface}");
    assert_eq!(b["next"], "code-graph-mcp grep -w -F on_save", "{surface}");
}

#[test]
fn cli_json_carries_an_additive_boundaries_field() {
    let p = indexed_fixture();
    let v = cli_json(&p, &["callgraph", "on_save", "--json"]);
    assert_eq!(v["results"], serde_json::json!([]));
    assert_on_save_boundaries(&v["boundaries"], "callgraph --json");
    let v = cli_json(&p, &["refs", "on_save", "--json"]);
    assert_eq!(v["total_references"], 0);
    assert_on_save_boundaries(&v["boundaries"], "refs --json");
    let v = cli_json(&p, &["impact", "on_save", "--json"]);
    assert_eq!(v["total_callers"], 0);
    assert_on_save_boundaries(&v["boundaries"], "impact --json");
}

/// Comment mentions and test files are look-alikes: the fixture has one of
/// each for `on_save`, and neither is a site (only line 8 of the controller).
#[test]
fn comments_and_test_files_are_not_sites() {
    let p = indexed_fixture();
    let v = cli_json(&p, &["callgraph", "on_save", "--json"]);
    let files: Vec<String> = v["boundaries"]["sites"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| format!("{}:{}", s["file_path"].as_str().unwrap(), s["line"]))
        .collect();
    assert_eq!(files, vec!["src/controller.py:8".to_string()]);
}

#[test]
fn sites_are_capped_at_five_and_the_rest_counted() {
    let p = indexed_fixture();
    let (out, _) = cli(&p, &["callgraph", "fire"]);
    assert_eq!(
        out,
        "fire (src/bus.js)
  7 dynamic-dispatch site(s) name 'fire' (not graph edges):
    src/bus.js:2  function reference
    src/bus.js:3  function reference
    src/bus.js:4  function reference
    src/bus.js:5  function reference
    src/bus.js:6  function reference
    … 2 more
    next: code-graph-mcp grep -w -F fire
"
    );
    let v = cli_json(&p, &["callgraph", "fire", "--json"]);
    assert_eq!(v["boundaries"]["total"], 7);
    assert_eq!(v["boundaries"]["sites"].as_array().unwrap().len(), 5);
}

#[test]
fn a_symbol_nothing_names_gets_one_short_line() {
    let p = indexed_fixture();
    let (out, _) = cli(&p, &["callgraph", "dead_helper"]);
    assert_eq!(
        out,
        "dead_helper (src/controller.py)\n  (no dynamic-dispatch site names 'dead_helper')\n"
    );
    let v = cli_json(&p, &["callgraph", "dead_helper", "--json"]);
    assert_eq!(
        v["boundaries"],
        serde_json::json!({"sites": [], "total": 0})
    );
}

/// The disclosure answers "who calls it", so it stays off answers to other
/// questions: callees only, an `imports`-filtered refs, a non-function symbol.
#[test]
fn the_field_is_absent_where_the_question_is_not_who_calls_it() {
    let p = indexed_fixture();
    let v = cli_json(
        &p,
        &["callgraph", "on_save", "--direction", "callees", "--json"],
    );
    assert!(v.get("boundaries").is_none(), "callees-only: {v}");
    let v = cli_json(&p, &["refs", "on_save", "--relation", "imports", "--json"]);
    assert!(v.get("boundaries").is_none(), "--relation imports: {v}");
    let v = cli_json(&p, &["refs", "Controller", "--json"]);
    assert_eq!(v["total_references"], 0, "precondition: {v}");
    assert!(v.get("boundaries").is_none(), "class: {v}");
}

/// Non-empty results are unchanged: these literals are the outputs of the
/// binary BEFORE this change (captured from HEAD b1539ff on this fixture).
#[test]
fn non_empty_results_are_byte_identical_to_before() {
    let p = indexed_fixture();
    let cases: [(&[&str], &str); 6] = [
        (
            &["callgraph", "direct_callee"],
            "direct_callee (src/controller.py)\n  ← called by: caller (src/controller.py) [function]\n",
        ),
        (
            &["callgraph", "direct_callee", "--json"],
            "{\"results\":[{\"depth\":1,\"direction\":\"callers\",\"file_path\":\"src/controller.py\",\"name\":\"caller\",\"node_id\":NID,\"parent_id\":PID,\"type\":\"function\"}],\"symbol\":\"direct_callee\"}\n",
        ),
        (
            &["impact", "direct_callee"],
            "Impact: direct_callee — Risk: LOW\n  1 direct, 1 caller total, 1 file, 0 routes (0 tests affected)\nCallers:\n  caller  (function) src/controller.py\n",
        ),
        (
            &["impact", "direct_callee", "--json"],
            "{\"affected_files\":1,\"affected_routes\":0,\"callers\":[{\"depth\":1,\"file\":\"src/controller.py\",\"name\":\"caller\",\"route\":null,\"type\":\"function\"}],\"direct_callers\":1,\"risk\":\"LOW\",\"symbol\":\"direct_callee\",\"test_callers\":[],\"tests_affected\":0,\"total_callers\":1,\"value_references\":0}\n",
        ),
        (
            &["refs", "direct_callee"],
            "1 references to 'direct_callee':\n  [calls] caller (src/controller.py:19)\n",
        ),
        (
            &["refs", "direct_callee", "--json"],
            "{\n  \"by_relation\": {\n    \"calls\": 1\n  },\n  \"references\": [\n    {\n      \"confidence\": \"extracted\",\n      \"file_path\": \"src/controller.py\",\n      \"name\": \"caller\",\n      \"node_id\": NID,\n      \"relation\": \"calls\",\n      \"start_line\": 19,\n      \"type\": \"function\"\n    }\n  ],\n  \"symbol\": \"direct_callee\",\n  \"total_references\": 1\n}\n",
        ),
    ];
    // Node ids depend on insertion order, not on this change: substitute the
    // live ones so the pin covers every other byte.
    let db = code_graph_mcp::storage::db::Database::open(
        &p.path()
            .join(code_graph_mcp::domain::CODE_GRAPH_DIR)
            .join("index.db"),
    )
    .unwrap();
    let id = |name: &str| -> String {
        db.conn()
            .query_row("SELECT id FROM nodes WHERE name = ?1", [name], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
            .to_string()
    };
    let (nid, pid) = (id("caller"), id("direct_callee"));
    for (args, want) in cases {
        let (out, code) = cli(&p, args);
        assert_eq!(code, 0, "{args:?}");
        let want = want.replace("NID", &nid).replace("PID", &pid);
        assert_eq!(out, want, "{args:?} changed");
    }
}

/// The printed next step runs, and finds the site (plus the comment and test
/// file the disclosure deliberately left out — it is the superset).
#[test]
fn the_next_command_runs() {
    let present = Command::new("rg").arg("--version").output().is_ok();
    assert!(
        present || std::env::var_os("CI").is_none(),
        "ripgrep must be installed on CI"
    );
    if !present {
        eprintln!("skipping: rg not installed");
        return;
    }
    let p = indexed_fixture();
    let (out, code) = cli(&p, &["grep", "-w", "-F", "on_save"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("src/controller.py:8"), "{out}");
    assert!(out.contains("tests/test_controller.py"), "{out}");
}

// ---- MCP ------------------------------------------------------------------

fn mcp_call(
    server: &code_graph_mcp::mcp::server::McpServer,
    tool: &str,
    args: serde_json::Value,
) -> serde_json::Value {
    let req = common::tool_call_json(tool, args);
    common::parse_tool_result(&server.handle_message(&req).unwrap())
}

#[test]
fn mcp_tools_carry_the_same_field() {
    let p = TempDir::new().unwrap();
    write_fixture(p.path());
    let server = common::init_server(&p);

    let v = mcp_call(
        &server,
        "get_call_graph",
        serde_json::json!({"symbol_name": "on_save"}),
    );
    assert_eq!(v["callers"], serde_json::json!([]), "{v}");
    assert_on_save_boundaries(&v["boundaries"], "get_call_graph");

    let v = mcp_call(
        &server,
        "find_references",
        serde_json::json!({"symbol_name": "on_save"}),
    );
    assert_eq!(v["total_references"], 0, "{v}");
    assert_on_save_boundaries(&v["boundaries"], "find_references");

    let v = mcp_call(
        &server,
        "get_ast_node",
        serde_json::json!({"symbol_name": "on_save", "include_impact": true}),
    );
    assert_on_save_boundaries(&v["impact"]["boundaries"], "get_ast_node impact");

    let v = mcp_call(
        &server,
        "find_references",
        serde_json::json!({"symbol_name": "on_save", "relation": "imports"}),
    );
    assert_eq!(v["total_references"], 0, "precondition: {v}");
    assert!(v.get("boundaries").is_none(), "relation imports: {v}");

    let v = mcp_call(
        &server,
        "get_call_graph",
        serde_json::json!({"symbol_name": "dead_helper"}),
    );
    assert_eq!(
        v["boundaries"],
        serde_json::json!({"sites": [], "total": 0}),
        "{v}"
    );

    // A called symbol: no field on any surface.
    let v = mcp_call(
        &server,
        "get_call_graph",
        serde_json::json!({"symbol_name": "direct_callee"}),
    );
    assert!(v.get("boundaries").is_none(), "{v}");
    let v = mcp_call(
        &server,
        "find_references",
        serde_json::json!({"symbol_name": "direct_callee"}),
    );
    assert!(v.get("boundaries").is_none(), "{v}");
    let v = mcp_call(
        &server,
        "get_ast_node",
        serde_json::json!({"symbol_name": "direct_callee", "include_impact": true}),
    );
    assert!(v["impact"].get("boundaries").is_none(), "{v}");
    let v = mcp_call(
        &server,
        "get_call_graph",
        serde_json::json!({"symbol_name": "on_save", "direction": "callees"}),
    );
    assert!(v.get("boundaries").is_none(), "{v}");
}

/// The dense-graph arm of `get_call_graph` builds its payload separately
/// (file-level rollup); an empty caller side there owes the field too.
#[test]
fn mcp_rollup_arm_carries_the_field() {
    let p = TempDir::new().unwrap();
    let mut code = String::from("function hub() {\n");
    for i in 0..150 {
        code.push_str(&format!("  callee_{i}();\n"));
    }
    code.push_str("}\n");
    for i in 0..150 {
        code.push_str(&format!("function callee_{i}() {{}}\n"));
    }
    code.push_str("bus.on('tick', hub);\n");
    std::fs::write(p.path().join("dense.js"), &code).unwrap();
    let server = common::init_server(&p);
    let v = mcp_call(
        &server,
        "get_call_graph",
        serde_json::json!({"symbol_name": "hub", "depth": 1}),
    );
    assert_eq!(v["mode"], "rollup_call_graph", "precondition: {v}");
    assert_eq!(v["callers"]["total_count"], 0, "precondition: {v}");
    assert_eq!(v["boundaries"]["total"], 1, "{v}");
    assert_eq!(v["boundaries"]["sites"][0]["line"], 303, "{v}");
    assert_eq!(
        v["boundaries"]["sites"][0]["shape"], "function_reference",
        "{v}"
    );
    // A callees-only question gets no field on this arm either.
    let v = mcp_call(
        &server,
        "get_call_graph",
        serde_json::json!({"symbol_name": "hub", "depth": 1, "direction": "callees"}),
    );
    assert_eq!(v["mode"], "rollup_call_graph", "precondition: {v}");
    assert!(v.get("boundaries").is_none(), "{v}");
}

/// A bash function is never scanned (bash has no shape table), and bash is
/// exactly where names are dispatched as words (`trap cleanup EXIT`). The
/// answer says the file was not scanned instead of the complete "none".
#[test]
fn a_definition_in_an_unscanned_language_says_so() {
    let p = TempDir::new().unwrap();
    std::fs::write(
        p.path().join("run.sh"),
        "#!/bin/bash\ncleanup() {\n  rm -f \"$TMPF\"\n}\ntrap cleanup EXIT\n",
    )
    .unwrap();
    let db_dir = p.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, p.path(), None, None).unwrap();
    let block = "  (no dynamic-dispatch site names 'cleanup' in the files scanned; not scanned: bash files)\n    next: code-graph-mcp grep -w -F cleanup\n";
    for cmd in ["callgraph", "refs", "impact"] {
        let (out, code) = cli(&p, &[cmd, "cleanup"]);
        assert_eq!(code, 0, "{cmd}: {out}");
        assert!(
            !out.contains("(no dynamic-dispatch site names 'cleanup')"),
            "{cmd}: {out}"
        );
        assert!(out.ends_with(block), "{cmd}: {out}");
    }
    let v = cli_json(&p, &["callgraph", "cleanup", "--json"]);
    assert_eq!(
        v["boundaries"]["not_scanned"],
        serde_json::json!({"languages": ["bash"]}),
        "{v}"
    );
}

/// Pre-tag review H1: a Rust `Q::name` value whose `Q` the scan cannot tie
/// to a definition (the crate's own name, an inline `mod`, a `use … as`
/// alias, a trait, a generic or qualified-self path) is not reported as a
/// site, so the answer must not claim that no site exists either.
#[test]
fn a_rust_path_through_an_unknown_qualifier_is_not_answered_none() {
    let p = TempDir::new().unwrap();
    let root = p.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"myapp\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        "pub mod handlers;
pub fn register(f: fn()) {}
pub fn on_tick() {}
mod inner {
    pub fn on_ping() {}
}
pub trait Store {
    fn persist(&self);
    fn flush(&self);
}
pub struct Db;
impl Store for Db {
    fn persist(&self) {}
    fn flush(&self) {}
}
pub struct Wrapper<T>(T);
impl<T> Wrapper<T> {
    pub fn keep(&self) {}
}
pub fn lonely() {}
pub fn setup(v: Vec<Db>, w: Vec<Wrapper<u8>>) {
    register(inner::on_ping);
    v.iter().for_each(Store::persist);
    v.iter().for_each(<Db as Store>::flush);
    w.iter().for_each(Wrapper::<u8>::keep);
}
",
    )
    .unwrap();
    std::fs::write(
        root.join("src/handlers.rs"),
        "pub fn on_load() {}\npub fn on_save() {}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        "use myapp::handlers as h;
fn main() {
    myapp::register(myapp::on_tick);
    myapp::register(h::on_load);
    myapp::register(myapp::handlers::on_save);
}
",
    )
    .unwrap();
    let db_dir = root.join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = code_graph_mcp::storage::db::Database::open(&db_dir.join("index.db")).unwrap();
    code_graph_mcp::indexer::pipeline::run_full_index(&db, root, None, None).unwrap();

    for name in ["on_tick", "on_ping", "on_load", "persist", "flush", "keep"] {
        let (out, code) = cli(&p, &["callgraph", name]);
        assert_eq!(code, 0, "{name}: {out}");
        assert!(
            !out.contains(&format!("(no dynamic-dispatch site names '{name}')")),
            "{name}: {out}"
        );
        assert!(
            out.ends_with(&format!(
                "  (no dynamic-dispatch site or call names '{name}' in the files scanned; not scanned: 1 Rust path with an unrecognized qualifier)\n    next: code-graph-mcp grep -w -F {name}\n"
            )),
            "{name}: {out}"
        );
        let v = cli_json(&p, &["callgraph", name, "--json"]);
        assert_eq!(
            v["boundaries"]["not_scanned"],
            serde_json::json!({"unresolved_paths": 1}),
            "{name}: {v}"
        );
    }
    // A qualifier that names the definition's module still reports the site.
    let (out, _) = cli(&p, &["callgraph", "on_save"]);
    assert!(
        out.contains("1 dynamic-dispatch site(s) name 'on_save'") && out.contains("src/main.rs:5"),
        "{out}"
    );
    assert!(!out.contains("not scanned"), "{out}");
    // A Rust function nothing names keeps the complete line.
    let (out, _) = cli(&p, &["callgraph", "lonely"]);
    assert!(
        out.ends_with("  (no dynamic-dispatch site or call names 'lonely')\n"),
        "{out}"
    );
    let v = cli_json(&p, &["callgraph", "lonely", "--json"]);
    assert_eq!(
        v["boundaries"],
        serde_json::json!({"sites": [], "total": 0, "unresolved_calls": {"total": 0}})
    );
}
