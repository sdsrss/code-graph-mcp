//! Production hardening tests: concurrency, stress, and edge-case scenarios.
//!
//! McpServer wraps a raw rusqlite::Connection which is Send but not Sync,
//! so concurrent tests use Arc<Mutex<McpServer>> to validate that interleaved
//! access from multiple threads causes no deadlocks or data corruption.

mod common;

use code_graph_mcp::mcp::server::McpServer;
use serde_json::json;
use std::fs;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

use common::{init_server, parse_tool_result, tool_call_json};

fn setup_project(file_count: usize) -> (TempDir, McpServer) {
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();

    for i in 0..file_count {
        let content = format!(
            "export function func_{}(x: number): number {{ return x + {}; }}\n\
             export function helper_{}(): string {{ return 'hello'; }}\n",
            i, i, i
        );
        fs::write(project.path().join(format!("src/mod_{}.ts", i)), content).unwrap();
    }

    let server = init_server(&project);

    // Trigger initial indexing
    let search = tool_call_json("semantic_code_search", json!({"query": "func_0"}));
    let _ = server.handle_message(&search).unwrap();

    (project, server)
}

/// Multi-threaded search calls from 10 threads against a Mutex-wrapped McpServer.
/// Access is serialized by the mutex (McpServer is Send but not Sync).
/// Validates no panics or mutex poisoning under multi-threaded scheduling.
#[test]
fn test_concurrent_tool_calls() {
    let (_project, server) = setup_project(20);
    let server = Arc::new(Mutex::new(server));

    let handles: Vec<_> = (0..10)
        .map(|i| {
            let srv = Arc::clone(&server);
            std::thread::spawn(move || {
                let msg = tool_call_json(
                    "semantic_code_search",
                    json!({"query": format!("func_{}", i)}),
                );
                let resp = srv.lock().unwrap().handle_message(&msg).unwrap();
                assert!(resp.is_some(), "thread {} got no response", i);
                let v: serde_json::Value = serde_json::from_str(resp.as_ref().unwrap()).unwrap();
                assert!(
                    v.get("result").is_some(),
                    "thread {} got no result: {:?}",
                    i,
                    v
                );
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread panicked");
    }
}

/// Stress test: index 200 files and verify all are tracked.
#[test]
fn test_large_repo_indexing() {
    let (_project, server) = setup_project(200);

    let msg = tool_call_json("get_index_status", json!({}));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);

    let files = result["files_count"].as_i64().unwrap();
    assert!(
        files >= 200,
        "should index at least 200 files, got {}",
        files
    );
}

/// Mixed tool calls (search, status, project_map) from 20 threads.
/// Tests that different tool handlers don't interfere with each other.
#[test]
fn test_concurrent_mixed_tool_calls() {
    let (_project, server) = setup_project(50);
    let server = Arc::new(Mutex::new(server));

    let handles: Vec<_> = (0..20)
        .map(|i| {
            let srv = Arc::clone(&server);
            std::thread::spawn(move || {
                let msg = if i % 3 == 0 {
                    tool_call_json(
                        "semantic_code_search",
                        json!({"query": format!("func_{}", i)}),
                    )
                } else if i % 3 == 1 {
                    tool_call_json("get_index_status", json!({}))
                } else {
                    tool_call_json("project_map", json!({}))
                };
                let resp = srv.lock().unwrap().handle_message(&msg).unwrap();
                assert!(resp.is_some(), "thread {} got no response", i);
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread panicked during concurrent access");
    }
}

/// All query tools should return gracefully on a completely empty project.
#[test]
fn test_empty_project_graceful() {
    let project = TempDir::new().unwrap();
    let server = init_server(&project);

    let tools = vec![
        ("semantic_code_search", json!({"query": "anything"})),
        ("project_map", json!({})),
        ("get_index_status", json!({})),
    ];
    for (name, args) in tools {
        let msg = tool_call_json(name, args);
        let resp = server.handle_message(&msg).unwrap();
        assert!(
            resp.is_some(),
            "{} should return response on empty project",
            name
        );
    }
}

/// Binary garbage and zero-byte files with recognized extensions
/// should not crash the indexer; valid files alongside them should still index.
#[test]
fn test_binary_files_dont_crash_indexing() {
    let project = TempDir::new().unwrap();
    // Create a valid file alongside binary garbage
    fs::write(
        project.path().join("valid.ts"),
        "export function hello(): string { return 'world'; }",
    )
    .unwrap();
    // Binary file with .ts extension
    fs::write(
        project.path().join("broken.ts"),
        [0xFF, 0xFE, 0x00, 0x01, 0xFF, 0xFE],
    )
    .unwrap();
    // Zero-byte file
    fs::write(project.path().join("empty.ts"), "").unwrap();

    let server = init_server(&project);

    // Should not crash — valid file should still be indexed
    let msg = tool_call_json("semantic_code_search", json!({"query": "hello"}));
    let resp = server.handle_message(&msg).unwrap();
    assert!(
        resp.is_some(),
        "should return response even with broken files"
    );
}

/// Re-indexing the same files multiple times should not duplicate nodes.
#[test]
fn test_repeated_indexing_is_idempotent() {
    let project = TempDir::new().unwrap();
    fs::write(
        project.path().join("main.ts"),
        "export function main() { return 42; }",
    )
    .unwrap();

    let server = init_server(&project);

    // Index multiple times via different tool calls
    for _ in 0..3 {
        let msg = tool_call_json("semantic_code_search", json!({"query": "main"}));
        let resp = server.handle_message(&msg).unwrap();
        assert!(resp.is_some());
    }

    // Verify node count didn't multiply
    let msg = tool_call_json("get_index_status", json!({}));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    let nodes = result["nodes_count"].as_i64().unwrap();
    // Should have a reasonable number of nodes, not 3x duplicates
    assert!(
        nodes < 50,
        "nodes should not multiply with repeated indexing, got {}",
        nodes
    );
}

/// Drift-guard: every name in `cli::usage::CG_QUERY_TOOLS` must be a tool
/// `McpServer::dispatch_tool` actually answers.
///
/// The list drives the recommend→use funnel: a session counts as "converted" if
/// it called one of these. A name that no tool emits can never match a
/// usage.jsonl key, so it contributes nothing and nothing complains —
/// `impact_analysis` sat there as dead configuration for ~100 releases after the
/// tool was removed (audit 2026-08-16 review Minor tail). The failure mode is
/// silent under-counting, which is exactly what this list exists to prevent.
#[test]
fn cg_query_tools_are_all_dispatchable() {
    let dispatch = fs::read_to_string("src/mcp/server/mod.rs").unwrap();
    let start = dispatch
        .find("fn dispatch_tool(")
        .expect("dispatch_tool moved — update this guard");
    let region = &dispatch[start..];
    let end = region
        .find("\n    }\n")
        .expect("could not find the end of dispatch_tool");
    let region = &region[..end];

    let arm = |name: &str| region.contains(&format!("\"{name}\""));

    let missing: Vec<&str> = code_graph_mcp::cli::usage::CG_QUERY_TOOLS
        .iter()
        .copied()
        .filter(|name| !arm(name))
        .collect();
    assert!(
        missing.is_empty(),
        "CG_QUERY_TOOLS names {missing:?} have no arm in McpServer::dispatch_tool — they can never \
         match a usage.jsonl key, so the recommend→use funnel silently under-counts. Remove them, \
         or add the dispatch arm."
    );
    // Negative control: an all-green list proves nothing unless the scan can
    // still miss something. A name no arm mentions must be reported.
    assert!(
        !arm("code_graph_no_such_tool"),
        "the dispatch-arm scan matches anything — the guard above is vacuous"
    );
    assert!(
        arm("find_dead_code"),
        "the dispatch-arm scan matches nothing"
    );
}

/// Strip line comments AND string-literal contents from one line of Rust, so the
/// layering scanner below sees only real code.
///
/// Both strips are load-bearing and were learned from live false positives:
/// a doc comment naming `crate::graph::routes` explains where an orchestration
/// moved TO, and `src/parser/relations/tests.rs` embeds fixture source such as
/// `"fn caller() { crate::snapshot::create(); }"` — neither is an import. A
/// single pass handles both so a `//` inside a string cannot truncate the line
/// early and hide an offender behind it.
fn code_only(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            if c == b'\\' {
                i += 2; // skip the escaped char, incl. an escaped quote
                continue;
            }
            if c == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == b'"' {
            in_string = true;
            i += 1;
            continue;
        }
        if c == b'/' && bytes.get(i + 1) == Some(&b'/') {
            break; // line comment (incl. `///` doc comments)
        }
        out.push(c as char);
        i += 1;
    }
    out
}

/// Does this line reference `crate::<module>` as real code?
///
/// The module name must END where it is written: a bare `starts_with` made
/// `use crate::clippy_helper::x;` an offender for the `cli` row (caught by the
/// negative control below, not by review).
fn references_module(line: &str, module: &str) -> bool {
    let code = code_only(line);
    let t = code.trim_start();
    // `crate::<module>::…` — a path INTO the module.
    if t.contains(&format!("crate::{module}::")) {
        return true;
    }
    // `use crate::<module>;` / `use crate::<module> as x;` — the module itself.
    let Some(rest) = t.strip_prefix(&format!("use crate::{module}")) else {
        return false;
    };
    matches!(rest.chars().next(), None | Some(';') | Some(' '))
}

/// Forbidden module-dependency edges — `(scanned path, module it must not reach, why)`.
///
/// The module graph is deliberately near-perfectly one-directional, and the 2026-08-16
/// audit found the single-edge guard that used to live here (`storage → graph` only)
/// had let three more upward edges grow back unnoticed: `cli → mcp::server` (borrowing
/// index-lock infrastructure), `outcome → cli` (borrowing generic helpers) and
/// `storage → search` (borrowing the tokenizer). All three were relocated downward —
/// `indexer::lock`, `utils::{paths,telemetry}`, `utils::{tokenizer,acronyms}` — and the
/// guard generalized to a table so the NEXT one fails a test instead of an audit.
///
/// Every row must be a genuinely dead edge at the time it is added; `cargo test` is
/// the only thing that decides whether that is still true.
const FORBIDDEN_EDGES: &[(&str, &str, &str)] = &[
    // storage is a leaf above domain: graph/search/mcp/cli all depend on IT.
    (
        "src/storage",
        "graph",
        "graph depends on storage, not the reverse",
    ),
    (
        "src/storage",
        "search",
        "query building must not reach up into the search layer (tokenizer/acronyms live in utils)",
    ),
    ("src/storage", "mcp", "storage is protocol-agnostic"),
    ("src/storage", "cli", "storage is surface-agnostic"),
    ("src/storage", "outcome", "storage is surface-agnostic"),
    // The two published surfaces must not borrow from each other.
    (
        "src/cli",
        "mcp",
        "CLI must not borrow MCP-server internals — shared index-lock infra lives in indexer::lock",
    ),
    (
        "src/outcome.rs",
        "cli",
        "outcome must not borrow CLI internals — shared helpers live in utils",
    ),
    ("src/outcome.rs", "mcp", "outcome is surface-agnostic"),
    // utils and domain are leaves: everything may depend on them, they on nothing.
    ("src/utils", "cli", "utils is a leaf"),
    ("src/utils", "mcp", "utils is a leaf"),
    ("src/utils", "outcome", "utils is a leaf"),
    ("src/utils", "storage", "utils is a leaf"),
    ("src/utils", "graph", "utils is a leaf"),
    ("src/utils", "search", "utils is a leaf"),
    ("src/utils", "indexer", "utils is a leaf"),
    ("src/domain.rs", "cli", "domain is the bottom layer"),
    ("src/domain.rs", "mcp", "domain is the bottom layer"),
    ("src/domain.rs", "storage", "domain is the bottom layer"),
    ("src/domain.rs", "indexer", "domain is the bottom layer"),
    // graph/indexer/parser sit below both surfaces.
    ("src/graph", "mcp", "graph is protocol-agnostic"),
    ("src/graph", "cli", "graph is surface-agnostic"),
    ("src/graph", "outcome", "graph is surface-agnostic"),
    ("src/indexer", "mcp", "indexer is protocol-agnostic"),
    ("src/indexer", "cli", "indexer is surface-agnostic"),
    ("src/indexer", "outcome", "indexer is surface-agnostic"),
    (
        "src/indexer",
        "graph",
        "graph reads the index, not the reverse",
    ),
    ("src/parser", "mcp", "parser is protocol-agnostic"),
    ("src/parser", "cli", "parser is surface-agnostic"),
    (
        "src/parser",
        "storage",
        "parser produces records, it does not persist them",
    ),
    ("src/parser", "graph", "parser is below the graph layer"),
    (
        "src/parser",
        "indexer",
        "the indexer drives the parser, not the reverse",
    ),
    // ── The six module roots that were never a SCANNED side ───────────────
    // Added 2026-08-22 (audit P2-7). The table only ever constrained eight of
    // the fourteen module roots, so `mcp → cli` and `resolve → cli/mcp` — dead
    // edges of exactly the class this table exists to catch — had no guard at
    // all. Rows are the ones `cargo test` confirms are dead today; the three
    // probed pairs that are LIVE (`resolve.rs → indexer`, `sandbox → storage`,
    // `search → indexer`) are deliberately absent and noted below.
    //
    // mcp is a top surface, peer of cli.
    (
        "src/mcp",
        "cli",
        "the two published surfaces must not borrow from each other — shared resolution lives in resolve.rs, shared paths in utils",
    ),
    ("src/mcp", "outcome", "outcome reads transcripts, surfaces consume it through their own commands"),
    // search sits below both surfaces, above storage.
    ("src/search", "cli", "search is surface-agnostic"),
    ("src/search", "mcp", "search is protocol-agnostic"),
    ("src/search", "outcome", "search is surface-agnostic"),
    ("src/search", "graph", "search ranks rows; graph traversal is a sibling layer"),
    // snapshot packages an existing index; it is driven BY the surfaces.
    ("src/snapshot", "cli", "snapshot is surface-agnostic"),
    ("src/snapshot", "mcp", "snapshot is protocol-agnostic"),
    ("src/snapshot", "outcome", "snapshot is surface-agnostic"),
    ("src/snapshot", "search", "snapshot moves bytes, it does not query"),
    ("src/snapshot", "graph", "snapshot moves bytes, it does not traverse"),
    // sandbox is a pure text/token compressor.
    ("src/sandbox", "cli", "sandbox is surface-agnostic"),
    ("src/sandbox", "mcp", "sandbox is protocol-agnostic"),
    ("src/sandbox", "outcome", "sandbox is surface-agnostic"),
    ("src/sandbox", "indexer", "compression reads what it is handed, it does not index"),
    // embedding owns the model and the context string; callers hand it rows.
    ("src/embedding", "cli", "embedding is surface-agnostic"),
    ("src/embedding", "mcp", "embedding is protocol-agnostic"),
    ("src/embedding", "outcome", "embedding is surface-agnostic"),
    ("src/embedding", "storage", "the indexer persists vectors; embedding only produces them"),
    ("src/embedding", "indexer", "the indexer drives embedding, not the reverse"),
    ("src/embedding", "search", "embedding produces vectors; ranking them is search's job"),
    // resolve.rs is the shared symbol resolver BOTH surfaces read, so it must
    // not know either of them — that is the whole point of hoisting it.
    (
        "src/resolve.rs",
        "cli",
        "resolve is what the surfaces share; reaching back into one of them re-splits the verdict",
    ),
    ("src/resolve.rs", "mcp", "resolve is protocol-agnostic"),
    ("src/resolve.rs", "search", "resolve names symbols; ranking is a separate layer"),
    ("src/resolve.rs", "graph", "resolve names symbols; traversal is a separate layer"),
    // NOT rows, and why: `resolve.rs → indexer` (query-time freshness calls
    // `ensure_file_indexed`), `search → indexer` (same), and
    // `sandbox → storage` (reads node rows to compress) are all LIVE and
    // downward. Listing a live edge here would make this test red on arrival,
    // which is the one thing the table must never do.
];

/// Layering drift-guard, table form. See [`FORBIDDEN_EDGES`].
#[test]
fn no_forbidden_module_dependency_edges() {
    use std::fs;
    use std::path::Path;

    fn walk(path: &Path, out: &mut Vec<std::path::PathBuf>) {
        if path.is_file() {
            if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path.to_path_buf());
            }
            return;
        }
        for entry in fs::read_dir(path).unwrap() {
            walk(&entry.unwrap().path(), out);
        }
    }

    let mut offenders = Vec::new();
    for (scanned, module, why) in FORBIDDEN_EDGES {
        let root = Path::new(scanned);
        assert!(
            root.exists(),
            "FORBIDDEN_EDGES names a path that no longer exists: {scanned} \
             — the table is stale, not the code"
        );
        let mut files = Vec::new();
        walk(root, &mut files);
        for file in files {
            let src = fs::read_to_string(&file).unwrap();
            for (i, line) in src.lines().enumerate() {
                if references_module(line, module) {
                    offenders.push(format!(
                        "{} -> crate::{} ({}) at {}:{}: {}",
                        scanned,
                        module,
                        why,
                        file.display(),
                        i + 1,
                        line.trim()
                    ));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "forbidden module-dependency edge(s) re-introduced:\n{}",
        offenders.join("\n")
    );
}

/// Every CLI `--depth` bound must be DERIVED from the traversal cap, never typed.
///
/// The MCP side already learned this: `COUNT_RANGES` carried literal `20` rows
/// for `get_call_graph.depth` and `find_http_route.depth` while
/// `get_call_graph_filtered` has always stopped at `CALL_GRAPH_MAX_DEPTH` (10),
/// so a `depth: 30` call answered `"applied": 20` and `"effective_max_depth": 10`
/// in one object — and `0.132.0` fixed it by deriving the rows from the constant.
///
/// The CLI then repeated it verbatim: `impact` and `trace` clamped to a literal
/// `20`, so `--depth 15` ran at 10 with nothing said and `--depth 99` was about
/// to start *publishing* "valid range is 1..=10" as "1..=20". Disclosing a
/// number the code never uses is worse than disclosing nothing, which is the
/// whole reason the disclosure exists. Caught in pre-ship review of `0.134.0`,
/// one commit before it shipped.
///
/// Scope: only the commands whose depth is capped AGAIN downstream. That set is
/// read off the code — a file whose `--depth` reaches `get_call_graph_filtered`,
/// directly or through `get_callers_with_route_info` — rather than from a
/// hand-list that would go stale. `deps` and `affected` are deliberately out of
/// scope: their depth flows into `file_closure`, which has no cap of its own, so
/// their literal IS the enforcing bound rather than a restatement of one.
///
/// The scan is textual because the bug's shape is textual — a literal upper
/// bound compiles perfectly well, so no type-level link can catch it.
#[test]
fn cli_depth_bounds_derive_from_the_traversal_cap() {
    use std::fs;
    use std::path::Path;

    // Names meaning "this depth gets re-clamped by the call-graph traversal".
    const CAPPED_BY_TRAVERSAL: &[&str] =
        &["get_call_graph_filtered", "get_callers_with_route_info"];

    let dir = Path::new("src/cli/commands");
    let mut offenders = Vec::new();
    let mut checked: Vec<String> = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let src = fs::read_to_string(&path).unwrap();
        if !CAPPED_BY_TRAVERSAL.iter().any(|n| src.contains(n)) {
            continue;
        }
        for (i, line) in src.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") || !trimmed.contains("clamp_arg(\"--depth\"") {
                continue;
            }
            checked.push(path.display().to_string());
            let upper = trimmed
                .rsplit(',')
                .next()
                .unwrap_or("")
                .trim()
                .trim_end_matches(");");
            if upper.chars().all(|c| c.is_ascii_digit()) {
                offenders.push(format!(
                    "{}:{}: --depth clamped to the literal `{}`, but this command's depth is \
                     re-clamped by the call-graph traversal — derive the bound from \
                     CALL_GRAPH_MAX_DEPTH, or the disclosed range and the honoured range \
                     drift apart: {}",
                    path.display(),
                    i + 1,
                    upper,
                    trimmed
                ));
            }
        }
    }
    assert!(
        checked.len() >= 2,
        "expected at least the `impact` and `trace` --depth clamps to be in scope; \
         found {checked:?} — the scan pattern has gone stale, not the code"
    );
    assert!(offenders.is_empty(), "{}", offenders.join("\n"));
}

/// `FORBIDDEN_EDGES` is a hand-written list, and a list stops covering the tree
/// the moment somebody adds a module root to `src/`. Six roots went unscanned
/// until the 2026-08-22 sweep noticed; nothing would have noticed the seventh.
///
/// This turns that sweep into a guard by asking the FILESYSTEM what the roots
/// are rather than restating them: a new `src/foo/` fails here until its
/// dependency direction is stated in the table (state it as forbidden, or as an
/// explicit exemption below with the reason).
#[test]
fn forbidden_edges_scans_every_module_root() {
    let scanned: std::collections::HashSet<&str> =
        FORBIDDEN_EDGES.iter().map(|(root, _, _)| *root).collect();

    // The crate roots are not modules with an inbound direction to police —
    // everything hangs off them by construction.
    const CRATE_ROOTS: &[&str] = &["lib.rs", "main.rs"];

    let mut missing = Vec::new();
    for entry in std::fs::read_dir("src").expect("src/ must be readable from the crate root") {
        let entry = entry.expect("readable dir entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        if CRATE_ROOTS.contains(&name.as_str()) {
            continue;
        }
        // A non-Rust file under src/ (fixtures, data) is not a module.
        if entry.path().is_file() && !name.ends_with(".rs") {
            continue;
        }
        let rel = format!("src/{name}");
        if !scanned.contains(rel.as_str()) {
            missing.push(rel);
        }
    }
    missing.sort();

    assert!(
        missing.is_empty(),
        "module root(s) under src/ that FORBIDDEN_EDGES never scans: {}\n\
         Add a row naming what each must not depend on. An unscanned root is a \
         silent gap, not an absence of rules — the table reads as complete.",
        missing.join(", ")
    );
}

/// Negative control for [`no_forbidden_module_dependency_edges`]: an all-green
/// table proves nothing unless the detector can still see an offender. Without
/// this, breaking `code_only` (say, stripping everything) would turn the guard
/// permanently green and silent — the "guard matches the file, not the
/// construct" class this repo has now hit three times.
#[test]
fn forbidden_edge_detector_actually_fires() {
    // Real imports and real path uses are caught.
    assert!(references_module("use crate::graph::routes::foo;", "graph"));
    assert!(references_module("    use crate::mcp::server::X;", "mcp"));
    assert!(references_module(
        "    let g = crate::cli::home_dir();",
        "cli"
    ));
    // Comments and string literals are not.
    assert!(!references_module(
        "/// orchestration lives in `crate::graph::routes`",
        "graph"
    ));
    assert!(!references_module("// use crate::mcp::server::X;", "mcp"));
    assert!(!references_module(
        r#"    let code = "fn caller() { crate::snapshot::create(); }";"#,
        "snapshot"
    ));
    // A `//` INSIDE a string must not truncate the line and hide what follows.
    assert!(references_module(
        r#"    let u = "http://x"; use crate::mcp::server::X;"#,
        "mcp"
    ));
    // Unrelated modules are not matched, and a prefix is not a module.
    assert!(!references_module(
        "use crate::storage::db::Database;",
        "graph"
    ));
    assert!(!references_module("use crate::clippy_helper::x;", "cli"));
}

/// Give `dir` a `.code-graph/index.db` (mirrors the private helper in
/// `src/cli.rs`'s own unit tests — duplicated here since that one is
/// `#[cfg(test)]`-private to the crate, not reachable from an integration test).
fn write_index(dir: &std::path::Path) {
    let idx = dir.join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    fs::create_dir_all(&idx).unwrap();
    fs::write(idx.join("index.db"), b"").unwrap();
}

/// META④ drift-guard: the Rust (`resolve_project_root_from`, src/cli.rs:38) and
/// JS (`resolveProjectRoot`, claude-plugin/scripts/project-root.js) project-root
/// resolvers are parallel implementations that MUST agree (M7 fix, v0.94.0).
/// This locks the specific case that split-brained before M7: cwd sits under a
/// STRAY nested `.code-graph` index (a monorepo-subdir relic) that is itself
/// below the real git root, which is also indexed. Both resolvers must pick the
/// git root, not the nearer stray index — otherwise the CLI and the JS hooks
/// read different `.code-graph` DBs for the same project.
///
/// JS invocation contract (confirmed by reading project-root.js in full plus its
/// consumer test `claude-plugin/scripts/pre-grep-guide.test.js`): the file has NO
/// CLI entrypoint — no argv parsing, no `require.main === module`, no stdout
/// write. It only `module.exports = { resolveProjectRoot }` for `require()`
/// (`pre-grep-guide.test.js` imports and calls the function directly, in-process
/// — it never shells out to the script). So "invoke via node" for a real
/// cross-process assertion means spawning `node -e` that requires the module by
/// absolute path and calls `resolveProjectRoot(cwd)` itself — this runs the
/// actual JS resolver logic in a real subprocess, not a fabricated assertion.
#[test]
fn project_root_resolution_rust_js_parity() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join(".git")).unwrap();
    write_index(root);
    let mid = root.join("packages").join("app");
    fs::create_dir_all(&mid).unwrap();
    write_index(&mid); // stray nested index, no .git of its own
    let cwd = mid.join("src");
    fs::create_dir_all(&cwd).unwrap();

    // Rust side: locked unconditionally, regardless of node availability below.
    let rust_root = code_graph_mcp::cli::resolve_project_root_from(&cwd);
    assert_eq!(
        fs::canonicalize(&rust_root).unwrap(),
        fs::canonicalize(root).unwrap(),
        "Rust resolver must pick the git root over the stray nested index (M7)"
    );

    let js_script_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("claude-plugin/scripts/project-root.js");
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(
            "const { resolveProjectRoot } = require(process.argv[2]); \
             const r = resolveProjectRoot(process.argv[1]); \
             process.stdout.write(r || '');",
        )
        .arg(&cwd)
        .arg(&js_script_path)
        .output();

    match out {
        Ok(o) if o.status.success() => {
            let js_root = String::from_utf8_lossy(&o.stdout).trim().to_string();
            assert!(
                !js_root.is_empty(),
                "JS resolver returned null/empty for a valid git+indexed root"
            );
            assert_eq!(
                fs::canonicalize(&rust_root).unwrap(),
                fs::canonicalize(&js_root).unwrap(),
                "Rust and JS project-root resolvers disagree on the git root (M7 split-brain)"
            );
        }
        _ => {
            // Degradation per the task brief: node unavailable/flaky in this test
            // harness. The Rust side is already locked above (unconditionally, not
            // inside this match arm); the JS resolver's stray-index-prefers-git-root
            // logic is separately covered by
            // `claude-plugin/scripts/pre-grep-guide.test.js`'s
            // "resolveProjectRoot: skips a STRAY nested subdir index, prefers the
            // .git root" test (a 2-level variant of this same scenario).
        }
    }
}

/// MED-1 drift-guard: the release profile must NOT set `panic = "abort"`.
///
/// `src/main.rs`'s per-request `std::panic::catch_unwind` (the H3 defense that
/// turns a handler panic into a JSON-RPC -32603 and keeps the long-lived stdio
/// session alive) is INERT under `panic = "abort"` — an abort tears the whole
/// process down before the catch can run. The unit/integration suite compiles
/// under the dev profile (unwind), so that defense is false-green in tests;
/// only the shipped release binary would abort. This guard reads the real
/// Cargo.toml at test time and fails if the release profile re-introduces the
/// abort setting.
#[test]
fn release_profile_must_unwind_for_catch_unwind_defense() {
    let manifest =
        fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("read Cargo.toml");

    // Slice out the [profile.release] section: from its header to the next
    // top-level `[` table header (or EOF).
    let header = "[profile.release]";
    let start = manifest
        .find(header)
        .expect("Cargo.toml must have a [profile.release] section");
    let after = &manifest[start + header.len()..];
    let end = after.find("\n[").map(|i| i + 1).unwrap_or(after.len());
    let section = &after[..end];

    // No UNCOMMENTED `panic = "abort"` key in the section.
    let offender = section.lines().find(|line| {
        let code = line.split('#').next().unwrap_or(""); // strip TOML comments
        let t = code.trim();
        t.starts_with("panic") && t.contains("abort")
    });
    assert!(
        offender.is_none(),
        "[profile.release] must not set `panic = \"abort\"` — it makes the \
         per-request catch_unwind in src/main.rs (session-survival defense) inert \
         in release builds. Offending line: {:?}",
        offender
    );
}

/// Every tool that takes a caller-supplied path must separator-normalize it at
/// TOOL ENTRY, so the freshness refresh and the index lookup use the same key.
///
/// The regression: `ensure_file_fresh_opt` normalized internally but returned
/// `Result<()>`, so its normalized value never escaped. Each caller then handed
/// the RAW argument to `get_nodes_by_file_path` / `get_call_graph_filtered` /
/// `get_module_exports`. A Windows client echoing back `src\mod_0.ts` refreshed
/// the right file and then missed the index (which stores `src/mod_0.ts`),
/// answering `File 'src\mod_0.ts' not found in index` for an indexed file.
///
/// Deliberately NOT `#[cfg(windows)]`. The forward-slash leg is the cross-
/// platform contract and runs everywhere — it is what catches a normalization
/// change that breaks the ordinary path. The backslash leg asserts only on
/// Windows *by design*: on Unix `\` is a legal filename character (only `/` and
/// NUL are illegal), so rewriting it there would be the #34 defect in the other
/// direction — see `indexer::merkle::normalize_rel_str_on`.
#[test]
fn tool_path_args_are_separator_normalized_at_entry() {
    let (_project, server) = setup_project(3);

    // (tool, arg key holding the path, other args, JSON pointer that is present
    // only when the path resolved to indexed rows)
    let cases: Vec<(&str, &str, serde_json::Value, &str)> = vec![
        (
            "get_ast_node",
            "file_path",
            json!({"symbol_name": "func_0"}),
            "/node_id",
        ),
        (
            "find_references",
            "file_path",
            json!({"symbol_name": "func_0"}),
            "/symbol",
        ),
        ("dependency_graph", "file_path", json!({}), "/file"),
        ("module_overview", "path", json!({}), "/path"),
        // `get_call_graph`'s presence pointer is deliberately weak: this fixture
        // has no call edges, and a wrong path filter yields the same empty
        // callers/callees as a right one — so only the Windows equality leg
        // below discriminates. It is listed because the audit found it missing
        // from this enumeration entirely; its failure mode (a mis-spelled filter
        // just silently drops edges) is the quietest of the six.
        (
            "get_call_graph",
            "file_path",
            json!({"symbol_name": "func_0", "direction": "callees"}),
            "/function",
        ),
    ];

    for (tool, key, extra, present_ptr) in cases {
        // `module_overview` takes a directory prefix; the others take a file.
        let unix_path = if tool == "module_overview" {
            "src/"
        } else {
            "src/mod_0.ts"
        };
        let win_path = unix_path.replace('/', "\\");

        let call = |p: &str| {
            let mut args = extra.clone();
            args[key] = json!(p);
            let resp = server.handle_message(&tool_call_json(tool, args)).unwrap();
            parse_tool_result(&resp)
        };

        // Forward slashes: must resolve on every platform.
        let unix_result = call(unix_path);
        assert!(
            unix_result.pointer(present_ptr).is_some(),
            "{tool}: forward-slash path {unix_path:?} must resolve; got {unix_result}"
        );
        assert!(
            unix_result.get("warning").is_none() && unix_result.get("error").is_none(),
            "{tool}: forward-slash path {unix_path:?} must not warn/error; got {unix_result}"
        );

        // Backslashes: identical answer on Windows, where `\` is a separator.
        if cfg!(windows) {
            let win_result = call(&win_path);
            assert_eq!(
                win_result, unix_result,
                "{tool}: native-separator path {win_path:?} must answer identically \
                 to {unix_path:?} — the path arg is not normalized at tool entry"
            );
        }
    }
}

/// Mechanical companion to the behavioural test above: every place a tool reads
/// a caller-supplied path out of `args` must have `normalize_path_arg` on the
/// same expression.
///
/// The behavioural test can only cover tools whose answer *changes observably*
/// on the current platform, and its case list is hand-maintained — which is how
/// `find_dead_code` (the 6th path-taking tool) shipped unnormalized while a
/// commit message claimed "five path-taking tools" were covered. Its failure
/// mode was also the quietest possible: a `\`-spelled prefix matched no row and
/// the tool reported "No dead code found".
///
/// This scan needs no fixture and no platform: it reads the source. A new tool
/// that reads `args["path"]` without normalizing fails here on the Linux leg.
#[test]
fn every_tool_path_arg_read_is_normalized_in_source() {
    const PATH_KEYS: [&str; 2] = ["path", "file_path"];
    let tools_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/mcp/server/tools");

    // Recursive, not read_dir: a tool module moved into a subdirectory would
    // silently leave the scan (audit 2026-08-02 mutation experiment showed
    // exactly that blind spot, alongside the `.get("path")` accessor spelling
    // handled below — the same spelling that produced the ignore_paths hole).
    fn collect_rs(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).expect("tools dir must exist — did the module move?") {
            let p = e.expect("dir entry").path();
            if p.is_dir() {
                collect_rs(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    collect_rs(&tools_dir, &mut files);
    files.sort();
    assert!(
        files.len() >= 8,
        "expected the tool modules to still live in {}; found {} .rs files",
        tools_dir.display(),
        files.len()
    );

    let mut offenders: Vec<String> = Vec::new();
    let mut checked = 0usize;

    for file in &files {
        let src = std::fs::read_to_string(file).unwrap();
        let lines: Vec<&str> = src.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let Some(key) = PATH_KEYS.iter().find(|k| {
                line.contains(&format!("args[\"{k}\"]"))
                    || line.contains(&format!("args.get(\"{k}\")"))
            }) else {
                continue;
            };
            checked += 1;
            // Gather the whole statement: `args["path"]` is usually the head of a
            // method chain that spans several lines and ends at the first `;`.
            let mut stmt = String::new();
            for l in lines.iter().skip(i).take(12) {
                stmt.push_str(l);
                stmt.push('\n');
                if l.contains(';') {
                    break;
                }
            }
            if !stmt.contains("normalize_path_arg") {
                offenders.push(format!(
                    "{}:{} (args[\"{key}\"]) — {}",
                    file.file_name().unwrap().to_string_lossy(),
                    i + 1,
                    stmt.trim().replace('\n', " ⏎ ")
                ));
            }
        }
    }

    assert!(
        checked >= 6,
        "the scan found only {checked} path-arg reads — the `args[\"path\"]` access \
         pattern probably changed and this guard is now scanning nothing"
    );
    assert!(
        offenders.is_empty(),
        "these tool path arguments reach the index without separator normalization \
         (add `.map(super::normalize_path_arg)` to the expression):\n  {}",
        offenders.join("\n  ")
    );
}

/// Optional-section flags must survive a warm cache.
///
/// `module_overview` cached the base result and returned it early, BEFORE folding
/// in `include_deps` / `include_dead` — and the flags are not part of the cache
/// key. Any earlier call warmed the entry (SessionStart injection and ordinary
/// exploration both do), so for the next 60s an `include_dead:true` call came
/// back byte-identical to a plain one: no `dead_code`, and no
/// `dead_code_unavailable` either. A caller cannot tell that from "nothing dead
/// here" — a false clean with a 60-second blast radius.
///
/// This is the cache-early-return axis of the same bug class the compact
/// forwarder guard covers; `project_map` got it right (centrality stays outside
/// its cache, with a comment) and this tool was the sibling hole.
#[test]
fn module_overview_optional_sections_survive_a_warm_cache() {
    let (_project, server) = setup_project(3);

    let call = |args: serde_json::Value| {
        let resp = server
            .handle_message(&tool_call_json("module_overview", args))
            .unwrap();
        parse_tool_result(&resp)
    };

    // 1. Warm the cache with a plain call — the precondition the bug needed.
    let plain = call(json!({"path": "src/"}));
    assert!(
        plain.get("dead_code").is_none(),
        "a plain call must not carry dead_code: {plain}"
    );

    // 2. Same path, flag on, well inside the 60s window.
    let dead = call(json!({"path": "src/", "include_dead": true}));
    assert!(
        dead.get("dead_code").is_some() || dead.get("dead_code_unavailable").is_some(),
        "include_dead:true on a cache-warm path must still answer the flag \
         (either a dead_code section or an explicit dead_code_unavailable); got {dead}"
    );

    // 3. Same for include_deps. A directory path can't have dependencies, so the
    //    honest answer is the explicit unavailable marker — silence is not.
    let deps = call(json!({"path": "src/", "include_deps": true}));
    assert!(
        deps.get("dependencies").is_some() || deps.get("dependencies_unavailable").is_some(),
        "include_deps:true on a cache-warm path must still answer the flag; got {deps}"
    );

    // 4. And a file path, where the fold produces a real dependencies section.
    let file_plain = call(json!({"path": "src/mod_0.ts"}));
    assert!(file_plain.get("dependencies").is_none());
    let file_deps = call(json!({"path": "src/mod_0.ts", "include_deps": true}));
    assert!(
        file_deps.get("dependencies").is_some(),
        "include_deps:true on a cache-warm FILE path must fold in dependencies; got {file_deps}"
    );
}

/// The source scan above reads the literal `args["<key>"]` form and only the
/// keys `path` / `file_path`. `find_dead_code` also takes a LIST of path
/// prefixes via `args.get("ignore_paths")` — a different accessor and a
/// different key, five lines below the `path` read that was just fixed, and
/// invisible to that scan twice over.
///
/// Same failure shape as the `path` one and just as quiet: the prefixes are
/// matched with `starts_with` against `/`-stored file paths, so a Windows
/// client's `ignore_paths: ["src\\generated"]` excludes nothing and the tool
/// over-reports dead code rather than under-reporting it.
#[test]
fn assert_ignore_paths_normalized_in_source() {
    // BOTH surfaces. `dead_code_report` is reached from the MCP tool AND from
    // `cmd_dead_code`, and the first version of this guard covered only the MCP
    // one — leaving the CLI half of the same change with zero coverage, the very
    // "fixed one, missed the sibling" shape it exists to catch.
    //
    // Source-scanned rather than behaviour-tested on purpose: the normalizer is
    // the identity on a Unix host by construction (`cfg!(windows)`), so no Linux
    // leg can observe the wiring. The behavioural contract — both spellings
    // exclude the same set once normalized — is pinned separately by
    // `test_dead_code_ignore_prefixes_are_separator_normalized`.
    //
    // The CLI half moved from `normalize_rel_str` to `normalize_user_path`
    // (CON-11: the prefix must resolve against the caller's cwd, like the scan
    // path, not merely have its separators swapped). `normalize_user_path`
    // delegates to `normalize_rel_str_on` for the relative branch and
    // `normalize_rel_path` for the absolute one, so it SUBSUMES the property
    // this guard was written for. Both spellings are accepted per site rather
    // than one, because pinning the exact call this guard happens to see today
    // is what makes it fire on a rename that preserved the property — the class
    // that disarmed the accessor-spelling guards in v0.129.0.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let sites: [(&str, &str, &[&str]); 2] = [
        (
            "src/mcp/server/tools/advanced.rs",
            r#"args.get("ignore_paths")"#,
            &["normalize_path_arg"],
        ),
        (
            "src/cli/commands/dead_code.rs",
            "let resolve_ignore = ",
            &["normalize_user_path", "normalize_rel_str"],
        ),
    ];

    for (file, anchor_text, accepted) in sites {
        let src = std::fs::read_to_string(root.join(file))
            .unwrap_or_else(|e| panic!("{file} moved or unreadable: {e}"));
        let lines: Vec<&str> = src.lines().collect();
        let anchor = lines
            .iter()
            .position(|l| l.contains(anchor_text))
            .unwrap_or_else(|| {
                panic!("anchor {anchor_text:?} moved out of {file} — update this guard")
            });
        let block: String = lines[anchor..(anchor + 14).min(lines.len())].join("\n");
        assert!(
            accepted.iter().any(|e| block.contains(e)),
            "dead-code ignore prefixes reach the index without separator \
             normalization ({file}:{}). They are matched with starts_with against \
             `/`-stored paths, so a backslash-spelled prefix excludes nothing and \
             the tool OVER-reports dead code. Expected one of {accepted:?}.\n{block}",
            anchor + 1
        );
    }
}

/// `sync-versions.js` writes N version sites; `pre-commit.sh` checks a list of
/// them. Those two lists were maintained by hand and had drifted by one: the
/// shipped snapshot workflow's npm pin was rewritten by the first and unchecked
/// by the second, so a hand-edited template committed on its own passed the hook
/// and was caught only at tag time, by release.yml re-running the sync (audit
/// 2026-08-29 ENG-07).
///
/// Adding the missing entry fixes today. This keeps tomorrow: the axis is "a
/// version site exists in one list and not the other".
///
/// BOTH sides are read as anchored BLOCKS, not as free text, and that is the
/// whole design. The first version of this guard asked whether the path appeared
/// anywhere in `pre-commit.sh` — which the new `report` line satisfies on its
/// own, so deleting the `VERSION_FILES` entry left it green. It also scanned
/// sync-versions.js for any quoted line, which swept up unrelated arrays and
/// kept the vacuity floor satisfied after the `file:` key was renamed away.
/// Both mistakes were found by mutating, not by reading.
#[test]
fn pre_commit_checks_every_version_site_sync_versions_writes() {
    fn quoted(line: &str) -> Option<String> {
        let start = line.find('\'')?;
        let rest = &line[start + 1..];
        let len = rest.find('\'')?;
        Some(rest[..len].to_string())
    }
    /// Lines of the `NAME = [` / `NAME=(` block that opens at `header`.
    fn block<'a>(src: &'a str, header: &str, close: &str) -> Vec<&'a str> {
        let Some(pos) = src.find(header) else {
            panic!("`{header}` moved — update this guard rather than letting it scan nothing")
        };
        src[pos..]
            .lines()
            .skip(1)
            .take_while(|l| l.trim() != close)
            .collect()
    }

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let sync = std::fs::read_to_string(root.join("scripts/sync-versions.js"))
        .expect("scripts/sync-versions.js moved — update this guard");
    let hook = std::fs::read_to_string(root.join("scripts/pre-commit.sh"))
        .expect("scripts/pre-commit.sh moved — update this guard");

    // Sites written by sync-versions.js: every `file: '<path>'` field, plus the
    // PLATFORM_PACKAGES array it iterates.
    let mut sites: Vec<String> = sync
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("file:"))
        .filter_map(quoted)
        .collect();
    sites.extend(
        block(&sync, "const PLATFORM_PACKAGES = [", "];")
            .into_iter()
            .filter_map(|l| quoted(l.trim())),
    );
    sites.sort();
    sites.dedup();

    // Sites checked by the hook: the VERSION_FILES array, and ONLY that array.
    // Reading the whole file would count a path that merely appears in a
    // `report` line — which is how the first version of this test passed while
    // the entry it was written for was absent.
    let checked: Vec<String> = block(&hook, "VERSION_FILES=(", ")")
        .into_iter()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let l = l.trim_matches('"');
            (!l.is_empty()).then(|| l.to_string())
        })
        .collect();

    // Vacuity floors on both sides: these scans read files they do not own, and
    // "found nothing" has to fail rather than pass. Ten sites today (Cargo.toml,
    // package.json, plugin.json, marketplace.json, five npm platform packages,
    // the snapshot template).
    assert!(
        sites.len() >= 10,
        "only {} version sites found in sync-versions.js — the scan lost its grip \
         on the file's shape, which is the failure mode that makes this guard pass \
         while checking nothing: {sites:?}",
        sites.len()
    );
    assert!(
        checked.len() >= 10,
        "only {} entries parsed out of VERSION_FILES: {checked:?}",
        checked.len()
    );

    let missing: Vec<&String> = sites.iter().filter(|p| !checked.contains(p)).collect();
    assert!(
        missing.is_empty(),
        "sync-versions.js rewrites these version sites and pre-commit.sh's \
         VERSION_FILES does not list them, so a hand-edit lands unnoticed: {missing:?}"
    );

    // And the reverse. This direction is not the dangerous one — a file checked
    // but never synced costs a confusing failure, not a silent bad release — but
    // the comment in pre-commit.sh promises that a site "cannot be added to one
    // list alone", and a one-way check does not deliver that (pre-tag review).
    let unsynced: Vec<&String> = checked.iter().filter(|p| !sites.contains(p)).collect();
    assert!(
        unsynced.is_empty(),
        "pre-commit.sh checks these files for a version sync-versions.js never \
         writes there: {unsynced:?}"
    );
}

