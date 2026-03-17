use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use tempfile::TempDir;

/// Path to the compiled binary, set by `cargo test`.
fn binary_path() -> String {
    env!("CARGO_BIN_EXE_code-graph-mcp").to_string()
}

/// Spawn the MCP server process with stdin/stdout piped.
/// `cwd` sets the working directory (the server indexes from cwd).
fn spawn_server(cwd: &std::path::Path) -> std::process::Child {
    Command::new(binary_path())
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn code-graph-mcp binary")
}

/// Send a JSON-RPC message followed by newline.
fn send(stdin: &mut impl Write, msg: &str) {
    writeln!(stdin, "{}", msg).expect("failed to write to stdin");
    stdin.flush().expect("failed to flush stdin");
}

/// Read one JSON-RPC response line with timeout.
/// Returns None on timeout.
fn read_with_timeout(rx: &mpsc::Receiver<String>, timeout: Duration) -> Option<String> {
    rx.recv_timeout(timeout).ok()
}

/// Read a JSON-RPC response (with "id" field), skipping any notifications.
/// MCP servers may send `notifications/progress` or `notifications/message`
/// interleaved with responses; this function filters them out.
fn read_response(rx: &mpsc::Receiver<String>, timeout: Duration) -> Option<String> {
    let start = std::time::Instant::now();
    loop {
        let remaining = timeout.checked_sub(start.elapsed())?;
        let line = rx.recv_timeout(remaining).ok()?;
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
        // JSON-RPC responses have "id"; notifications don't
        if parsed.get("id").is_some() {
            return Some(line);
        }
        // else: notification, skip and read next line
    }
}

/// Spawn a background reader thread that sends lines to the channel.
fn spawn_reader(stdout: std::process::ChildStdout) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            match line {
                Ok(l) if !l.trim().is_empty() => {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
                _ => break,
            }
        }
    });
    rx
}

const TIMEOUT: Duration = Duration::from_secs(30);

fn initialize_msg() -> String {
    serde_json::json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "plugin-e2e-test", "version": "0.1" }
        }
    })
    .to_string()
}

fn jsonrpc_request(id: u64, method: &str, params: serde_json::Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0", "id": id,
        "method": method, "params": params
    })
    .to_string()
}

fn tool_call_msg(id: u64, tool_name: &str, args: serde_json::Value) -> String {
    jsonrpc_request(
        id,
        "tools/call",
        serde_json::json!({
            "name": tool_name,
            "arguments": args
        }),
    )
}

