mod common;

use std::fs;
use tempfile::TempDir;

use code_graph_mcp::mcp::server::McpServer;
use code_graph_mcp::storage::db::Database;
use code_graph_mcp::storage::queries::*;

use common::{parse_tool_result, tool_call_json};

/// `semantic_code_search` answers a `{results, search_mode, ...}` envelope on
/// every path since the 2026-08-16 batch (the hybrid path was a bare array
/// before, which silently dropped `ignored_arguments`/`freshness`). The
/// array arm below is kept so this helper also reads pre-envelope captures.
fn search_hits(v: &serde_json::Value) -> Vec<serde_json::Value> {
    v.as_array()
        .cloned()
        .or_else(|| v.get("results").and_then(|r| r.as_array()).cloned())
        .unwrap_or_default()
}

#[test]
fn test_e2e_index_and_search() {
    let project = TempDir::new().unwrap();

    // Create a realistic project structure
    fs::create_dir_all(project.path().join("src/auth")).unwrap();
    fs::create_dir_all(project.path().join("src/api")).unwrap();

    fs::write(
        project.path().join("src/auth/token.ts"),
        r#"
import jwt from 'jsonwebtoken';

export function validateToken(token: string): boolean {
    const decoded = jwt.verify(token, process.env.SECRET);
    return decoded !== null;
}
"#,
    )
    .unwrap();

    fs::write(
        project.path().join("src/api/login.ts"),
        r#"
import { validateToken } from '../auth/token';

export function handleLogin(req: Request, res: Response) {
    const user = validateToken(req.headers.authorization);
    if (!user) { res.status(401); return; }
    res.json({ userId: user.id });
}
"#,
    )
    .unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();

    // Initialize
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();

    // Search for auth-related code
    let search = tool_call_json(
        "semantic_code_search",
        serde_json::json!({"query": "validateToken", "top_k": 3}),
    );
    let resp = server.handle_message(&search).unwrap();
    let results = parse_tool_result(&resp);
    let results_arr = search_hits(&results);
    assert!(!results_arr.is_empty(), "search should find results");
    let names: Vec<&str> = results_arr
        .iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    assert!(names.contains(&"validateToken"), "got names: {:?}", names);

    // Get call graph for handleLogin
    let graph = tool_call_json(
        "get_call_graph",
        serde_json::json!({
            "symbol_name": "handleLogin",
            "direction": "callees",
            "depth": 2
        }),
    );
    let resp = server.handle_message(&graph).unwrap();
    let result = parse_tool_result(&resp);
    assert_eq!(result["function"], "handleLogin");

    // Get index status
    let status = tool_call_json("get_index_status", serde_json::json!({}));
    let resp = server.handle_message(&status).unwrap();
    let result = parse_tool_result(&resp);
    assert!(
        result["files_count"].as_i64().unwrap() >= 2,
        "should have indexed at least 2 files"
    );
    assert!(
        result["nodes_count"].as_i64().unwrap() >= 2,
        "should have at least 2 nodes"
    );

    // Get AST node
    let ast = tool_call_json(
        "get_ast_node",
        serde_json::json!({
            "file_path": "src/auth/token.ts",
            "symbol_name": "validateToken",
            "include_references": true
        }),
    );
    let resp = server.handle_message(&ast).unwrap();
    let result = parse_tool_result(&resp);
    assert_eq!(result["name"], "validateToken");
    assert!(result["code_content"].as_str().unwrap().contains("verify"));

    // Read snippet for a node
    let node_id = result["node_id"].as_i64().unwrap();
    let snippet = tool_call_json(
        "read_snippet",
        serde_json::json!({
            "node_id": node_id,
            "context_lines": 2
        }),
    );
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

    fs::write(
        project.path().join("server.ts"),
        r#"
function handleLogin(req: Request, res: Response) {
    res.json({ ok: true });
}

function getUsers(req: Request, res: Response) {
    res.json([]);
}

app.post('/api/login', handleLogin);
app.get('/api/users', getUsers);
"#,
    )
    .unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();

    // Initialize and trigger indexing
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();

    // Find route
    let route = tool_call_json(
        "find_http_route",
        serde_json::json!({
            "route_path": "/api/login"
        }),
    );
    let resp = server.handle_message(&route).unwrap();
    let result = parse_tool_result(&resp);
    assert_eq!(result["route"], "/api/login");
    let handlers = result["handlers"].as_array().unwrap();
    assert!(!handlers.is_empty(), "should find route handler");
}