/// The trimmed, comment-stripped directive lines of a YAML block.
///
/// Every containment check below MUST go through this. The first version of this
/// guard called `.contains("save-if: false")` on the raw text and passed against
/// the *prose explaining why save-if is set* — deleting the actual setting left
/// it green. That is the same "the words are present, so it must be true" failure
/// the rest of this file exists to catch.
fn yaml_directives(block: &str) -> Vec<&str> {
    block
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

/// Slice one job out of a workflow file: from `  <name>:` to the next key at the
/// same two-space indent (or EOF). Textual on purpose — the repo has no YAML
/// dev-dependency and the properties below are all lexical.
fn workflow_job<'a>(yaml: &'a str, job: &str) -> &'a str {
    let header = format!("\n  {job}:\n");
    let start = yaml
        .find(&header)
        .unwrap_or_else(|| panic!("job `{job}` not found — it was renamed or removed"))
        + header.len();
    let rest = &yaml[start..];
    // Next line beginning with exactly two spaces then a non-space.
    let end = rest
        .match_indices("\n  ")
        .find(|(i, _)| rest.as_bytes().get(i + 3).is_some_and(|c| *c != b' '))
        .map(|(i, _)| i + 1)
        .unwrap_or(rest.len());
    &rest[..end]
}

/// Drift guard: `release.yml`'s cache-consuming jobs and the `cache-warm.yml`
/// jobs that prime them must stay in lockstep.
///
/// This is enforced by test rather than by comment because the comment already
/// failed in production. `cache-warm.yml` has carried "MUST mirror release.yml
/// build byte-for-byte" since it was written, and v0.101.0 still shipped with
/// `key:` on one side and the automatic job-scoped key on the other — a green
/// priming run followed by "No cache found" on all five release targets. The
/// invariants below are exactly the ones whose violation is silent: a cache that
/// is written under a name nothing reads costs storage and reports success.
///
/// The `gate` pairing is newer (audit 2026-07-27 P1-3) and has the same shape,
/// with one asymmetry that is deliberate and therefore also pinned: `gate` runs
/// on `refs/tags/*`, where a saved cache is invisible to the next tag, so it is
/// restore-only and `warm-gate` is the sole writer.
#[test]
fn release_and_cache_warm_workflows_do_not_drift() {
    let wf = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows");
    // Normalise CRLF. Git used to check these YAML files out with CRLF on
    // Windows, and `workflow_job`'s exact `"\n  job:\n"` match then found
    // nothing — the guard failed on the windows-latest CI leg with "job `gate`
    // not found", which reads as a rename rather than a line ending.
    // `.gitattributes` (`* text=auto eol=lf`) now pins the working tree on every
    // platform, but this stays: a clone made before that file landed keeps its
    // CRLF working tree until the next checkout, and the cost here is one
    // `replace`. Everything else goes through `.lines()`, which strips `\r`.
    let read_lf = |p: std::path::PathBuf| -> String {
        fs::read_to_string(&p)
            .unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
            .replace("\r\n", "\n")
    };
    let release = read_lf(wf.join("release.yml"));
    let warm = read_lf(wf.join("cache-warm.yml"));

    // 1. One toolchain pin and one rust-cache pin across both files. A different
    //    rustc hashes to a different cache namespace, so a drifted pin produces a
    //    cache that can never be restored — silently, as a cold build.
    for (action, label) in [
        ("dtolnay/rust-toolchain@", "rust toolchain"),
        ("Swatinem/rust-cache@", "rust-cache"),
    ] {
        let pins: std::collections::BTreeSet<&str> = release
            .lines()
            .chain(warm.lines())
            .filter_map(|l| l.trim().strip_prefix("- uses: "))
            .filter(|u| u.starts_with(action))
            .map(|u| u.split_whitespace().next().unwrap_or(u))
            .collect();
        assert_eq!(
            pins.len(),
            1,
            "{label} is pinned to {} different SHAs across release.yml and \
             cache-warm.yml: {pins:?}. The warm cache is namespaced by toolchain \
             and action version, so a split pin means release.yml silently builds \
             cold.",
            pins.len()
        );
    }

    // 2. Every `shared-key` release.yml consumes must be written by cache-warm.yml.
    let shared_keys = |src: &str| -> std::collections::BTreeSet<String> {
        src.lines()
            .filter_map(|l| l.trim().strip_prefix("shared-key:"))
            .map(|v| v.trim().to_string())
            .collect()
    };
    let consumed = shared_keys(&release);
    let produced = shared_keys(&warm);
    assert!(
        !consumed.is_empty(),
        "release.yml declares no shared-key at all — rust-cache's automatic key \
         embeds the JOB name, so a cache written by cache-warm.yml can never be \
         restored here. This is the exact v0.101.0 failure."
    );
    let orphans: Vec<&String> = consumed.difference(&produced).collect();
    assert!(
        orphans.is_empty(),
        "release.yml restores shared-key(s) {orphans:?} that no cache-warm.yml job \
         writes. Actions caches are ref-scoped, so nothing on refs/tags/* can warm \
         them — every release would build cold while the workflow looks correct. \
         cache-warm.yml writes: {produced:?}"
    );

    // 3. `gate` is restore-only; `warm-gate` is the writer. Reversing this puts a
    //    tag-scoped cache nobody can read into the repo's LRU budget, evicting
    //    entries that ARE readable.
    let gate = yaml_directives(workflow_job(&release, "gate"));
    let warm_gate = yaml_directives(workflow_job(&warm, "warm-gate"));
    assert!(
        gate.contains(&"save-if: false"),
        "release.yml `gate` must set `save-if: false`. It runs on refs/tags/*, \
         where a saved cache is invisible to the next tag but still consumes the \
         repo's 10 GB LRU budget."
    );
    assert!(
        !warm_gate.contains(&"save-if: false"),
        "cache-warm.yml `warm-gate` is the ONLY writer of the release-gate cache; \
         with save-if disabled on both sides the gate is permanently cold."
    );
    assert!(
        warm_gate.contains(&"cache-on-failure: true"),
        "cache-warm.yml `warm-gate` must set `cache-on-failure: true` — it defaults \
         to FALSE, so a lint-red main would also mean no cache is saved, keeping \
         the gate cold for exactly as long as main stays red."
    );

    // 4. The commands whose artifacts the cache holds must match. `cargo fmt` is
    //    excluded: it compiles nothing, so it cannot affect the cache contents.
    //
    //    Both single-line `run: cargo …` directives AND cargo lines inside a
    //    `run: |` block are collected. Matching only the directive form left a
    //    hole running the wrong way: any future single-line step could be hidden
    //    from this guard by wrapping it in a block scalar, silently. Note
    //    `every_ci_cargo_invocation_is_locked` already splits on `cargo ` and so
    //    does see block-scalar lines — the two guards disagreed about them until
    //    now (pre-ship review 2026-09-06, finding 5).
    let cargo_invocations = |directives: &[&str]| -> Vec<String> {
        directives
            .iter()
            .filter_map(|l| {
                l.strip_prefix("run: cargo ")
                    .or_else(|| l.strip_prefix("cargo "))
            })
            .map(|c| c.trim_end_matches('\\').trim().to_string())
            .filter(|c| !c.starts_with("fmt"))
            .collect()
    };
    let gate_builds = cargo_invocations(&gate);
    let warm_builds = cargo_invocations(&warm_gate);
    assert!(
        !gate_builds.is_empty(),
        "no `run: cargo` steps found in release.yml `gate` — the job was \
         restructured and this guard no longer reads it"
    );

    // 4a. The gate's own contract (audit 2026-07-27 P1-3). Warming a cache for a
    //     job that no longer checks anything is worse than no gate at all: the
    //     pipeline still reports a green "Gate" and publishes to npm, which is
    //     irreversible. Each of these was a specific hole the audit found.
    // `--locked` is part of the needle, not incidental: this gate is what stands
    // between a tag and an irreversible publish, and a gate that re-resolves is
    // checking a dependency set the committed Cargo.lock never described
    // (audit 2026-09-05 ENG-01; `every_ci_cargo_invocation_is_locked` covers the
    // rest of the workflows).
    for (needle, why) in [
        ("run: cargo fmt --check", "formatting"),
        (
            "run: cargo clippy --locked --no-default-features --all-targets -- -D warnings",
            "clippy on the default feature set",
        ),
        (
            "run: cargo clippy --locked --features embed-model --all-targets -- -D warnings",
            "clippy on the feature set release binaries actually ship",
        ),
        (
            "run: cargo test --locked --no-default-features",
            "the Rust test suite on the default (no-features) set — what `cargo install` users build (audit 2026-08-16 P1-18)",
        ),
        (
            "run: cargo test --locked --features embed-model",
            "the Rust test suite",
        ),
    ] {
        assert!(
            gate.contains(&needle),
            "release.yml `gate` no longer runs {why} (`{needle}`). release.yml's \
             only trigger is a tag push and publishing to npm is irreversible, so \
             this job is the last check before release."
        );
    }

    // 4b. …and it must still block. A `gate` job nothing depends on runs in
    //     parallel with `build` and cannot stop a publish, while still showing a
    //     green check named "Gate".
    let build = yaml_directives(workflow_job(&release, "build"));
    assert!(
        build.contains(&"needs: gate"),
        "release.yml `build` must declare `needs: gate`. Without it the gate runs \
         alongside the build instead of before it, so a red gate does not stop the \
         publish — it only annotates it."
    );
    // 4c. Environment parity for the test step. `CODE_GRAPH_DISABLE_MODEL_DOWNLOAD`
    //     stops the spawned `serve` integration tests background-fetching a ~90 MB
    //     model on a runner that has none. It affects runtime, not the cache, so a
    //     split here is invisible to every other assertion — it just makes one of
    //     the two jobs slow and flaky, and the gate is the last check before an
    //     irreversible publish.
    let env_key = "CODE_GRAPH_DISABLE_MODEL_DOWNLOAD: '1'";
    assert_eq!(
        gate.contains(&env_key),
        warm_gate.contains(&env_key),
        "`{env_key}` must be set in BOTH release.yml `gate` and cache-warm.yml \
         `warm-gate`, or in neither — they run the same test command and must run \
         it under the same environment."
    );

    // Gate-only cargo invocations, each with the reason it cannot cost a cold
    // compile. An allowlist rather than a silent exemption: tightening the scan
    // above to see block scalars would otherwise fail on steps that are
    // legitimately exempt, and "it happens to be invisible to the parser" is not
    // a reason anyone can check later.
    const GATE_ONLY_CARGO: &[(&str, &str)] = &[(
        "test --locked --features embed-model --lib --",
        "the real-weight gate. Same feature set and the same targets as the \
         `Test (embed-model)` step directly above it, so cargo has already built \
         everything it needs — measured 13.4 s cold, 0.14 s warm.",
    )];

    for cmd in &gate_builds {
        if let Some((_, why)) = GATE_ONLY_CARGO.iter().find(|(p, _)| cmd.starts_with(p)) {
            assert!(
                !warm_builds.iter().any(|w| w == cmd),
                "`cargo {cmd}` is on the gate-only allowlist ({why}) but cache-warm.yml \
                 `warm-gate` now runs it too — drop the allowlist entry rather than \
                 keeping a stale exemption."
            );
            continue;
        }
        assert!(
            warm_builds.iter().any(|w| w == cmd),
            "release.yml `gate` runs `cargo {cmd}` but cache-warm.yml `warm-gate` \
             does not. Lint and feature flags participate in cargo's fingerprint, \
             so a command only the gate runs is a command the gate compiles cold. \
             If it genuinely compiles nothing new, add it to GATE_ONLY_CARGO with \
             the measurement that says so."
        );
    }
}

