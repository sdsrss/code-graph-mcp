use std::fs;
use tempfile::TempDir;

use code_graph_mcp::mcp::server::McpServer;
use code_graph_mcp::storage::db::Database;
use code_graph_mcp::storage::queries::*;

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

    // 3. tools/list
    let msg = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
    let resp = server.handle_message(msg).unwrap().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    let tools = parsed["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), code_graph_mcp::mcp::tools::TOOL_COUNT);

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

#[test]
fn test_e2e_resources_read_unknown_uri() {
    let project = TempDir::new().unwrap();
    let server = McpServer::from_project_root(project.path()).unwrap();
    let msg = r#"{"jsonrpc":"2.0","id":1,"method":"resources/read","params":{"uri":"code-graph://nonexistent"}}"#;
    let resp = server.handle_message(msg).unwrap().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert!(parsed["error"].is_object());
    assert_eq!(parsed["error"]["code"], -32602);
    assert!(parsed["error"]["message"].as_str().unwrap().contains("Unknown resource URI"));
}

#[test]
fn test_e2e_impact_analysis() {
    let project = TempDir::new().unwrap();

    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(project.path().join("src/utils.ts"), r#"
export function formatDate(d: Date): string {
    return d.toISOString();
}
"#).unwrap();

    fs::write(project.path().join("src/service.ts"), r#"
import { formatDate } from './utils';

export function createReport(data: any) {
    return { date: formatDate(new Date()), data };
}

export function createLog(msg: string) {
    return formatDate(new Date()) + ': ' + msg;
}
"#).unwrap();

    fs::write(project.path().join("src/handler.ts"), r#"
import { createReport } from './service';

export function handleRequest(req: Request, res: Response) {
    const report = createReport(req.body);
    res.json(report);
}
"#).unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();

    // Trigger indexing
    let search = tool_call_json("semantic_code_search", serde_json::json!({"query": "formatDate"}));
    let _ = server.handle_message(&search).unwrap();

    // impact_analysis on formatDate — should have callers
    let msg = tool_call_json("impact_analysis", serde_json::json!({
        "symbol_name": "formatDate",
        "change_type": "signature",
        "depth": 3
    }));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    assert_eq!(result["symbol"], "formatDate");
    assert_eq!(result["change_type"], "signature");
    assert!(result["risk_level"].is_string(), "should have risk_level");
    assert!(result["summary"].as_str().unwrap().contains("formatDate"));
    assert!(result["direct_callers"].is_array());
    assert!(result["transitive_callers"].is_array());
    assert!(result["affected_files"].is_number());

    // Direct callers should include createReport and createLog
    let direct = result["direct_callers"].as_array().unwrap();
    let direct_names: Vec<&str> = direct.iter()
        .filter_map(|c| c["name"].as_str()).collect();
    assert!(direct_names.contains(&"createReport"), "direct callers should include createReport, got {:?}", direct_names);
    assert!(direct_names.contains(&"createLog"), "direct callers should include createLog, got {:?}", direct_names);

    // impact_analysis on a leaf function — should have LOW risk
    let msg = tool_call_json("impact_analysis", serde_json::json!({
        "symbol_name": "handleRequest"
    }));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    assert_eq!(result["symbol"], "handleRequest");
    assert_eq!(result["risk_level"], "LOW", "leaf function should be LOW risk");
}

#[test]
fn test_e2e_module_overview() {
    let project = TempDir::new().unwrap();

    fs::create_dir_all(project.path().join("src/auth")).unwrap();
    fs::write(project.path().join("src/auth/validator.ts"), r#"
export function validateEmail(email: string): boolean {
    return email.includes('@');
}

export function validatePassword(pw: string): boolean {
    return pw.length >= 8;
}
"#).unwrap();

    fs::write(project.path().join("src/auth/session.ts"), r#"
import { validateEmail, validatePassword } from './validator';

export function login(email: string, pw: string) {
    if (validateEmail(email) && validatePassword(pw)) {
        return { token: 'abc' };
    }
    throw new Error('invalid');
}
"#).unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();

    // Trigger indexing
    let search = tool_call_json("semantic_code_search", serde_json::json!({"query": "validate"}));
    let _ = server.handle_message(&search).unwrap();

    // module_overview for a directory prefix
    let msg = tool_call_json("module_overview", serde_json::json!({
        "path": "src/auth/"
    }));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    assert_eq!(result["path"], "src/auth/");
    assert!(result["files_count"].as_i64().unwrap() >= 2, "should cover at least 2 files");
    assert!(result["summary"].as_str().unwrap().contains("src/auth/"));

    let exports = result["exports"].as_array().unwrap();
    assert!(exports.len() >= 3, "should have at least 3 exports (validateEmail, validatePassword, login), got {}", exports.len());
    let export_names: Vec<&str> = exports.iter()
        .filter_map(|e| e["name"].as_str()).collect();
    assert!(export_names.contains(&"validateEmail"), "exports should contain validateEmail, got {:?}", export_names);
    assert!(export_names.contains(&"validatePassword"), "exports should contain validatePassword, got {:?}", export_names);
    assert!(export_names.contains(&"login"), "exports should contain login, got {:?}", export_names);

    // Each export should have expected fields
    for exp in exports {
        assert!(exp["node_id"].is_number(), "export should have node_id");
        assert!(exp["name"].is_string(), "export should have name");
        assert!(exp["type"].is_string(), "export should have type");
        assert!(exp["file"].is_string(), "export should have file");
        assert!(exp["caller_count"].is_number(), "export should have caller_count");
    }

    // hot_paths should include functions that have callers
    let hot_paths = result["hot_paths"].as_array().unwrap();
    let hot_names: Vec<&str> = hot_paths.iter()
        .filter_map(|h| h["name"].as_str()).collect();
    assert!(hot_names.contains(&"validateEmail") || hot_names.contains(&"validatePassword"),
        "hot_paths should include called functions, got {:?}", hot_names);

    // module_overview for a single file
    let msg = tool_call_json("module_overview", serde_json::json!({
        "path": "src/auth/validator.ts"
    }));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    assert_eq!(result["files_count"], 1);
    let exports = result["exports"].as_array().unwrap();
    assert_eq!(exports.len(), 2, "validator.ts should have 2 exports");
}

#[test]
fn test_e2e_dependency_graph() {
    let project = TempDir::new().unwrap();

    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(project.path().join("src/db.ts"), r#"
export function query(sql: string): any[] {
    return [];
}

export function connect(): void {}
"#).unwrap();

    fs::write(project.path().join("src/repo.ts"), r#"
import { query, connect } from './db';

export function findUser(id: number) {
    connect();
    return query('SELECT * FROM users WHERE id = ' + id);
}
"#).unwrap();

    fs::write(project.path().join("src/api.ts"), r#"
import { findUser } from './repo';

export function getUser(req: Request, res: Response) {
    const user = findUser(parseInt(req.params.id));
    res.json(user);
}
"#).unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();

    // Trigger indexing
    let search = tool_call_json("semantic_code_search", serde_json::json!({"query": "findUser"}));
    let _ = server.handle_message(&search).unwrap();

    // dependency_graph for the middle file (repo.ts) — should have both directions
    let msg = tool_call_json("dependency_graph", serde_json::json!({
        "file_path": "src/repo.ts",
        "direction": "both",
        "depth": 2
    }));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    assert_eq!(result["file"], "src/repo.ts");
    assert!(result["summary"].as_str().unwrap().contains("src/repo.ts"));

    // repo.ts depends on db.ts (outgoing)
    let depends_on = result["depends_on"].as_array().unwrap();
    let outgoing_files: Vec<&str> = depends_on.iter()
        .filter_map(|d| d["file"].as_str()).collect();
    assert!(outgoing_files.iter().any(|f| f.contains("db.ts")),
        "repo.ts should depend on db.ts, got: {:?}", outgoing_files);

    // api.ts depends on repo.ts (incoming)
    let depended_by = result["depended_by"].as_array().unwrap();
    let incoming_files: Vec<&str> = depended_by.iter()
        .filter_map(|d| d["file"].as_str()).collect();
    assert!(incoming_files.iter().any(|f| f.contains("api.ts")),
        "api.ts should depend on repo.ts, got: {:?}", incoming_files);

    // Each dependency entry should have expected fields
    for dep in depends_on.iter().chain(depended_by.iter()) {
        assert!(dep["file"].is_string(), "dependency should have file");
        assert!(dep["symbols"].is_number(), "dependency should have symbols count");
        assert!(dep["depth"].is_number(), "dependency should have depth");
    }

    // dependency_graph with outgoing-only direction
    let msg = tool_call_json("dependency_graph", serde_json::json!({
        "file_path": "src/repo.ts",
        "direction": "outgoing"
    }));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    assert!(!result["depends_on"].as_array().unwrap().is_empty(),
        "outgoing direction should return depends_on");

    // dependency_graph with incoming-only direction
    let msg = tool_call_json("dependency_graph", serde_json::json!({
        "file_path": "src/repo.ts",
        "direction": "incoming"
    }));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    assert!(!result["depended_by"].as_array().unwrap().is_empty(),
        "incoming direction should return depended_by");

    // dependency_graph for leaf file (db.ts) — no outgoing deps
    let msg = tool_call_json("dependency_graph", serde_json::json!({
        "file_path": "src/db.ts"
    }));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    assert_eq!(result["file"], "src/db.ts");
    let depends_on = result["depends_on"].as_array().unwrap();
    assert!(depends_on.is_empty(), "db.ts should have no outgoing dependencies");
}

#[test]
fn test_e2e_prompts_get_unknown() {
    let project = TempDir::new().unwrap();
    let server = McpServer::from_project_root(project.path()).unwrap();
    let msg = r#"{"jsonrpc":"2.0","id":1,"method":"prompts/get","params":{"name":"nonexistent-prompt"}}"#;
    let resp = server.handle_message(msg).unwrap().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert!(parsed["error"].is_object());
    assert_eq!(parsed["error"]["code"], -32602);
    assert!(parsed["error"]["message"].as_str().unwrap().contains("Unknown prompt"));
}

#[test]
fn test_insert_node_cached_returns_same_as_insert_node() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = Database::open(&tmp.path().join("test.db")).unwrap();

    let file_id = upsert_file(db.conn(), &FileRecord {
        path: "test.ts".into(),
        blake3_hash: "abc123".into(),
        last_modified: 0,
        language: Some("typescript".into()),
    }).unwrap();

    let id = insert_node_cached(db.conn(), &NodeRecord {
        file_id,
        node_type: "function".into(),
        name: "foo".into(),
        qualified_name: None,
        start_line: 1,
        end_line: 5,
        code_content: "function foo() {}".into(),
        signature: Some("foo()".into()),
        doc_comment: None,
        context_string: None,
        name_tokens: None,
        return_type: None,
        param_types: None,
    }).unwrap();

    assert!(id > 0);
    let nodes = get_nodes_by_name(db.conn(), "foo").unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].id, id);
}

#[test]
fn test_insert_edge_cached_deduplicates() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = Database::open(&tmp.path().join("test.db")).unwrap();

    let file_id = upsert_file(db.conn(), &FileRecord {
        path: "test.ts".into(),
        blake3_hash: "abc".into(),
        last_modified: 0,
        language: Some("typescript".into()),
    }).unwrap();

    let n1 = insert_node_cached(db.conn(), &NodeRecord {
        file_id, node_type: "function".into(), name: "a".into(),
        qualified_name: None, start_line: 1, end_line: 2,
        code_content: "".into(), signature: None, doc_comment: None, context_string: None,
        name_tokens: None, return_type: None, param_types: None,
    }).unwrap();
    let n2 = insert_node_cached(db.conn(), &NodeRecord {
        file_id, node_type: "function".into(), name: "b".into(),
        qualified_name: None, start_line: 3, end_line: 4,
        code_content: "".into(), signature: None, doc_comment: None, context_string: None,
        name_tokens: None, return_type: None, param_types: None,
    }).unwrap();

    // First insert should succeed
    assert!(insert_edge_cached(db.conn(), n1, n2, "calls", None).unwrap());
    // Duplicate should be ignored
    assert!(!insert_edge_cached(db.conn(), n1, n2, "calls", None).unwrap());
}

#[test]
fn test_index_skips_unparseable_files_without_crashing() {
    use code_graph_mcp::indexer::pipeline::run_full_index;

    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();

    // Create a valid TS file
    fs::write(project_dir.path().join("good.ts"), "function works() {}").unwrap();
    // Create a file with supported extension but binary content
    fs::write(project_dir.path().join("bad.ts"), &[0xFF, 0xFE, 0x00, 0x01]).unwrap();
    // Another valid file
    fs::write(project_dir.path().join("also_good.ts"), "function alsoWorks() {}").unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    let result = run_full_index(&db, project_dir.path(), None).unwrap();

    // Bad file skipped, but good files indexed
    assert!(result.files_indexed >= 2, "Should index at least the 2 good files, got {}", result.files_indexed);
    let nodes = get_nodes_by_name(db.conn(), "works").unwrap();
    assert_eq!(nodes.len(), 1);
    let nodes2 = get_nodes_by_name(db.conn(), "alsoWorks").unwrap();
    assert_eq!(nodes2.len(), 1);
}

#[test]
fn test_batch_indexing_commits_partial_on_many_files() {
    use code_graph_mcp::indexer::pipeline::run_full_index;

    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();

    // Create 10 valid files
    for i in 0..10 {
        fs::write(
            project_dir.path().join(format!("file{}.ts", i)),
            format!("function func{}() {{}}", i),
        ).unwrap();
    }

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    let result = run_full_index(&db, project_dir.path(), None).unwrap();

    assert_eq!(result.files_indexed, 10);
    // Verify all functions exist
    for i in 0..10 {
        let nodes = get_nodes_by_name(db.conn(), &format!("func{}", i)).unwrap();
        assert_eq!(nodes.len(), 1, "func{} should exist", i);
    }
}
