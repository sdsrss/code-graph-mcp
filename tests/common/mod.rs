//! Shared test utilities for integration/hardening tests.
//!
//! Provides helpers for building JSON-RPC tool calls and parsing responses.

#![allow(dead_code)]

use code_graph_mcp::mcp::server::McpServer;
use tempfile::TempDir;

/// Build a JSON-RPC 2.0 `tools/call` request string.
pub fn tool_call_json(tool_name: &str, args: serde_json::Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": tool_name,
            "arguments": args
        }
    })
    .to_string()
}

/// Extract the parsed tool result from a JSON-RPC response.
///
/// Assumes the response wraps the tool output as a JSON string inside
/// `result.content[0].text`.
pub fn parse_tool_result(response: &Option<String>) -> serde_json::Value {
    let resp = response.as_ref().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(resp).unwrap();
    let text = parsed["result"]["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(text).unwrap()
}

/// Create an McpServer from a TempDir project root and send the `initialize` handshake.
pub fn init_server(project: &TempDir) -> McpServer {
    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();
    server
}