/// `(basename, sha256)` for every model-asset pin line in a workflow.
///
/// Reads the `sha256sum -c` heredocs. Free function rather than a closure so the
/// guard below can run it on a deliberately tampered copy and prove it can fail —
/// a pin-equality assertion that has never been shown to reject anything is
/// indistinguishable from one that reads the wrong lines.
fn model_asset_pins(workflow: &str) -> Vec<(String, String)> {
    const ASSETS: [&str; 3] = ["model.safetensors", "tokenizer.json", "config.json"];
    let mut pins = Vec::new();
    for line in workflow.lines() {
        let mut parts = line.split_whitespace();
        let (Some(hash), Some(path)) = (parts.next(), parts.next()) else {
            continue;
        };
        if parts.next().is_some()
            || hash.len() != 64
            || !hash.bytes().all(|b| b.is_ascii_hexdigit())
        {
            continue;
        }
        if let Some(asset) = ASSETS.iter().find(|a| path.ends_with(**a)) {
            pins.push(((*asset).to_string(), hash.to_string()));
        }
    }
    pins
}

/// Every job that downloads `model-assets-v1` pins the SAME bytes.
///
/// Three copies now: release.yml's gate (the cheap pre-publish real-weight
/// check), release.yml's publish job (which packages `models.tar.gz`), and
/// cache-warm.yml's backfill regressions. A re-cut of that release updates one
/// copy at a time, and drift is invisible in the worst direction — the gate
/// would certify bytes that are not the bytes shipped, with every job green
/// (audit 2026-09-05 ENG-02; pre-ship review 2026-09-06).
#[test]
fn release_model_pins_agree_across_jobs() {
    let wf = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows");
    let read = |name: &str| -> String {
        std::fs::read_to_string(wf.join(name))
            .unwrap_or_else(|e| panic!("read {name}: {e}"))
            .replace("\r\n", "\n")
    };
    let release = read("release.yml");
    let mut pins = model_asset_pins(&release);
    pins.extend(model_asset_pins(&read("cache-warm.yml")));

    // Vacuity floor: three assets × three download sites. If a job stops
    // pinning, or the heredoc shape changes so these lines stop parsing, this
    // fails rather than passing over an empty list.
    assert_eq!(
        pins.len(),
        9,
        "expected 3 pinned model assets in each of the 3 jobs that download them; \
         got {pins:?}"
    );

    for asset in ["model.safetensors", "tokenizer.json", "config.json"] {
        let hashes: Vec<&String> = pins
            .iter()
            .filter(|(a, _)| a == asset)
            .map(|(_, h)| h)
            .collect();
        assert_eq!(hashes.len(), 3, "{asset}: expected 3 pins, got {hashes:?}");
        assert!(
            hashes.iter().all(|h| *h == hashes[0]),
            "{asset} is pinned to more than one hash ({hashes:?}): a job would \
             verify bytes other than the ones packaged for users"
        );
    }

    // Prove the check can reject. Flipping one nibble of the first pin must be
    // caught; without this the assertions above pass just as happily against a
    // parser that returns two copies of the same line.
    //
    // The replacement nibble is chosen against the current one rather than being
    // the literal '0': a hash that already starts with '0' would make the
    // "tamper" a no-op, and this self-check would then fail on a CORRECT pin —
    // reddening the suite on the day someone re-cuts `model-assets-v1` and the
    // weights happen to hash that way, with a message blaming the parser. One in
    // sixteen re-cuts (pre-ship review, 2026-09-06).
    //
    // Tampering is applied to release.yml alone, which carries two of the three
    // copies by itself (gate + publish), so one flipped nibble is enough to make
    // them disagree.
    let first = &pins[0].1;
    let flipped = if first.starts_with('0') { '1' } else { '0' };
    let tampered = release.replacen(first.as_str(), &format!("{flipped}{}", &first[1..]), 1);
    let tampered_pins = model_asset_pins(&tampered);
    let mismatched = tampered_pins
        .iter()
        .filter(|(a, _)| *a == pins[0].0)
        .map(|(_, h)| h)
        .collect::<Vec<_>>();
    assert_eq!(
        mismatched.len(),
        2,
        "release.yml alone must still carry two copies for this self-check to \
         mean anything; got {mismatched:?}"
    );
    assert_ne!(
        mismatched[0], mismatched[1],
        "the guard must see a one-nibble difference between release.yml's two \
         pin blocks"
    );
}

