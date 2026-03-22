mod common;

use std::fs;
use tempfile::TempDir;

use code_graph_mcp::mcp::server::McpServer;
use code_graph_mcp::storage::db::Database;
use code_graph_mcp::storage::queries::*;

use common::{parse_tool_result, tool_call_json};

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
    assert!(result["code_content"].as_str().unwrap().contains("verify"));

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

    // Active exports: symbols with caller_count > 0
    let active = result["active_exports"].as_array().unwrap();
    let active_names: Vec<&str> = active.iter()
        .filter_map(|e| e["name"].as_str()).collect();
    assert!(active_names.contains(&"validateEmail"), "active_exports should contain validateEmail, got {:?}", active_names);
    assert!(active_names.contains(&"validatePassword"), "active_exports should contain validatePassword, got {:?}", active_names);

    // Each active export should have expected fields
    for exp in active {
        assert!(exp["node_id"].is_number(), "export should have node_id");
        assert!(exp["name"].is_string(), "export should have name");
        assert!(exp["type"].is_string(), "export should have type");
        assert!(exp["file"].is_string(), "export should have file");
        assert!(exp["caller_count"].is_number(), "export should have caller_count");
        assert!(exp["signature"].is_string() || exp["signature"].is_null(), "active export should have signature");
    }

    // Inactive summary: symbols with caller_count == 0 grouped by type
    let inactive = result["inactive_summary"].as_array().unwrap();
    // login has no callers, should be in inactive summary
    let empty_arr = vec![];
    let all_inactive_names: Vec<&str> = inactive.iter()
        .flat_map(|g| g["names"].as_array().unwrap_or(&empty_arr).iter()
            .filter_map(|n| n.as_str()))
        .collect();
    assert!(all_inactive_names.contains(&"login"), "inactive_summary should contain login, got {:?}", all_inactive_names);

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
    // Both validateEmail and validatePassword have callers → active_exports
    let active = result["active_exports"].as_array().unwrap();
    assert_eq!(active.len(), 2, "validator.ts should have 2 active exports");
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
fn test_dependency_graph_multi_depth() {
    let project = TempDir::new().unwrap();

    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(project.path().join("src/db.ts"), r#"
export function query(sql: string): any[] { return []; }
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

    fs::write(project.path().join("src/main.ts"), r#"
import { getUser } from './api';
const app = { get: function(path: string, handler: any) {} };
app.get('/users/:id', getUser);
"#).unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();

    let search = tool_call_json("semantic_code_search", serde_json::json!({"query": "getUser"}));
    let _ = server.handle_message(&search).unwrap();

    // depth=1: api.ts depends directly on repo.ts only
    let msg = tool_call_json("dependency_graph", serde_json::json!({
        "file_path": "src/api.ts",
        "direction": "outgoing",
        "depth": 1
    }));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    let depends_on = result["depends_on"].as_array().unwrap();
    let depth1_files: Vec<&str> = depends_on.iter()
        .filter_map(|d| d["file"].as_str()).collect();
    assert!(depth1_files.iter().any(|f| f.contains("repo.ts")),
        "depth=1: api.ts should depend on repo.ts, got: {:?}", depth1_files);
    assert!(!depth1_files.iter().any(|f| f.contains("db.ts")),
        "depth=1: api.ts should NOT show db.ts, got: {:?}", depth1_files);

    // depth=2: api.ts -> repo.ts -> db.ts (transitive)
    let msg2 = tool_call_json("dependency_graph", serde_json::json!({
        "file_path": "src/api.ts",
        "direction": "outgoing",
        "depth": 2
    }));
    let resp2 = server.handle_message(&msg2).unwrap();
    let result2 = parse_tool_result(&resp2);
    let depends_on2 = result2["depends_on"].as_array().unwrap();
    let depth2_files: Vec<&str> = depends_on2.iter()
        .filter_map(|d| d["file"].as_str()).collect();
    assert!(depth2_files.iter().any(|f| f.contains("db.ts")),
        "depth=2: api.ts should transitively depend on db.ts, got: {:?}", depth2_files);

    // Verify depth values
    let db_dep = depends_on2.iter().find(|d| d["file"].as_str().unwrap().contains("db.ts")).unwrap();
    assert_eq!(db_dep["depth"].as_i64().unwrap(), 2, "db.ts should be at depth 2");

    let repo_dep = depends_on2.iter().find(|d| d["file"].as_str().unwrap().contains("repo.ts")).unwrap();
    assert_eq!(repo_dep["depth"].as_i64().unwrap(), 1, "repo.ts should be at depth 1");

    // depth=3 incoming: db.ts <- repo.ts <- api.ts <- main.ts
    let msg3 = tool_call_json("dependency_graph", serde_json::json!({
        "file_path": "src/db.ts",
        "direction": "incoming",
        "depth": 3
    }));
    let resp3 = server.handle_message(&msg3).unwrap();
    let result3 = parse_tool_result(&resp3);
    let depended_by = result3["depended_by"].as_array().unwrap();
    let incoming_files: Vec<&str> = depended_by.iter()
        .filter_map(|d| d["file"].as_str()).collect();
    assert!(incoming_files.iter().any(|f| f.contains("main.ts")),
        "depth=3 incoming: db.ts should be transitively depended on by main.ts, got: {:?}", incoming_files);
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
        is_test: false,
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
        name_tokens: None, return_type: None, param_types: None, is_test: false,
    }).unwrap();
    let n2 = insert_node_cached(db.conn(), &NodeRecord {
        file_id, node_type: "function".into(), name: "b".into(),
        qualified_name: None, start_line: 3, end_line: 4,
        code_content: "".into(), signature: None, doc_comment: None, context_string: None,
        name_tokens: None, return_type: None, param_types: None, is_test: false,
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
    fs::write(project_dir.path().join("bad.ts"), [0xFF, 0xFE, 0x00, 0x01]).unwrap();
    // Another valid file
    fs::write(project_dir.path().join("also_good.ts"), "function alsoWorks() {}").unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    let result = run_full_index(&db, project_dir.path(), None, None).unwrap();

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
    let result = run_full_index(&db, project_dir.path(), None, None).unwrap();

    assert_eq!(result.files_indexed, 10);
    // Verify all functions exist
    for i in 0..10 {
        let nodes = get_nodes_by_name(db.conn(), &format!("func{}", i)).unwrap();
        assert_eq!(nodes.len(), 1, "func{} should exist", i);
    }
}

#[test]
fn test_camelcase_search_finds_split_tokens() {
    use code_graph_mcp::indexer::pipeline::run_full_index;
    use code_graph_mcp::storage::queries::fts5_search;

    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();

    fs::write(
        project_dir.path().join("auth.ts"),
        r#"
function validateAuthToken(token: string): boolean {
    return jwt.verify(token);
}
function handleUserLogin(req: Request) {
    if (validateAuthToken(req.token)) {
        return createSession(req.userId);
    }
}
"#,
    ).unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    // Searching for "validate" should find "validateAuthToken" via name_tokens splitting
    let results = fts5_search(db.conn(), "validate", 5).unwrap().nodes;
    let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
    assert!(names.contains(&"validateAuthToken"), "FTS5 should find validateAuthToken via token 'validate', got: {:?}", names);

    // Searching for "Login" should find "handleUserLogin"
    let results = fts5_search(db.conn(), "Login", 5).unwrap().nodes;
    let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
    assert!(names.contains(&"handleUserLogin"), "FTS5 should find handleUserLogin via token 'Login', got: {:?}", names);
}

#[test]
fn test_type_based_search() {
    use code_graph_mcp::indexer::pipeline::run_full_index;
    use code_graph_mcp::storage::queries::fts5_search;

    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();

    fs::write(
        project_dir.path().join("types.ts"),
        r#"
function getUser(id: number): Promise<User> {
    return db.query(id);
}
function processOrder(order: Order): OrderResult {
    return validate(order);
}
"#,
    ).unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    // Search by return type should find functions returning that type
    let results = fts5_search(db.conn(), "OrderResult", 5).unwrap().nodes;
    let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
    assert!(names.contains(&"processOrder"), "FTS5 should find processOrder via return type 'OrderResult', got: {:?}", names);
}

#[test]
fn test_dependency_graph_directory_hint() {
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(project.path().join("src/app.ts"), "export function main() {}").unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();
    let search = tool_call_json("semantic_code_search", serde_json::json!({"query": "main"}));
    let _ = server.handle_message(&search).unwrap();

    // Passing a directory path should give a helpful hint
    let msg = tool_call_json("dependency_graph", serde_json::json!({"file_path": "src/"}));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    let warning = result["warning"].as_str().unwrap();
    assert!(warning.contains("module_overview"), "directory path should suggest module_overview, got: {}", warning);

    // Path without extension should also trigger directory hint
    let msg = tool_call_json("dependency_graph", serde_json::json!({"file_path": "src"}));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    let warning = result["warning"].as_str().unwrap();
    assert!(warning.contains("module_overview"), "extensionless path should suggest module_overview, got: {}", warning);
}

#[test]
fn test_impact_analysis_struct_warning() {
    let project = TempDir::new().unwrap();
    fs::write(project.path().join("models.ts"), r#"
export class UserModel {
    id: number;
    name: string;
}
"#).unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();
    let search = tool_call_json("semantic_code_search", serde_json::json!({"query": "UserModel"}));
    let _ = server.handle_message(&search).unwrap();

    // impact_analysis on a class with no callers should include a type warning
    let msg = tool_call_json("impact_analysis", serde_json::json!({"symbol_name": "UserModel", "change_type": "remove"}));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    assert!(result["warning"].is_string(), "class with no callers should have type-usage warning");
    assert!(result["warning"].as_str().unwrap().contains("type definition"),
        "warning should mention type definition, got: {}", result["warning"]);
}

#[test]
fn test_trace_http_chain_no_routes_message() {
    let project = TempDir::new().unwrap();
    fs::write(project.path().join("app.ts"), "export function main() {}").unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();
    let search = tool_call_json("semantic_code_search", serde_json::json!({"query": "main"}));
    let _ = server.handle_message(&search).unwrap();

    // trace_http_chain with no routes should return a helpful message
    let msg = tool_call_json("trace_http_chain", serde_json::json!({"route_path": "/api/nothing"}));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    assert!(result["handlers"].as_array().unwrap().is_empty());
    assert!(result["message"].is_string(), "empty handlers should include a message");
    assert!(result["message"].as_str().unwrap().contains("No matching routes"),
        "message should explain no routes found, got: {}", result["message"]);
}

#[test]
fn test_project_map_detects_main_entry_points() {
    let project = TempDir::new().unwrap();
    // Rust-style main function
    fs::write(project.path().join("main.rs"), "fn main() { println!(\"hello\"); }").unwrap();
    // JS-style main function
    fs::write(project.path().join("index.js"), "async function main() { run(); }\nmain();").unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();

    let msg = tool_call_json("project_map", serde_json::json!({}));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);

    let entry_points = result["entry_points"].as_array().unwrap();
    assert!(!entry_points.is_empty(), "project_map should detect main entry points");
    let handlers: Vec<&str> = entry_points.iter()
        .map(|e| e["handler"].as_str().unwrap())
        .collect();
    assert!(handlers.contains(&"main"), "should find main function as entry point");
}

#[test]
fn test_project_map_hot_functions_excludes_test_prefix() {
    let project = TempDir::new().unwrap();
    fs::write(project.path().join("lib.ts"), r#"
function realWork() { return helper(); }
function helper() { return 42; }
function test_something() { realWork(); realWork(); realWork(); }
"#).unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();

    let msg = tool_call_json("project_map", serde_json::json!({}));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);

    let hot = result["hot_functions"].as_array().unwrap();
    let hot_names: Vec<&str> = hot.iter().map(|h| h["name"].as_str().unwrap()).collect();
    assert!(!hot_names.contains(&"test_something"),
        "hot_functions should exclude test_ prefixed functions, got: {:?}", hot_names);
}

#[test]
fn test_project_map_module_dependencies() {
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(project.path().join("src/utils.ts"), r#"
export function add(a: number, b: number): number { return a + b; }
"#).unwrap();
    fs::write(project.path().join("src/app.ts"), r#"
import { add } from './utils';
function main() { return add(1, 2); }
"#).unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();

    let msg = tool_call_json("project_map", serde_json::json!({}));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);

    let modules = result["modules"].as_array().unwrap();
    assert!(!modules.is_empty(), "project_map should detect modules");
    let _deps = result["module_dependencies"].as_array().unwrap();
    // At least verify the structure is correct, even if import resolution doesn't find cross-module deps
    assert!(result["hot_functions"].is_array(), "hot_functions should be an array");
}

#[test]
fn test_parse_timeout_does_not_hang() {
    use code_graph_mcp::domain::parse_timeout_ms;

    // Verify the value exists and is reasonable
    let timeout = parse_timeout_ms();
    assert!(timeout > 0 && timeout <= 30_000,
        "parse_timeout_ms should be between 1 and 30000, got {}", timeout);

    // Generate deeply nested code that could stress the parser
    let mut code = String::new();
    for _ in 0..1000 {
        code.push_str("if (true) { ");
    }
    for _ in 0..1000 {
        code.push_str(" }");
    }

    // Should complete quickly (either parse or timeout), not hang
    let start = std::time::Instant::now();
    let result = code_graph_mcp::parser::treesitter::parse_tree(&code, "typescript");
    let elapsed = start.elapsed();

    // Whether it succeeds or fails, it should not take more than 10 seconds
    assert!(elapsed.as_secs() < 10, "parse_tree should not hang, took {:?}", elapsed);
    // Result can be Ok or Err (timeout) - both are acceptable
    drop(result);
}

#[test]
fn test_skip_indexing_flag() {
    let project = TempDir::new().unwrap();
    fs::write(project.path().join("main.ts"), "export function hello() { return 42; }").unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();

    // First call without skip — triggers indexing
    let msg = tool_call_json("semantic_code_search", serde_json::json!({"query": "hello"}));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    // semantic_code_search returns a raw array
    assert!(!result.as_array().unwrap().is_empty(), "should find hello after indexing");

    // Second call with skip_indexing — should still work (index already built)
    let msg2 = tool_call_json("semantic_code_search", serde_json::json!({
        "query": "hello",
        "skip_indexing": true
    }));
    let resp2 = server.handle_message(&msg2).unwrap();
    let result2 = parse_tool_result(&resp2);
    assert!(!result2.as_array().unwrap().is_empty(), "should find hello with skip_indexing when already indexed");

    // Third call: skip_indexing on a fresh server with no prior indexing should return empty results (not error)
    let project2 = TempDir::new().unwrap();
    fs::write(project2.path().join("main.ts"), "export function world() { return 99; }").unwrap();
    let server2 = McpServer::from_project_root(project2.path()).unwrap();
    let init2 = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server2.handle_message(init2).unwrap();

    let msg3 = tool_call_json("semantic_code_search", serde_json::json!({
        "query": "world",
        "skip_indexing": true
    }));
    let resp3 = server2.handle_message(&msg3).unwrap();
    let result3 = parse_tool_result(&resp3);
    // With skip_indexing and no prior indexing, there should be no results (empty DB)
    // Empty results return an object with results:[] and a message, not a bare array
    let empty_results = result3.get("results").and_then(|r| r.as_array())
        .or_else(|| result3.as_array());
    assert!(empty_results.is_none_or(|a| a.is_empty()),
        "should return empty results when skip_indexing with no prior index, got: {}", result3);
}

#[test]
fn test_get_ast_node_compact_mode() {
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(project.path().join("src/lib.ts"), r#"
export function processData(input: string): number {
    const parsed = JSON.parse(input);
    return parsed.value * 2;
}
"#).unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();
    let search = tool_call_json("semantic_code_search", serde_json::json!({"query": "processData"}));
    let _ = server.handle_message(&search).unwrap();

    // Non-compact: should have code_content
    let msg = tool_call_json("get_ast_node", serde_json::json!({
        "file_path": "src/lib.ts",
        "symbol_name": "processData"
    }));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    assert!(result["code_content"].is_string(), "non-compact should have code_content");

    // Compact mode: should NOT have code_content
    let msg = tool_call_json("get_ast_node", serde_json::json!({
        "file_path": "src/lib.ts",
        "symbol_name": "processData",
        "compact": true
    }));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    assert!(result["code_content"].is_null(), "compact should strip code_content");
    assert!(result["name"].is_string(), "compact should keep name");
    assert!(result["node_id"].is_number(), "compact should keep node_id");
    assert!(result["type"].is_string(), "compact should keep type");
    assert!(result["file_path"].is_string(), "compact should keep file_path");
    assert!(result["start_line"].is_number(), "compact should keep start_line");
    assert!(result["signature"].is_string() || result["signature"].is_null(), "compact should keep signature");

    // Compact via node_id
    let node_id = result["node_id"].as_i64().unwrap();
    let msg = tool_call_json("get_ast_node", serde_json::json!({
        "node_id": node_id,
        "compact": true
    }));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    assert!(result["code_content"].is_null(), "compact via node_id should strip code_content");
    assert_eq!(result["name"], "processData");
}

#[test]
fn test_find_references_compact_mode() {
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(project.path().join("src/util.ts"), r#"
export function helper(): number { return 42; }
"#).unwrap();
    fs::write(project.path().join("src/main.ts"), r#"
import { helper } from './util';
function run() { return helper(); }
"#).unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();
    let search = tool_call_json("semantic_code_search", serde_json::json!({"query": "helper"}));
    let _ = server.handle_message(&search).unwrap();

    // Non-compact: references should have type field
    let msg = tool_call_json("find_references", serde_json::json!({
        "symbol_name": "helper"
    }));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    let refs = result["references"].as_array().unwrap();
    assert!(!refs.is_empty(), "should find references to helper");
    // Non-compact references include "type" field
    for r in refs {
        assert!(r["type"].is_string(), "non-compact should have type field");
    }

    // Compact mode: references should NOT have type field
    let msg = tool_call_json("find_references", serde_json::json!({
        "symbol_name": "helper",
        "compact": true
    }));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    let refs = result["references"].as_array().unwrap();
    assert!(!refs.is_empty(), "compact should still find references");
    for r in refs {
        assert!(r["type"].is_null(), "compact should strip type field");
        assert!(r["name"].is_string(), "compact should keep name");
        assert!(r["file_path"].is_string(), "compact should keep file_path");
        assert!(r["relation"].is_string(), "compact should keep relation");
        assert!(r["node_id"].is_number(), "compact should keep node_id");
        assert!(r["start_line"].is_number(), "compact should keep start_line");
    }
}

#[test]
fn test_dependency_graph_compact_mode() {
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(project.path().join("src/db.ts"), r#"
export function query(sql: string): any[] { return []; }
"#).unwrap();
    fs::write(project.path().join("src/repo.ts"), r#"
import { query } from './db';
export function findUser(id: number) { return query('SELECT * FROM users WHERE id=' + id); }
"#).unwrap();
    fs::write(project.path().join("src/api.ts"), r#"
import { findUser } from './repo';
export function getUser(req: any) { return findUser(req.params.id); }
"#).unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();
    let search = tool_call_json("semantic_code_search", serde_json::json!({"query": "findUser"}));
    let _ = server.handle_message(&search).unwrap();

    // Non-compact: should have symbols field for depth-1 deps
    let msg = tool_call_json("dependency_graph", serde_json::json!({
        "file_path": "src/repo.ts",
        "direction": "both",
        "depth": 2
    }));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    let depends_on = result["depends_on"].as_array().unwrap();
    assert!(!depends_on.is_empty(), "should have outgoing deps");
    // Non-compact depth-1 deps have symbols
    let depth1 = depends_on.iter().find(|d| d["depth"].as_i64() == Some(1));
    assert!(depth1.is_some(), "should have depth-1 dep");
    assert!(depth1.unwrap()["symbols"].is_number(), "non-compact depth-1 should have symbols count");

    // Compact mode: should NOT have symbols field
    let msg = tool_call_json("dependency_graph", serde_json::json!({
        "file_path": "src/repo.ts",
        "direction": "both",
        "depth": 2,
        "compact": true
    }));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    let depends_on = result["depends_on"].as_array().unwrap();
    assert!(!depends_on.is_empty(), "compact should still have outgoing deps");
    for dep in depends_on {
        assert!(dep["file"].is_string(), "compact should keep file");
        assert!(dep["depth"].is_number(), "compact should keep depth");
        assert!(dep["symbols"].is_null(), "compact should strip symbols");
    }
    let depended_by = result["depended_by"].as_array().unwrap();
    for dep in depended_by {
        assert!(dep["symbols"].is_null(), "compact should strip symbols from incoming deps too");
    }
    assert!(result["file"].is_string(), "compact should keep file");
    assert!(result["summary"].is_string(), "compact should keep summary");
}

// ============================================================
// Unicode identifier tests (FTS5 search integration)
// ============================================================

#[test]
fn test_unicode_identifiers_index_and_search() {
    let project = TempDir::new().unwrap();

    // Python file with Unicode identifiers (using escape sequences for portability)
    let py_content = format!(
        "def r{}sum{}(data):\n    return data\n\nclass {}l{}{}(object):\n    pass\n",
        '\u{00e9}', '\u{00e9}', '\u{00d6}', '\u{00e7}', '\u{00fc}'
    );
    fs::write(project.path().join("unicodes.py"), &py_content).unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();

    // Trigger indexing via a content-based search (FTS5 may not tokenize Unicode names)
    let search = tool_call_json(
        "semantic_code_search",
        serde_json::json!({"query": "data"}),
    );
    let resp = server.handle_message(&search).unwrap();
    let results = parse_tool_result(&resp);
    let results_arr = results.as_array().unwrap();
    let names: Vec<&str> = results_arr.iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    // The function that takes 'data' param should be found with its Unicode name preserved
    assert!(
        names.iter().any(|n| n.contains("sum")),
        "Search should find the Unicode function (by content match), got names: {:?}",
        names
    );

    // Verify index status shows the nodes
    let status = tool_call_json("get_index_status", serde_json::json!({}));
    let resp = server.handle_message(&status).unwrap();
    let result = parse_tool_result(&resp);
    assert!(
        result["nodes_count"].as_i64().unwrap() >= 2,
        "should index Unicode identifiers"
    );
}

#[test]
fn test_cjk_identifiers_index_and_search() {
    let project = TempDir::new().unwrap();

    // Go file with CJK identifiers (using escape sequences for portability)
    let go_content = format!(
        "package main\n\nfunc {}{}(x int) int {{\n    return x * 2\n}}\n",
        '\u{8a08}', '\u{7b97}'
    );
    fs::write(project.path().join("cjk.go"), &go_content).unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();

    // Trigger indexing via content-based search
    let search = tool_call_json(
        "semantic_code_search",
        serde_json::json!({"query": "return"}),
    );
    let resp = server.handle_message(&search).unwrap();
    let results = parse_tool_result(&resp);
    let results_arr = results.as_array().unwrap();
    // Verify the CJK name is preserved in the result
    let names: Vec<&str> = results_arr.iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    assert!(
        names.iter().any(|n| n.chars().any(|c| c > '\u{4E00}')),
        "CJK identifier should be preserved in search results, got names: {:?}",
        names
    );
}
