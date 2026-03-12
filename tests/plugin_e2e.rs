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
    assert_eq!(tools.len(), 14, "expected 14 tools, got {}", tools.len());

    // Verify each tool has name, description, and inputSchema
    for tool in tools {
        assert!(tool["name"].is_string(), "tool missing name");
        assert!(tool["description"].is_string(), "tool missing description");
        assert!(tool["inputSchema"].is_object(), "tool missing inputSchema");
    }

    drop(stdin);
    let _ = child.wait();
}