/// Every `--exact` test name in a real-weight CI step actually exists.
///
/// `cargo test <filter>` exits 0 when the filter matches nothing — measured on
/// this repo: `0 passed … EXIT=0`. So a rename would silently restore the exact
/// state these steps exist to end: green having executed nothing, with
/// `CODE_GRAPH_REQUIRE_MODEL` powerless because its assert lives inside a test
/// that never runs. Both steps assert an `N passed` line at run time; this
/// asserts the names at build time, so the break surfaces in a local
/// `cargo test` instead of in a release (pre-ship review, 2026-09-06).
///
/// Covers both halves of the split: release.yml's gate runs the cheap
/// load-and-embed check before publish, and cache-warm.yml's cron runs the two
/// intermittently-failing backfill regressions off the critical path.
#[test]
fn release_gate_runs_the_real_weight_tests_by_name() {
    let wf = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let read = |p: std::path::PathBuf| -> String {
        std::fs::read_to_string(&p)
            .unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
            .replace("\r\n", "\n")
    };
    // Resolve each name to the ONE file that must define it, rather than
    // searching a concatenation of both. Two holes closed at once (pre-ship
    // review 2026-09-06, finding 7):
    //
    //   - matching only the leaf after `rsplit("::")` let a MODULE move through.
    //     Measured: rewriting the workflow to
    //     `embedding::model::renamed_mod::test_embed_produces_correct_dims`
    //     left this guard green while the real command ran `0 tests`.
    //   - concatenating both sources let a name defined in either satisfy either
    //     workflow, so a test moved between targets would not be noticed.
    //
    // A `::`-qualified name addresses a lib test by module path: everything
    // before the last two segments is the file under `src/`, and the segment
    // before the leaf is the enclosing `mod`. A bare name is an integration
    // test in the stdio target.
    let source_for = |name: &str| -> (std::path::PathBuf, Option<String>) {
        let segments: Vec<&str> = name.split("::").collect();
        if segments.len() < 2 {
            return (root.join("tests/mcp_stdio_integration.rs"), None);
        }
        let file = format!("src/{}.rs", segments[..segments.len() - 2].join("/"));
        (
            root.join(file),
            Some(segments[segments.len() - 2].to_string()),
        )
    };

    // The names sit between `--exact` and the next flag, across a line
    // continuation. Read them from the workflow rather than hard-coding them
    // here, or this guard pins a copy instead of the thing that runs.
    let names_after_exact = |yaml: &str| -> Vec<String> {
        let mut names = Vec::new();
        let mut after_exact = false;
        for tok in yaml.split_whitespace() {
            if tok == "--exact" {
                after_exact = true;
                continue;
            }
            if !after_exact {
                continue;
            }
            if tok == "\\" {
                continue;
            }
            if tok.starts_with('-') {
                after_exact = false;
                continue;
            }
            names.push(tok.to_string());
        }
        names
    };

    let mut checked = 0usize;
    for (file, expected_count) in [("release.yml", 1usize), ("cache-warm.yml", 2usize)] {
        let yaml = read(wf.join(file));
        let names = names_after_exact(&yaml);
        assert_eq!(
            names.len(),
            expected_count,
            "expected {file}'s real-weight step to name exactly {expected_count} \
             test(s) after `--exact`; parsed {names:?}"
        );
        for name in &names {
            let (path, enclosing_mod) = source_for(name);
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string();
            // A missing file is itself the failure: it means the module chain in
            // the workflow no longer maps to anything on disk.
            assert!(
                path.is_file(),
                "{file} runs `--exact {name}`, whose module path points at {rel}, \
                 which does not exist. cargo test exits 0 on a filter that matches \
                 nothing, so this would ship as a green step that ran no \
                 real-weight test at all."
            );
            let src = read(path.clone());
            let leaf = name.rsplit("::").next().unwrap_or(name);
            assert!(
                src.contains(&format!("fn {leaf}(")),
                "{file} runs `--exact {name}`, but {rel} defines no `fn {leaf}`. \
                 cargo test exits 0 on a filter that matches nothing, so this \
                 would ship as a green step that ran no real-weight test at all."
            );
            if let Some(m) = &enclosing_mod {
                assert!(
                    src.contains(&format!("mod {m}")),
                    "{file} runs `--exact {name}`, but {rel} has no `mod {m}` — the \
                     test moved modules. The leaf name alone would still match, and \
                     the real command would run 0 tests."
                );
            }
            checked += 1;
        }
        assert!(
            yaml.contains(&format!("test result: ok\\. {expected_count} passed")),
            "{file}'s real-weight step must assert that its test(s) actually RAN. \
             Naming them is not enough: a filter matching zero tests still exits 0."
        );
    }
    assert_eq!(
        checked, 3,
        "vacuity floor: 1 gate test + 2 cron tests must all have been checked"
    );

    let release = read(wf.join("release.yml"));
    assert!(
        release.contains("test result: ok\\. 1 passed"),
        "release.yml's real-weight step must assert that its test actually RAN. \
         Naming them is not enough: a filter matching zero tests still exits 0."
    );
}

/// Fold shell line continuations so a command split across physical lines is
/// scanned as the one command it is.
///
/// Returns `(1-based line of the FIRST physical line, joined text)`. A
/// commented-out command is documentation, not a step, and also terminates any
/// continuation in progress — what was accumulated so far is still checked, so
/// that direction fails closed.
fn logical_shell_lines(src: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut acc: Option<(usize, String)> = None;
    for (i, raw) in src.lines().enumerate() {
        let trimmed = raw.trim();
        if trimmed.starts_with('#') {
            out.extend(acc.take());
            continue;
        }
        let (body, continues) = match trimmed.strip_suffix('\\') {
            Some(b) => (b.trim_end(), true),
            None => (trimmed, false),
        };
        match acc.as_mut() {
            Some((_, s)) => {
                s.push(' ');
                s.push_str(body);
            }
            None => acc = Some((i + 1, body.to_string())),
        }
        if !continues {
            out.extend(acc.take());
        }
    }
    out.extend(acc.take());
    out
}

/// Cargo subcommands that RESOLVE dependencies. `cargo fmt` and `cargo install`
/// are excluded on purpose: fmt reads no lockfile, and the one `install`
/// (cargo-audit) already pins its own.
///
/// `publish` and `package` are listed although no workflow runs one today: this
/// guard's whole job is to catch the invocation nobody remembers to add the flag
/// to, and an omission here fails open (pre-ship review 2026-09-05).
const RESOLVING_SUBCOMMANDS: &[&str] = &[
    "build", "test", "check", "clippy", "bench", "run", "publish", "package",
];