// CON-14 (audit 2026-08-29): `get_call_graph` in route mode hands the whole
// call to the HTTP tracer, which reads neither `file_path` nor `direction`. The
// schema declares both, and `file_path`'s description ("Disambiguate same-name
// functions") makes a promise the route arm does not keep — so narrowing a
// route trace by file returned the unnarrowed answer, silently. The schema
// cannot say "not in this mode"; the answer can.
#[test]
fn route_mode_discloses_the_arguments_it_does_not_read() {
    let project = TempDir::new().unwrap();
    fs::write(
        project.path().join("server.ts"),
        r#"
function handleLogin(req: Request, res: Response) {
    res.json({ ok: true });
}
app.post('/api/login', handleLogin);
"#,
    )
    .unwrap();
    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();

    let call = tool_call_json(
        "get_call_graph",
        serde_json::json!({
            "route_path": "/api/login",
            "file_path": "server.ts",
            "direction": "callers",
            "compact": true
        }),
    );
    let resp = server.handle_message(&call).unwrap();
    let result = parse_tool_result(&resp);
    let ignored = result["ignored_arguments"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert!(
        ignored.contains(&"file_path".to_string()),
        "route mode must disclose that file_path did nothing; got {result}"
    );
    assert!(
        ignored.contains(&"direction".to_string()),
        "direction is inert here too — its description says so, but a description \
         is not the answer the caller reads; got {result}"
    );
    assert!(
        ignored.contains(&"compact".to_string()),
        "compact is inert in route mode as well, and unlike `direction` its \
         description makes no exception — so a caller asking for a smaller answer \
         gets the full one; got {result}"
    );
    // The answer itself is unaffected: disclosure, not refusal.
    assert!(
        !result["handlers"].as_array().unwrap().is_empty(),
        "the trace must still answer; got {result}"
    );

    // Symbol mode honors both, so neither may be reported there — otherwise the
    // fix is "always claim these were ignored", which is a new lie.
    let call = tool_call_json(
        "get_call_graph",
        serde_json::json!({
            "symbol_name": "handleLogin",
            "file_path": "server.ts",
            "direction": "callers"
        }),
    );
    let resp = server.handle_message(&call).unwrap();
    let result = parse_tool_result(&resp);
    assert!(
        result.get("ignored_arguments").is_none(),
        "symbol mode reads file_path and direction; nothing to disclose. got {result}"
    );
}

#[test]
fn test_e2e_incremental_reindex() {
    let project = TempDir::new().unwrap();

    // Initial file
    fs::write(project.path().join("app.ts"), "function original() {}").unwrap();
    let server = common::init_server(&project);

    // Trigger full index
    let status = tool_call_json("get_index_status", serde_json::json!({}));
    let _ = server.handle_message(&status).unwrap();

    // Search for original
    let search = tool_call_json(
        "semantic_code_search",
        serde_json::json!({"query": "original"}),
    );
    let resp = server.handle_message(&search).unwrap();
    let result = parse_tool_result(&resp);
    assert!(!search_hits(&result).is_empty());

    // Modify file
    fs::write(project.path().join("app.ts"), "function modified() {}").unwrap();

    // Explicit rebuild to sync before search (avoids timing-dependent incremental detection)
    let rebuild = tool_call_json("rebuild_index", serde_json::json!({"confirm": true}));
    let _ = server.handle_message(&rebuild).unwrap();

    // Search again
    let search = tool_call_json(
        "semantic_code_search",
        serde_json::json!({"query": "modified"}),
    );
    let resp = server.handle_message(&search).unwrap();
    let result = parse_tool_result(&resp);
    let hits = search_hits(&result);
    let names: Vec<&str> = hits.iter().filter_map(|r| r["name"].as_str()).collect();
    assert!(
        names.contains(&"modified"),
        "should find modified function, got: {:?}",
        names
    );
}

/// What the wire carries, and what the handler still enforces (audit
/// 2026-08-29 CON-13).
///
/// Four tools reject a call that omits every one of their alternatives. The
/// accurate schema for that is `anyOf`, and publishing it is measurably not an
/// option: for one build these four carried it and the client dropped exactly
/// those four while keeping the three without it — see
/// `no_tool_publishes_an_anyof_the_client_drops` in `mcp::tools`.
///
/// So the constraint cannot live in the schema's KEYWORDS — but "cannot be
/// expressed as `anyOf`" was read for a release as "cannot be published", and
/// those are different claims. `description` is schema too, and it is the half
/// the model actually reads. Each of the four now spells the disjunction there,
/// and this pins all three halves of what is left: the wire stays free of the
/// keyword that makes a tool disappear, the published text states the
/// requirement, and the handler keeps rejecting the call.
///
/// The last two matter on their own, in opposite directions. A requirement
/// nobody enforces is the same defect as an unstated one — and a requirement
/// enforced but never published is how a caller finds out by being refused.
#[test]
fn tools_list_shape_matches_what_each_handler_enforces() {
    let project = TempDir::new().unwrap();
    fs::write(
        project.path().join("app.ts"),
        "function greet(name: string): string { return name; }\n",
    )
    .unwrap();
    let server = McpServer::from_project_root(project.path()).unwrap();

    let msg = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#;
    let resp = server.handle_message(msg).unwrap().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    let tools = parsed["result"]["tools"].as_array().unwrap();

    for tool in tools {
        assert!(
            tool["inputSchema"].get("anyOf").is_none(),
            "'{}' reaches the client carrying `anyOf`; the client answers that by \
             dropping the tool entirely: {tool}",
            tool["name"]
        );
    }

    // (tool, the alternatives its handler accepts)
    let expected: [(&str, &[&str]); 4] = [
        ("get_call_graph", &["symbol_name", "route_path"]),
        ("get_ast_node", &["symbol_name", "node_id"]),
        ("ast_search", &["query", "type", "returns", "params"]),
        ("find_references", &["symbol_name", "node_id"]),
    ];

    let mut checked = 0usize;
    for (name, arms) in expected {
        let tool = tools
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("tools/list no longer advertises '{name}'"));

        // Half one: the requirement is PUBLISHED. Exactly one property carries
        // the clause, and that one property names every alternative — joining all
        // the descriptions instead would pass on incidental mentions
        // (`get_call_graph.direction` already says "route_path" for its own
        // reasons), which is a guard that cannot go red.
        let stating: Vec<(&str, &str)> = tool["inputSchema"]["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("'{name}' publishes no properties object"))
            .iter()
            .filter_map(|(key, spec)| {
                let desc = spec["description"].as_str()?;
                desc.contains("A call must carry")
                    .then_some((key.as_str(), desc))
            })
            .collect();
        assert_eq!(
            stating.len(),
            1,
            "'{name}' must state its one-of requirement in exactly one property \
             description; found {}: {:?}",
            stating.len(),
            stating.iter().map(|(k, _)| *k).collect::<Vec<_>>()
        );
        let (holder, clause) = stating[0];
        // …and it must sit on one of the alternatives. Moving `get_ast_node`'s
        // clause verbatim onto `include_references` (a boolean) satisfied both
        // the count and the arm-naming check while leaving `symbol_name` — the
        // property a caller reads first — saying nothing about the requirement.
        assert!(
            arms.contains(&holder),
            "'{name}' states its one-of rule on '{holder}', which is not one of \
             {arms:?}; a caller reading the alternatives never sees it"
        );
        for arm in arms {
            assert!(
                clause.contains(arm),
                "'{name}' publishes a one-of clause that never names '{arm}', so a \
                 caller reading the schema cannot tell it satisfies the \
                 requirement: {clause:?}"
            );
        }

        // Half two: the requirement is ENFORCED. `compact` is a real argument for
        // each of these and satisfies nothing: the call is well-formed and still
        // names none of the alternatives.
        let call = format!(
            r#"{{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{{"name":"{name}","arguments":{{"compact":true}}}}}}"#
        );
        let resp = server.handle_message(&call).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        let text = parsed["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
            + parsed["error"]["message"].as_str().unwrap_or_default();
        // Refusal first, and as its own assertion. Asking only "does the response
        // text mention an arm" cannot go red for `ast_search`: a SUCCESSFUL
        // payload carries "type" as a result field name, so the arm is satisfied
        // by the answer the tool should not have given. Measured — with
        // `ast_search.rs`'s check disabled the old single assertion stayed green.
        // `isError` is the same signal the sibling numeric/boolean refusal guards
        // in `mcp::server` assert on.
        assert!(
            parsed["result"]["isError"].as_bool().unwrap_or(false) || parsed.get("error").is_some(),
            "'{name}' ANSWERED a call naming none of {arms:?} instead of refusing \
             it: {parsed}"
        );
        assert!(
            arms.iter().any(|a| text.contains(a)),
            "'{name}' refused a call naming none of {arms:?} without naming any of \
             them, so the caller cannot tell what would satisfy it: {parsed}"
        );
        checked += 1;
    }
    assert_eq!(checked, 4, "every row must have been exercised");
}

#[test]
fn test_e2e_full_protocol_lifecycle() {
    let project = TempDir::new().unwrap();
    fs::write(
        project.path().join("app.ts"),
        r#"
function greet(name: string): string {
    return "hello " + name;
}
"#,
    )
    .unwrap();

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
        ("impact-analysis", "symbol_name", "greet", "get_ast_node"),
        ("understand-module", "path", "app.ts", "module_overview"),
        // The advertised form, not the folded backend name: see
        // `prompts_name_only_tools_the_client_can_actually_call`.
        ("trace-request", "route", "/api/users", "get_call_graph"),
    ] {
        let msg = serde_json::json!({
            "jsonrpc": "2.0", "id": 5, "method": "prompts/get",
            "params": { "name": name, "arguments": { arg_name: arg_val } }
        })
        .to_string();
        let resp = server.handle_message(&msg).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        let text = parsed["result"]["messages"][0]["content"]["text"]
            .as_str()
            .unwrap();
        assert!(
            text.contains(expected_text),
            "prompt '{}' should mention '{}', got: {}",
            name,
            expected_text,
            text
        );
    }

    // 7. resources/read
    let msg = r#"{"jsonrpc":"2.0","id":6,"method":"resources/read","params":{"uri":"code-graph://project-summary"}}"#;
    let resp = server.handle_message(msg).unwrap().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    let text = parsed["result"]["contents"][0]["text"].as_str().unwrap();
    let summary: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(summary["schema_version"].is_number());

    // 8. tool call — triggers indexing
    let search = tool_call_json(
        "semantic_code_search",
        serde_json::json!({"query": "greet"}),
    );
    let resp = server.handle_message(&search).unwrap();
    let result = parse_tool_result(&resp);
    // hybrid → bare array; FTS5-only (no model in CI) → {results, vector_available}
    assert!(
        result.is_array() || result.get("results").is_some(),
        "search should return results (array or FTS5-only object), got: {}",
        result
    );

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
    let search = tool_call_json(
        "semantic_code_search",
        serde_json::json!({"query": "function"}),
    );
    let _ = server.handle_message(&search).unwrap();

    // Read project summary
    let msg = r#"{"jsonrpc":"2.0","id":1,"method":"resources/read","params":{"uri":"code-graph://project-summary"}}"#;
    let resp = server.handle_message(msg).unwrap().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    let text = parsed["result"]["contents"][0]["text"].as_str().unwrap();
    let summary: serde_json::Value = serde_json::from_str(text).unwrap();

    assert!(
        summary["files"].as_i64().unwrap() >= 2,
        "should have at least 2 files indexed"
    );
    assert!(
        summary["nodes"].as_i64().unwrap() >= 2,
        "should have at least 2 nodes"
    );
    assert!(summary["schema_version"].as_i64().unwrap() >= 1);
}

#[test]
fn test_e2e_prompts_get_all() {
    let project = TempDir::new().unwrap();
    let server = McpServer::from_project_root(project.path()).unwrap();

    let cases = vec![
        (
            "impact-analysis",
            "symbol_name",
            "handleLogin",
            "get_ast_node",
        ),
        ("understand-module", "path", "src/auth/", "module_overview"),
        ("trace-request", "route", "/api/users", "get_call_graph"),
    ];

    for (name, arg_name, arg_val, expected_substr) in cases {
        let msg = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "prompts/get",
            "params": { "name": name, "arguments": { arg_name: arg_val } }
        })
        .to_string();
        let resp = server.handle_message(&msg).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        let messages = parsed["result"]["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        let text = messages[0]["content"]["text"].as_str().unwrap();
        assert!(
            text.contains(arg_val),
            "prompt '{}' message should contain argument '{}', got: {}",
            name,
            arg_val,
            text
        );
        assert!(
            text.contains(expected_substr),
            "prompt '{}' message should reference tool '{}', got: {}",
            name,
            expected_substr,
            text
        );
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
    assert!(parsed["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Unknown resource URI"));
}

#[test]
fn test_e2e_module_overview() {
    let project = TempDir::new().unwrap();

    fs::create_dir_all(project.path().join("src/auth")).unwrap();
    fs::write(
        project.path().join("src/auth/validator.ts"),
        r#"
export function validateEmail(email: string): boolean {
    return email.includes('@');
}

export function validatePassword(pw: string): boolean {
    return pw.length >= 8;
}
"#,
    )
    .unwrap();

    fs::write(
        project.path().join("src/auth/session.ts"),
        r#"
import { validateEmail, validatePassword } from './validator';

export function login(email: string, pw: string) {
    if (validateEmail(email) && validatePassword(pw)) {
        return { token: 'abc' };
    }
    throw new Error('invalid');
}

// Uncalled, and a different node type from `login`, so `inactive_summary` has
// two groups to order. With one group the ordering assertion below is
// constructively true and guards nothing.
export class SessionStore {
    clear() { }
}
"#,
    )
    .unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();

    // Trigger indexing
    let search = tool_call_json(
        "semantic_code_search",
        serde_json::json!({"query": "validate"}),
    );
    let _ = server.handle_message(&search).unwrap();

    // module_overview for a directory prefix
    let msg = tool_call_json(
        "module_overview",
        serde_json::json!({
            "path": "src/auth/"
        }),
    );
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    assert_eq!(result["path"], "src/auth/");
    assert!(
        result["files_count"].as_i64().unwrap() >= 2,
        "should cover at least 2 files"
    );
    assert!(result["summary"].as_str().unwrap().contains("src/auth/"));

    // Active exports: symbols with caller_count > 0
    let active = result["active_exports"].as_array().unwrap();
    let active_names: Vec<&str> = active.iter().filter_map(|e| e["name"].as_str()).collect();
    assert!(
        active_names.contains(&"validateEmail"),
        "active_exports should contain validateEmail, got {:?}",
        active_names
    );
    assert!(
        active_names.contains(&"validatePassword"),
        "active_exports should contain validatePassword, got {:?}",
        active_names
    );

    // Each active export should have expected fields
    for exp in active {
        assert!(exp["node_id"].is_number(), "export should have node_id");
        assert!(exp["name"].is_string(), "export should have name");
        assert!(exp["type"].is_string(), "export should have type");
        assert!(exp["file"].is_string(), "export should have file");
        assert!(
            exp["caller_count"].is_number(),
            "export should have caller_count"
        );
        assert!(
            exp["signature"].is_string() || exp["signature"].is_null(),
            "active export should have signature"
        );
    }

    // Inactive summary: symbols with caller_count == 0 grouped by type
    let inactive = result["inactive_summary"].as_array().unwrap();
    // login has no callers, should be in inactive summary
    let empty_arr = vec![];
    let all_inactive_names: Vec<&str> = inactive
        .iter()
        .flat_map(|g| {
            g["names"]
                .as_array()
                .unwrap_or(&empty_arr)
                .iter()
                .filter_map(|n| n.as_str())
        })
        .collect();
    assert!(
        all_inactive_names.contains(&"login"),
        "inactive_summary should contain login, got {:?}",
        all_inactive_names
    );
    // The group order must be deterministic. It was built by iterating a
    // HashMap, so the same binary over the same index emitted a different order
    // on every run — irreproducible LLM-visible output, and noise in any
    // run-to-run comparison. A single run cannot observe randomness, so assert
    // the invariant that makes it impossible: sorted by type.
    let group_types: Vec<&str> = inactive.iter().filter_map(|g| g["type"].as_str()).collect();
    // Non-vacuity: with fewer than two groups the ordering assertion below is
    // constructively true and would survive any regression.
    assert!(
        group_types.len() >= 2,
        "fixture must produce at least two inactive groups for the ordering \
         assertion to mean anything; got {:?}",
        group_types
    );
    let mut sorted_types = group_types.clone();
    sorted_types.sort_unstable();
    assert_eq!(
        group_types, sorted_types,
        "inactive_summary groups must be ordered by type, not by HashMap iteration order; got {:?}",
        group_types
    );

    // hot_paths should include functions that have callers
    let hot_paths = result["hot_paths"].as_array().unwrap();
    let hot_names: Vec<&str> = hot_paths
        .iter()
        .filter_map(|h| h["name"].as_str())
        .collect();
    assert!(
        hot_names.contains(&"validateEmail") || hot_names.contains(&"validatePassword"),
        "hot_paths should include called functions, got {:?}",
        hot_names
    );

    // module_overview for a single file
    let msg = tool_call_json(
        "module_overview",
        serde_json::json!({
            "path": "src/auth/validator.ts"
        }),
    );
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
    fs::write(
        project.path().join("src/db.ts"),
        r#"
export function query(sql: string): any[] {
    return [];
}

export function connect(): void {}
"#,
    )
    .unwrap();

    fs::write(
        project.path().join("src/repo.ts"),
        r#"
import { query, connect } from './db';

export function findUser(id: number) {
    connect();
    return query('SELECT * FROM users WHERE id = ' + id);
}
"#,
    )
    .unwrap();

    fs::write(
        project.path().join("src/api.ts"),
        r#"
import { findUser } from './repo';

export function getUser(req: Request, res: Response) {
    const user = findUser(parseInt(req.params.id));
    res.json(user);
}
"#,
    )
    .unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();

    // Trigger indexing
    let search = tool_call_json(
        "semantic_code_search",
        serde_json::json!({"query": "findUser"}),
    );
    let _ = server.handle_message(&search).unwrap();

    // dependency_graph for the middle file (repo.ts) — should have both directions
    let msg = tool_call_json(
        "dependency_graph",
        serde_json::json!({
            "file_path": "src/repo.ts",
            "direction": "both",
            "depth": 2
        }),
    );
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    assert_eq!(result["file"], "src/repo.ts");
    assert!(result["summary"].as_str().unwrap().contains("src/repo.ts"));

    // repo.ts depends on db.ts (outgoing)
    let depends_on = result["depends_on"].as_array().unwrap();
    let outgoing_files: Vec<&str> = depends_on
        .iter()
        .filter_map(|d| d["file"].as_str())
        .collect();
    assert!(
        outgoing_files.iter().any(|f| f.contains("db.ts")),
        "repo.ts should depend on db.ts, got: {:?}",
        outgoing_files
    );

    // api.ts depends on repo.ts (incoming)
    let depended_by = result["depended_by"].as_array().unwrap();
    let incoming_files: Vec<&str> = depended_by
        .iter()
        .filter_map(|d| d["file"].as_str())
        .collect();
    assert!(
        incoming_files.iter().any(|f| f.contains("api.ts")),
        "api.ts should depend on repo.ts, got: {:?}",
        incoming_files
    );

    // Each dependency entry should have expected fields
    for dep in depends_on.iter().chain(depended_by.iter()) {
        assert!(dep["file"].is_string(), "dependency should have file");
        assert!(
            dep["symbols"].is_number(),
            "dependency should have symbols count"
        );
        assert!(dep["depth"].is_number(), "dependency should have depth");
    }

    // dependency_graph with outgoing-only direction
    let msg = tool_call_json(
        "dependency_graph",
        serde_json::json!({
            "file_path": "src/repo.ts",
            "direction": "outgoing"
        }),
    );
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    assert!(
        !result["depends_on"].as_array().unwrap().is_empty(),
        "outgoing direction should return depends_on"
    );

    // dependency_graph with incoming-only direction
    let msg = tool_call_json(
        "dependency_graph",
        serde_json::json!({
            "file_path": "src/repo.ts",
            "direction": "incoming"
        }),
    );
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    assert!(
        !result["depended_by"].as_array().unwrap().is_empty(),
        "incoming direction should return depended_by"
    );

    // dependency_graph for leaf file (db.ts) — no outgoing deps
    let msg = tool_call_json(
        "dependency_graph",
        serde_json::json!({
            "file_path": "src/db.ts"
        }),
    );
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    assert_eq!(result["file"], "src/db.ts");
    let depends_on = result["depends_on"].as_array().unwrap();
    assert!(
        depends_on.is_empty(),
        "db.ts should have no outgoing dependencies"
    );
}

#[test]
fn test_dependency_graph_multi_depth() {
    let project = TempDir::new().unwrap();

    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(
        project.path().join("src/db.ts"),
        r#"
export function query(sql: string): any[] { return []; }
export function connect(): void {}
"#,
    )
    .unwrap();

    fs::write(
        project.path().join("src/repo.ts"),
        r#"
import { query, connect } from './db';
export function findUser(id: number) {
    connect();
    return query('SELECT * FROM users WHERE id = ' + id);
}
"#,
    )
    .unwrap();

    fs::write(
        project.path().join("src/api.ts"),
        r#"
import { findUser } from './repo';
export function getUser(req: Request, res: Response) {
    const user = findUser(parseInt(req.params.id));
    res.json(user);
}
"#,
    )
    .unwrap();

    fs::write(
        project.path().join("src/main.ts"),
        r#"
import { getUser } from './api';
const app = { get: function(path: string, handler: any) {} };
app.get('/users/:id', getUser);
"#,
    )
    .unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();

    let search = tool_call_json(
        "semantic_code_search",
        serde_json::json!({"query": "getUser"}),
    );
    let _ = server.handle_message(&search).unwrap();

    // depth=1: api.ts depends directly on repo.ts only
    let msg = tool_call_json(
        "dependency_graph",
        serde_json::json!({
            "file_path": "src/api.ts",
            "direction": "outgoing",
            "depth": 1
        }),
    );
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    let depends_on = result["depends_on"].as_array().unwrap();
    let depth1_files: Vec<&str> = depends_on
        .iter()
        .filter_map(|d| d["file"].as_str())
        .collect();
    assert!(
        depth1_files.iter().any(|f| f.contains("repo.ts")),
        "depth=1: api.ts should depend on repo.ts, got: {:?}",
        depth1_files
    );
    assert!(
        !depth1_files.iter().any(|f| f.contains("db.ts")),
        "depth=1: api.ts should NOT show db.ts, got: {:?}",
        depth1_files
    );

    // depth=2: api.ts -> repo.ts -> db.ts (transitive)
    let msg2 = tool_call_json(
        "dependency_graph",
        serde_json::json!({
            "file_path": "src/api.ts",
            "direction": "outgoing",
            "depth": 2
        }),
    );
    let resp2 = server.handle_message(&msg2).unwrap();
    let result2 = parse_tool_result(&resp2);
    let depends_on2 = result2["depends_on"].as_array().unwrap();
    let depth2_files: Vec<&str> = depends_on2
        .iter()
        .filter_map(|d| d["file"].as_str())
        .collect();
    assert!(
        depth2_files.iter().any(|f| f.contains("db.ts")),
        "depth=2: api.ts should transitively depend on db.ts, got: {:?}",
        depth2_files
    );

    // Verify depth values
    let db_dep = depends_on2
        .iter()
        .find(|d| d["file"].as_str().unwrap().contains("db.ts"))
        .unwrap();
    assert_eq!(
        db_dep["depth"].as_i64().unwrap(),
        2,
        "db.ts should be at depth 2"
    );

    let repo_dep = depends_on2
        .iter()
        .find(|d| d["file"].as_str().unwrap().contains("repo.ts"))
        .unwrap();
    assert_eq!(
        repo_dep["depth"].as_i64().unwrap(),
        1,
        "repo.ts should be at depth 1"
    );

    // depth=3 incoming: db.ts <- repo.ts <- api.ts <- main.ts
    let msg3 = tool_call_json(
        "dependency_graph",
        serde_json::json!({
            "file_path": "src/db.ts",
            "direction": "incoming",
            "depth": 3
        }),
    );
    let resp3 = server.handle_message(&msg3).unwrap();
    let result3 = parse_tool_result(&resp3);
    let depended_by = result3["depended_by"].as_array().unwrap();
    let incoming_files: Vec<&str> = depended_by
        .iter()
        .filter_map(|d| d["file"].as_str())
        .collect();
    assert!(
        incoming_files.iter().any(|f| f.contains("main.ts")),
        "depth=3 incoming: db.ts should be transitively depended on by main.ts, got: {:?}",
        incoming_files
    );
}

#[test]
fn test_e2e_prompts_get_unknown() {
    let project = TempDir::new().unwrap();
    let server = McpServer::from_project_root(project.path()).unwrap();
    let msg =
        r#"{"jsonrpc":"2.0","id":1,"method":"prompts/get","params":{"name":"nonexistent-prompt"}}"#;
    let resp = server.handle_message(msg).unwrap().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert!(parsed["error"].is_object());
    assert_eq!(parsed["error"]["code"], -32602);
    assert!(parsed["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Unknown prompt"));
}

#[test]
fn test_insert_node_cached_returns_same_as_insert_node() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = Database::open(&tmp.path().join("test.db")).unwrap();

    let file_id = upsert_file(
        db.conn(),
        &FileRecord {
            path: "test.ts".into(),
            blake3_hash: "abc123".into(),
            last_modified: 0,
            language: Some("typescript".into()),
        },
    )
    .unwrap();

    let id = insert_node_cached(
        db.conn(),
        &NodeRecord {
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
        },
    )
    .unwrap();

    assert!(id > 0);
    let nodes = get_nodes_by_name(db.conn(), "foo").unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].id, id);
}