#[test]
fn test_stdio_initialize_and_tools_list() {
    let project = TempDir::new().unwrap();
    let mut child = spawn_server(project.path());
    let mut stdin = child.stdin.take().unwrap();
    let rx = spawn_reader(child.stdout.take().unwrap());

    // Initialize
    send(&mut stdin, &initialize_msg());
    let resp = read_with_timeout(&rx, TIMEOUT).expect("no response to initialize");
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(parsed["result"]["protocolVersion"], "2024-11-05");

    // tools/list
    send(
        &mut stdin,
        &jsonrpc_request(2, "tools/list", serde_json::json!({})),
    );
    let resp = read_with_timeout(&rx, TIMEOUT).expect("no response to tools/list");
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    let tools = parsed["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), code_graph_mcp::mcp::tools::TOOL_COUNT,
        "expected {} tools, got {}", code_graph_mcp::mcp::tools::TOOL_COUNT, tools.len());

    // Verify each tool has name, description, and inputSchema
    for tool in tools {
        assert!(tool["name"].is_string(), "tool missing name");
        assert!(tool["description"].is_string(), "tool missing description");
        assert!(tool["inputSchema"].is_object(), "tool missing inputSchema");
    }

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn test_stdio_full_workflow() {
    let project = TempDir::new().unwrap();
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    std::fs::write(project.path().join("src/auth.ts"), r#"
function validateToken(token: string): boolean {
    return token.length > 0;
}

function handleLogin(req: Request) {
    if (validateToken(req.token)) {
        return { ok: true };
    }
}
"#).unwrap();

    let mut child = spawn_server(project.path());
    let mut stdin = child.stdin.take().unwrap();
    let rx = spawn_reader(child.stdout.take().unwrap());

    // Initialize
    send(&mut stdin, &initialize_msg());
    let _ = read_with_timeout(&rx, TIMEOUT).expect("no response to initialize");

    // semantic_code_search — triggers indexing (may produce progress notifications)
    send(&mut stdin, &tool_call_msg(2, "semantic_code_search", serde_json::json!({
        "query": "validateToken", "top_k": 5
    })));
    let resp = read_response(&rx, TIMEOUT).expect("no response to semantic_code_search");
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    let text = parsed["result"]["content"][0]["text"].as_str().unwrap();
    let results: serde_json::Value = serde_json::from_str(text).unwrap();
    let arr = results.as_array().unwrap();
    assert!(!arr.is_empty(), "search should return results");

    // get_ast_node
    send(&mut stdin, &tool_call_msg(3, "get_ast_node", serde_json::json!({
        "file_path": "src/auth.ts",
        "symbol_name": "validateToken"
    })));
    let resp = read_response(&rx, TIMEOUT).expect("no response to get_ast_node");
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    let text = parsed["result"]["content"][0]["text"].as_str().unwrap();
    let node: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(node["name"], "validateToken");
    let node_id = node["node_id"].as_i64().unwrap();

    // read_snippet
    send(&mut stdin, &tool_call_msg(4, "read_snippet", serde_json::json!({
        "node_id": node_id
    })));
    let resp = read_response(&rx, TIMEOUT).expect("no response to read_snippet");
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    let text = parsed["result"]["content"][0]["text"].as_str().unwrap();
    let snippet: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(snippet["name"], "validateToken");
    assert!(snippet["code_content"].as_str().unwrap().contains("token"));

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn test_stdio_protocol_endpoints() {
    let project = TempDir::new().unwrap();
    let mut child = spawn_server(project.path());
    let mut stdin = child.stdin.take().unwrap();
    let rx = spawn_reader(child.stdout.take().unwrap());

    // Initialize
    send(&mut stdin, &initialize_msg());
    let _ = read_with_timeout(&rx, TIMEOUT).unwrap();

    // resources/list
    send(&mut stdin, &jsonrpc_request(2, "resources/list", serde_json::json!({})));
    let resp = read_with_timeout(&rx, TIMEOUT).expect("no response to resources/list");
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    let resources = parsed["result"]["resources"].as_array().unwrap();
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0]["uri"], "code-graph://project-summary");

    // resources/read
    send(&mut stdin, &jsonrpc_request(3, "resources/read",
        serde_json::json!({"uri": "code-graph://project-summary"})));
    let resp = read_with_timeout(&rx, TIMEOUT).expect("no response to resources/read");
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    let text = parsed["result"]["contents"][0]["text"].as_str().unwrap();
    let summary: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(summary["schema_version"].is_number());

    // prompts/list
    send(&mut stdin, &jsonrpc_request(4, "prompts/list", serde_json::json!({})));
    let resp = read_with_timeout(&rx, TIMEOUT).expect("no response to prompts/list");
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    let prompts = parsed["result"]["prompts"].as_array().unwrap();
    assert_eq!(prompts.len(), 3);

    // prompts/get
    send(&mut stdin, &jsonrpc_request(5, "prompts/get", serde_json::json!({
        "name": "impact-analysis",
        "arguments": { "symbol_name": "foo" }
    })));
    let resp = read_with_timeout(&rx, TIMEOUT).expect("no response to prompts/get");
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    let text = parsed["result"]["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(text.contains("foo"));
    assert!(text.contains("impact_analysis"));

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn test_stdio_malformed_input() {
    let project = TempDir::new().unwrap();
    let mut child = spawn_server(project.path());
    let mut stdin = child.stdin.take().unwrap();
    let rx = spawn_reader(child.stdout.take().unwrap());

    // Send malformed JSON
    send(&mut stdin, "this is not valid json{{{");
    let resp = read_with_timeout(&rx, TIMEOUT).expect("no response to malformed input");
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert!(parsed["error"].is_object());
    assert_eq!(parsed["error"]["code"], -32700, "expected parse error code");

    // Verify process still works after error
    send(&mut stdin, &initialize_msg());
    let resp = read_with_timeout(&rx, TIMEOUT).expect("process should still respond after parse error");
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(parsed["result"]["protocolVersion"], "2024-11-05");

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn test_stdio_graceful_eof() {
    let project = TempDir::new().unwrap();
    let mut child = spawn_server(project.path());

    // Immediately drop stdin to send EOF
    drop(child.stdin.take());

    // Wait for process to exit with timeout
    let (tx, rx) = mpsc::channel();
    let mut child_for_thread = child;
    std::thread::spawn(move || {
        let status = child_for_thread.wait();
        let _ = tx.send(status);
    });

    let status = rx.recv_timeout(Duration::from_secs(5))
        .expect("process should exit within 5 seconds on EOF")
        .expect("wait should succeed");

    assert!(status.success(), "process should exit cleanly on EOF, got: {:?}", status);
}

#[test]
fn test_stdio_unknown_tool() {
    let project = TempDir::new().unwrap();
    let mut child = spawn_server(project.path());
    let mut stdin = child.stdin.take().unwrap();
    let rx = spawn_reader(child.stdout.take().unwrap());

    send(&mut stdin, &initialize_msg());
    let _ = read_with_timeout(&rx, TIMEOUT).unwrap();

    // Call nonexistent tool
    send(&mut stdin, &tool_call_msg(2, "nonexistent_tool", serde_json::json!({})));
    let resp = read_with_timeout(&rx, TIMEOUT).expect("no response to unknown tool");
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();

    // Server wraps unknown tool in MCP content with isError: true
    assert!(parsed["result"]["isError"].as_bool().unwrap_or(false),
        "unknown tool should have isError: true");
    let text = parsed["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Error"), "should contain error message, got: {}", text);

    drop(stdin);
    let _ = child.wait();
}