/// Every dependency-resolving cargo invocation in `src` that lacks `--locked`.
///
/// Returns `(invocations examined, offenders as (line, text))`. Each `cargo `
/// occurrence is sliced from its own start to the start of the next one, so a
/// chained `cargo build --locked && cargo test` is two invocations and the flag
/// on the first cannot vouch for the second.
fn unlocked_cargo_invocations(src: &str) -> (usize, Vec<(usize, String)>) {
    let mut checked = 0usize;
    let mut unlocked: Vec<(usize, String)> = Vec::new();
    for (line_no, logical) in logical_shell_lines(src) {
        let starts: Vec<usize> = logical.match_indices("cargo ").map(|(i, _)| i).collect();
        for (n, &start) in starts.iter().enumerate() {
            let end = starts.get(n + 1).copied().unwrap_or(logical.len());
            let call = &logical[start..end];
            let sub = call["cargo ".len()..]
                .split_whitespace()
                .next()
                .unwrap_or("");
            if !RESOLVING_SUBCOMMANDS.contains(&sub) {
                continue;
            }
            checked += 1;
            if !call.contains("--locked") {
                unlocked.push((line_no, call.trim().to_string()));
            }
        }
    }
    (checked, unlocked)
}

/// Every cargo invocation in CI carries `--locked` (audit 2026-09-05 ENG-01).
///
/// Without it cargo silently RE-RESOLVES when `Cargo.toml` and `Cargo.lock`
/// disagree and writes a new lock into the runner's workspace. Nothing goes red;
/// the tag just ships a binary linked against versions the `audit` job — which
/// scans the COMMITTED `Cargo.lock` — never saw. `--locked` turns that silence
/// into a failed step, on the one file where the difference is auditable.
///
/// A test rather than a comment for the usual reason: the property is invisible
/// per-line. A twenty-second invocation added without the flag looks exactly
/// like the twenty-one that have it, and the first evidence would be a release.
#[test]
fn every_ci_cargo_invocation_is_locked() {
    let wf = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows");
    let mut unlocked: Vec<String> = Vec::new();
    let mut checked = 0usize;
    let mut files = 0usize;
    for entry in fs::read_dir(&wf).expect("read .github/workflows") {
        let path = entry.expect("dir entry").path();
        // Both spellings: GitHub reads `.yaml` too, and skipping it here would
        // let a whole workflow opt out of this guard by its file extension.
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "yml" && ext != "yaml" {
            continue;
        }
        files += 1;
        let src = fs::read_to_string(&path).expect("read workflow");
        let (n, offenders) = unlocked_cargo_invocations(&src);
        checked += n;
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        unlocked.extend(
            offenders
                .into_iter()
                .map(|(line, call)| format!("{name}:{line}: {call}")),
        );
    }
    assert!(
        files >= 5,
        "only {files} workflow files found — the scan lost its grip"
    );
    assert!(
        checked >= 21,
        "only {checked} cargo invocations found across {files} workflows — the scan \
         lost its grip and would pass vacuously"
    );
    assert!(
        unlocked.is_empty(),
        "these CI cargo invocations can silently re-resolve dependencies:\n  {}",
        unlocked.join("\n  ")
    );
}

/// Negative control for [`every_ci_cargo_invocation_is_locked`]: an all-green
/// workflow tree proves nothing unless the scanner can still see an offender.
///
/// Every case below is a shape the line-by-line scanner this replaced got wrong
/// (audit 2026-09-05, ENG-01's fourth point — the report recorded only the first
/// of them):
///   * `--locked` on the continuation line → FALSE POSITIVE (fails closed, but
///     it made the guard unusable with the wrapping style three workflows use);
///   * the subcommand on the continuation line → FALSE NEGATIVE: `cargo \` left
///     `\` as the "subcommand", which is not in the resolving list, so the whole
///     invocation was invisible;
///   * a second `cargo` chained after a locked first → FALSE NEGATIVE: the scan
///     read one subcommand per line and `--locked` anywhere on it vouched for
///     every call on that line.
#[test]
fn locked_scanner_reads_continuations_and_chains() {
    // Offenders are matched by suffix, not equality: the exact text a scanner
    // quotes back is cosmetic, and pinning it here would make this control fail
    // on formatting before it reached the shapes it exists to cover.
    let unlocked = |src: &str| -> Vec<String> {
        unlocked_cargo_invocations(src)
            .1
            .into_iter()
            .map(|(_, c)| c)
            .collect()
    };
    let checked = |src: &str| unlocked_cargo_invocations(src).0;
    let reports = |src: &str, call: &str| {
        let hits = unlocked(src);
        assert_eq!(hits.len(), 1, "expected exactly one offender in {src:?}");
        assert!(
            hits[0].ends_with(call),
            "offender {:?} should end with {call:?}",
            hits[0]
        );
    };

    // Plain, on one line: both directions.
    assert!(unlocked("        run: cargo test --locked --no-default-features").is_empty());
    reports(
        "        run: cargo test --no-default-features",
        "cargo test --no-default-features",
    );

    // The flag arriving on the continuation line is still the same invocation.
    assert!(
        unlocked("          cargo test \\\n            --locked --no-default-features").is_empty(),
        "a `--locked` on the continuation line must count for the command it continues"
    );

    // …and so is the SUBCOMMAND arriving there. This one was invisible.
    let split_sub = "          cargo \\\n            test --no-default-features";
    assert_eq!(
        checked(split_sub),
        1,
        "`cargo \\` + `test …` is one resolving invocation, not zero"
    );
    reports(split_sub, "cargo test --no-default-features");

    // A chain: the flag on the first call must not vouch for the second.
    let chained = "        run: cargo build --locked --release && cargo test --no-default-features";
    assert_eq!(checked(chained), 2, "a chained line holds two invocations");
    reports(chained, "cargo test --no-default-features");

    // Exclusions still hold: comments are documentation, and the two
    // subcommands that resolve nothing stay out of the count.
    assert_eq!(checked("          # cargo test --no-default-features"), 0);
    assert_eq!(checked("        run: cargo fmt --check"), 0);
    assert_eq!(
        checked("        run: cargo install --locked --version ^0.22 cargo-audit"),
        0,
        "`cargo-audit` is not a `cargo ` invocation and `install` does not resolve here"
    );
}

/// Every `curl` in CI carries `--max-time` (audit 2026-09-05, ENG-03 neighbour).
///
/// curl has NO default transfer timeout, and `--retry` does not fire on a
/// connected-but-stalled transfer — it retries failures, and a socket that
/// accepts bytes at 1 byte/minute has not failed. The repo has already paid for
/// this once: a stalled ripgrep mirror sat 29 minutes on the release critical
/// path with the tag pushed and nothing published (2026-08-19).
///
/// Per-line and therefore invisible: a sixth `curl` added without the flag looks
/// exactly like the five that have it, and the evidence arrives as a release
/// that never finishes.
#[test]
fn every_ci_curl_has_a_transfer_timeout() {
    let wf = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows");
    let mut untimed: Vec<String> = Vec::new();
    let mut checked = 0usize;
    let mut files = 0usize;
    for entry in fs::read_dir(&wf).expect("read .github/workflows") {
        let path = entry.expect("dir entry").path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "yml" && ext != "yaml" {
            continue;
        }
        files += 1;
        let src = fs::read_to_string(&path).expect("read workflow");
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        // Continuations matter here more than anywhere: every model fetch in
        // this repo wraps the URL onto a second line.
        for (line_no, logical) in logical_shell_lines(&src) {
            if !logical.contains("curl ") {
                continue;
            }
            checked += 1;
            if !logical.contains("--max-time") {
                untimed.push(format!("{name}:{line_no}: {}", logical.trim()));
            }
        }
    }
    assert!(
        files >= 5,
        "only {files} workflow files found — the scan lost its grip"
    );
    assert!(
        checked >= 5,
        "only {checked} curl invocations found across {files} workflows — the scan \
         lost its grip and would pass vacuously"
    );
    assert!(
        untimed.is_empty(),
        "these CI downloads can stall indefinitely (curl has no default timeout \
         and --retry does not fire on a stalled transfer):\n  {}",
        untimed.join("\n  ")
    );
}

/// Every job name in a workflow file.
///
/// Job headers are the only lines at exactly two spaces of indentation inside
/// the `jobs:` block — `on:` keys sit at the same depth, which is why the scan
/// starts after `jobs:` and not at the top of the file.
fn workflow_job_names(src: &str) -> Vec<&str> {
    let Some(start) = src.find("\njobs:\n") else {
        return Vec::new();
    };
    src[start..]
        .lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("  ")?;
            if rest.starts_with(' ') || rest.starts_with('#') {
                return None;
            }
            let name = rest.strip_suffix(':')?;
            (!name.is_empty() && !name.contains(' ')).then_some(name)
        })
        .collect()
}

/// Jobs in `src` with no job-level `timeout-minutes`.
fn jobs_missing_timeout(src: &str) -> Vec<String> {
    workflow_job_names(src)
        .into_iter()
        .filter(|name| !workflow_job(src, name).contains("\n    timeout-minutes:"))
        .map(str::to_string)
        .collect()
}

/// Every CI job declares `timeout-minutes` (audit 2026-09-06).
///
/// GitHub's default is SIX HOURS. This repo has already paid for that default
/// twice from one incident: on 2026-08-19 a stalled apt mirror held a release
/// gate 29 minutes and a CI leg 40 before someone killed them by hand, and an
/// earlier run rode the 6h ceiling to the end. Those were fixed with step-level
/// timeouts on the step that stalled — which bounds that step and nothing else.
///
/// The property is invisible per job: 10 of the 13 jobs here had no bound at
/// all, and a job that hangs looks exactly like a job that is slow until the
/// ceiling arrives. Sizes are set from measured durations at each site, so this
/// bounds a hang without turning a cold cache into a red release.
#[test]
fn every_ci_job_declares_a_timeout() {
    let wf = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows");
    let mut unbounded: Vec<String> = Vec::new();
    let mut jobs = 0usize;
    let mut files = 0usize;
    for entry in fs::read_dir(&wf).expect("read .github/workflows") {
        let path = entry.expect("dir entry").path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "yml" && ext != "yaml" {
            continue;
        }
        files += 1;
        let src = fs::read_to_string(&path).expect("read workflow");
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        jobs += workflow_job_names(&src).len();
        unbounded.extend(
            jobs_missing_timeout(&src)
                .into_iter()
                .map(|j| format!("{name}: {j}")),
        );
    }
    assert!(
        files >= 5,
        "only {files} workflow files found — the scan lost its grip"
    );
    assert!(
        jobs >= 13,
        "only {jobs} jobs found across {files} workflows — the scan lost its grip \
         and would pass vacuously"
    );
    assert!(
        unbounded.is_empty(),
        "these CI jobs fall back to GitHub's 6-hour default, so a hang is \
         indistinguishable from slowness until the ceiling:\n  {}",
        unbounded.join("\n  ")
    );
}

/// Negative control for [`every_ci_job_declares_a_timeout`] and
/// [`ci_schedule_runs_only_the_audit_job`]'s shared job scanner.
#[test]
fn job_scanner_finds_jobs_and_missing_timeouts() {
    let wf = "\
name: X

on:
  push:
    branches: [main]

jobs:
  # a comment at job depth is not a job
  alpha:
    name: Alpha
    timeout-minutes: 5
    runs-on: ubuntu-latest
    steps:
      - run: echo hi
  beta:
    name: Beta
    runs-on: ubuntu-latest
    steps:
      - name: a step-level timeout is NOT a job bound
        timeout-minutes: 3
        run: echo hi
";
    assert_eq!(
        workflow_job_names(wf),
        vec!["alpha", "beta"],
        "`on:` keys sit at job depth too and must not be counted as jobs"
    );
    assert_eq!(
        jobs_missing_timeout(wf),
        vec!["beta".to_string()],
        "a step-level `timeout-minutes` (6-space indent) must not satisfy the \
         job-level bound — it bounds one step, which is the gap this guard exists \
         to close"
    );
}

/// ci.yml's daily `schedule:` exists for the `audit` job and nothing else.
///
/// `on: schedule` is WORKFLOW-scoped, not job-scoped. Adding it so `cargo audit`
/// sees advisories published between two pushes (audit 2026-09-05 ENG-03) also
/// enrolls the 3-OS test matrix, the embed leg and the Node suite in a nightly
/// run nobody asked for — and nothing goes red, so the only evidence is the
/// bill. The per-job `if:` gates are what keep the trigger narrow, and they are
/// opt-OUT: a job added later inherits the cron by default.
#[test]
fn ci_schedule_runs_only_the_audit_job() {
    const GATE: &str = "\n    if: github.event_name != 'schedule'\n";
    const CRON_JOB: &str = "audit";
    let ci = fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/ci.yml"),
    )
    .expect("read ci.yml");
    assert!(
        ci.contains("\n  schedule:\n"),
        "ci.yml lost its `schedule:` trigger — `cargo audit` is back to seeing \
         advisories only when someone pushes"
    );

    let names = workflow_job_names(&ci);
    assert!(
        names.len() >= 5,
        "only {} jobs found in ci.yml ({names:?}) — the scan lost its grip and \
         would pass vacuously",
        names.len()
    );
    assert!(
        names.contains(&CRON_JOB),
        "ci.yml has no `{CRON_JOB}` job ({names:?}) — it was renamed or moved, and \
         the daily schedule now runs either nothing or everything"
    );

    for name in &names {
        let gated = workflow_job(&ci, name).contains(GATE);
        if *name == CRON_JOB {
            assert!(
                !gated,
                "ci.yml `{name}` is gated out of `schedule` — the daily trigger \
                 exists for this job alone, so gating it makes the cron a no-op \
                 that still reports green"
            );
        } else {
            assert!(
                gated,
                "ci.yml `{name}` runs on the daily `schedule:` trigger. Add\n \
                 `{}` to the job, or move the cron out of this workflow.",
                GATE.trim_matches('\n')
            );
        }
    }
}

/// The arm64 cross-compile install must be the SAME in both workflows.
///
/// The two copies are twins, and on 2026-09-04 that was the whole finding: from
/// one push, `cache-warm.yml`'s copy died on the 15-minute ceiling with the
/// mirror still feeding it while `release.yml`'s identical copy passed. Nothing
/// separated them but which mirror each runner drew. So hardening one and not
/// the other leaves the failure live on whichever side gets missed — and the
/// side that matters is release.yml, which sits above an irreversible npm
/// publish and is the copy nobody sees fail until a tag is already pushed.
///
/// Compares the step BODY, not just its presence: the earlier drift guard in
/// this file pins toolchains, cache keys and gate commands, and would not have
/// noticed one copy keeping a 15-minute bare ceiling while the other grew a
/// retry.
#[test]
fn the_arm64_cross_compile_install_is_identical_in_both_workflows() {
    fn step_body(src: &str) -> Vec<String> {
        let lines: Vec<&str> = src.lines().collect();
        let start = lines
            .iter()
            .position(|l| l.trim() == "- name: Install cross-compilation tools")
            .expect("no `Install cross-compilation tools` step in this workflow");
        // Run to the next sibling step, ignoring comment lines so the two copies
        // may explain themselves differently — the reasoning lives in
        // release.yml and cache-warm.yml points at it.
        lines[start + 1..]
            .iter()
            .take_while(|l| !l.trim_start().starts_with("- name:"))
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect()
    }

    // Normalising CRLF is not cosmetic here: this test compares two files as
    // text, and a Windows checkout would otherwise report every line unequal.
    let read_lf = |p: std::path::PathBuf| -> String {
        fs::read_to_string(&p)
            .unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
            .replace("\r\n", "\n")
    };

    let wf = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows");
    let release = step_body(&read_lf(wf.join("release.yml")));
    let warm = step_body(&read_lf(wf.join("cache-warm.yml")));

    assert!(
        release.iter().any(|l| l.contains("timeout-minutes:")),
        "vacuity floor: the scanned body does not even contain the step's own \
         timeout, so this comparison stopped matching the workflow layout: \
         {release:?}"
    );
    assert!(
        release.iter().any(|l| l.contains("apt-get install")),
        "vacuity floor: the scanned body contains no apt install: {release:?}"
    );
    assert_eq!(
        release, warm,
        "release.yml and cache-warm.yml install the arm64 cross toolchain \
         differently. They are the same step against the same mirrors; harden \
         or edit them as a pair."
    );
}