#[test]
fn test_insert_edge_cached_deduplicates() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = Database::open(&tmp.path().join("test.db")).unwrap();

    let file_id = upsert_file(
        db.conn(),
        &FileRecord {
            path: "test.ts".into(),
            blake3_hash: "abc".into(),
            last_modified: 0,
            language: Some("typescript".into()),
        },
    )
    .unwrap();

    let n1 = insert_node_cached(
        db.conn(),
        &NodeRecord {
            file_id,
            node_type: "function".into(),
            name: "a".into(),
            qualified_name: None,
            start_line: 1,
            end_line: 2,
            code_content: "".into(),
            signature: None,
            doc_comment: None,
            context_string: None,
            name_tokens: None,
            return_type: None,
            param_types: None,
            is_test: false,
        },
    )
    .unwrap();
    let n2 = insert_node_cached(
        db.conn(),
        &NodeRecord {
            file_id,
            node_type: "function".into(),
            name: "b".into(),
            qualified_name: None,
            start_line: 3,
            end_line: 4,
            code_content: "".into(),
            signature: None,
            doc_comment: None,
            context_string: None,
            name_tokens: None,
            return_type: None,
            param_types: None,
            is_test: false,
        },
    )
    .unwrap();

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
    fs::write(
        project_dir.path().join("also_good.ts"),
        "function alsoWorks() {}",
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    let result = run_full_index(&db, project_dir.path(), None, None).unwrap();

    // Bad file skipped, but good files indexed
    assert!(
        result.files_indexed >= 2,
        "Should index at least the 2 good files, got {}",
        result.files_indexed
    );
    let nodes = get_nodes_by_name(db.conn(), "works").unwrap();
    assert_eq!(nodes.len(), 1);
    let nodes2 = get_nodes_by_name(db.conn(), "alsoWorks").unwrap();
    assert_eq!(nodes2.len(), 1);
}

/// Ten files, all indexed, every symbol present.
///
/// Renamed from `test_batch_indexing_commits_partial_on_many_files`, which
/// claimed coverage it never had: `BATCH_SIZE` is 500, so ten files are ONE
/// batch and no boundary is crossed (2026-08-16 audit §四). The real
/// batch-boundary behaviour — the cross-batch deferred resolution that audit
/// 2026-08-02 P0-1 was about — is covered in `src/indexer/pipeline/tests.rs`,
/// which builds fixtures large enough to actually span batches.
#[test]
fn test_full_index_covers_every_file_in_a_multi_file_project() {
    use code_graph_mcp::indexer::pipeline::run_full_index;

    let project_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();

    // Create 10 valid files
    for i in 0..10 {
        fs::write(
            project_dir.path().join(format!("file{}.ts", i)),
            format!("function func{}() {{}}", i),
        )
        .unwrap();
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
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    // Searching for "validate" should find "validateAuthToken" via name_tokens splitting
    let results = fts5_search(db.conn(), "validate", 5).unwrap().nodes;
    let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
    assert!(
        names.contains(&"validateAuthToken"),
        "FTS5 should find validateAuthToken via token 'validate', got: {:?}",
        names
    );

    // Searching for "Login" should find "handleUserLogin"
    let results = fts5_search(db.conn(), "Login", 5).unwrap().nodes;
    let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
    assert!(
        names.contains(&"handleUserLogin"),
        "FTS5 should find handleUserLogin via token 'Login', got: {:?}",
        names
    );
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
    )
    .unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    run_full_index(&db, project_dir.path(), None, None).unwrap();

    // Search by return type should find functions returning that type
    let results = fts5_search(db.conn(), "OrderResult", 5).unwrap().nodes;
    let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
    assert!(
        names.contains(&"processOrder"),
        "FTS5 should find processOrder via return type 'OrderResult', got: {:?}",
        names
    );
}

#[test]
fn test_dependency_graph_directory_hint() {
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(
        project.path().join("src/app.ts"),
        "export function main() {}",
    )
    .unwrap();

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
    assert!(
        warning.contains("module_overview"),
        "directory path should suggest module_overview, got: {}",
        warning
    );

    // Path without extension should also trigger directory hint
    let msg = tool_call_json("dependency_graph", serde_json::json!({"file_path": "src"}));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    let warning = result["warning"].as_str().unwrap();
    assert!(
        warning.contains("module_overview"),
        "extensionless path should suggest module_overview, got: {}",
        warning
    );
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
    let msg = tool_call_json(
        "trace_http_chain",
        serde_json::json!({"route_path": "/api/nothing"}),
    );
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    assert!(result["handlers"].as_array().unwrap().is_empty());
    assert!(
        result["message"].is_string(),
        "empty handlers should include a message"
    );
    assert!(
        result["message"]
            .as_str()
            .unwrap()
            .contains("No matching routes"),
        "message should explain no routes found, got: {}",
        result["message"]
    );
}

// MCP half of the ANY/ALL wildcard contract (pre-tag review SF-2): reverting
// filter_routes_by_method to exact equality must fail HERE, not only in the
// CLI e2e — the MCP surface is where the original drift (case-sensitive
// comparison) lived, and no test had ever exercised its verb filter.
#[test]
fn test_trace_http_chain_verb_matches_wildcard_and_filters_mismatch() {
    let project = TempDir::new().unwrap();
    fs::write(
        project.path().join("app.py"),
        "from flask import Flask\napp = Flask(__name__)\n\n@app.route('/orders')\ndef list_orders():\n    return []\n\n@app.route('/submit', methods=['POST'])\ndef submit():\n    return []\n",
    )
    .unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();
    let warm = tool_call_json(
        "semantic_code_search",
        serde_json::json!({"query": "orders"}),
    );
    let _ = server.handle_message(&warm).unwrap();

    // ANY-stored Flask route must satisfy an explicit GET request.
    let msg = tool_call_json(
        "trace_http_chain",
        serde_json::json!({"route_path": "GET /orders"}),
    );
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    let handlers = result["handlers"].as_array().unwrap();
    assert_eq!(
        handlers.len(),
        1,
        "ANY-stored route must match GET, got: {result}"
    );
    assert_eq!(handlers[0]["handler_name"], "list_orders");

    // Explicit-method route still filters: GET must not match a POST route.
    let msg = tool_call_json(
        "trace_http_chain",
        serde_json::json!({"route_path": "GET /submit"}),
    );
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    assert!(
        result["handlers"].as_array().unwrap().is_empty(),
        "explicit POST route must not match GET, got: {result}"
    );
}

#[test]
fn test_project_map_detects_main_entry_points() {
    let project = TempDir::new().unwrap();
    // Rust-style main function
    fs::write(
        project.path().join("main.rs"),
        "fn main() { println!(\"hello\"); }",
    )
    .unwrap();
    // JS-style main function
    fs::write(
        project.path().join("index.js"),
        "async function main() { run(); }\nmain();",
    )
    .unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();

    let msg = tool_call_json("project_map", serde_json::json!({}));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);

    let entry_points = result["entry_points"].as_array().unwrap();
    assert!(
        !entry_points.is_empty(),
        "project_map should detect main entry points"
    );
    let handlers: Vec<&str> = entry_points
        .iter()
        .map(|e| e["handler"].as_str().unwrap())
        .collect();
    assert!(
        handlers.contains(&"main"),
        "should find main function as entry point"
    );
}

#[test]
fn test_project_map_hot_functions_excludes_test_prefix() {
    let project = TempDir::new().unwrap();
    fs::write(
        project.path().join("lib.ts"),
        r#"
function realWork() { return helper(); }
function helper() { return 42; }
function test_something() { realWork(); realWork(); realWork(); }
"#,
    )
    .unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();

    let msg = tool_call_json("project_map", serde_json::json!({}));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);

    let hot = result["hot_functions"].as_array().unwrap();
    let hot_names: Vec<&str> = hot.iter().map(|h| h["name"].as_str().unwrap()).collect();
    assert!(
        !hot_names.contains(&"test_something"),
        "hot_functions should exclude test_ prefixed functions, got: {:?}",
        hot_names
    );
}

#[test]
fn test_project_map_module_dependencies() {
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(
        project.path().join("src/utils.ts"),
        r#"
export function add(a: number, b: number): number { return a + b; }
"#,
    )
    .unwrap();
    fs::write(
        project.path().join("src/app.ts"),
        r#"
import { add } from './utils';
function main() { return add(1, 2); }
"#,
    )
    .unwrap();

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
    assert!(
        result["hot_functions"].is_array(),
        "hot_functions should be an array"
    );
}

#[test]
fn test_parse_timeout_does_not_hang() {
    use code_graph_mcp::domain::parse_timeout_ms;

    // Verify the value exists and is reasonable
    let timeout = parse_timeout_ms();
    assert!(
        timeout > 0 && timeout <= 30_000,
        "parse_timeout_ms should be between 1 and 30000, got {}",
        timeout
    );

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
    assert!(
        elapsed.as_secs() < 10,
        "parse_tree should not hang, took {:?}",
        elapsed
    );
    // Result can be Ok or Err (timeout) - both are acceptable
    drop(result);
}

