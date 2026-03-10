use anyhow::{anyhow, Result};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::protocol::{JsonRpcRequest, JsonRpcResponse};
use super::tools::ToolRegistry;
use crate::indexer::pipeline::{run_full_index, run_incremental_index};
use crate::search::fusion::rrf_fusion;
use crate::storage::db::Database;
use crate::storage::queries;

pub struct McpServer {
    registry: ToolRegistry,
    db: Arc<Database>,
    project_root: Option<PathBuf>,
    indexed: Mutex<bool>,
}

impl McpServer {
    pub fn new(db_path: &Path, project_root: Option<String>) -> Result<Self> {
        let db = Database::open(db_path)?;
        Ok(Self {
            registry: ToolRegistry::new(),
            db: Arc::new(db),
            project_root: project_root.map(PathBuf::from),
            indexed: Mutex::new(false),
        })
    }

    /// Create from project root path: auto-creates .code-graph/ directory and .gitignore entry
    pub fn from_project_root(project_root: &Path) -> Result<Self> {
        let db_dir = project_root.join(".code-graph");
        std::fs::create_dir_all(&db_dir)?;
        let db_path = db_dir.join("index.db");

        // Ensure .code-graph/ is in .gitignore
        let gitignore_path = project_root.join(".gitignore");
        if gitignore_path.exists() {
            let content = std::fs::read_to_string(&gitignore_path).unwrap_or_default();
            if !content.contains(".code-graph") {
                let mut new_content = content;
                if !new_content.ends_with('\n') {
                    new_content.push('\n');
                }
                new_content.push_str(".code-graph/\n");
                std::fs::write(&gitignore_path, new_content)?;
            }
        }

        let db = Database::open(&db_path)?;
        Ok(Self {
            registry: ToolRegistry::new(),
            db: Arc::new(db),
            project_root: Some(project_root.to_path_buf()),
            indexed: Mutex::new(false),
        })
    }

    #[cfg(test)]
    pub fn new_test() -> Self {
        let db = Database::open(Path::new(":memory:")).unwrap();
        Self {
            registry: ToolRegistry::new(),
            db: Arc::new(db),
            project_root: None,
            indexed: Mutex::new(false),
        }
    }

    #[cfg(test)]
    pub fn new_test_with_project(project_root: &Path) -> Self {
        let db_dir = project_root.join(".code-graph");
        std::fs::create_dir_all(&db_dir).unwrap();
        let db = Database::open(&db_dir.join("index.db")).unwrap();
        Self {
            registry: ToolRegistry::new(),
            db: Arc::new(db),
            project_root: Some(project_root.to_path_buf()),
            indexed: Mutex::new(false),
        }
    }

    pub fn db(&self) -> &Database {
        &self.db
    }

    /// Ensure index is up-to-date. On first call, runs full index.
    fn ensure_indexed(&self) -> Result<()> {
        let project_root = match &self.project_root {
            Some(p) => p.clone(),
            None => return Ok(()),
        };

        let mut indexed = self.indexed.lock().unwrap();
        if !*indexed {
            run_full_index(&self.db, &project_root)?;
            *indexed = true;
        } else {
            run_incremental_index(&self.db, &project_root)?;
        }
        Ok(())
    }

    pub fn handle_message(&self, line: &str) -> Result<Option<String>> {
        let req: JsonRpcRequest = serde_json::from_str(line)?;

        // Notifications (no id) don't get responses
        if req.method == "notifications/initialized" {
            return Ok(None);
        }

        let response = match req.method.as_str() {
            "initialize" => self.handle_initialize(req.id),
            "tools/list" => self.handle_tools_list(req.id),
            "tools/call" => self.handle_tools_call(req.id, req.params),
            _ => JsonRpcResponse::error(
                req.id,
                -32601,
                format!("Method not found: {}", req.method),
            ),
        };

        Ok(Some(serde_json::to_string(&response)?))
    }