/// Drift guard: every `github.ref` / `github.ref_name` EXPRESSION in release.yml
/// must carry the `github.event.inputs.tag ||` fallback.
///
/// `workflow_dispatch` runs from the default branch, so on a re-release
/// `github.ref` is `refs/heads/main` and `github.ref_name` is `main` — neither
/// names the tag being released. Every site that forgets the fallback therefore
/// acts on main's HEAD instead of the tag's source, and does so silently: the
/// run is green, it just built and published the wrong commit.
///
/// This is a test rather than a comment because the site count has been wrong
/// three separate times. It went in as five, a later audit found a sixth
/// (checkout in `gate`), and the 2026-07-27 audit found a seventh — the
/// top-level `concurrency.group`, where the miss meant a tag-push run and a
/// dispatch re-run of the same version got DIFFERENT group keys and published
/// concurrently, which is exactly what the comment above that block says it
/// prevents. Each was found by reading, one at a time.
#[test]
fn release_workflow_ref_expressions_all_fall_back_to_the_dispatch_tag() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/release.yml");
    let yaml = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .replace("\r\n", "\n");

    // `yaml_directives` drops whole-line comments, which is where every prose
    // mention of `github.ref` in this file lives — including the ones explaining
    // this very contract. Without that filter the guard would flag its own docs.
    // Requires only that the expression CONSULTS `inputs.tag`, not that it uses
    // the `tag ||` spelling: `concurrency.group` deliberately uses
    // `tag && format('refs/tags/{0}', tag) || github.ref` instead, because a
    // group key is compared as a literal string and the plain form yields a
    // different key for the push and the dispatch (see that block's comment).
    let offenders: Vec<&str> = yaml_directives(&yaml)
        .into_iter()
        .filter(|l| l.contains("${{") && l.contains("github.ref"))
        .filter(|l| !l.contains("github.event.inputs.tag"))
        .collect();

    assert!(
        offenders.is_empty(),
        "release.yml has {} `github.ref`/`github.ref_name` expression(s) with no \
         `github.event.inputs.tag ||` fallback. On a workflow_dispatch re-release \
         these resolve to the DEFAULT BRANCH, not the tag, and the run stays \
         green while acting on the wrong commit:\n  {}",
        offenders.len(),
        offenders.join("\n  ")
    );

    // Negative control: the fallback sites must actually exist. A future edit
    // that deletes them all would otherwise leave this test green on an empty
    // set — the vacuous pass this file exists to prevent.
    let with_fallback = yaml_directives(&yaml)
        .into_iter()
        .filter(|l| l.contains("github.event.inputs.tag"))
        .count();
    assert!(
        with_fallback >= 7,
        "expected at least the 7 known `inputs.tag` fallback sites in release.yml, \
         found {with_fallback} — if a site was legitimately removed, lower this \
         number in the same commit and say which one"
    );
}

/// Supply chain: every `uses:` in every workflow is pinned to a full commit SHA.
///
/// A mutable tag (`@v6`) is a standing write-authority grant to whoever can move
/// that tag — and these workflows run with `contents: write` and, in the publish
/// job, an `NPM_TOKEN`. The repo already SHA-pinned all 13 third-party uses and
/// left all 21 first-party `actions/*` ones on major tags, which is the harder
/// half to justify: `actions/checkout` runs first in every job, including the
/// one holding the npm token.
///
/// Enforced here rather than by review because the audit flagged this twice
/// (07-24 #17, 07-27 P2-25) and nothing changed either time. The 40-hex check
/// also catches the likelier accident: pasting a SHORT sha, which GitHub accepts
/// today and resolves ambiguously as the repo grows.
#[test]
fn every_workflow_action_is_pinned_to_a_full_commit_sha() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows");
    let mut checked = 0usize;
    let mut unpinned: Vec<String> = Vec::new();

    let mut files: Vec<_> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "yml" || x == "yaml"))
        .collect();
    // The SHIPPED workflow templates too: they are installed into USERS' repos
    // (with `contents: write`), where a floating tag is a supply-chain hole we
    // hand to every adopter. They were pinned by hand in the 2026-08-16 batch;
    // without this leg the next template edit can silently revert to `@v4`.
    let tpl_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("claude-plugin/templates");
    files.extend(
        fs::read_dir(&tpl_dir)
            .unwrap_or_else(|e| panic!("read {}: {e}", tpl_dir.display()))
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "yml" || x == "yaml")),
    );
    files.sort();
    assert!(
        files.len() > 5,
        "workflow file discovery shrank to {} files — this guard would pass vacuously",
        files.len()
    );

    for path in &files {
        let yaml = fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
            .replace("\r\n", "\n");
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        for line in yaml_directives(&yaml) {
            let Some(rest) = line
                .strip_prefix("- uses: ")
                .or_else(|| line.strip_prefix("uses: "))
            else {
                continue;
            };
            checked += 1;
            // `owner/repo@<ref>` — the ref runs to whitespace or the ` # vN` note.
            let git_ref = rest
                .split_whitespace()
                .next()
                .and_then(|spec| spec.split_once('@').map(|(_, r)| r))
                .unwrap_or("");
            let pinned = git_ref.len() == 40
                && git_ref
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
            if !pinned {
                unpinned.push(format!("{name}: {line}"));
            }
        }
    }

    // 41 is the measured count across the five workflow files, not a round
    // number: the first version of this floor said 21 (the count of first-party
    // uses I had just pinned), which a parse regression halving the real count
    // would have sailed straight through.
    assert!(
        checked >= 43,
        "expected at least the 43 known `uses:` sites across the workflows and the \
         shipped templates, found {checked} — a parse change would make this guard \
         vacuous. The floor was 41 before the two template sites joined; leaving it \
         there gave those two zero vacuity protection (a template edited into YAML \
         flow style would drop the count to 42 and still pass). If a workflow or \
         step was legitimately removed, lower this in the same commit."
    );
    assert!(
        unpinned.is_empty(),
        "{} workflow action(s) are on a mutable ref instead of a 40-hex commit \
         SHA. Whoever can move that tag can run code in a job that holds \
         `contents: write` (and, in publish, NPM_TOKEN):\n  {}",
        unpinned.len(),
        unpinned.join("\n  ")
    );
}

/// Every JS test file that spawns with a redirected HOME must also neutralize
/// `CLAUDE_CONFIG_DIR`.
///
/// `claudeHome()` is `process.env.CLAUDE_CONFIG_DIR || homedir/.claude`, so the
/// env var OUTRANKS a redirected HOME. A spawn built as
/// `{ ...process.env, HOME: sandbox }` passes it straight through, and for a
/// developer who exports it (the documented multi-profile setup) the test then
/// operates on their live config — measured on this branch: `npm test` wrote a
/// fabricated `9.9.9` plugin version into the real plugins cache, and a real
/// `uninstall --unadopt-all` deleted `<config>/plugins/cache/code-graph-mcp/`.
///
/// A file satisfies this by deleting the variable at module load (covers every
/// spawn) or by setting it explicitly on each child env. This is a test because
/// the class has now been half-fixed twice: v0.108.1 closed it in
/// `tests/cli_e2e.rs` and `doctor.test.js` while missing `install-e2e.test.js`,
/// and the fix for that missed six more files. CI never sees it — the variable
/// is unset on the runners — so it can only fail on a developer's machine.
/// Enumerate the JS test corpus the way CI, pre-commit and the release gate do.
///
/// ENG-06 (audit 2026-08-29): this was a non-recursive `read_dir` in two guards
/// at once, while all three discovery chains have been recursive since the fix
/// pinned by `scripts/test-discovery-drift-guard.test.js`. The first JS test file
/// placed in a subdirectory would therefore have RUN in CI while being invisible
/// to every guard that grades the corpus — the guards' scan surface was narrower
/// than the surface they guard. Zero nested files exist today, which is why this
/// was cheap to fix and would have stayed unnoticed.
///
/// The `>= N` floors stay at the call sites: they are per-guard vacuity
/// detectors, and folding them in here is how a shared helper quietly disarms the
/// checks that lived in its callers.
fn js_test_files() -> Vec<std::path::PathBuf> {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return; // an optional directory; the call-site floor catches a bad root
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if name == "node_modules" || name.starts_with('.') {
                    continue;
                }
                walk(&path, out);
            } else if name.ends_with(".test.js") {
                out.push(path);
            }
        }
    }

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dir in ["claude-plugin/scripts", "scripts"] {
        let d = root.join(dir);
        // The ROOTS are asserted; only nested dirs may vanish silently. The first
        // version of this helper let `walk` swallow a missing root and claimed
        // "the call-site floor catches a bad root" — pre-tag review measured that
        // it does not: `claude-plugin/scripts` alone holds 34 test files against a
        // floor of 20, so losing all 6 in `scripts/` would leave both callers
        // green while scanning less. The old non-recursive code panicked by name;
        // this restores that.
        assert!(
            d.is_dir(),
            "{dir} is not a directory — the JS corpus moved; update js_test_files() \
             instead of letting these guards scan a smaller tree"
        );
        let mut found = Vec::new();
        walk(&d, &mut found);
        assert!(
            !found.is_empty(),
            "{dir} holds no *.test.js — a per-root floor, because the call-site \
             floors are met by the larger root alone"
        );
        found.sort();
        files.append(&mut found);
    }
    files
}