/// Every `start_line` anywhere in a response, so a shape change in one tool's
/// envelope cannot quietly turn the assertion below into "found nothing".
fn all_start_lines(v: &serde_json::Value, out: &mut Vec<i64>) {
    match v {
        serde_json::Value::Object(map) => {
            for (k, val) in map {
                if k == "start_line" {
                    if let Some(n) = val.as_i64() {
                        out.push(n);
                    }
                }
                all_start_lines(val, out);
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| all_start_lines(x, out)),
        _ => {}
    }
}

/// `module_overview` and `find_dead_code` call `ensure_file_fresh_opt(path)` — a
/// FILE refresher — and both are normally called with a directory (or nothing).
/// A directory is classified fresh, so the call is a no-op, `did_reindex` stays
/// false, the 60s overview cache is never evicted, and the answer carries
/// pre-edit line numbers with no `freshness` disclosure. Neither tool was in
/// `RESULT_REFRESH_TOOLS`, the mechanism built for answers whose files are only
/// known after the query runs (audit 2026-08-29 CON-02).
#[test]
fn directory_scoped_tools_refresh_their_result_set() {
    // One project per tool: a shared server would let the first tool's edit land
    // in the index through the second's `ensure_indexed`, and the second case
    // would then be measuring the first one's leftovers.
    assert_directory_tool_refreshes("module_overview", serde_json::json!({"path": "src"}), 1);
    assert_directory_tool_refreshes(
        "find_dead_code",
        serde_json::json!({"path": "src", "min_lines": 1}),
        4,
    );
}

/// `line` is where the symbol this tool reports sits before the edit; the edit
/// prepends three lines.
fn assert_directory_tool_refreshes(tool: &str, args: serde_json::Value, line: i64) {
    let project = TempDir::new().unwrap();
    fs::create_dir(project.path().join("src")).unwrap();
    let file = project.path().join("src/a.ts");
    // `alpha` has a caller, so it lands in `active_exports` (which carries line
    // numbers) rather than the name-only `inactive_summary`; `orphan` has none,
    // so it is what `find_dead_code` reports.
    const PRISTINE: &str =
        "export function alpha() {\n  return 1;\n}\nexport function orphan() {\n  return 2;\n}\n";
    const EDITED: &str = "// a\n// b\n// c\nexport function alpha() {\n  return 1;\n}\nexport function orphan() {\n  return 2;\n}\n";
    fs::write(
        project.path().join("src/b.ts"),
        "import { alpha } from './a';\nexport function beta() { return alpha(); }\n",
    )
    .unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();

    let ask = |tool: &str, args: serde_json::Value| {
        let msg = tool_call_json(tool, args);
        parse_tool_result(&server.handle_message(&msg).unwrap())
    };

    fs::write(&file, PRISTINE).unwrap();
    // Three calls, not two. `from_project_root` starts `last_incremental_check`
    // 60s in the past against a 30s debounce, so the call after the first one
    // runs an incremental backstop pass and RESETS that timer. Only from there
    // on is a query inside the debounce window — which is the window this defect
    // lives in, and the one a real session spends almost all of its time in.
    ask(tool, args.clone()); // full index
    let before = ask(tool, args.clone()); // backstop pass; debounce now armed
    let mut lines = Vec::new();
    all_start_lines(&before, &mut lines);
    assert!(
        lines.contains(&line),
        "{tool} baseline must report line {line}: {before}"
    );

    // Push it down three lines. Same call, well within the 60s cache window.
    fs::write(&file, EDITED).unwrap();
    let after = ask(tool, args.clone());
    let mut lines = Vec::new();
    all_start_lines(&after, &mut lines);
    assert!(
        lines.contains(&(line + 3)),
        "{tool} answered pre-edit line numbers with no disclosure: {after}"
    );
}

/// `HONORED_UNDECLARED_ARGS` documents `skip_indexing` as "read by every tool
/// through `should_skip_indexing`" — and every tool does read it, in its own
/// dispatch arm. FRS-2 (result-set freshness) arrived later and wrapped those
/// arms from OUTSIDE, so a caller that said "do not index" still got a write
/// handle, a resync and a re-dispatch. Declared-is-not-honored on a contract
/// that has both documentation and tests (audit 2026-08-29 CON-01).
#[test]
fn skip_indexing_suppresses_result_set_freshness() {
    let project = TempDir::new().unwrap();
    let file = project.path().join("main.ts");
    fs::write(&file, "export function hello() { return 42; }\n").unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();

    let ask = |args: serde_json::Value| {
        let msg = tool_call_json("ast_search", args);
        parse_tool_result(&server.handle_message(&msg).unwrap())
    };
    let first_line = |v: &serde_json::Value| -> i64 {
        v.get("results")
            .and_then(|r| r.as_array())
            .and_then(|a| a.first())
            .and_then(|h| h.get("start_line"))
            .and_then(|l| l.as_i64())
            .unwrap_or_else(|| panic!("no start_line in {v}"))
    };

    let before = ask(serde_json::json!({"query": "hello"}));
    assert_eq!(first_line(&before), 1, "baseline: {before}");

    // Push the symbol down. On disk it is line 4; the index still says 1.
    fs::write(
        &file,
        "// a\n// b\n// c\nexport function hello() { return 42; }\n",
    )
    .unwrap();

    let skipped = ask(serde_json::json!({"query": "hello", "skip_indexing": true}));
    assert_eq!(
        first_line(&skipped),
        1,
        "skip_indexing:true must answer from the index, not re-index: {skipped}"
    );
    assert!(
        skipped.get("freshness").is_none(),
        "a skipped call did no refresh, so it must not disclose one: {skipped}"
    );

    // Positive control in the same test: without the flag the SAME edit is
    // picked up, so the assertions above cannot pass by freshness being broken.
    let refreshed = ask(serde_json::json!({"query": "hello"}));
    assert_eq!(
        first_line(&refreshed),
        4,
        "without skip_indexing the edit must be picked up: {refreshed}"
    );
}

#[test]
fn test_skip_indexing_flag() {
    let project = TempDir::new().unwrap();
    fs::write(
        project.path().join("main.ts"),
        "export function hello() { return 42; }",
    )
    .unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();

    // First call without skip — triggers indexing
    let msg = tool_call_json(
        "semantic_code_search",
        serde_json::json!({"query": "hello"}),
    );
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    // semantic_code_search answers the {results, ...} envelope on every path
    // (2026-08-16 batch); search_hits unwraps it.
    assert!(
        !search_hits(&result).is_empty(),
        "should find hello after indexing"
    );

    // Second call with skip_indexing — should still work (index already built)
    let msg2 = tool_call_json(
        "semantic_code_search",
        serde_json::json!({
            "query": "hello",
            "skip_indexing": true
        }),
    );
    let resp2 = server.handle_message(&msg2).unwrap();
    let result2 = parse_tool_result(&resp2);
    assert!(
        !search_hits(&result2).is_empty(),
        "should find hello with skip_indexing when already indexed"
    );

    // Third call: skip_indexing on a fresh server with no prior indexing should return empty results (not error)
    let project2 = TempDir::new().unwrap();
    fs::write(
        project2.path().join("main.ts"),
        "export function world() { return 99; }",
    )
    .unwrap();
    let server2 = McpServer::from_project_root(project2.path()).unwrap();
    let init2 = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server2.handle_message(init2).unwrap();

    let msg3 = tool_call_json(
        "semantic_code_search",
        serde_json::json!({
            "query": "world",
            "skip_indexing": true
        }),
    );
    let resp3 = server2.handle_message(&msg3).unwrap();
    let result3 = parse_tool_result(&resp3);
    // With skip_indexing and no prior indexing, there should be no results (empty DB)
    // Empty results return an object with results:[] and a message, not a bare array
    let empty_results = result3
        .get("results")
        .and_then(|r| r.as_array())
        .or_else(|| result3.as_array());
    assert!(
        empty_results.is_none_or(|a| a.is_empty()),
        "should return empty results when skip_indexing with no prior index, got: {}",
        result3
    );
}

#[test]
fn test_get_ast_node_compact_mode() {
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(
        project.path().join("src/lib.ts"),
        r#"
export function processData(input: string): number {
    const parsed = JSON.parse(input);
    return parsed.value * 2;
}
"#,
    )
    .unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();
    let search = tool_call_json(
        "semantic_code_search",
        serde_json::json!({"query": "processData"}),
    );
    let _ = server.handle_message(&search).unwrap();

    // Non-compact: should have code_content
    let msg = tool_call_json(
        "get_ast_node",
        serde_json::json!({
            "file_path": "src/lib.ts",
            "symbol_name": "processData"
        }),
    );
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    assert!(
        result["code_content"].is_string(),
        "non-compact should have code_content"
    );

    // Compact mode: should NOT have code_content
    let msg = tool_call_json(
        "get_ast_node",
        serde_json::json!({
            "file_path": "src/lib.ts",
            "symbol_name": "processData",
            "compact": true
        }),
    );
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    assert!(
        result["code_content"].is_null(),
        "compact should strip code_content"
    );
    assert!(result["name"].is_string(), "compact should keep name");
    assert!(result["node_id"].is_number(), "compact should keep node_id");
    assert!(result["type"].is_string(), "compact should keep type");
    assert!(
        result["file_path"].is_string(),
        "compact should keep file_path"
    );
    assert!(
        result["start_line"].is_number(),
        "compact should keep start_line"
    );
    assert!(
        result["signature"].is_string() || result["signature"].is_null(),
        "compact should keep signature"
    );

    // Compact via node_id
    let node_id = result["node_id"].as_i64().unwrap();
    let msg = tool_call_json(
        "get_ast_node",
        serde_json::json!({
            "node_id": node_id,
            "compact": true
        }),
    );
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    assert!(
        result["code_content"].is_null(),
        "compact via node_id should strip code_content"
    );
    assert_eq!(result["name"], "processData");
}

#[test]
fn test_find_references_compact_mode() {
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(
        project.path().join("src/util.ts"),
        r#"
export function helper(): number { return 42; }
"#,
    )
    .unwrap();
    fs::write(
        project.path().join("src/main.ts"),
        r#"
import { helper } from './util';
function run() { return helper(); }
"#,
    )
    .unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();
    let search = tool_call_json(
        "semantic_code_search",
        serde_json::json!({"query": "helper"}),
    );
    let _ = server.handle_message(&search).unwrap();

    // Non-compact: references should have type field
    let msg = tool_call_json(
        "find_references",
        serde_json::json!({
            "symbol_name": "helper"
        }),
    );
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    let refs = result["references"].as_array().unwrap();
    assert!(!refs.is_empty(), "should find references to helper");
    // Non-compact references include "type" field
    for r in refs {
        assert!(r["type"].is_string(), "non-compact should have type field");
    }

    // Compact mode: references should NOT have type field
    let msg = tool_call_json(
        "find_references",
        serde_json::json!({
            "symbol_name": "helper",
            "compact": true
        }),
    );
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
        assert!(
            r["start_line"].is_number(),
            "compact should keep start_line"
        );
    }
}

