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

/// Layering drift-guard: the storage layer must never import from the graph
/// layer — graph depends on storage, not the reverse. M9a moved the one
/// offending orchestration (`get_callers_with_route_info`) up into
/// `src/graph/routes.rs`. Re-introducing `use crate::graph` anywhere under
/// src/storage/ recreates the cycle this test exists to forbid.
#[test]
fn no_storage_module_imports_graph() {
    use std::fs;
    use std::path::Path;
    let mut offenders = Vec::new();
    fn walk(dir: &Path, offenders: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, offenders);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                let src = fs::read_to_string(&path).unwrap();
                for (i, line) in src.lines().enumerate() {
                    // Strip line/doc comments so a comment MENTIONING crate::graph
                    // (e.g. "orchestration lives in `crate::graph::routes`") is not
                    // a false offender — only real code imports count.
                    let code = line.split("//").next().unwrap_or("");
                    let t = code.trim_start();
                    if t.starts_with("use crate::graph") || t.contains("crate::graph::") {
                        offenders.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
                    }
                }
            }
        }
    }
    walk(Path::new("src/storage"), &mut offenders);
    assert!(
        offenders.is_empty(),
        "storage must not import graph (cycle). Offenders:\n{}",
        offenders.join("\n")
    );
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
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let sites: [(&str, &str, &str); 2] = [
        (
            "src/mcp/server/tools/advanced.rs",
            r#"args.get("ignore_paths")"#,
            "normalize_path_arg",
        ),
        (
            "src/cli.rs",
            "let ignore_prefixes: Vec<String>",
            "normalize_rel_str",
        ),
    ];

    for (file, anchor_text, expected) in sites {
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
            block.contains(expected),
            "dead-code ignore prefixes reach the index without separator \
             normalization ({file}:{}). They are matched with starts_with against \
             `/`-stored paths, so a backslash-spelled prefix excludes nothing and \
             the tool OVER-reports dead code.\n{block}",
            anchor + 1
        );
    }
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
    let gate_builds: Vec<&str> = gate
        .iter()
        .filter_map(|l| l.strip_prefix("run: cargo "))
        .filter(|c| !c.starts_with("fmt"))
        .collect();
    assert!(
        !gate_builds.is_empty(),
        "no `run: cargo` steps found in release.yml `gate` — the job was \
         restructured and this guard no longer reads it"
    );

    // 4a. The gate's own contract (audit 2026-07-27 P1-3). Warming a cache for a
    //     job that no longer checks anything is worse than no gate at all: the
    //     pipeline still reports a green "Gate" and publishes to npm, which is
    //     irreversible. Each of these was a specific hole the audit found.
    for (needle, why) in [
        ("run: cargo fmt --check", "formatting"),
        (
            "run: cargo clippy --no-default-features --all-targets -- -D warnings",
            "clippy on the default feature set",
        ),
        (
            "run: cargo clippy --features embed-model --all-targets -- -D warnings",
            "clippy on the feature set release binaries actually ship",
        ),
        (
            "run: cargo test --features embed-model",
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

    for cmd in gate_builds {
        assert!(
            warm_gate.iter().any(|l| *l == format!("run: cargo {cmd}")),
            "release.yml `gate` runs `cargo {cmd}` but cache-warm.yml `warm-gate` \
             does not. Lint and feature flags participate in cargo's fingerprint, \
             so a command only the gate runs is a command the gate compiles cold."
        );
    }
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
    files.sort();
    assert!(
        !files.is_empty(),
        "no workflow files found under {} — this guard would pass vacuously",
        dir.display()
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
        checked >= 41,
        "expected at least the 41 known `uses:` sites across the workflows, found \
         {checked} — a parse change would make this guard vacuous. If a workflow \
         or step was legitimately removed, lower this in the same commit."
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
#[test]
fn js_test_files_neutralize_claude_config_dir() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    for dir in ["claude-plugin/scripts", "scripts"] {
        let d = root.join(dir);
        let mut found: Vec<_> = fs::read_dir(&d)
            .unwrap_or_else(|e| panic!("read {}: {e}", d.display()))
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(".test.js"))
            })
            .collect();
        found.sort();
        files.append(&mut found);
    }
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
            // A `process.env.HOME = …` immediately followed by a
            // `process.env.CLAUDE_CONFIG_DIR = …` is the explicit-pairing form
            // (install-e2e's generated child script uses it), so look one line
            // ahead before calling it an offender.
            let paired_next = lines
                .get(i + 1)
                .is_some_and(|n| n.contains("process.env.CLAUDE_CONFIG_DIR"));
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
                offenders.push(format!(
                    "{}:{}: {}",
                    path.file_name().unwrap().to_string_lossy(),
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
                // Match `args.get("key")` and `args["key"]`.
                for (idx, _) in line.match_indices("args") {
                    let rest = &line[idx + 4..];
                    let key = if let Some(r) = rest.strip_prefix(".get(\"") {
                        r.split('"').next()
                    } else if let Some(r) = rest.strip_prefix("[\"") {
                        r.split('"').next()
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