#[test]
fn js_test_files_neutralize_claude_config_dir() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let files = js_test_files();
    assert!(
        files.len() >= 20,
        "expected the JS test suite to be discovered, found {} files — a path \
         change would make this guard vacuous",
        files.len()
    );

    let mut offenders = Vec::new();
    for path in &files {
        let src = fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
            .replace("\r\n", "\n");
        // Three properties, each earned by a version of this guard that failed
        // without it:
        //   1. line-anchored, not `src.contains` — commenting the statement out
        //      leaves the text in the file, and a `contains` check stayed green
        //      for a file that had just stopped neutralizing anything;
        //   2. at MODULE SCOPE (zero indentation) — a `delete` nested in a
        //      function body may never run;
        //   3. BEFORE the first `test(`. NOT because of a load-time race — all
        //      three resolvers (`claude-config.js` `claudeHome`, and adopt.js's
        //      two registry paths) read the variable at CALL time, and nothing
        //      binds a derived constant at require time, so a delete after the
        //      first `require` is in fact safe. It is required anyway because
        //      "somewhere in the file" is not a property: a delete inside a test
        //      body runs only if that test runs, and only for the tests after it.
        let first_test = src
            .lines()
            .position(|l| l.trim_start().starts_with("test("))
            .unwrap_or(usize::MAX);
        let neutralized = src
            .lines()
            .enumerate()
            .any(|(i, l)| i < first_test && l.starts_with("delete process.env.CLAUDE_CONFIG_DIR"));
        if neutralized {
            continue;
        }
        let lines: Vec<&str> = src.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim_start();
            // A `process.env.HOME = …` inside a run of `process.env.X = …`
            // statements that also sets CLAUDE_CONFIG_DIR is the explicit-pairing
            // form (install-e2e's generated child script uses it).
            //
            // This read exactly ONE line ahead until 2026-09-01, which is the same
            // magic-number mistake the spawn window below documents three versions
            // of: adding `process.env.USERPROFILE = home;` between the HOME line
            // and the CLAUDE_CONFIG_DIR line (ENG-05) pushed the pairing out of
            // view and reddened two correctly-sandboxed files. The bound is now the
            // redirect BLOCK — consecutive `process.env.* =` assignments — so
            // inserting another env name into it cannot break the pairing. The 24
            // is a runaway cap, not a semantic limit, matching the sibling window.
            let paired_next = lines
                .iter()
                .skip(i + 1)
                .take(24)
                .take_while(|n| n.trim_start().starts_with("process.env."))
                .any(|n| n.contains("process.env.CLAUDE_CONFIG_DIR"));
            // Two shapes leak the variable:
            //   * a spawn spreading `...process.env` (an env object built from
            //     scratch, `{ HOME: dir, PATH: '' }`, never inherits it);
            //   * an IN-PROCESS redirect, `process.env.HOME = <sandbox>`, where
            //     the module under test calls `claudeHome()` directly. The first
            //     version of this guard checked only spawns and therefore missed
            //     `adopt.test.js`, which wrote five `projects/<slug>/memory/`
            //     trees into a canary config dir while the guard stayed green.
            //
            // The spawn check spans the ENV OBJECT LITERAL, not one line and not a
            // fixed line count. Three versions of this got it wrong:
            //   * one line only — `env: { ...process.env,\n HOME: home }` is the
            //     Prettier-formatted spelling of the very lines this guard was
            //     written for, and it went unseen;
            //   * `take_while` including the first line — a single-line spawn
            //     contains its own `}`, so the window came back EMPTY and the
            //     guard went quiet on the shape it was already catching;
            //   * a fixed 5-line window — five intervening env keys put `HOME:`
            //     at offset 6, outside it. A magic number is not a bound.
            // The literal ends at the first `}`; 24 is a runaway cap, not a
            // semantic limit.
            let window: Vec<&str> = std::iter::once(*line)
                .chain(
                    lines
                        .iter()
                        .skip(i + 1)
                        .take(24)
                        .take_while(|l| !l.contains('}'))
                        .copied(),
                )
                .collect();
            // The exemption must be an actual KEY in this literal. Matching bare
            // `CLAUDE_CONFIG_DIR` anywhere in the window let two things launder a
            // real leak: a correctly-sandboxed SECOND spawn a few lines down, and
            // — worse, because this codebase writes exactly that — a COMMENT
            // saying the variable is handled. Both measured green before this.
            let mentions_key = |l: &&str| {
                let t = l.trim_start();
                !t.starts_with("//")
                    && !t.starts_with('*')
                    && (l.contains("CLAUDE_CONFIG_DIR:") || l.contains("CLAUDE_CONFIG_DIR ="))
            };
            let leaks_via_spawn = line.contains("...process.env")
                && window.iter().any(|l| l.contains("HOME:"))
                && !window.iter().any(mentions_key);
            let leaks_in_process = t.starts_with("process.env.HOME =");
            if (leaks_via_spawn || leaks_in_process)
                && !line.contains("CLAUDE_CONFIG_DIR")
                && !paired_next
            {
                // Relative path, not basename: now that the corpus is walked
                // recursively (ENG-06), two files can share a name and a bare
                // `doctor.test.js:12` would not say which one.
                offenders.push(format!(
                    "{}:{}: {}",
                    path.strip_prefix(root).unwrap_or(path).display(),
                    i + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "{} JS test spawn(s) redirect HOME but inherit CLAUDE_CONFIG_DIR, so they \
         act on the real Claude config for anyone who exports it. Fix by adding \
         `delete process.env.CLAUDE_CONFIG_DIR;` at module load, or by setting it \
         on the child env:\n  {}",
        offenders.len(),
        offenders.join("\n  ")
    );
}

/// Running the JS test suite must not destroy the shared tmp dir its own
/// children are using.
///
/// `cgTmpDir()` is `<os.tmpdir()>/code-graph-mcp`, ONE path per machine, holding
/// live hook cooldown flags and `update-*` download staging. `lifecycle.js`
/// `uninstall()` deletes it wholesale — correct in production, where an
/// uninstall should leave no residue. The trap is that every OTHER path that
/// same function deletes (`CACHE_DIR`, `pluginsCacheDir()`) is derived from
/// HOME, so a test that redirects HOME alone believes it is sandboxed while this
/// one `rmSync` reaches straight out to the real machine-global directory.
///
/// The victims are whichever sibling test file happens to be mid-flight in
/// another `node --test` process: `pre-grep-guide.test.js` loses the cooldown
/// flag it wrote milliseconds earlier and its re-grep denies instead of
/// observing; `auto-update.test.js` loses the `update-<ms>` staging dir between
/// extract and copy and `downloadAndInstall` reports `pluginUpdated:false`.
/// Both were filed as separate mysteries, and the first was mis-attributed to a
/// command-hash cleanup race that the unique-per-test command names make
/// arithmetically impossible.
///
/// This guard is BEHAVIOURAL on purpose. The obvious alternative — scan
/// `*.test.js` for spawns that set HOME without TMPDIR — is a source-scanning
/// guard over a helper-indirected call (`runScript` builds the env, the spawn
/// sites never mention HOME), and this repository has already watched that shape
/// go silently inert across a refactor. Pointing TMPDIR at a throwaway, planting
/// a sentinel where `cgTmpDir()` will resolve, and running the suite asserts the
/// property itself: no file list, no env-literal parsing, and a new offender in
/// a file that does not exist yet still fails it.
///
/// Two directions, one property. Deleting that directory is the loud failure;
/// LITTERING it is the quiet one, and the sentinel check cannot see it. Three
/// hook test files wrote cooldown flags and read-fanout state straight into the
/// real `cgTmpDir()` — measured on the commit before this one, 14 entries per
/// full-suite run (10 `.code-graph-postinject-*`, 4 `.code-graph-readfan-*`),
/// none of them cleaned by the test that created them and all of them reclaimed
/// only by `pruneCgTmp`'s 24h sweep. On a box that runs the suite dozens of
/// times a day that is a growing directory the developer's LIVE hooks also read,
/// and a `post-grep-inject.test.js` cleanup helper that had been spelling a flag
/// name production stopped writing hid it in plain sight. So the assertion is
/// the stronger one: after the run this directory holds the sentinel and nothing
/// else. A test file that needs the real flag path still gets it — it just has
/// to own its own TMPDIR, the way `tmp-dir.test.js` and `lifecycle.test.js` do.
///
/// Runs in every feature set, on every platform. Skipping the `embed-model`
/// build would save ~50 s twice (the CI embed job and the release gate's second
/// `cargo test`) to re-assert a property nothing here touches embeddings for —
/// but `#[cfg(not(feature = "embed-model"))]` on a `#[test]` would be the only
/// one in this repository, nothing anywhere asserts a test count, and `default`
/// is `[]` only until someone flips it (Cargo.toml records that it WAS flipped
/// once). The day it gains `embed-model`, that cfg deletes this guard from the
/// pre-commit hook, the three-OS matrix and the release gate at once, with
/// nothing going red. Buying 100 s with an unguarded axis is the trade this
/// whole investigation exists to argue against.
#[test]
fn js_test_suite_leaves_the_shared_tmp_dir_intact() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let files = js_test_files();
    assert!(
        files.len() >= 20,
        "expected the JS test suite to be discovered, found {} files — a path \
         change would make this guard vacuous",
        files.len()
    );

    // The sandbox stands in for the machine's real tmp dir: the children inherit
    // TMPDIR, so `cgTmpDir()` resolves HERE for every one of them. TMP and TEMP
    // are set too — node reads TMPDIR first on POSIX but ignores it entirely on
    // Windows, where setting only TMPDIR would leave this guard inert on exactly
    // the platform it claims to cover. Running on all three platforms is the
    // point rather than an oversight: the three-name requirement IS the
    // Windows-specific half of this bug class.
    //
    // HOME is redirected into the same sandbox, and that is a correction rather
    // than belt-and-braces. With the real HOME inherited, this test rewrote the
    // developer's own `~/.cache/code-graph/binary-path` (find-binary's on-disk
    // resolution cache) WHILE `tests/cli_e2e.rs` was resolving a binary through
    // it in a sibling process, and turned two of its feature-detection tests red
    // — measured: `cli_e2e` 293/2 with this test in the run, 295/0 with it
    // skipped. A guard that reddens unrelated tests is not paying for itself.
    let sandbox = tempfile::tempdir().unwrap();
    let cg_tmp = sandbox.path().join("code-graph-mcp");
    fs::create_dir_all(&cg_tmp).unwrap();
    let sentinel = cg_tmp.join(".sentinel-shared-tmp-guard");
    fs::write(&sentinel, b"").unwrap();

    // A third machine-global axis rides the same run for free: CLAUDE_CONFIG_DIR.
    // `claudeHome()` is `CLAUDE_CONFIG_DIR || homedir/.claude`, so the variable
    // OUTRANKS a redirected HOME, and a developer who exports it (the documented
    // multi-profile setup) has their live config in the blast radius of any spawn
    // built as `{ ...process.env, HOME: sandbox }`.
    //
    // `js_test_files_neutralize_claude_config_dir` already covers this — by
    // SCANNING each file for a module-scope `delete`. That guard catches a file
    // that omits the line even when no test happens to exercise a leaking path,
    // which this one cannot; this one catches a neutralization that parses but
    // does not work, or a new leak reached through a helper the scanner cannot
    // follow, which that one cannot. They are complementary, and this repository
    // has already watched a source-scanning guard go silently inert across a
    // refactor, so the behavioural half is worth having. It costs no extra
    // runtime: the suite spawn below already exists.
    //
    // Setting the variable rather than leaving it unset is the whole point, and
    // the precise claim matters. The scanning guard runs and fails on CI like any
    // other test, so the STATIC form of this property is already covered there;
    // what the runners never see is the LEAK, because they leave the variable
    // unset and every write then lands in the redirected HOME. Pointing it at a
    // canary is what puts the BEHAVIOURAL form on CI for the first time. (The
    // "CI never sees it" in that guard's own doc comment is about the leak, not
    // about itself; read the other way it argues for deleting a guard that
    // works.)
    //
    // Measured before this was added: with the variable pointed at a canary, a
    // full suite run leaves the tree byte-identical, so it starts GREEN and is a
    // regression guard rather than a bug fix.
    let canary_cfg = sandbox.path().join("canary-config");
    fs::create_dir_all(canary_cfg.join("plugins").join("cache")).unwrap();
    let canary_settings = canary_cfg.join("settings.json");
    fs::write(&canary_settings, b"{\"canary\":true}").unwrap();

    // Output goes to FILES, not pipes: this reads the child's exit with a
    // deadline below, and a piped stdout that nobody drains deadlocks the child
    // once the pipe buffer fills — which would turn a hang-guard into a hang.
    //
    // stdout and stderr stay SEPARATE. Merged into one file, the vacuity check
    // below reads "some bytes arrived" — and a node that starts but rejects its
    // arguments writes 37 bytes of `node: bad option: …` to STDERR and exits 9,
    // which satisfied the merged check while nothing ran. That is not
    // hypothetical: `--test-concurrency` requires Node >= 20.10, so every older
    // node takes exactly that path, on the flag added right below.
    // The one axis the sandbox does NOT cover is cwd: `current_dir(root)` is the
    // repository, so `.code-graph/index.db` is reachable, and `doctor.test.js`,
    // `hook-fire.test.js` and `lifecycle.e2e.test.js` each touch its `-shm`/`-wal`
    // sidecars. Measured as a READ — index.db's sha256, `user_version` and
    // `COUNT(*) FROM nodes` are unchanged across a full run; opening a WAL
    // database as a reader is enough to touch the sidecars. It cannot reproduce
    // the `~/.cache` collision that had this test reddening its neighbours,
    // because no Rust test opens the live repo index. Left un-sandboxed because
    // `current_dir(sandbox)` would break the repo-relative resolution those
    // files depend on; named here so the next person does not have to re-derive
    // that it is deliberate.
    let out_log = sandbox.path().join("js-suite.out");
    let err_log = sandbox.path().join("js-suite.err");
    let stdout = fs::File::create(&out_log).unwrap();
    let stderr = fs::File::create(&err_log).unwrap();

    // `--test-concurrency=1`, matching ci.yml. Its comment records WHY, and the
    // reason applies here verbatim: `cg-answer` and `find-binary` race on
    // find-binary's on-disk resolution cache and on `_CG_ANSWER_BINARY` under
    // per-file parallelism. Serial costs ~28 s against ~11 s warm-parallel and
    // buys determinism, in a test whose whole subject is cross-process
    // interference. `install-e2e.test.js` is NOT excluded the way ci.yml
    // excludes it — it is one of the three files whose sandbox this commit
    // fixed, so leaving it out would leave the fix unguarded; with HOME
    // sandboxed it self-skips its binary-dependent cases cleanly.
    // `--test-reporter=tap` pins the output shape. node's default reporter is
    // TTY-dependent (`spec` on a terminal, `tap` otherwise), and the assert
    // below looks for a specific marker — reading it out of a format that
    // changes with where the output happens to land is not a check.
    let spawned = std::process::Command::new("node")
        .arg("--test")
        .arg("--test-concurrency=1")
        .arg("--test-reporter=tap")
        .args(&files)
        .current_dir(root)
        .env("HOME", sandbox.path())
        .env("USERPROFILE", sandbox.path())
        .env("TMPDIR", sandbox.path())
        .env("TMP", sandbox.path())
        .env("TEMP", sandbox.path())
        .env("CLAUDE_CONFIG_DIR", &canary_cfg)
        .stdout(stdout)
        .stderr(stderr)
        .spawn();

    let Ok(mut child) = spawned else {
        // node genuinely absent. Reached ONLY when the process fails to SPAWN,
        // never when the suite runs and reports failures — the sentinel check
        // below is deliberately independent of whether the JS tests passed.
        //
        // On CI this arm is a silent pass, so it is not allowed to be one:
        // every job that reaches this test runs `actions/setup-node`, so a
        // missing node there means the workflow changed under the guard, not
        // that the guard is inapplicable.
        assert!(
            std::env::var_os("CI").is_none(),
            "node could not be spawned on CI, where every job running this test \
             installs it — the guard would have passed without asserting anything"
        );
        return;
    };

    // A bound, because an unbounded child is the failure this repository has
    // actually paid for: two jobs sat 29 and 40 minutes on a stuck step and one
    // ran into the 6 h ceiling (ci.yml's ripgrep note). 10 minutes is ~20x the
    // measured 28 s, so it fires only for a genuine hang.
    //
    // `kill()` signals the RUNNER only. Under `--test-concurrency=1` one
    // per-file node child is alive underneath it and may itself hold an
    // `execFileSync` grandchild; neither is reaped here. Left as is rather than
    // reaching for `process_group` + `killpg`, which is POSIX-only and would put
    // a cfg fork on the one path that must not have bugs of its own — on a
    // wedged CI runner the job tears the container down regardless, and on a
    // developer box this is a stray node, not lost work. Named so nobody has to
    // rediscover it from a leftover process.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    // The tail is COPIED into the message, not pointed at:
                    // `sandbox` is a TempDir, `panic!` unwinds, and Drop deletes
                    // the whole tree — so a path here would name a file that no
                    // longer exists by the time anyone reads the failure, on the
                    // one path where the diagnostic is the entire value.
                    let tail = fs::read_to_string(&out_log)
                        .map(|s| s.chars().rev().take(2000).collect::<String>())
                        .map(|s| s.chars().rev().collect::<String>())
                        .unwrap_or_else(|e| format!("<could not read the log: {e}>"));
                    panic!(
                        "the JS suite did not finish within 600s; killed it rather \
                         than letting it hold the job. Last 2000 chars of its \
                         output:\n{tail}"
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(e) => panic!("waiting on the JS suite failed: {e}"),
        }
    }

    // Positive evidence that the RUNNER ran, not that bytes arrived. TAP's
    // summary line (`# tests 1160`) is emitted only after the runner has walked
    // the whole file list; a node that rejected its arguments, or one whose
    // stdout went nowhere, cannot produce it. "The file is non-empty" was the
    // weaker form and it is what merging stderr in here defeated.
    let out = fs::read_to_string(&out_log).unwrap_or_default();
    let err = fs::read_to_string(&err_log).unwrap_or_default();
    assert!(
        out.contains("\n# tests "),
        "the JS suite never reported a TAP test count, so nothing was actually \
         run and the guard would pass vacuously. Its stdout was {} bytes and its \
         stderr said: {}",
        out.len(),
        if err.trim().is_empty() {
            "<nothing>"
        } else {
            err.trim()
        }
    );

    assert!(
        sentinel.exists(),
        "running the JS test suite deleted {}, a path that stands in for the \
         machine-global `cgTmpDir()` every developer's live hooks share. Some \
         test spawns plugin code (`lifecycle.js uninstall` is the wholesale \
         deleter) with HOME redirected but TMPDIR inherited. Redirect TMPDIR, \
         TMP and TEMP at module scope in the offending file, the way \
         `tmp-dir.test.js` does.",
        sentinel.display()
    );

    // The other direction: residue. Read the directory rather than counting, so
    // the failure names the offending files — a bare count tells the next person
    // that something littered without telling them what, and these names carry
    // their own attribution (`postinject` and `readfan` are the writing hooks).
    let mut residue: Vec<String> = fs::read_dir(&cg_tmp)
        .map(|d| {
            d.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n != ".sentinel-shared-tmp-guard")
                .collect()
        })
        .unwrap_or_default();
    residue.sort();
    assert!(
        residue.is_empty(),
        "the JS test suite left {} entries in {}, which stands in for the \
         machine-global `cgTmpDir()` the developer's live hooks share — nothing \
         reclaims them for 24h. Redirect TMPDIR, TMP and TEMP into a \
         `mkdtempSync` sandbox at MODULE scope in the offending file, before the \
         require that pulls in `tmp-dir.js` (it resolves `CG_TMP_DIR` at require \
         time, so a later assignment is inert), and delete the sandbox in a \
         `test.after`. Leftovers:\n{}",
        residue.len(),
        cg_tmp.display(),
        residue.join("\n")
    );

    // Third axis: the canary config directory must come back exactly as it went
    // in. Two failure shapes, and they are not equally evidenced. A NEW path
    // appearing under it is the one that reproduces: deleting the module-scope
    // neutralizer from `lifecycle.test.js` leaks `statusline-providers.json`,
    // and from `adopt.test.js` leaks a whole `projects/<slug>/memory/` tree —
    // two files, two different writers, both caught by the listing below. An
    // EXISTING file rewritten IN PLACE leaves that listing identical, so the
    // second assertion covers it; no mutation reproduced that shape (three
    // tried), which makes it defensive rather than demonstrated. It is one
    // comparison against a known literal and cannot pass vacuously, so it stays.
    fn walk(dir: &std::path::Path, base: &std::path::Path, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let p = entry.path();
            out.push(
                p.strip_prefix(base)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
            // `file_type()` off the DirEntry, NOT `p.is_dir()`: the latter
            // follows symlinks, so a leaked symlink pointing at its own ancestor
            // would recurse until the stack is gone. That path is only reachable
            // once a leak already exists — i.e. exactly when this function's
            // diagnostic is the whole point — and a stack abort prints none of
            // it.
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                walk(&p, base, out);
            }
        }
    }
    let mut tree = Vec::new();
    walk(&canary_cfg, &canary_cfg, &mut tree);
    tree.sort();
    assert_eq!(
        tree,
        vec!["plugins", "plugins/cache", "settings.json"],
        "the JS test suite wrote into CLAUDE_CONFIG_DIR. `claudeHome()` is \
         `CLAUDE_CONFIG_DIR || homedir/.claude`, so the variable OUTRANKS the \
         redirected HOME and a developer who exports it had that write land in \
         their LIVE config. Fix it in the offending file the way its siblings \
         do: `delete process.env.CLAUDE_CONFIG_DIR` at module scope, before the \
         first `test(` — which is also what \
         `js_test_files_neutralize_claude_config_dir` scans for."
    );
    assert_eq!(
        fs::read_to_string(&canary_settings).unwrap_or_default(),
        "{\"canary\":true}",
        "the JS test suite rewrote {} in place. Same cause and same fix as \
         above; this assertion exists because an in-place rewrite leaves the \
         path listing identical.",
        canary_settings.display()
    );
}

/// The `ignored_arguments` disclosure tells an LLM caller "this argument did
/// nothing". That claim is sound only while the set of arguments the handlers
/// READ matches the set the published schema DECLARES, plus the exemptions in
/// `HONORED_UNDECLARED_ARGS` (src/mcp/server/mod.rs). A handler that starts
/// reading a new undeclared key silently breaks it — the pre-tag review of
/// v0.116.0 found two such keys already live (`function_name`, `skip_indexing`),
/// each of which the first version of the feature reported as ignored while it
/// was in force.
///
/// This pins the whole read-but-undeclared set. Adding one fails here until the
/// author decides which it is: declare it in the schema, exempt it in
/// `HONORED_UNDECLARED_ARGS`, or confirm its tool publishes no schema at all
/// (`confirm` / `min_lines` / `ignore_paths` are that third case — their tools
/// are skipped before the exemption list is consulted).
#[test]
fn test_no_new_undeclared_mcp_args() {
    fn read_keys(dir: &std::path::Path, out: &mut std::collections::BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                read_keys(&path, out);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let src = std::fs::read_to_string(&path).unwrap();
            for line in src.lines() {
                // Comments name these keys while EXPLAINING them (including the
                // one in advanced.rs that spells the bracket form verbatim), so a
                // whole-file scan reports prose as code.
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") || trimmed.starts_with('*') {
                    continue;
                }
                let bytes = line.as_bytes();
                // Match `args.get("key")`, `args["key"]`, and the typed accessors
                // `arg_*(args, "key", …)` that CON-15 introduced. The third form
                // is why this scan is written as three patterns rather than one:
                // it is TEXT, and when the accessor spelling changed under it the
                // pinned set silently lost `min_lines`. The `read.len() > 10`
                // floor below is what turned that into a red test.
                for (idx, _) in line.match_indices("args") {
                    let rest = &line[idx + 4..];
                    let key = if let Some(r) = rest.strip_prefix(".get(\"") {
                        r.split('"').next()
                    } else if let Some(r) = rest.strip_prefix("[\"") {
                        r.split('"').next()
                    } else if let Some(r) = rest.strip_prefix(", \"") {
                        // `arg_u64(args, "key", 20)` — only when `args` is itself
                        // preceded by `arg_…(`, so an unrelated `foo(args, "x")`
                        // is not mistaken for a caller-supplied key.
                        line[..idx]
                            .rfind("arg_")
                            .filter(|at| line[*at..idx].ends_with('('))
                            .and_then(|_| r.split('"').next())
                    } else {
                        None
                    };
                    // Skip identifiers ENDING in "args" (dead_args, dep_args …):
                    // those are locally-built payloads, not caller input.
                    let preceded_by_ident = idx > 0
                        && matches!(bytes[idx - 1], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_');
                    if let (Some(k), false) = (key, preceded_by_ident) {
                        out.insert(k.to_string());
                    }
                }
            }
        }
    }

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut read: std::collections::BTreeSet<String> = Default::default();
    read_keys(&root.join("src/mcp/server"), &mut read);
    assert!(
        read.len() > 10,
        "the scan found only {} argument reads — the match pattern has drifted \
         and this guard is now vacuous",
        read.len()
    );

    let tools_rs = std::fs::read_to_string(root.join("src/mcp/tools.rs")).unwrap();
    // Declared property names look like `"name": { "type":` in the json! schemas.
    let declared: std::collections::BTreeSet<String> = tools_rs
        .lines()
        .filter_map(|l| {
            let t = l.trim_start();
            let name = t.strip_prefix('"')?.split('"').next()?;
            let after = t.strip_prefix(&format!("\"{name}\""))?.trim_start();
            let after = after.strip_prefix(':')?.trim_start();
            after
                .strip_prefix('{')
                .filter(|r| r.trim_start().starts_with("\"type\""))
                .map(|_| name.to_string())
        })
        .collect();
    assert!(
        declared.contains("symbol_name") && declared.contains("query"),
        "schema-property scan drifted (got {} names) — guard would be vacuous",
        declared.len()
    );

    let undeclared: Vec<&str> = read
        .iter()
        .filter(|k| !declared.contains(*k))
        .map(|s| s.as_str())
        .collect();
    // Pinned set as of v0.116.0. `function_name` + `skip_indexing` are exempted
    // in HONORED_UNDECLARED_ARGS; the other three belong to schema-less tools.
    let expected = [
        "confirm",
        "function_name",
        "ignore_paths",
        // Same third case as its neighbours: `find_similar_code` publishes no
        // schema. Not a new argument — it was read as
        // `args.get("max_distance")` spread over three lines, which this
        // line-based scan could not see until CON-15 moved it to `arg_f64`.
        "max_distance",
        "min_lines",
        "skip_indexing",
    ];
    assert_eq!(
        undeclared, expected,
        "the set of MCP arguments read but not declared in the published schema \
         changed. Each entry must be one of: declared in src/mcp/tools.rs, listed \
         in HONORED_UNDECLARED_ARGS (src/mcp/server/mod.rs), or read only by a \
         tool that publishes no schema. Update this pin once classified."
    );
}