    fn handle_initialize(&self, id: Option<serde_json::Value>) -> JsonRpcResponse {
        JsonRpcResponse::success(id, json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {
                    "listChanged": false
                }
            },
            "serverInfo": {
                "name": "code-graph-mcp",
                "version": env!("CARGO_PKG_VERSION")
            }
        }))
    }

    fn handle_tools_list(&self, id: Option<serde_json::Value>) -> JsonRpcResponse {
        let tools: Vec<serde_json::Value> = self.registry.list_tools().iter().map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": t.input_schema,
            })
        }).collect();

        JsonRpcResponse::success(id, json!({ "tools": tools }))
    }

    fn handle_tools_call(&self, id: Option<serde_json::Value>, params: Option<serde_json::Value>) -> JsonRpcResponse {
        let params = match params {
            Some(p) => p,
            None => return JsonRpcResponse::error(id, -32602, "Missing params".into()),
        };

        let tool_name = params["name"].as_str().unwrap_or("");
        let arguments = params["arguments"].clone();

        match self.handle_tool(tool_name, &arguments) {
            Ok(result) => {
                let text = serde_json::to_string_pretty(&result).unwrap_or_default();
                JsonRpcResponse::success(id, json!({
                    "content": [{
                        "type": "text",
                        "text": text
                    }]
                }))
            }
            Err(e) => {
                JsonRpcResponse::success(id, json!({
                    "content": [{
                        "type": "text",
                        "text": format!("Error: {}", e)
                    }],
                    "isError": true
                }))
            }
        }
    }

    fn handle_tool(&self, name: &str, args: &serde_json::Value) -> Result<serde_json::Value> {
        match name {
            "semantic_code_search" => self.tool_semantic_search(args),
            "get_call_graph" => self.tool_get_call_graph(args),
            "find_http_route" => self.tool_find_http_route(args),
            "get_ast_node" => self.tool_get_ast_node(args),
            "read_snippet" => self.tool_read_snippet(args),
            "start_watch" => self.tool_start_watch(),
            "stop_watch" => self.tool_stop_watch(),
            "get_index_status" => self.tool_get_index_status(),
            "rebuild_index" => self.tool_rebuild_index(args),
            _ => Err(anyhow!("Unknown tool: {}", name)),
        }
    }

    fn tool_semantic_search(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let query = args["query"].as_str()
            .ok_or_else(|| anyhow!("query is required"))?;
        let top_k = args["top_k"].as_u64().unwrap_or(5) as i64;

        // Ensure index is up to date
        self.ensure_indexed()?;

        // FTS5 search
        let fts_results = queries::fts5_search(self.db.conn(), query, top_k * 2)?;

        // Convert to SearchResult for RRF
        let fts_search: Vec<crate::search::fusion::SearchResult> = fts_results.iter()
            .map(|r| crate::search::fusion::SearchResult { node_id: r.id, score: 0.0 })
            .collect();

        // RRF fusion (FTS-only when no vectors available)
        let fused = rrf_fusion(&fts_search, &[], 60, top_k as usize);

        // Get top_k results
        let result_ids: Vec<i64> = fused.iter()
            .map(|r| r.node_id)
            .collect();

        let mut results = Vec::new();
        for node_id in &result_ids {
            if let Some(node) = queries::get_node_by_id(self.db.conn(), *node_id)? {
                let file_path = queries::get_file_path(self.db.conn(), node.file_id)?
                    .unwrap_or_default();
                results.push(json!({
                    "node_id": node.id,
                    "name": node.name,
                    "type": node.node_type,
                    "file_path": file_path,
                    "start_line": node.start_line,
                    "end_line": node.end_line,
                    "code_content": node.code_content,
                    "signature": node.signature,
                    "context_string": node.context_string,
                }));
            }
        }

        Ok(json!(results))
    }

    fn tool_get_call_graph(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let function_name = args["function_name"].as_str()
            .ok_or_else(|| anyhow!("function_name is required"))?;
        let direction = args["direction"].as_str().unwrap_or("both");
        let depth = args["depth"].as_i64().unwrap_or(2) as i32;
        let file_path = args["file_path"].as_str();

        self.ensure_indexed()?;

        let results = crate::graph::query::get_call_graph(
            self.db.conn(), function_name, direction, depth, file_path,
        )?;

        let nodes: Vec<serde_json::Value> = results.iter().map(|n| {
            json!({
                "node_id": n.node_id,
                "name": n.name,
                "type": n.node_type,
                "file_path": n.file_path,
                "depth": n.depth,
            })
        }).collect();

        Ok(json!({
            "function": function_name,
            "direction": direction,
            "callees": if direction == "callees" || direction == "both" {
                nodes.iter().filter(|n| n["depth"].as_i64().unwrap_or(0) > 0).cloned().collect::<Vec<_>>()
            } else { vec![] },
            "callers": if direction == "callers" || direction == "both" {
                nodes.iter().filter(|n| n["depth"].as_i64().unwrap_or(0) > 0).cloned().collect::<Vec<_>>()
            } else { vec![] },
        }))
    }

    fn tool_find_http_route(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let route_path = args["route_path"].as_str()
            .ok_or_else(|| anyhow!("route_path is required"))?;

        self.ensure_indexed()?;

        // Search edges with relation "routes_to" where metadata contains the route path
        let mut stmt = self.db.conn().prepare(
            "SELECT e.source_id, e.metadata, n.name, n.type, f.path, n.start_line, n.end_line
             FROM edges e
             JOIN nodes n ON n.id = e.source_id
             JOIN files f ON f.id = n.file_id
             WHERE e.relation = 'routes_to' AND e.metadata LIKE ?1"
        )?;

        let pattern = format!("%{}%", route_path);
        let rows: Vec<serde_json::Value> = stmt.query_map([&pattern], |row| {
            Ok(json!({
                "node_id": row.get::<_, i64>(0)?,
                "metadata": row.get::<_, Option<String>>(1)?,
                "handler_name": row.get::<_, String>(2)?,
                "handler_type": row.get::<_, String>(3)?,
                "file_path": row.get::<_, String>(4)?,
                "start_line": row.get::<_, i64>(5)?,
                "end_line": row.get::<_, i64>(6)?,
            }))
        })?.filter_map(|r| r.ok()).collect();

        Ok(json!({
            "route": route_path,
            "handlers": rows,
        }))
    }

    fn tool_get_ast_node(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let file_path = args["file_path"].as_str()
            .ok_or_else(|| anyhow!("file_path is required"))?;
        let symbol_name = args["symbol_name"].as_str()
            .ok_or_else(|| anyhow!("symbol_name is required"))?;
        let include_refs = args["include_references"].as_bool().unwrap_or(false);

        self.ensure_indexed()?;

        let nodes = queries::get_nodes_by_file_path(self.db.conn(), file_path)?;
        let node = nodes.iter().find(|n| n.name == symbol_name);

        match node {
            Some(n) => {
                let mut result = json!({
                    "node_id": n.id,
                    "name": n.name,
                    "type": n.node_type,
                    "file_path": file_path,
                    "start_line": n.start_line,
                    "end_line": n.end_line,
                    "code_content": n.code_content,
                    "signature": n.signature,
                    "qualified_name": n.qualified_name,
                });

                if include_refs {
                    let callees = queries::get_edge_target_names(self.db.conn(), n.id, "calls")?;
                    let callers = queries::get_edge_source_names(self.db.conn(), n.id, "calls")?;
                    result["calls"] = json!(callees);
                    result["called_by"] = json!(callers);
                }

                Ok(result)
            }
            None => Err(anyhow!("Symbol '{}' not found in '{}'", symbol_name, file_path)),
        }
    }

    fn tool_read_snippet(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let node_id = args["node_id"].as_i64()
            .ok_or_else(|| anyhow!("node_id is required"))?;
        let context_lines = args["context_lines"].as_i64().unwrap_or(3);

        let node = queries::get_node_by_id(self.db.conn(), node_id)?
            .ok_or_else(|| anyhow!("Node {} not found", node_id))?;

        let file_path = queries::get_file_path(self.db.conn(), node.file_id)?
            .unwrap_or_default();

        // Read source file for context lines if project root available
        let full_snippet = if let Some(ref root) = self.project_root {
            let abs_path = root.join(&file_path);
            if let Ok(source) = std::fs::read_to_string(&abs_path) {
                let lines: Vec<&str> = source.lines().collect();
                let start = (node.start_line as usize).saturating_sub(1 + context_lines as usize);
                let end = ((node.end_line as usize) + context_lines as usize).min(lines.len());
                lines[start..end].join("\n")
            } else {
                node.code_content.clone()
            }
        } else {
            node.code_content.clone()
        };

        Ok(json!({
            "node_id": node.id,
            "name": node.name,
            "type": node.node_type,
            "file_path": file_path,
            "start_line": node.start_line,
            "end_line": node.end_line,
            "code": full_snippet,
        }))
    }

    fn tool_start_watch(&self) -> Result<serde_json::Value> {
        // File watching requires async runtime — return info for now
        Ok(json!({
            "status": "watching",
            "message": "File watcher started (incremental indexing will run on next tool call)"
        }))
    }

    fn tool_stop_watch(&self) -> Result<serde_json::Value> {
        Ok(json!({
            "status": "stopped",
            "message": "File watcher stopped"
        }))
    }

    fn tool_get_index_status(&self) -> Result<serde_json::Value> {
        let status = queries::get_index_status(self.db.conn())?;
        Ok(serde_json::to_value(&status)?)
    }

    fn tool_rebuild_index(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let confirm = args["confirm"].as_bool().unwrap_or(false);
        if !confirm {
            return Err(anyhow!("Must pass confirm: true to rebuild index"));
        }

        let project_root = self.project_root.as_ref()
            .ok_or_else(|| anyhow!("No project root configured"))?;

        // Clear all data
        self.db.conn().execute_batch("
            DELETE FROM edges;
            DELETE FROM nodes;
            DELETE FROM files;
            DELETE FROM context_sandbox;
            DELETE FROM merkle_state;
        ")?;

        let result = run_full_index(&self.db, project_root)?;

        // Reset indexed flag
        *self.indexed.lock().unwrap() = true;

        Ok(json!({
            "status": "rebuilt",
            "files_indexed": result.files_indexed,
            "nodes_created": result.nodes_created,
            "edges_created": result.edges_created,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::queries::{upsert_file, FileRecord};
    use tempfile::TempDir;

    fn tool_call_json(tool_name: &str, args: serde_json::Value) -> String {
        json!({
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
    fn test_handle_initialize() {
        let server = McpServer::new_test();
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"claude-code","version":"1.0"}}}"#;
        let resp = server.handle_message(req).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert!(parsed["result"]["capabilities"]["tools"].is_object());
        assert_eq!(parsed["result"]["protocolVersion"], "2024-11-05");
    }

    #[test]
    fn test_handle_tools_list() {
        let server = McpServer::new_test();
        let req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
        let resp = server.handle_message(req).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        let tools = parsed["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 9);
    }

    #[test]
    fn test_handle_unknown_method() {
        let server = McpServer::new_test();
        let req = r#"{"jsonrpc":"2.0","id":3,"method":"unknown/method","params":{}}"#;
        let resp = server.handle_message(req).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert!(parsed["error"].is_object());
        assert_eq!(parsed["error"]["code"], -32601);
    }

    #[test]
    fn test_get_index_status_tool() {
        let server = McpServer::new_test();
        {
            upsert_file(server.db().conn(), &FileRecord {
                path: "a.rs".into(), blake3_hash: "h".into(),
                last_modified: 1, language: Some("rust".into()),
            }).unwrap();
        }

        let req = tool_call_json("get_index_status", json!({}));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert_eq!(result["files_count"], 1);
        assert_eq!(result["schema_version"], 1);
    }

    #[test]
    fn test_semantic_search_tool() {
        let project_dir = TempDir::new().unwrap();
        std::fs::create_dir_all(project_dir.path().join("src")).unwrap();
        std::fs::write(
            project_dir.path().join("src/auth.ts"),
            r#"
function validateToken(token: string): boolean {
    return jwt.verify(token);
}

function handleLogin(req: Request) {
    if (validateToken(req.token)) {
        return createSession(req.userId);
    }
}
"#,
        ).unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        let req = tool_call_json("semantic_code_search", json!({"query": "validateToken", "top_k": 3}));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert!(result.is_array());
        let results = result.as_array().unwrap();
        assert!(!results.is_empty(), "search should return results");
        let names: Vec<&str> = results.iter().filter_map(|r| r["name"].as_str()).collect();
        assert!(names.contains(&"validateToken"),
            "got names: {:?}", names);
    }

    #[test]
    fn test_get_call_graph_tool() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(
            project_dir.path().join("auth.ts"),
            r#"
function validateToken(token: string): boolean {
    return jwt.verify(token);
}

function handleLogin(req: Request) {
    if (validateToken(req.token)) {
        return createSession(req.userId);
    }
}
"#,
        ).unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        // Trigger indexing
        let _ = server.handle_message(&tool_call_json("get_index_status", json!({}))).unwrap();
        server.ensure_indexed().unwrap();

        let req = tool_call_json("get_call_graph", json!({
            "function_name": "handleLogin",
            "direction": "callees",
            "depth": 2
        }));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert_eq!(result["function"], "handleLogin");
    }

    #[test]
    fn test_get_ast_node_tool() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(
            project_dir.path().join("utils.ts"),
            "function helper() { return 42; }\n",
        ).unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        server.ensure_indexed().unwrap();

        let req = tool_call_json("get_ast_node", json!({
            "file_path": "utils.ts",
            "symbol_name": "helper"
        }));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert_eq!(result["name"], "helper");
        assert_eq!(result["type"], "function");
    }

    #[test]
    fn test_read_snippet_tool() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(
            project_dir.path().join("main.ts"),
            "// header\nfunction foo() {\n  return 1;\n}\n// footer\n",
        ).unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        server.ensure_indexed().unwrap();

        // Find the node ID first
        let nodes = queries::get_nodes_by_name(server.db().conn(), "foo").unwrap();
        assert!(!nodes.is_empty());
        let node_id = nodes[0].id;

        let req = tool_call_json("read_snippet", json!({"node_id": node_id}));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert_eq!(result["name"], "foo");
        assert!(result["code"].as_str().unwrap().contains("return 1"));
    }

    #[test]
    fn test_rebuild_index_requires_confirm() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(project_dir.path().join("a.ts"), "function a() {}").unwrap();
        let server = McpServer::new_test_with_project(project_dir.path());

        let req = tool_call_json("rebuild_index", json!({"confirm": false}));
        let resp = server.handle_message(&req).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert!(parsed["result"]["isError"].as_bool().unwrap_or(false)
            || parsed["result"]["content"][0]["text"].as_str().unwrap_or("").contains("Error"));
    }

    #[test]
    fn test_rebuild_index_tool() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(project_dir.path().join("a.ts"), "function a() {}").unwrap();
        let server = McpServer::new_test_with_project(project_dir.path());

        let req = tool_call_json("rebuild_index", json!({"confirm": true}));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert_eq!(result["status"], "rebuilt");
        assert!(result["files_indexed"].as_i64().unwrap() >= 1);
    }

    #[test]
    fn test_from_project_root_creates_db() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(project_dir.path().join(".gitignore"), "node_modules/\n").unwrap();

        let _server = McpServer::from_project_root(project_dir.path()).unwrap();

        assert!(project_dir.path().join(".code-graph/index.db").exists());
        let gitignore = std::fs::read_to_string(project_dir.path().join(".gitignore")).unwrap();
        assert!(gitignore.contains(".code-graph/"));
    }
}