#[test]
fn test_fts5_keyword_query_does_not_leak_syntax_error() {
    // Regression: a query consisting solely of FTS5 reserved words (NOT/AND/OR/NEAR)
    // leaked the raw "fts5: syntax error near \"NOT\"" error to the caller because
    // sanitized tokens were passed bare to MATCH and re-parsed as operators.
    // Each token is now wrapped in double quotes so it's treated as a phrase.
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(
        project.path().join("src/a.ts"),
        "export function helper(): number { return 1; }\n",
    )
    .unwrap();
    let server = common::init_server(&project);

    for query in ["NOT", "AND OR NOT", "NEAR", "OR"] {
        let msg = tool_call_json("semantic_code_search", serde_json::json!({"query": query}));
        let resp = server.handle_message(&msg).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(resp.as_ref().unwrap()).unwrap();
        let is_err = parsed["result"]["isError"] == serde_json::Value::Bool(true);
        let text = parsed["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("");
        assert!(
            !is_err || !text.contains("fts5:"),
            "FTS5 keyword query '{query}' should not leak raw FTS5 syntax error; got: {text}"
        );
    }
}

#[test]
fn test_module_overview_empty_path_errors() {
    // Regression: path:"" used to normalize to "" the same way path:"." does,
    // silently returning the whole-project overview. Common variable-substitution
    // bug at the call site — must error instead of silently dumping everything.
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(
        project.path().join("src/a.ts"),
        "export function a() { return 1; }\n",
    )
    .unwrap();
    let server = common::init_server(&project);

    let msg = tool_call_json("module_overview", serde_json::json!({"path": ""}));
    let resp = server.handle_message(&msg).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(resp.as_ref().unwrap()).unwrap();
    assert_eq!(
        parsed["result"]["isError"],
        serde_json::Value::Bool(true),
        "empty path must error: {:?}",
        parsed
    );
    let text = parsed["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    assert!(
        text.contains("must not be empty"),
        "should explain the empty-path requirement; got: {text}"
    );

    // "." should still work as the "match all" alias
    let msg = tool_call_json("module_overview", serde_json::json!({"path": "."}));
    let resp = server.handle_message(&msg).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(resp.as_ref().unwrap()).unwrap();
    assert_ne!(
        parsed["result"]["isError"],
        serde_json::Value::Bool(true),
        "'.' should still be the match-all alias: {:?}",
        parsed
    );
}

/// MCP `find_references` must publish the edge confidence tier and honour a
/// `min_confidence` floor, like its CLI twin `refs`.
///
/// Rename audits are the single worst place to hide a by-name collision, and this
/// was the one symbol surface that neither tagged nor filtered: `x.save()` with an
/// untyped receiver produces `ambiguous` edges to EVERY `save` in the repo, and the
/// tool returned them indistinguishable from an import-resolved hit
/// (audit 2026-08-16 P1-11).
#[test]
fn test_find_references_reports_confidence_and_honours_min_confidence() {
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    // Two same-named methods + an untyped receiver: `run` binds to BOTH by name,
    // so both edges land in the `ambiguous` tier.
    fs::write(
        project.path().join("src/a.ts"),
        "export class A { save(): number { return 1; } }\n",
    )
    .unwrap();
    fs::write(
        project.path().join("src/b.ts"),
        "export class B { save(): number { return 2; } }\n",
    )
    .unwrap();
    fs::write(
        project.path().join("src/main.ts"),
        "export function run(x: any): number { return x.save(); }\n",
    )
    .unwrap();
    let server = common::init_server(&project);

    // No floor (the default): every reference comes back, each tagged.
    let msg = tool_call_json(
        "find_references",
        serde_json::json!({"symbol_name": "save", "file_path": "src/a.ts"}),
    );
    let result = parse_tool_result(&server.handle_message(&msg).unwrap());
    let refs = result["references"].as_array().unwrap();
    assert!(
        !refs.is_empty(),
        "should find the ambiguous caller: {result}"
    );
    for r in refs {
        let c = r["confidence"]
            .as_str()
            .unwrap_or_else(|| panic!("every reference must carry its confidence tier, got: {r}"));
        assert!(
            ["extracted", "inferred", "ambiguous"].contains(&c),
            "unexpected tier '{c}' in {r}"
        );
    }
    assert!(
        refs.iter().any(|r| r["confidence"] == "ambiguous"),
        "the untyped-receiver call must be reported as ambiguous: {result}"
    );
    assert!(
        result.get("confidence_filtered").is_none(),
        "no floor means nothing filtered: {result}"
    );

    // Floor at `inferred`: the ambiguous fan-out drops out AND the drop is disclosed.
    let msg = tool_call_json(
        "find_references",
        serde_json::json!({
            "symbol_name": "save",
            "file_path": "src/a.ts",
            "min_confidence": "inferred",
        }),
    );
    let filtered = parse_tool_result(&server.handle_message(&msg).unwrap());
    assert!(
        filtered["references"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["confidence"] != "ambiguous"),
        "min_confidence=inferred must drop the ambiguous tier: {filtered}"
    );
    assert!(
        filtered["confidence_filtered"].as_u64().unwrap_or(0) > 0,
        "a filtered-down list must disclose the count, or it reads as complete: {filtered}"
    );

    // Compact mode keeps the tier — it is the field the caller judges a hit by.
    let msg = tool_call_json(
        "find_references",
        serde_json::json!({"symbol_name": "save", "file_path": "src/a.ts", "compact": true}),
    );
    let compact = parse_tool_result(&server.handle_message(&msg).unwrap());
    for r in compact["references"].as_array().unwrap() {
        assert!(
            r["confidence"].is_string(),
            "compact must keep confidence, got: {r}"
        );
    }
}

/// Entry-validate the new enum, like `relation` above: a typo'd tier must name
/// the valid set, not silently pass every row through.
#[test]
fn test_find_references_invalid_min_confidence_errors() {
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(
        project.path().join("src/a.ts"),
        "export function helper(): number { return 1; }\nfunction run() { return helper(); }\n",
    )
    .unwrap();
    let server = common::init_server(&project);

    let msg = tool_call_json(
        "find_references",
        // NB: "high"/"low"/"exact" are documented ALIASES (domain::normalize_confidence),
        // so a bogus value has to be genuinely bogus to test the gate.
        serde_json::json!({"symbol_name": "helper", "min_confidence": "very_high"}),
    );
    let resp = server.handle_message(&msg).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(resp.as_ref().unwrap()).unwrap();
    assert_eq!(
        parsed["result"]["isError"],
        serde_json::Value::Bool(true),
        "invalid min_confidence must error: {parsed:?}"
    );
    let text = parsed["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    assert!(
        text.contains("extracted") && text.contains("ambiguous"),
        "the error must name the valid set; got: {text}"
    );
}

#[test]
fn test_find_references_invalid_relation_errors() {
    // Regression: unknown `relation` values used to fall through to None (no filter)
    // and silently return "all" results, masking caller typos like relation:"call".
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(
        project.path().join("src/a.ts"),
        "export function helper(): number { return 1; }\nfunction run() { return helper(); }\n",
    )
    .unwrap();
    let server = common::init_server(&project);
    let init = tool_call_json(
        "semantic_code_search",
        serde_json::json!({"query": "helper"}),
    );
    let _ = server.handle_message(&init);

    let msg = tool_call_json(
        "find_references",
        serde_json::json!({
            "symbol_name": "helper",
            "relation": "BOGUS_RELATION",
        }),
    );
    let resp = server.handle_message(&msg).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(resp.as_ref().unwrap()).unwrap();
    assert_eq!(
        parsed["result"]["isError"],
        serde_json::Value::Bool(true),
        "invalid relation must error, not silently fall back: {:?}",
        parsed
    );
    let text = parsed["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    assert!(
        text.contains("Unknown relation"),
        "should explain the typo; got: {text}"
    );
}

/// Task 5: the new `references` relation filter must be ACCEPTED (not rejected
/// as "Unknown relation filter") and must surface the `references` edge(s).
///
/// `make_widget() -> WidgetConfig` emits a `references` edge from `make_widget`
/// to `WidgetConfig` (return-type usage), so a `relation:"references"` query on
/// `WidgetConfig` must return `make_widget` and nothing about an unknown filter.
#[test]
fn test_find_references_references_relation_accepted() {
    let project = TempDir::new().unwrap();
    fs::write(
        project.path().join("lib.rs"),
        r#"
pub struct WidgetConfig { pub size: u32 }
pub fn make_widget() -> WidgetConfig { WidgetConfig { size: 1 } }
"#,
    )
    .unwrap();
    let server = common::init_server(&project);
    let warm = tool_call_json(
        "semantic_code_search",
        serde_json::json!({"query": "WidgetConfig"}),
    );
    let _ = server.handle_message(&warm);

    let msg = tool_call_json(
        "find_references",
        serde_json::json!({
            "symbol_name": "WidgetConfig",
            "relation": "references",
        }),
    );
    let resp = server.handle_message(&msg).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(resp.as_ref().unwrap()).unwrap();

    // Must NOT be rejected as an unknown filter.
    assert_ne!(
        parsed["result"]["isError"],
        serde_json::Value::Bool(true),
        "relation:\"references\" must be accepted, not rejected: {parsed:?}"
    );
    let text = parsed["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    assert!(
        !text.contains("Unknown relation"),
        "relation:\"references\" must not hit the unknown-filter path; got: {text}"
    );

    // Must surface the real references edge (make_widget -> WidgetConfig).
    let result = parse_tool_result(&resp);
    let refs = result["references"].as_array().unwrap();
    assert!(
        refs.iter()
            .any(|r| r["name"] == "make_widget" && r["relation"] == "references"),
        "expected a references edge from make_widget to WidgetConfig; got: {refs:?}"
    );
}

#[test]
fn test_get_call_graph_symbol_and_route_mutually_exclusive() {
    // Regression: passing both symbol_name and route_path used to silently dispatch
    // to route mode and drop symbol_name on the floor. The schema marks them
    // mutually exclusive — enforce it so conflicting input surfaces as an error.
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(
        project.path().join("src/a.ts"),
        "export function helper(): number { return 1; }\n",
    )
    .unwrap();
    let server = common::init_server(&project);
    let init = tool_call_json(
        "semantic_code_search",
        serde_json::json!({"query": "helper"}),
    );
    let _ = server.handle_message(&init);

    let msg = tool_call_json(
        "get_call_graph",
        serde_json::json!({
            "symbol_name": "helper",
            "route_path": "GET /api/x",
        }),
    );
    let resp = server.handle_message(&msg).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(resp.as_ref().unwrap()).unwrap();
    assert_eq!(
        parsed["result"]["isError"],
        serde_json::Value::Bool(true),
        "conflicting symbol_name+route_path must error: {:?}",
        parsed
    );
    let text = parsed["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    assert!(
        text.contains("mutually exclusive"),
        "should explain the conflict; got: {text}"
    );
}

#[test]
fn test_dependency_graph_compact_mode() {
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(
        project.path().join("src/db.ts"),
        r#"
export function query(sql: string): any[] { return []; }
"#,
    )
    .unwrap();
    fs::write(
        project.path().join("src/repo.ts"),
        r#"
import { query } from './db';
export function findUser(id: number) { return query('SELECT * FROM users WHERE id=' + id); }
"#,
    )
    .unwrap();
    fs::write(
        project.path().join("src/api.ts"),
        r#"
import { findUser } from './repo';
export function getUser(req: any) { return findUser(req.params.id); }
"#,
    )
    .unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();
    let search = tool_call_json(
        "semantic_code_search",
        serde_json::json!({"query": "findUser"}),
    );
    let _ = server.handle_message(&search).unwrap();

    // Non-compact: should have symbols field for depth-1 deps
    let msg = tool_call_json(
        "dependency_graph",
        serde_json::json!({
            "file_path": "src/repo.ts",
            "direction": "both",
            "depth": 2
        }),
    );
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    let depends_on = result["depends_on"].as_array().unwrap();
    assert!(!depends_on.is_empty(), "should have outgoing deps");
    // Non-compact depth-1 deps have symbols
    let depth1 = depends_on.iter().find(|d| d["depth"].as_i64() == Some(1));
    assert!(depth1.is_some(), "should have depth-1 dep");
    assert!(
        depth1.unwrap()["symbols"].is_number(),
        "non-compact depth-1 should have symbols count"
    );

    // Compact mode: should NOT have symbols field
    let msg = tool_call_json(
        "dependency_graph",
        serde_json::json!({
            "file_path": "src/repo.ts",
            "direction": "both",
            "depth": 2,
            "compact": true
        }),
    );
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    let depends_on = result["depends_on"].as_array().unwrap();
    assert!(
        !depends_on.is_empty(),
        "compact should still have outgoing deps"
    );
    for dep in depends_on {
        assert!(dep["file"].is_string(), "compact should keep file");
        assert!(dep["depth"].is_number(), "compact should keep depth");
        assert!(dep["symbols"].is_null(), "compact should strip symbols");
    }
    let depended_by = result["depended_by"].as_array().unwrap();
    for dep in depended_by {
        assert!(
            dep["symbols"].is_null(),
            "compact should strip symbols from incoming deps too"
        );
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
    let search = tool_call_json("semantic_code_search", serde_json::json!({"query": "data"}));
    let resp = server.handle_message(&search).unwrap();
    let results = parse_tool_result(&resp);
    let results_arr = search_hits(&results);
    let names: Vec<&str> = results_arr
        .iter()
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
    let results_arr = search_hits(&results);
    // Verify the CJK name is preserved in the result
    let names: Vec<&str> = results_arr
        .iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    assert!(
        names.iter().any(|n| n.chars().any(|c| c > '\u{4E00}')),
        "CJK identifier should be preserved in search results, got names: {:?}",
        names
    );
}

// --- Protocol error-path tests ---

#[test]
fn test_malformed_json_returns_parse_error() {
    let project = TempDir::new().unwrap();
    let server = common::init_server(&project);

    let resp = server.handle_message("not valid json{{{").unwrap();
    let parsed: serde_json::Value = serde_json::from_str(resp.as_ref().unwrap()).unwrap();
    assert!(parsed["error"].is_object());
    assert_eq!(parsed["error"]["code"], -32700); // Parse error
}

#[test]
fn test_wrong_jsonrpc_version_returns_error() {
    let project = TempDir::new().unwrap();
    let server = common::init_server(&project);

    let msg = serde_json::json!({
        "jsonrpc": "1.0",
        "id": 1,
        "method": "tools/list",
    });
    let resp = server.handle_message(&msg.to_string()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(resp.as_ref().unwrap()).unwrap();
    assert!(parsed["error"].is_object());
}

#[test]
fn test_tools_call_missing_name_returns_error() {
    let project = TempDir::new().unwrap();
    let server = common::init_server(&project);

    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "arguments": {}
        }
    });
    let resp = server.handle_message(&msg.to_string()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(resp.as_ref().unwrap()).unwrap();
    assert!(
        parsed["error"].is_object() || parsed["result"]["isError"] == true,
        "Missing tool name should error: {:?}",
        parsed
    );
}

/// CON-09 (audit 2026-08-29): a non-object `arguments` must be diagnosed as the
/// envelope problem it is, not reported as a missing parameter.
///
/// Every tool reads its parameters with `args["path"]`, which yields Null on a
/// JSON string, so this call used to answer "Error: Missing path" — sending the
/// caller to look at a parameter it did pass. `note_ignored_arguments` could not
/// cover for it either: it bails when `as_object()` is None, so nothing was
/// disclosed at all.
#[test]
fn test_tools_call_non_object_arguments_names_the_real_problem() {
    let project = TempDir::new().unwrap();
    let server = common::init_server(&project);

    for bad in [
        serde_json::json!("src/main.rs"),
        serde_json::json!(42),
        serde_json::json!(["src/main.rs"]),
        serde_json::json!(true),
    ] {
        let msg = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "module_overview", "arguments": bad }
        });
        let resp = server.handle_message(&msg.to_string()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(resp.as_ref().unwrap()).unwrap();
        assert_eq!(
            parsed["error"]["code"], -32602,
            "must be invalid-params: {parsed:?}"
        );
        let message = parsed["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("'arguments' must be a JSON object"),
            "the error must name the envelope, not a parameter: {message}"
        );
        assert!(
            !message.contains("Missing path"),
            "the old misdiagnosis must not come back: {message}"
        );
    }
}

/// The negative control for the check above: absent and null `arguments` are
/// legitimate — several tools take no parameters — so the type gate must not
/// turn into "arguments are mandatory".
#[test]
fn test_tools_call_absent_or_null_arguments_still_dispatch() {
    let project = TempDir::new().unwrap();
    let server = common::init_server(&project);

    for params in [
        serde_json::json!({ "name": "get_index_status" }),
        serde_json::json!({ "name": "get_index_status", "arguments": null }),
        serde_json::json!({ "name": "get_index_status", "arguments": {} }),
    ] {
        let msg = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": params
        });
        let resp = server.handle_message(&msg.to_string()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(resp.as_ref().unwrap()).unwrap();
        assert!(
            parsed["error"].is_null(),
            "a tool taking no arguments must still dispatch: {parsed:?}"
        );
    }
}

#[test]
fn test_unknown_method_returns_error() {
    let project = TempDir::new().unwrap();
    let server = common::init_server(&project);

    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "nonexistent/method",
    });
    let resp = server.handle_message(&msg.to_string()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(resp.as_ref().unwrap()).unwrap();
    assert!(parsed["error"].is_object());
    assert_eq!(parsed["error"]["code"], -32601); // Method not found
}

// ---- Audit regression fixes (2026-04-17) ----

/// Fix #1: resolve_fuzzy_name must prefer exact name over substring matches.
/// Without this, `find_references("handle")` would report ambiguity because
/// `handle_foo`, `handle_bar` also match the LIKE '%handle%' fuzzy query.
#[test]
fn test_find_references_prefers_exact_over_substring() {
    let project = TempDir::new().unwrap();
    fs::write(
        project.path().join("lib.ts"),
        r#"
function handle() { return 1; }
function handle_one() { return handle(); }
function handle_two() { return handle(); }
function caller() { return handle(); }
"#,
    )
    .unwrap();

    let server = common::init_server(&project);
    let msg = tool_call_json(
        "find_references",
        serde_json::json!({"symbol_name":"handle","compact":true}),
    );
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);

    // Exact-name `handle` exists → must NOT report ambiguity with handle_one/handle_two
    assert!(
        result.get("error").is_none(),
        "find_references('handle') falsely reported ambiguity: {}",
        result
    );
    assert_eq!(result["symbol"], "handle");
    let refs = result["references"].as_array().unwrap();
    assert!(
        !refs.is_empty(),
        "expected at least one caller of handle, got empty"
    );
}

// Fix #2 (truncate_large_strings homogeneous arrays) is covered by a unit
// test inside src/mcp/server/helpers.rs — helpers is a private module so
// it must be tested from within the crate.

/// Fix #3a: project_map.hot_functions must only contain function/method types.
#[test]
fn test_project_map_hot_functions_excludes_structs() {
    let project = TempDir::new().unwrap();
    fs::write(
        project.path().join("lib.rs"),
        r#"
pub struct Foo;
pub fn bar() -> Foo { baz(); baz(); baz(); Foo }
pub fn baz() -> i32 { 1 }
pub fn call_bar() { bar(); bar(); bar(); }
"#,
    )
    .unwrap();

    let server = common::init_server(&project);
    let msg = tool_call_json("project_map", serde_json::json!({}));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);

    let hot = result["hot_functions"].as_array().unwrap();
    for h in hot {
        let ty = h["type"].as_str().unwrap_or("");
        assert!(
            ty == "function" || ty == "method",
            "hot_functions must not include non-function types: {}",
            h
        );
    }
}

/// Fix #3b: entry_points must carry `kind` distinguishing `main` vs `http_route`.
#[test]
fn test_project_map_entry_points_have_kind() {
    let project = TempDir::new().unwrap();
    fs::write(
        project.path().join("main.rs"),
        "fn main() { println!(\"hi\"); }",
    )
    .unwrap();
    let server = common::init_server(&project);

    let msg = tool_call_json("project_map", serde_json::json!({}));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);

    let eps = result["entry_points"].as_array().unwrap();
    assert!(!eps.is_empty(), "expected main entry point");
    let kinds: Vec<&str> = eps.iter().filter_map(|e| e["kind"].as_str()).collect();
    assert!(
        kinds.contains(&"main"),
        "main fn should produce kind='main', got kinds={:?}",
        kinds
    );
}