/// Every `apt-get` call in CI must be bounded by a step-level `timeout-minutes`.
///
/// On 2026-08-19 an apt mirror stopped answering mid-`update` and THREE
/// workflows stalled on it at once — `ci.yml`'s with-embed leg (40 min), the
/// `release.yml` gate (29 min, with the v0.122.0 tag already pushed) and
/// `cache-warm.yml` — each of which would have run to the 6-hour job ceiling
/// had they not been killed by hand. The repo had already lost one run that way.
///
/// The presence check those steps carry (`command -v rg`) does NOT bound this:
/// no GitHub runner ships ripgrep, as `ci.yml`'s own comment states, so on Linux
/// the check always falls through to apt. Only the timeout bounds it, which is
/// why this guard pins the timeout rather than the check.
///
/// Scans every workflow rather than a fixed list of steps: the same install was
/// copy-pasted into four places, and the fifth copy is the one this catches.
#[test]
fn every_apt_step_in_ci_is_bounded_by_a_timeout() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows");
    let mut unbounded = Vec::new();
    let mut scanned_steps = 0usize;

    for entry in std::fs::read_dir(&dir).expect("read .github/workflows") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_none_or(|e| e != "yml" && e != "yaml") {
            continue;
        }
        let src = std::fs::read_to_string(&path).expect("read workflow");
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        // Split into steps on the `- name:` / `- uses:` list markers, keeping
        // each step's own body with it.
        let mut step_start: Option<usize> = None;
        let lines: Vec<&str> = src.lines().collect();
        let mut steps: Vec<(usize, Vec<&str>)> = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim_start();
            if t.starts_with("- name:") || t.starts_with("- uses:") {
                if let Some(s) = step_start {
                    steps.push((s, lines[s..i].to_vec()));
                }
                step_start = Some(i);
            }
        }
        if let Some(s) = step_start {
            steps.push((s, lines[s..].to_vec()));
        }

        for (start, body) in steps {
            // Strip YAML comments before looking for the call: several of these
            // steps carry a comment explaining WHY apt is bounded, and counting
            // that as an apt call attributed the finding to whichever step the
            // comment happened to trail.
            let joined: String = body
                .iter()
                .filter(|l| !l.trim_start().starts_with('#'))
                .copied()
                .collect::<Vec<_>>()
                .join("\n");
            if !joined.contains("apt-get") {
                continue;
            }
            scanned_steps += 1;
            if !joined.contains("timeout-minutes:") {
                let label = body
                    .first()
                    .map(|l| l.trim().to_string())
                    .unwrap_or_default();
                unbounded.push(format!("{name}:{} {label}", start + 1));
            }
        }
    }

    assert!(
        scanned_steps >= 4,
        "found only {scanned_steps} apt step(s) — the scanner stopped matching the \
         workflow layout, so this guard is passing vacuously"
    );
    assert!(
        unbounded.is_empty(),
        "these CI steps run apt-get with no step-level timeout-minutes, so a \
         stalled mirror hangs the job to the 6-hour ceiling instead of failing \
         fast: {unbounded:?}"
    );
}

/// CON-08 (audit 2026-08-29): every numeric MCP parameter must be BOUNDED, not
/// merely floored.
///
/// `centrality_limit` was the only one without an upper bound — `.unwrap_or(10)
/// .max(1)` straight into `betweenness_centrality`, while every sibling clamps
/// (top_k / limit 1-100, depth 1-20 and 1-10, context_lines 0-100). Betweenness
/// is computed per call, so an unbounded limit is an unbounded computation, an
/// unbounded response and an unbounded render.
///
/// A parity table over an axis nothing was guarding. The axis is otherwise clean
/// today, which is exactly when the table is cheap: it costs one line per new
/// numeric parameter and refuses the next unbounded one.
///
/// Scanned per STATEMENT (from `args["name"]` to the terminating `;`) because the
/// clamp is written both inline and across four lines, and a line-based scan
/// would see only the inline half.
#[test]
fn every_numeric_mcp_argument_is_clamped() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let tools_dir = root.join("src/mcp/server/tools");

    // Numeric reads that are deliberately unbounded, each with the reason it is
    // safe. Keep this short: an entry here is a promise that the value cannot
    // size a computation, a response, or an allocation.
    const UNBOUNDED_BY_DESIGN: &[(&str, &str)] = &[
        // An identity, not a size: it selects one row by primary key. A huge
        // value finds nothing.
        ("advanced.rs", "node_id"),
        ("refs.rs", "node_id"),
        // A filter threshold. Raising it HIDES rows (`hidden_below_threshold`
        // discloses how many), so a large value shrinks the answer — with one
        // boundary this claim does not cover, found in pre-tag review and left
        // as-is because it predates and is untouched by this batch:
        // `find_dead_code` does `arg_u64(...)? as u32`, so `min_lines >= 2^32`
        // wraps toward 0 and returns MORE rows, not fewer. Still bounded by the
        // result set, so it sizes nothing new; recorded so the next reader does
        // not take the sentence above as unconditional.
        ("advanced.rs", "min_lines"),
        // The three below are NOT new sites. They were written as
        // `args.get("k").and_then(|v| v.as_i64())`, which this scan never matched
        // — it looked for `args["k"]` only — so they were unguarded and silent
        // until CON-15 moved them onto the typed accessors and the widened scan
        // surfaced them. Each is classified here rather than clamped:
        //
        // A similarity THRESHOLD, not a count. A large value accepts more
        // neighbours, but how many come back is `top_k`, which is clamped.
        ("advanced.rs", "max_distance"),
        // A filter threshold forwarded to `find_dead_code`'s `min_lines`, whose
        // own entry above records why raising it only shrinks the answer.
        ("overview.rs", "dead_min_lines"),
    ];

    let mut files: Vec<_> = std::fs::read_dir(&tools_dir)
        .expect("tools dir moved — update this guard")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .collect();
    files.sort();
    assert!(
        files.len() >= 5,
        "expected the tools module to hold several files, found {} — did the layout move?",
        files.len()
    );

    // Two spellings, because this is a TEXT scan and CON-15 changed the shape
    // under it: the old `args["key"].as_u64()` and the typed accessors
    // (`arg_u64(args, "key", default)`) that replaced it. The `checked` floor
    // below caught the drift — it dropped from 15 to 1 — which is exactly what
    // that floor exists for; without it this guard would have gone green while
    // scanning nothing. Both spellings stay listed: the raw form is still legal
    // Rust and a future handler could reach for it.
    let mut checked = 0usize;
    let mut unbounded: Vec<String> = Vec::new();
    // (file, key, tool) for every `arg_clamped` site, cross-checked against the
    // parsed COUNT_RANGES rows below.
    let mut clamped_calls: Vec<(String, String, String)> = Vec::new();
    for path in &files {
        let src = std::fs::read_to_string(path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().to_string();

        let mut record = |key: &str, stmt: &str| {
            checked += 1;
            let exempt = UNBOUNDED_BY_DESIGN
                .iter()
                .any(|(f, k)| *f == name.as_str() && *k == key);
            // `arg_clamped` IS the bound: it looks `(tool, key)` up in
            // `COUNT_RANGES` and clamps inside the helper, so requiring a literal
            // `.clamp(` in the statement would flag exactly the reads that moved
            // to the single-source table.
            let clamped_by_helper = stmt.contains("arg_clamped(");
            if !stmt.contains(".clamp(") && !clamped_by_helper && !exempt {
                unbounded.push(format!("{name}: {key}"));
            }
        };

        let mut rest = src.as_str();
        while let Some(i) = rest.find("args[\"") {
            let after = &rest[i + "args[\"".len()..];
            let Some(key_end) = after.find("\"]") else {
                break;
            };
            let key = &after[..key_end];
            let stmt_end = after.find(';').unwrap_or(after.len());
            let stmt = &after[..stmt_end];
            if stmt.contains(".as_u64()")
                || stmt.contains(".as_i64()")
                || stmt.contains(".as_f64()")
            {
                record(key, stmt);
            }
            rest = &after[key_end..];
        }

        for accessor in [
            "arg_u64(args, \"",
            "arg_f64(args, \"",
            // Included so the `node_id` entries in UNBOUNDED_BY_DESIGN stay live
            // rather than becoming dead exemptions for a shape nothing scans.
            "arg_opt_i64(args, \"",
            // `arg_clamped` carries no `.clamp(` in its statement — the bound
            // lives in `COUNT_RANGES` and is applied inside the helper, which is
            // the point of it. Counted here so the floor below stays meaningful,
            // and exempted from the `.clamp(` requirement just below.
            "arg_clamped(args, \"",
        ] {
            let mut rest = src.as_str();
            while let Some(i) = rest.find(accessor) {
                let after = &rest[i + accessor.len()..];
                let Some(key_end) = after.find('"') else {
                    break;
                };
                let key = &after[..key_end];
                let stmt_end = after.find(';').unwrap_or(after.len());
                // Accessor name prepended: `record` decides boundedness from the
                // statement text, and the helper that supplies the bound is named
                // in the accessor, not in what follows it.
                record(key, &format!("{accessor}{}", &after[..stmt_end]));
                // `arg_clamped(args, "key", "tool", …)` — capture the pair so the
                // row cross-check below can run without any runtime coverage.
                if accessor.starts_with("arg_clamped") {
                    if let Some(t) = after[key_end + 1..].trim_start().strip_prefix(", \"") {
                        if let Some(t_end) = t.find('"') {
                            clamped_calls.push((
                                name.clone(),
                                key.to_string(),
                                t[..t_end].to_string(),
                            ));
                        }
                    }
                }
                rest = &after[key_end..];
            }
        }
    }

    // Every `arg_clamped(args, "key", "tool", …)` must have a COUNT_RANGES row.
    // Without this the scan counts any `arg_clamped(` as bounded-by-construction and a
    // missing row is caught only if some test happens to exercise that tool at
    // runtime — while in a release build the helper silently falls back.
    {
        let helpers = std::fs::read_to_string(root.join("src/mcp/server/helpers.rs"))
            .expect("helpers.rs moved — update this guard");
        let table_start = helpers
            .find("pub(super) const COUNT_RANGES")
            .expect("COUNT_RANGES renamed or moved — update this guard");
        let table_end = helpers[table_start..]
            .find("\n];")
            .map(|i| table_start + i)
            .expect("COUNT_RANGES table shape changed — update this guard");
        let table = &helpers[table_start..table_end];
        // `("tool", "key",` — a comment containing `"applied": 20` cannot match,
        // the separator there is a colon.
        // Whitespace-tolerant: rows whose bound is an expression rather than a
        // literal are formatted across several lines, so `(` and the first string
        // are not adjacent.
        let mut rows: Vec<(String, String)> = Vec::new();
        let mut rest = table;
        while let Some(i) = rest.find('(') {
            let after = rest[i + 1..].trim_start();
            if let Some(a) = after.strip_prefix('"') {
                if let Some(a_end) = a.find('"') {
                    let tail = a[a_end + 1..].trim_start();
                    if let Some(t) = tail.strip_prefix(',') {
                        if let Some(b) = t.trim_start().strip_prefix('"') {
                            if let Some(b_end) = b.find('"') {
                                if b[b_end + 1..].trim_start().starts_with(',') {
                                    rows.push((a[..a_end].to_string(), b[..b_end].to_string()));
                                }
                            }
                        }
                    }
                }
            }
            rest = &rest[i + 1..];
        }
        assert!(
            rows.len() >= 11,
            "parsed only {} COUNT_RANGES rows — the table shape changed and this \
             cross-check is now looking at nothing",
            rows.len()
        );
        for (file, key, tool) in &clamped_calls {
            assert!(
                rows.iter().any(|(t, k)| t == tool && k == key),
                "{file}: arg_clamped(.., \"{key}\", \"{tool}\", ..) has no COUNT_RANGES row — \
                 it would fall back to the default in release and disclose nothing"
            );
        }
        assert!(
            !clamped_calls.is_empty(),
            "no arg_clamped call sites found — the accessor spelling changed"
        );
    }

    assert!(
        checked >= 8,
        "the scan found only {checked} numeric argument reads — the accessor spelling probably \
         changed and this guard is now looking at nothing"
    );
    assert!(
        unbounded.is_empty(),
        "numeric MCP argument(s) with no upper bound: {unbounded:?}. Add `.clamp(lo, hi)` in the \
         handler, or list the site in UNBOUNDED_BY_DESIGN with the reason it cannot size a \
         computation, a response, or an allocation."
    );
}

/// Nothing the full-index pipeline can reach may issue its own `BEGIN`.
///
/// `rebuild_index` (`src/mcp/server/mod.rs`) wraps the ENTIRE full index in one
/// `unchecked_transaction`, so every writer below it must nest.
/// `Connection::unchecked_transaction` always issues a bare `BEGIN`, which under
/// that wrapper is "cannot start a transaction within a transaction" and rolls
/// the whole rebuild back — not just the one batch. `Database::savepoint` /
/// `storage::db::savepoint_on` nest, and behave identically when the same code
/// runs standalone (SQLite auto-starts a transaction for an outermost
/// SAVEPOINT).
///
/// Nine sites were converted for audit 2026-09-05 CORE-02. This guard exists
/// because converting them without one leaves the tenth to reintroduce the bug
/// in silence — the exact shape NEW-01 flagged for the `--locked` sweep. One of
/// the nine (`ensure_embedding_cache_valid`) was NOT in the audit report's list
/// and was found only by walking the call graph, which is the other reason a
/// hand-maintained list is not the guard.
///
/// Scope is the two layers that run under the wrapper. `src/storage/db.rs` owns
/// the migration/open path and `mcp/server` owns the wrapper itself; both are
/// legitimately outermost and are out of scope here rather than allowlisted.
#[test]
fn pipeline_and_query_layers_never_begin_their_own_transaction() {
    use std::path::Path;
    fn collect_rs(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for e in fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
            let p = e.unwrap().path();
            if p.is_dir() {
                collect_rs(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }

    let mut files = Vec::new();
    for root in ["src/indexer", "src/storage/queries"] {
        collect_rs(Path::new(root), &mut files);
    }
    files.sort();
    assert!(
        files.len() >= 10,
        "only {} files scanned — the roots moved and this guard went inert",
        files.len()
    );

    // Positive control: a scanner that silently matches nothing (a renamed
    // method, a `code_only` change) would pass this test forever.
    assert!(
        code_only("        let tx = conn.unchecked_transaction()?;")
            .contains(".unchecked_transaction()"),
        "the matcher no longer recognises the call it exists to find"
    );

    let mut offenders = Vec::new();
    for path in &files {
        // Whole-file test modules (`#[cfg(test)] mod tests;` in the parent) have
        // no in-file `#[cfg(test)]` to stop at, and they legitimately open an
        // OUTER transaction to prove the nesting works.
        if path.file_name().is_some_and(|f| f == "tests.rs") {
            continue;
        }
        let src = fs::read_to_string(path).unwrap();
        // Test modules sit at the end of these files and legitimately open an
        // OUTER transaction to prove the nesting works. Stop at the first
        // `#[cfg(test)]`; `code_only` drops comments and string bodies so a
        // doc-comment mentioning the call is not an offender.
        let body = match src.find("#[cfg(test)]") {
            Some(i) => &src[..i],
            None => &src[..],
        };
        for (n, line) in body.lines().enumerate() {
            if code_only(line).contains(".unchecked_transaction()") {
                offenders.push(format!("{}:{}", path.display(), n + 1));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "these run under rebuild_index's transaction and would abort it with a bare BEGIN.\n\
         Use `db.savepoint(\"sp_...\")`, or `storage::db::savepoint_on(conn, \"sp_...\")` \
         where only a &Connection is held:\n  {}",
        offenders.join("\n  ")
    );
}
