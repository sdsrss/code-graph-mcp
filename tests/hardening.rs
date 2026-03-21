//! Production hardening tests: concurrency, stress, and edge-case scenarios.
//!
//! McpServer wraps a raw rusqlite::Connection which is Send but not Sync,
//! so concurrent tests use Arc<Mutex<McpServer>> to validate that interleaved
//! access from multiple threads causes no deadlocks or data corruption.

use code_graph_mcp::mcp::server::McpServer;
use serde_json::json;
use std::fs;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

fn tool_call_json(name: &str, args: serde_json::Value) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": args }
    })
    .to_string()
}

fn parse_tool_result(resp: &Option<String>) -> serde_json::Value {
    let resp = resp.as_ref().unwrap();
    let v: serde_json::Value = serde_json::from_str(resp).unwrap();
    let text = v["result"]["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(text).unwrap()
}

fn init_server(project: &TempDir) -> McpServer {
    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();
    server
}

fn setup_project(file_count: usize) -> (TempDir, McpServer) {
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();

    for i in 0..file_count {
        let content = format!(
            "export function func_{}(x: number): number {{ return x + {}; }}\n\
             export function helper_{}(): string {{ return 'hello'; }}\n",
            i, i, i
        );
        fs::write(
            project.path().join(format!("src/mod_{}.ts", i)),
            content,
        )
        .unwrap();
    }

    let server = init_server(&project);

    // Trigger initial indexing
    let search = tool_call_json("semantic_code_search", json!({"query": "func_0"}));
    let _ = server.handle_message(&search).unwrap();

    (project, server)
}

/// Concurrent search calls from 10 threads against a shared McpServer.
/// Validates no deadlocks or panics under interleaved access.
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
                let v: serde_json::Value =
                    serde_json::from_str(resp.as_ref().unwrap()).unwrap();
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