/// Fix #4: dependency_graph must drop the synthetic `<external>` bucket.
#[test]
fn test_dependency_graph_filters_external_sentinel() {
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(
        project.path().join("src/app.rs"),
        r#"
use std::collections::HashMap;
pub fn load() -> HashMap<String, String> { HashMap::new() }
"#,
    )
    .unwrap();
    fs::write(
        project.path().join("src/main.rs"),
        r#"
mod app;
fn main() { let _ = app::load(); }
"#,
    )
    .unwrap();

    let server = common::init_server(&project);
    let msg = tool_call_json(
        "dependency_graph",
        serde_json::json!({"file_path":"src/main.rs"}),
    );
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);

    let depends_on = result["depends_on"].as_array().unwrap();
    for d in depends_on {
        assert_ne!(
            d["file"].as_str().unwrap_or(""),
            "<external>",
            "depends_on must not contain <external>: {:?}",
            depends_on
        );
    }
}

/// `find_similar_code` must surface `cutoff_applied` when `max_distance` filters
/// candidates out below `top_k`.
///
/// This test used to be VACUOUS IN EVERY CI LEG (2026-08-16 audit §四). It had
/// four silent `return`s and its only assertion behind `if count < 5`, and both
/// the default and the `embed-model` build took the same early exit: the tool
/// refuses with "No embeddings found" because the fixture has none, so the
/// cutoff-tracking code it names was never executed. Measured, not inferred —
/// instrumenting the arms showed the identical error payload on both legs.
///
/// The path is reachable without a model: `find_similar_code` needs
/// `vec_enabled` (sqlite-vec is bundled regardless of the feature) and at least
/// one embedded node. Embeddings are just f32 arrays, so the vectors are seeded
/// directly and the assertions run unconditionally on both legs.
#[test]
fn test_find_similar_code_reports_cutoff() {
    use code_graph_mcp::domain::EMBEDDING_DIM;
    use code_graph_mcp::storage::queries;

    let project = TempDir::new().unwrap();
    fs::write(
        project.path().join("lib.rs"),
        r#"
pub fn alpha() -> i32 { 1 }
pub fn beta() -> i32 { 2 }
pub fn gamma() -> i32 { 3 }
"#,
    )
    .unwrap();
    let server = common::init_server(&project);
    // `initialize` alone does not index; the first tool call is what triggers
    // `ensure_indexed`. Force it before seeding, or there are no node ids to seed.
    // A tool call that goes through `ensure_indexed`. `get_index_status` reads
    // state without indexing, so it leaves the DB empty; this one errors (no
    // embeddings yet, which is the point) but indexes on the way there.
    let _ = server.handle_message(&tool_call_json(
        "find_similar_code",
        serde_json::json!({"symbol_name":"alpha"}),
    ));
    assert!(
        server.db().vec_enabled(),
        "sqlite-vec is bundled in every build — without it this test cannot mean anything"
    );

    // Seed mutually distant unit vectors: each symbol gets a different basis
    // vector, so every pairwise L2 distance is sqrt(2) ≈ 1.414 — comfortably
    // above the tight max_distance below and comfortably below the loose one.
    let mut seeded = 0usize;
    for (i, name) in ["alpha", "beta", "gamma"].iter().enumerate() {
        for (id, _) in queries::get_node_ids_by_name(server.db().conn(), name).unwrap() {
            let mut v = vec![0.0f32; EMBEDDING_DIM];
            v[i] = 1.0;
            queries::insert_node_vectors_batch(server.db().conn(), &[(id, v)]).unwrap();
            seeded += 1;
        }
    }
    assert!(
        seeded >= 3,
        "expected alpha/beta/gamma to be indexed, seeded {seeded}"
    );

    let ask = |max_distance: f64| {
        let msg = tool_call_json(
            "find_similar_code",
            serde_json::json!({"symbol_name":"alpha","top_k":5,"max_distance":max_distance}),
        );
        let resp = server.handle_message(&msg).unwrap().expect("a response");
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert!(
            parsed["result"]["isError"] != serde_json::Value::Bool(true),
            "the seeded vectors must get past the no-embeddings guard: {parsed}"
        );
        let text = parsed["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        serde_json::from_str::<serde_json::Value>(&text).unwrap()
    };

    // Positive control FIRST. Without it, a tool that returns nothing for any
    // input would satisfy the cutoff assertion below for the wrong reason —
    // which is exactly how the previous version of this test passed.
    let loose = ask(2.0);
    assert!(
        loose["count"].as_i64().unwrap_or(0) > 0,
        "a permissive max_distance must return the seeded neighbours: {loose}"
    );

    // Tight cutoff: every neighbour sits at sqrt(2), so nothing survives — and
    // the response has to SAY that rather than look like an empty index.
    let tight = ask(0.0);
    let count = tight["count"].as_i64().unwrap_or(0);
    assert!(
        count < 5,
        "a 0.0 cutoff must filter the neighbours out: {tight}"
    );
    // Demanded outright, with no "…or the result set is empty" escape hatch: the
    // fixture guarantees candidates existed and were dropped, so an undisclosed
    // empty answer is exactly the failure this test is named for. (That escape
    // hatch is what let the previous version pass while asserting nothing.)
    assert_eq!(
        tight["cutoff_applied"],
        serde_json::json!(true),
        "a cutoff that removed candidates must say so — an undisclosed short list \
         reads to the caller as 'nothing similar exists': {tight}"
    );
    assert!(
        tight["cutoff_dropped"].as_i64().unwrap_or(0) >= 2,
        "both non-query neighbours were dropped; the count must say so: {tight}"
    );
}

/// v0.11.2 fix: `module_overview` must not leak inline `#[cfg(test)]` functions
/// whose names don't match the `test_*` / `*Test` naming heuristic.
#[test]
fn test_module_overview_excludes_cfg_test_functions() {
    let project = TempDir::new().unwrap();
    fs::write(
        project.path().join("lib.rs"),
        r#"
pub fn compute_thing() -> i32 { 42 }

#[cfg(test)]
mod tests {
    #[test]
    fn arrays_are_homogeneous() { assert_eq!(1, 1); }

    #[test]
    fn nothing_prefix_matches_test() { assert_eq!(2, 2); }
}
"#,
    )
    .unwrap();

    let server = common::init_server(&project);
    let msg = tool_call_json("module_overview", serde_json::json!({"path":"."}));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);

    // All exported names across active + inactive — no leaked test fns.
    let mut all_names: Vec<String> = Vec::new();
    if let Some(active) = result["active_exports"].as_array() {
        for e in active {
            if let Some(n) = e["name"].as_str() {
                all_names.push(n.into());
            }
        }
    }
    if let Some(inactive) = result["inactive_summary"].as_array() {
        for bucket in inactive {
            if let Some(names) = bucket["names"].as_array() {
                for n in names {
                    if let Some(s) = n.as_str() {
                        all_names.push(s.into());
                    }
                }
            }
        }
    }
    assert!(
        all_names.iter().any(|n| n == "compute_thing"),
        "expected real export 'compute_thing' in overview, got: {:?}",
        all_names
    );
    for leak in ["arrays_are_homogeneous", "nothing_prefix_matches_test"] {
        assert!(
            !all_names.iter().any(|n| n == leak),
            "#[cfg(test)] fn '{}' leaked into module_overview: {:?}",
            leak,
            all_names
        );
    }
}

/// v0.22.x fix: `module_overview compact: true + include_dead: true` must
/// preserve the `dead_code` field. Previously `compact_module_overview`
/// silently dropped it because the field wasn't in the forwarding allowlist —
/// users got "include_dead silently no-op'd" with no way to see why.
#[test]
fn test_module_overview_compact_preserves_dead_code() {
    let project = TempDir::new().unwrap();
    fs::write(
        project.path().join("lib.rs"),
        r#"
pub fn used_fn() -> i32 { 42 }
pub fn caller() -> i32 { used_fn() }

// Orphan: no callers, exported but not imported anywhere
pub fn orphan_long_function_name() -> i32 {
    let a = 1;
    let b = 2;
    let c = 3;
    a + b + c
}
"#,
    )
    .unwrap();

    let server = common::init_server(&project);
    let msg = tool_call_json(
        "module_overview",
        serde_json::json!({
            "path": ".", "include_dead": true, "compact": true
        }),
    );
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);

    assert!(
        result.get("dead_code").is_some(),
        "compact mode must forward dead_code; got keys: {:?}",
        result.as_object().map(|o| o.keys().collect::<Vec<_>>())
    );
    let dead = &result["dead_code"];
    assert!(
        dead["orphan_count"].is_number(),
        "dead_code must have numeric orphan_count, got: {}",
        dead
    );
}

/// project_map include_centrality (roadmap 2026-07-18 §2.4): the CLI-only
/// `centrality` gets an MCP surface as a project_map flag. Multi-hop chain
/// (entry → bridge → leaf ×2) makes `bridge` the chokepoint. Compact is a
/// whitelist rebuild (feedback_compact_field_allowlist) — the second call
/// pins that the new top-level field is forwarded, not silently dropped.
#[test]
fn test_project_map_include_centrality_and_compact_forwarding() {
    let project = TempDir::new().unwrap();
    fs::write(
        project.path().join("lib.rs"),
        r#"
pub fn entry_a() -> i32 { bridge() }
pub fn entry_b() -> i32 { bridge() }
pub fn bridge() -> i32 { leaf_x() + leaf_y() }
pub fn leaf_x() -> i32 { 1 }
pub fn leaf_y() -> i32 { 2 }
"#,
    )
    .unwrap();

    let server = common::init_server(&project);
    let msg = tool_call_json(
        "project_map",
        serde_json::json!({
            "include_centrality": true
        }),
    );
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    let cent = result["centrality"].as_array().unwrap_or_else(|| {
        panic!(
            "include_centrality must attach a centrality array; got keys: {:?}",
            result.as_object().map(|o| o.keys().collect::<Vec<_>>())
        )
    });
    assert!(
        cent.iter().any(|c| c["name"] == "bridge"),
        "bridge sits on every entry→leaf path and must rank; got: {cent:?}"
    );

    // Default off: no field.
    let resp_off = server
        .handle_message(&tool_call_json("project_map", serde_json::json!({})))
        .unwrap();
    assert!(
        parse_tool_result(&resp_off).get("centrality").is_none(),
        "centrality must be opt-in"
    );

    // Compact whitelist must forward it (trimmed rows keep name+file+score).
    let resp_c = server
        .handle_message(&tool_call_json(
            "project_map",
            serde_json::json!({"include_centrality": true, "compact": true}),
        ))
        .unwrap();
    let result_c = parse_tool_result(&resp_c);
    let cent_c = result_c["centrality"].as_array().unwrap_or_else(|| {
        panic!(
            "compact must forward centrality (allowlist trap); got keys: {:?}",
            result_c.as_object().map(|o| o.keys().collect::<Vec<_>>())
        )
    });
    assert!(
        cent_c
            .iter()
            .any(|c| c["name"] == "bridge" && c["betweenness"].is_number()),
        "compact rows keep name+betweenness; got: {cent_c:?}"
    );
}

/// v0.22.x fix: `ast_search query=<identifier> type=<X>` must fall back to
/// SQL `name LIKE '%<identifier>%'` when FTS rank drowns the matching type
/// under unrelated hits. Pre-fix `query="Result" type=struct` returned 0 even
/// though IndexResult/CallGraphResult/etc. exist, because top FTS hits for
/// "Result" are functions like `compress_results`.
#[test]
fn test_ast_search_query_plus_type_fallback_to_name_like() {
    let project = TempDir::new().unwrap();
    // Two structs with "Result" in name + LOTS of fns with "result" in name AND
    // body — pushes FTS rank toward functions so the post-type-filter empties
    // out, forcing the fallback to fire. Without enough fn noise, FTS returns
    // the structs in its top window and the fallback never triggers.
    let mut src = String::from("pub struct IndexResult { pub n: i32 }\npub struct CallGraphResult { pub n: i32 }\npub struct OtherStruct { pub n: i32 }\n");
    for i in 0..50 {
        src.push_str(&format!(
            "pub fn handle_result_{i}(result: i32) -> i32 {{ let result = result + {i}; result }}\n",
            i = i,
        ));
    }
    fs::write(project.path().join("lib.rs"), &src).unwrap();

    let server = common::init_server(&project);
    let msg = tool_call_json(
        "ast_search",
        serde_json::json!({
            "query": "Result", "type": "struct", "limit": 10
        }),
    );
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    let count = result["count"].as_u64().unwrap_or(0);
    assert!(count >= 2,
        "query='Result' type=struct must surface IndexResult + CallGraphResult via name-LIKE fallback, got count={}, results={}",
        count, result["results"]);
    let names: Vec<String> = result["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["name"].as_str().map(|s| s.to_string()))
        .collect();
    assert!(
        names.iter().any(|n| n == "IndexResult"),
        "IndexResult missing from fallback results: {:?}",
        names
    );
    assert!(
        names.iter().any(|n| n == "CallGraphResult"),
        "CallGraphResult missing from fallback results: {:?}",
        names
    );
    // Since the 2026-08-16 P1-8 fix the candidate pool is sized for the filter
    // (domain::search_fetch_count), so on a fixture this size FTS itself returns
    // the structs and the name-LIKE fallback is no longer needed to answer —
    // asserting the fallback hint here would pin a path this input no longer
    // takes. The fallback still exists for pools that genuinely saturate, and is
    // pinned on its own mechanism (`fallback_used`) by
    // search::ast_query::tests::name_substring_fallback_answers_a_saturated_pool.
    // What matters to the caller is asserted instead: the answer is COMPLETE.
    assert_eq!(
        result["matched_total"].as_u64(),
        Some(2),
        "both matching structs must be counted, got: {result}"
    );
    assert!(
        result.get("truncated").is_none(),
        "limit=10 fits 2 matches — nothing was cut, got: {result}"
    );
}

/// v0.11.2 fix: disambiguation suggestions carry `node_id` AND `start_line`
/// so callers can pick a specific definition — and same-file multi-defs
/// (e.g. two `fn new()` in one module for different impl blocks) are flagged
/// instead of silently merged.
#[test]
fn test_disambiguation_suggestions_include_node_id_and_start_line() {
    let project = TempDir::new().unwrap();
    fs::write(
        project.path().join("lib.rs"),
        r#"
pub struct Foo;
pub struct Bar;

impl Foo {
    pub fn new() -> Self { Foo }
}

impl Bar {
    pub fn new() -> Self { Bar }
}

pub fn make_them() {
    let _ = Foo::new();
    let _ = Bar::new();
}
"#,
    )
    .unwrap();
    let server = common::init_server(&project);

    // find_references on an ambiguous same-file symbol should enumerate
    // per-definition suggestions with node_id + start_line.
    let msg = tool_call_json(
        "find_references",
        serde_json::json!({"symbol_name":"new","file_path":"lib.rs"}),
    );
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);

    assert!(
        result.get("error").is_some(),
        "expected ambiguity error for same-file multi-def 'new': {}",
        result
    );
    let suggestions = result["suggestions"]
        .as_array()
        .expect("suggestions array missing");
    assert!(
        suggestions.len() >= 2,
        "expected ≥2 suggestions for two fn new(), got {}: {}",
        suggestions.len(),
        result
    );
    for s in suggestions {
        assert!(
            s["node_id"].as_i64().is_some(),
            "suggestion missing node_id: {}",
            s
        );
        assert!(
            s["start_line"].as_i64().is_some(),
            "suggestion missing start_line: {}",
            s
        );
    }
    let lines: Vec<i64> = suggestions
        .iter()
        .filter_map(|s| s["start_line"].as_i64())
        .collect();
    assert!(
        lines.windows(2).any(|w| w[0] != w[1]),
        "expected distinct start_line values across same-name defs, got: {:?}",
        lines
    );

    // Caller should now be able to pass node_id from the suggestion
    // and get a clean single-definition result.
    let picked = suggestions[0].clone();
    let nid = picked["node_id"].as_i64().unwrap();
    let msg2 = tool_call_json("find_references", serde_json::json!({"node_id": nid}));
    let resp2 = server.handle_message(&msg2).unwrap();
    let result2 = parse_tool_result(&resp2);
    assert!(
        result2.get("error").is_none(),
        "node_id selection should not be ambiguous: {}",
        result2
    );
}

