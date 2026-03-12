use std::fs;
use tempfile::TempDir;

use code_graph_mcp::mcp::server::McpServer;

fn tool_call_json(tool_name: &str, args: serde_json::Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": tool_name,
            "arguments": args
        }
    }).to_string()
}

fn parse_tool_result(response: &Option<String>) -> serde_json::Value {
    let resp = response.as_ref().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(resp).unwrap();
    let text = parsed["result"]["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(text).unwrap()
}

#[test]
fn test_e2e_index_and_search() {
    let project = TempDir::new().unwrap();

    // Create a realistic project structure
    fs::create_dir_all(project.path().join("src/auth")).unwrap();
    fs::create_dir_all(project.path().join("src/api")).unwrap();

    fs::write(project.path().join("src/auth/token.ts"), r#"
import jwt from 'jsonwebtoken';

export function validateToken(token: string): boolean {
    const decoded = jwt.verify(token, process.env.SECRET);
    return decoded !== null;
}
"#).unwrap();

    fs::write(project.path().join("src/api/login.ts"), r#"
import { validateToken } from '../auth/token';

export function handleLogin(req: Request, res: Response) {
    const user = validateToken(req.headers.authorization);
    if (!user) { res.status(401); return; }
    res.json({ userId: user.id });
}
"#).unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();

    // Initialize
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();

    // Search for auth-related code
    let search = tool_call_json("semantic_code_search", serde_json::json!({"query": "validateToken", "top_k": 3}));
    let resp = server.handle_message(&search).unwrap();
    let results = parse_tool_result(&resp);
    let results_arr = results.as_array().unwrap();
    assert!(!results_arr.is_empty(), "search should find results");
    let names: Vec<&str> = results_arr.iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    assert!(names.contains(&"validateToken"), "got names: {:?}", names);

    // Get call graph for handleLogin
    let graph = tool_call_json("get_call_graph", serde_json::json!({
        "function_name": "handleLogin",
        "direction": "callees",
        "depth": 2
    }));
    let resp = server.handle_message(&graph).unwrap();
    let result = parse_tool_result(&resp);
    assert_eq!(result["function"], "handleLogin");

    // Get index status
    let status = tool_call_json("get_index_status", serde_json::json!({}));
    let resp = server.handle_message(&status).unwrap();
    let result = parse_tool_result(&resp);
    assert!(result["files_count"].as_i64().unwrap() >= 2, "should have indexed at least 2 files");
    assert!(result["nodes_count"].as_i64().unwrap() >= 2, "should have at least 2 nodes");

    // Get AST node
    let ast = tool_call_json("get_ast_node", serde_json::json!({
        "file_path": "src/auth/token.ts",
        "symbol_name": "validateToken",
        "include_references": true
    }));
    let resp = server.handle_message(&ast).unwrap();
    let result = parse_tool_result(&resp);
    assert_eq!(result["name"], "validateToken");
    assert!(result["code_content"].as_str().unwrap().contains("verify"));

    // Read snippet for a node
    let node_id = result["node_id"].as_i64().unwrap();
    let snippet = tool_call_json("read_snippet", serde_json::json!({
        "node_id": node_id,
        "context_lines": 2
    }));
    let resp = server.handle_message(&snippet).unwrap();
    let result = parse_tool_result(&resp);
    assert_eq!(result["name"], "validateToken");
    assert!(result["code"].as_str().unwrap().contains("verify"));

    // Rebuild index
    let rebuild = tool_call_json("rebuild_index", serde_json::json!({"confirm": true}));
    let resp = server.handle_message(&rebuild).unwrap();
    let result = parse_tool_result(&resp);
    assert_eq!(result["status"], "rebuilt");
    assert!(result["files_indexed"].as_i64().unwrap() >= 2);

    // .code-graph directory should exist
    assert!(project.path().join(".code-graph/index.db").exists());
}

#[test]
fn test_e2e_express_route_discovery() {
    let project = TempDir::new().unwrap();

    fs::write(project.path().join("server.ts"), r#"
function handleLogin(req: Request, res: Response) {
    res.json({ ok: true });
}

function getUsers(req: Request, res: Response) {
    res.json([]);
}

app.post('/api/login', handleLogin);
app.get('/api/users', getUsers);
"#).unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();

    // Initialize and trigger indexing
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();

    // Find route
    let route = tool_call_json("find_http_route", serde_json::json!({
        "route_path": "/api/login"
    }));
    let resp = server.handle_message(&route).unwrap();
    let result = parse_tool_result(&resp);
    assert_eq!(result["route"], "/api/login");
    let handlers = result["handlers"].as_array().unwrap();
    assert!(!handlers.is_empty(), "should find route handler");
}

#[test]
fn test_e2e_incremental_reindex() {
    let project = TempDir::new().unwrap();

    // Initial file
    fs::write(project.path().join("app.ts"), "function original() {}").unwrap();
    let server = McpServer::from_project_root(project.path()).unwrap();

    // Trigger full index
    let status = tool_call_json("get_index_status", serde_json::json!({}));
    let _ = server.handle_message(&status).unwrap();

    // Search for original
    let search = tool_call_json("semantic_code_search", serde_json::json!({"query": "original"}));
    let resp = server.handle_message(&search).unwrap();
    let result = parse_tool_result(&resp);
    assert!(!result.as_array().unwrap().is_empty());

    // Modify file
    fs::write(project.path().join("app.ts"), "function modified() {}").unwrap();

    // Search again (triggers incremental index)
    let search = tool_call_json("semantic_code_search", serde_json::json!({"query": "modified"}));
    let resp = server.handle_message(&search).unwrap();
    let result = parse_tool_result(&resp);
    let names: Vec<&str> = result.as_array().unwrap().iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    assert!(names.contains(&"modified"), "should find modified function, got: {:?}", names);
}

#[test]
fn test_e2e_full_protocol_lifecycle() {
    let project = TempDir::new().unwrap();
    fs::write(project.path().join("app.ts"), r#"
function greet(name: string): string {
    return "hello " + name;
}
"#).unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();

    // 1. initialize
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    let resp = server.handle_message(init).unwrap().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(parsed["result"]["protocolVersion"], "2024-11-05");
    assert!(parsed["result"]["capabilities"]["tools"].is_object());

    // 2. notifications/initialized — returns None (no response)
    let notif = r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#;
    let resp = server.handle_message(notif).unwrap();
    assert!(resp.is_none());

    // 3. tools/list — 14 tools
    let msg = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
    let resp = server.handle_message(msg).unwrap().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    let tools = parsed["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 14);

    // 4. resources/list — 1 resource
    let msg = r#"{"jsonrpc":"2.0","id":3,"method":"resources/list","params":{}}"#;
    let resp = server.handle_message(msg).unwrap().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    let resources = parsed["result"]["resources"].as_array().unwrap();
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0]["uri"], "code-graph://project-summary");

    // 5. prompts/list — 3 prompts
    let msg = r#"{"jsonrpc":"2.0","id":4,"method":"prompts/list","params":{}}"#;
    let resp = server.handle_message(msg).unwrap().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    let prompts = parsed["result"]["prompts"].as_array().unwrap();
    assert_eq!(prompts.len(), 3);
    let names: Vec<&str> = prompts.iter().filter_map(|p| p["name"].as_str()).collect();
    assert!(names.contains(&"impact-analysis"));
    assert!(names.contains(&"understand-module"));
    assert!(names.contains(&"trace-request"));

    // 6. prompts/get for each prompt
    for (name, arg_name, arg_val, expected_text) in [
        ("impact-analysis", "symbol_name", "greet", "impact_analysis"),
        ("understand-module", "path", "app.ts", "module_overview"),
        ("trace-request", "route", "/api/users", "trace_http_chain"),
    ] {
        let msg = serde_json::json!({
            "jsonrpc": "2.0", "id": 5, "method": "prompts/get",
            "params": { "name": name, "arguments": { arg_name: arg_val } }
        }).to_string();
        let resp = server.handle_message(&msg).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        let text = parsed["result"]["messages"][0]["content"]["text"].as_str().unwrap();
        assert!(text.contains(expected_text),
            "prompt '{}' should mention '{}', got: {}", name, expected_text, text);
    }

    // 7. resources/read
    let msg = r#"{"jsonrpc":"2.0","id":6,"method":"resources/read","params":{"uri":"code-graph://project-summary"}}"#;
    let resp = server.handle_message(msg).unwrap().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    let text = parsed["result"]["contents"][0]["text"].as_str().unwrap();
    let summary: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(summary["schema_version"].is_number());

    // 8. tool call — triggers indexing
    let search = tool_call_json("semantic_code_search", serde_json::json!({"query": "greet"}));
    let resp = server.handle_message(&search).unwrap();
    let result = parse_tool_result(&resp);
    assert!(result.is_array());

    // 9. ping
    let msg = r#"{"jsonrpc":"2.0","id":7,"method":"ping","params":{}}"#;
    let resp = server.handle_message(msg).unwrap().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert!(parsed["result"].is_object());
}

#[test]
fn test_e2e_resources_read() {
    let project = TempDir::new().unwrap();
    fs::write(project.path().join("a.ts"), "function a() { return 1; }").unwrap();
    fs::write(project.path().join("b.ts"), "function b() { return 2; }").unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();

    // Trigger indexing via search
    let search = tool_call_json("semantic_code_search", serde_json::json!({"query": "function"}));
    let _ = server.handle_message(&search).unwrap();

    // Read project summary
    let msg = r#"{"jsonrpc":"2.0","id":1,"method":"resources/read","params":{"uri":"code-graph://project-summary"}}"#;
    let resp = server.handle_message(msg).unwrap().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    let text = parsed["result"]["contents"][0]["text"].as_str().unwrap();
    let summary: serde_json::Value = serde_json::from_str(text).unwrap();

    assert!(summary["files"].as_i64().unwrap() >= 2, "should have at least 2 files indexed");
    assert!(summary["nodes"].as_i64().unwrap() >= 2, "should have at least 2 nodes");
    assert!(summary["schema_version"].as_i64().unwrap() >= 1);
}

#[test]
fn test_e2e_prompts_get_all() {
    let project = TempDir::new().unwrap();
    let server = McpServer::from_project_root(project.path()).unwrap();

    let cases = vec![
        ("impact-analysis", "symbol_name", "handleLogin", "impact_analysis"),
        ("understand-module", "path", "src/auth/", "module_overview"),
        ("trace-request", "route", "/api/users", "trace_http_chain"),
    ];

    for (name, arg_name, arg_val, expected_substr) in cases {
        let msg = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "prompts/get",
            "params": { "name": name, "arguments": { arg_name: arg_val } }
        }).to_string();
        let resp = server.handle_message(&msg).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        let messages = parsed["result"]["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        let text = messages[0]["content"]["text"].as_str().unwrap();
        assert!(text.contains(arg_val),
            "prompt '{}' message should contain argument '{}', got: {}", name, arg_val, text);
        assert!(text.contains(expected_substr),
            "prompt '{}' message should reference tool '{}', got: {}", name, expected_substr, text);
    }
}
