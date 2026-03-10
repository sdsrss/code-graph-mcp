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