/// Audit #6: get_call_graph must flag a same-file overload (≥2 non-test defs of
/// one name in one file) as ambiguous — matching the CLI after the shared
/// crate::resolve unification. (The CLI `impact` side is covered by
/// test_cli_impact_same_file_overload_is_ambiguous.)
#[test]
fn test_mcp_callgraph_impact_same_file_overload_is_ambiguous() {
    let project = TempDir::new().unwrap();
    fs::write(
        project.path().join("lib.rs"),
        r#"
pub struct Foo;
pub struct Bar;

impl Foo {
    pub fn new() -> Self { Foo }
}

impl Bar {
    pub fn new() -> Self { Bar }
}

pub fn make_them() {
    let _ = Foo::new();
    let _ = Bar::new();
}
"#,
    )
    .unwrap();
    let server = common::init_server(&project);

    for tool in ["get_call_graph"] {
        // No file_path / node_id → must report ambiguity, not silently merge the
        // two distinct `new` definitions.
        let msg = tool_call_json(tool, serde_json::json!({"symbol_name": "new"}));
        let resp = server.handle_message(&msg).unwrap();
        let result = parse_tool_result(&resp);
        let err = result.get("error").and_then(|e| e.as_str()).unwrap_or("");
        assert!(
            err.contains("Ambiguous symbol 'new'"),
            "{tool} must report same-file ambiguity for 'new'; got: {result}"
        );
        // Accurate same-file guidance (not the dead-end "Specify file_path").
        assert!(
            err.contains("same file"),
            "{tool} same-file message must name the same-file case; got: {err}"
        );
        let suggestions = result["suggestions"]
            .as_array()
            .unwrap_or_else(|| panic!("{tool}: suggestions array missing: {result}"));
        assert!(
            suggestions.len() >= 2,
            "{tool}: expected ≥2 node_id suggestions; got: {result}"
        );
        for s in suggestions {
            assert!(
                s["node_id"].as_i64().is_some(),
                "{tool} suggestion needs node_id: {s}"
            );
        }
    }
}

/// MCP `find_dead_code` must not certify a directory it never looked in.
///
/// The tool's whole output is an assertion of ABSENCE, so a `path` matching no
/// indexed file is zero coverage — yet it answered `{"results": [], "summary":
/// "No dead code found …"}` + success, indistinguishable from a genuinely clean
/// directory, on the surface an LLM consumes. The CLI twin has probed and exited
/// 1 since v0.91.0; the MCP half never got the probe (audit 2026-08-16 P1-22).
#[test]
fn test_find_dead_code_bogus_path_is_not_a_clean_bill_of_health() {
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(
        project.path().join("src/lib.ts"),
        "export function used(): number { return 1; }\nexport function run(): number { return used(); }\n",
    )
    .unwrap();
    let server = common::init_server(&project);

    // A well-formed, in-root path that names nothing indexed.
    let msg = tool_call_json(
        "find_dead_code",
        serde_json::json!({"path": "src/does_not_exist"}),
    );
    let resp = server.handle_message(&msg).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(resp.as_ref().unwrap()).unwrap();
    assert_eq!(
        parsed["result"]["isError"],
        serde_json::Value::Bool(true),
        "an unindexed path must be an error, not a clean report: {parsed:?}"
    );
    let text = parsed["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("");
    assert!(
        text.contains("src/does_not_exist"),
        "the error must name the path; got: {text}"
    );
    assert!(
        !text.contains("No dead code found"),
        "must not also claim cleanliness; got: {text}"
    );

    // Control 1: a path that DOES match indexed files answers normally, even
    // when the answer is empty — otherwise the probe would turn every clean
    // directory into an error (the inverse bug).
    let msg = tool_call_json("find_dead_code", serde_json::json!({"path": "src"}));
    let ok = parse_tool_result(&server.handle_message(&msg).unwrap());
    assert!(
        ok["results"].is_array(),
        "an indexed path must still get a report: {ok}"
    );
    assert!(ok.get("error").is_none(), "got: {ok}");

    // Control 2: the two spellings that mean "everything" must not trip the
    // probe — no stored path equals "" or begins with "src//".
    for spelling in ["", "src/"] {
        let msg = tool_call_json("find_dead_code", serde_json::json!({"path": spelling}));
        let parsed: serde_json::Value =
            serde_json::from_str(server.handle_message(&msg).unwrap().as_ref().unwrap()).unwrap();
        assert_ne!(
            parsed["result"]["isError"],
            serde_json::Value::Bool(true),
            "path {spelling:?} means 'everything indexed', not a miss: {parsed:?}"
        );
    }
}

/// v0.11.2 fix: `find_dead_code` must filter out shell-invoked plugin entry
/// points by default (claude-plugin/** prefix). Users opt in to the full list
/// by passing `ignore_paths: []`.
#[test]
fn test_find_dead_code_default_ignores_plugin_scripts() {
    let project = TempDir::new().unwrap();
    // A clearly-unused function in a regular src file.
    fs::write(
        project.path().join("lib.rs"),
        r#"
pub fn genuinely_dead_thing() {
    let x = 1;
    let y = 2;
    let z = x + y;
    println!("{}", z);
}
"#,
    )
    .unwrap();
    // Simulate a claude-plugin hook script — function invoked only via shell.
    fs::create_dir_all(project.path().join("claude-plugin/scripts")).unwrap();
    // `uninstall` has no in-file caller here. It was self-called at module
    // level in earlier versions of this fixture, but since the JS relation
    // extractor now attributes module-level calls to `<module>` and those
    // edges resolve same-file, adding a module-level `uninstall();` would
    // make this function non-dead and defeat the ignore-prefix assertion.
    fs::write(
        project.path().join("claude-plugin/scripts/lifecycle.js"),
        r#"
function uninstall() {
    console.log("hook cleanup step 1");
    console.log("hook cleanup step 2");
    console.log("hook cleanup step 3");
}
"#,
    )
    .unwrap();

    let server = common::init_server(&project);

    // Default call — `uninstall` must NOT appear; real dead code still visible.
    let msg = tool_call_json("find_dead_code", serde_json::json!({"min_lines": 3}));
    let resp = server.handle_message(&msg).unwrap();
    let result = parse_tool_result(&resp);
    let names: Vec<&str> = result["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    assert!(
        !names.contains(&"uninstall"),
        "claude-plugin/ entry point leaked as dead code: {:?}",
        names
    );
    assert!(
        result["ignored_count"].as_u64().unwrap_or(0) >= 1,
        "expected at least 1 ignored result, got: {}",
        result
    );
    assert_eq!(
        result["ignore_paths_defaulted"], true,
        "defaulted ignore should be flagged: {}",
        result
    );

    // Opt-out — pass `[]` and the plugin script now shows up.
    let msg2 = tool_call_json(
        "find_dead_code",
        serde_json::json!({"min_lines": 3, "ignore_paths": []}),
    );
    let resp2 = server.handle_message(&msg2).unwrap();
    let result2 = parse_tool_result(&resp2);
    let names2: Vec<&str> = result2["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    assert!(
        names2.contains(&"uninstall"),
        "ignore_paths=[] should surface plugin entry points, got: {:?}",
        names2
    );
    assert_eq!(
        result2["ignore_paths_defaulted"], false,
        "explicit [] must not be flagged as defaulted: {}",
        result2
    );
}

// ─────────────────────────────────────────────────────────────────────────
// Phase 1: bare-identifier function-value references — integration RED tests.
// R12 + R15 are EXPECTED TO FAIL until candidate generation + impact wiring
// land. R13 (cross-language drop) + R14 (calls/references separation) are
// guardrails that lock the precision boundary and may already pass.
// ─────────────────────────────────────────────────────────────────────────

const INIT_MSG: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;

#[test]
fn test_r12_callback_reference_resolves_cross_file() {
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(
        project.path().join("src/handlers.rs"),
        r#"
pub fn handler() {}
"#,
    )
    .unwrap();
    fs::write(
        project.path().join("src/app.rs"),
        r#"
pub fn caller() {
    register(handler);
}
fn register<F>(_f: F) {}
"#,
    )
    .unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    server.handle_message(INIT_MSG).unwrap();
    let _ = server
        .handle_message(&tool_call_json(
            "semantic_code_search",
            serde_json::json!({"query": "handler"}),
        ))
        .unwrap();

    let resp = server
        .handle_message(&tool_call_json(
            "find_references",
            serde_json::json!({"symbol_name": "handler", "relation": "references"}),
        ))
        .unwrap();
    let result = parse_tool_result(&resp);
    let names: Vec<&str> = result["references"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    assert!(names.contains(&"caller"),
        "find_references(handler, references) should include caller via a value-reference edge; got {:?}", names);
}

#[test]
fn test_r13_value_reference_does_not_cross_language() {
    // Rust `caller` passes bare `process`; `process` exists ONLY as a JS function.
    // Same-language resolution must DROP the Rust edge — the JS `process` must
    // stay unreferenced (cross-language attribution is a fatal false positive).
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(
        project.path().join("src/app.rs"),
        r#"
pub fn caller() { schedule(process); }
fn schedule<F>(_f: F) {}
"#,
    )
    .unwrap();
    fs::write(
        project.path().join("src/worker.js"),
        r#"
function process() {}
"#,
    )
    .unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    server.handle_message(INIT_MSG).unwrap();
    let _ = server
        .handle_message(&tool_call_json(
            "semantic_code_search",
            serde_json::json!({"query": "process"}),
        ))
        .unwrap();

    let resp = server
        .handle_message(&tool_call_json(
            "find_references",
            serde_json::json!({"symbol_name": "process", "relation": "references"}),
        ))
        .unwrap();
    let result = parse_tool_result(&resp);
    let names: Vec<&str> = result["references"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    assert!(!names.contains(&"caller"),
        "a Rust value-reference must NOT resolve to a same-named JS function (cross-language drop); got {:?}", names);
}

#[test]
fn test_r14_value_reference_excluded_from_call_graph() {
    // `handler` is passed as a callback (referenced) but NEVER called. It must
    // surface in find_references. The complementary "NOT a direct CALLER" half of
    // the calls-vs-references separation is asserted on the CLI impact surface in
    // test_cli_impact_json_reports_value_references.
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(
        project.path().join("src/app.rs"),
        r#"
pub fn caller() { register(handler); }
fn register<F>(_f: F) {}
fn handler() {}
"#,
    )
    .unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    server.handle_message(INIT_MSG).unwrap();
    let _ = server
        .handle_message(&tool_call_json(
            "semantic_code_search",
            serde_json::json!({"query": "handler"}),
        ))
        .unwrap();

    // find_references must surface the referencer.
    let fr = parse_tool_result(
        &server
            .handle_message(&tool_call_json(
                "find_references",
                serde_json::json!({"symbol_name": "handler", "relation": "references"}),
            ))
            .unwrap(),
    );
    let ref_names: Vec<&str> = fr["references"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    assert!(
        ref_names.contains(&"caller"),
        "find_references should surface the callback referencer; got {:?}",
        ref_names
    );
}

#[test]
fn test_code_explorer_agent_references_only_live_tools() {
    // Guard: the shipped code-explorer sub-agent
    // (claude-plugin/agents/code-explorer.md) lists MCP tools in its frontmatter.
    // Every mcp__code-graph__<name> it references must be a live public tool in
    // the registry — catches stale references to folded/removed tools (e.g.
    // trace_http_chain, which became get_call_graph route_path in v0.18.4).
    let agent_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("claude-plugin/agents/code-explorer.md");
    let Ok(agent_md) = fs::read_to_string(&agent_path) else {
        // Plugin dir absent (e.g. minimal package build) — nothing to check.
        eprintln!("skip: {} not found", agent_path.display());
        return;
    };

    let registry = code_graph_mcp::mcp::tools::ToolRegistry::new();
    let live: Vec<&str> = registry
        .list_tools()
        .iter()
        .map(|t| t.name.as_str())
        .collect();

    const PREFIX: &str = "mcp__code-graph__";
    let mut referenced = 0;
    for (idx, _) in agent_md.match_indices(PREFIX) {
        let name: String = agent_md[idx + PREFIX.len()..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        assert!(
            live.contains(&name.as_str()),
            "code-explorer.md references non-live MCP tool '{}' (live tools: {:?})",
            name,
            live
        );
        referenced += 1;
    }
    assert!(
        referenced >= 1,
        "expected code-explorer.md to reference at least one mcp__code-graph__ tool"
    );
}

/// P2 (2026-08-16 audit §四): the published `find_references` schema declares
/// which `relation` values a client may send. It listed six while the graph emits
/// seven, so `exports` and `routes_to` were visible in `relation:"all"` results
/// and undeclared as filters — the declared-vs-honored gap, in the direction where
/// the client is the one held back.
///
/// Pinned to `RELATION_FILTER_VOCAB`, the same list the CLI and the handler read,
/// so a new edge type cannot land on two of the three surfaces.
#[test]
fn find_references_schema_enum_matches_the_relation_vocabulary() {
    let registry = code_graph_mcp::mcp::tools::ToolRegistry::new();
    let tools = registry.list_tools();
    let tool = tools
        .iter()
        .find(|t| t.name == "find_references")
        .expect("find_references must be a live tool");
    let declared: Vec<&str> = tool.input_schema["properties"]["relation"]["enum"]
        .as_array()
        .expect("relation must declare an enum")
        .iter()
        .map(|v| v.as_str().expect("enum values are strings"))
        .collect();

    for rel in code_graph_mcp::domain::RELATION_FILTER_VOCAB.iter() {
        assert!(
            declared.contains(rel),
            "schema omits '{rel}', an edge type the handler accepts and the graph returns \
             (declared: {declared:?})"
        );
    }
    assert!(
        declared.contains(&"all"),
        "schema must keep the 'all' escape hatch: {declared:?}"
    );
    assert_eq!(
        declared.len(),
        code_graph_mcp::domain::RELATION_FILTER_VOCAB.len() + 1,
        "schema must not declare a relation the handler rejects: {declared:?}"
    );
}

/// v0.79.1 audit sibling-hole (the deferred half of HIGH #1): an inline Rust
/// `#[cfg(test)]` unit test with a descriptive snake_case name in a `src/` file is
/// `is_test=1` in the DB, but the `is_test_symbol` name/path heuristic MISSES it
/// (no `test_` prefix, not a `tests/` path). HIGH #1 fixed `impact`/`classify_impact`;
/// the parallel surfaces — `get_ast_node` (include_references + include_impact) and
/// `get_call_graph` — kept filtering on the weak heuristic, so the inline test leaked
/// into the production caller view and inflated the impact risk count. This drives the
/// real parser (so the `is_test` flag is genuine) end-to-end across all three surfaces.
#[test]
fn test_e2e_inline_unit_test_caller_excluded_across_surfaces() {
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    // `descriptive_*_check` names contain no "test" substring and live in src/, so the
    // name/path heuristic classifies them as PROD; only the AST `is_test` flag catches
    // them. `real_caller` is a genuine production caller.
    fs::write(
        project.path().join("src/lib.rs"),
        r#"
pub fn target_fn() -> i32 { 42 }

pub fn real_caller() -> i32 { target_fn() }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn descriptive_behavior_check() {
        // Direct call (not inside a macro arg) so the edge is extracted.
        let v = target_fn();
        assert_eq!(v, 42);
    }
    #[test]
    fn another_descriptive_check() {
        let v = target_fn();
        assert_eq!(v, 42);
    }
}
"#,
    )
    .unwrap();

    let server = McpServer::from_project_root(project.path()).unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;
    server.handle_message(init).unwrap();

    // --- get_ast_node include_references: inline tests hidden by default ---
    let refs = tool_call_json(
        "get_ast_node",
        serde_json::json!({
            "symbol_name": "target_fn", "file_path": "src/lib.rs", "include_references": true
        }),
    );
    let r = parse_tool_result(&server.handle_message(&refs).unwrap());
    let called_by: Vec<&str> = r["called_by"]
        .as_array()
        .map(|a| a.iter().filter_map(|c| c["name"].as_str()).collect())
        .unwrap_or_default();
    assert!(
        called_by.contains(&"real_caller"),
        "prod caller must be present; got called_by={called_by:?}"
    );
    assert!(!called_by.contains(&"descriptive_behavior_check")
            && !called_by.contains(&"another_descriptive_check"),
        "inline unit tests (is_test=1, heuristic-invisible) leaked into prod callers: {called_by:?}");
    assert_eq!(
        r["test_callers_hidden"].as_i64(),
        Some(2),
        "both inline unit tests must be counted as hidden test callers; got {}",
        r
    );

    // --- get_ast_node include_impact: inline tests must not inflate risk count ---
    let imp = tool_call_json(
        "get_ast_node",
        serde_json::json!({
            "symbol_name": "target_fn", "file_path": "src/lib.rs", "include_impact": true
        }),
    );
    let r = parse_tool_result(&server.handle_message(&imp).unwrap());
    assert_eq!(
        r["impact"]["direct_callers"].as_i64(),
        Some(1),
        "only real_caller is a prod direct caller; got impact={}",
        r["impact"]
    );
    assert_eq!(
        r["impact"]["test_callers_filtered"].as_i64(),
        Some(2),
        "impact must exclude both inline unit tests; got impact={}",
        r["impact"]
    );

    // --- get_call_graph callers: inline tests excluded from default view ---
    let cg = tool_call_json(
        "get_call_graph",
        serde_json::json!({
            "symbol_name": "target_fn", "direction": "callers", "depth": 2
        }),
    );
    let r = parse_tool_result(&server.handle_message(&cg).unwrap());
    // Normal (non-rollup) get_call_graph keys the list under `callers`/`callees`.
    let callers: Vec<&str> = r["callers"]
        .as_array()
        .map(|a| a.iter().filter_map(|n| n["name"].as_str()).collect())
        .unwrap_or_default();
    assert!(
        callers.contains(&"real_caller"),
        "prod caller must appear in the default call graph; got callers={callers:?}"
    );
    assert!(
        !callers.contains(&"descriptive_behavior_check")
            && !callers.contains(&"another_descriptive_check"),
        "inline unit tests leaked into the default call graph: {callers:?}"
    );
}

/// Regression (real-user QA): C++ classes declared in a `.h` header (the most common
/// C++ layout — declaration in `.h`, definition in `.cpp`) were invisible. `.h` is
/// C-vs-C++ ambiguous by extension so it was parsed as C, whose grammar can't extract
/// `class` — the class SYMBOLS and their base-class `inherits` edges never existed
/// (overview/callgraph/dead-code/find_references were blind to them). A `.h` with C++
/// markers now parses as C++. Also guards the sibling bug this exposed: a class with
/// an INLINE constructor (`Circle(double){}`) produces a `method Circle` node sharing
/// the class name, and the `inherits` edge must attach ONLY to the class node.
#[test]
fn test_cpp_header_classes_and_single_inherits_edge() {
    use code_graph_mcp::indexer::pipeline::run_full_index;
    let project = TempDir::new().unwrap();
    fs::write(
        project.path().join("shape.h"),
        r#"#pragma once
class Shape {
public:
    virtual double area() const = 0;
};
class Circle : public Shape {
    double r;
public:
    Circle(double radius) : r(radius) {}
    double area() const override;
};
"#,
    )
    .unwrap();
    let db_dir = project.path().join(code_graph_mcp::domain::CODE_GRAPH_DIR);
    fs::create_dir_all(&db_dir).unwrap();
    let db = Database::open(&db_dir.join("index.db")).unwrap();
    run_full_index(&db, project.path(), None, None).unwrap();

    // Classes from the .h header are extracted (were dropped when .h parsed as C).
    let types_of = |name: &str| -> Vec<String> {
        get_nodes_by_name(db.conn(), name)
            .unwrap()
            .iter()
            .map(|n| n.node_type.clone())
            .collect()
    };
    let shape_types = types_of("Shape");
    assert!(
        shape_types.iter().any(|t| t == "class"),
        "Shape class must be extracted from the .h header; got node types: {shape_types:?}"
    );
    let circle_types = types_of("Circle");
    assert!(
        circle_types.iter().any(|t| t == "class"),
        "Circle class must be extracted from the .h header; got node types: {circle_types:?}"
    );

    // Exactly ONE inherits edge — from the CLASS node, never the inline-constructor
    // `method Circle` that shares the name.
    let inherits: Vec<(String, String)> = db
        .conn()
        .prepare(
            "SELECT s.type, t.name FROM edges e \
         JOIN nodes s ON s.id = e.source_id JOIN nodes t ON t.id = e.target_id \
         WHERE e.relation = 'inherits'",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(inherits, vec![("class".to_string(), "Shape".to_string())],
        "exactly one inherits edge from the class node (not the constructor method); got: {inherits:?}");
}

/// Prompt text is LLM-visible metadata: whatever it names, the model will try
/// to call. A name the server dispatches but `tools/list` never advertised does
/// not exist as far as an MCP client is concerned, so a prompt that teaches one
/// hands the model a guaranteed dead end — and nothing errors server-side,
/// which is why two of the three prompts carried one for several releases
/// (audit 2026-08-22 P2-6; `impact_analysis` was the same defect one round
/// earlier).
#[test]
fn prompts_name_only_tools_the_client_can_actually_call() {
    let project = TempDir::new().unwrap();
    let server = McpServer::from_project_root(project.path()).unwrap();

    let list = server
        .handle_message(r#"{"jsonrpc":"2.0","id":1,"method":"prompts/list"}"#)
        .unwrap()
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&list).unwrap();
    let prompts = parsed["result"]["prompts"].as_array().unwrap().clone();
    assert!(!prompts.is_empty(), "precondition: prompts exist");

    for prompt in &prompts {
        let name = prompt["name"].as_str().unwrap();
        // Supply every declared argument so no placeholder swallows the text.
        let mut args = serde_json::Map::new();
        if let Some(decl) = prompt["arguments"].as_array() {
            for a in decl {
                args.insert(
                    a["name"].as_str().unwrap().to_string(),
                    serde_json::json!("X"),
                );
            }
        }
        let msg = serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "prompts/get",
            "params": { "name": name, "arguments": args }
        })
        .to_string();
        let resp = server.handle_message(&msg).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        let text = parsed["result"]["messages"][0]["content"]["text"]
            .as_str()
            .unwrap();

        for hidden in code_graph_mcp::domain::NON_LISTED_MCP_TOOLS {
            assert!(
                !text.contains(hidden),
                "prompt '{name}' tells the model to use '{hidden}', which tools/list \
                 never advertised — the client cannot call it. Teach the advertised \
                 form instead (a LIVE tool plus its flag). Text: {text}"
            );
        }
        // Not just "says nothing forbidden": a prompt that named no tool at all
        // would pass the loop above while teaching nothing.
        assert!(
            code_graph_mcp::domain::LIVE_MCP_TOOLS
                .iter()
                .any(|live| text.contains(live)),
            "prompt '{name}' names no advertised tool at all: {text}"
        );
    }
}

/// The sibling of the prompt guard above, for the two surfaces
/// [`NON_LISTED_MCP_TOOLS`]'s own doc comment already claimed: "Anything the
/// model READS (prompt text, tool descriptions, `instructions`) must name only
/// LIVE_MCP_TOOLS".
///
/// Only the prompt half was ever enforced. The other two went unchecked for as
/// long as the sentence has been there, and one had drifted: `get_call_graph`'s
/// description carried "(folds the old trace_http_chain)", teaching the model
/// a name `tools/list` does not advertise. A guard that names three surfaces
/// and walks one is worse than a guard that names one, because the doc comment
/// is then read as coverage.
///
/// This reads the SHIPPED responses rather than the source text: a description
/// assembled at runtime, or an `instructions` field built by concatenation, is
/// what the model actually sees, and scanning `tools.rs` for string literals
/// would miss both.
#[test]
fn tool_descriptions_and_instructions_name_no_unlisted_tool() {
    let project = TempDir::new().unwrap();
    // A real source file: an empty directory yields the non-project stub, whose
    // `tools/list` is empty — the check would pass by having nothing to check.
    fs::write(
        project.path().join("app.ts"),
        "export function greet(name: string): string { return name; }\n",
    )
    .unwrap();
    let server = McpServer::from_project_root(project.path()).unwrap();

    let init = server
        .handle_message(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
        )
        .unwrap()
        .unwrap();
    let init: serde_json::Value = serde_json::from_str(&init).unwrap();
    let instructions = init["result"]["instructions"].as_str().unwrap_or("");
    assert!(
        !instructions.is_empty(),
        "precondition: the server ships an `instructions` field to check"
    );

    let list = server
        .handle_message(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#)
        .unwrap()
        .unwrap();
    let list: serde_json::Value = serde_json::from_str(&list).unwrap();
    let tools = list["result"]["tools"].as_array().unwrap().clone();
    assert_eq!(
        tools.len(),
        code_graph_mcp::mcp::tools::TOOL_COUNT,
        "precondition: the full tool list is advertised, not the stub"
    );

    // Whole tool objects, not just `description`: an enum label or a parameter
    // doc that names a folded tool teaches it just as well.
    let mut surfaces: Vec<(String, String)> = vec![
        ("instructions (as shipped here)".into(), instructions.into()),
        // BOTH variants, by name rather than through the server: `initialize`
        // returns one of them depending on CODE_GRAPH_QUIET_HOOKS, so a check
        // that only reads the response covers whichever the ambient env happens
        // to select and leaves the other permanently unguarded. The plugin sets
        // that variable, so the quiet text is the one most users read.
        (
            "INSTRUCTIONS_QUIET".into(),
            code_graph_mcp::mcp::server::INSTRUCTIONS_QUIET.into(),
        ),
        (
            "INSTRUCTIONS_NOISY".into(),
            code_graph_mcp::mcp::server::INSTRUCTIONS_NOISY.into(),
        ),
    ];
    for t in &tools {
        let name = t["name"].as_str().unwrap_or("?").to_string();
        surfaces.push((format!("tool '{name}'"), serde_json::to_string(t).unwrap()));
    }

    for (label, text) in &surfaces {
        for hidden in code_graph_mcp::domain::NON_LISTED_MCP_TOOLS {
            assert!(
                !text.contains(hidden),
                "{label} names '{hidden}', which tools/list never advertised — the \
                 client cannot offer it, so the model reads a call it cannot make. \
                 Name the advertised form (a LIVE tool plus its flag), or drop the \
                 reference if it is only history. Text: {text}"
            );
        }
    }
}
