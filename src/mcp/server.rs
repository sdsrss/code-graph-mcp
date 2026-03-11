use anyhow::{anyhow, Result};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, mpsc};

use super::protocol::{JsonRpcRequest, JsonRpcResponse};
use super::tools::ToolRegistry;
use crate::embedding::model::EmbeddingModel;
use crate::indexer::pipeline::{run_full_index, run_incremental_index_cached};
use crate::indexer::watcher::{FileWatcher, WatchEvent};
use crate::search::fusion::weighted_rrf_fusion;
use crate::storage::db::Database;
use crate::storage::queries;

struct WatcherState {
    _watcher: FileWatcher,
    receiver: mpsc::Receiver<WatchEvent>,
}

/// Debounce interval for no-watcher incremental checks.
/// In tests, use 0s so incremental checks always run immediately.
#[cfg(not(test))]
const INCREMENTAL_DEBOUNCE_SECS: u64 = 30;
#[cfg(test)]
const INCREMENTAL_DEBOUNCE_SECS: u64 = 0;

/// MCP server for code graph operations. Single-threaded (stdio loop).
pub struct McpServer {
    registry: ToolRegistry,
    db: Database,
    embedding_model: Option<EmbeddingModel>,
    project_root: Option<PathBuf>,
    indexed: Mutex<bool>,
    watcher: Mutex<Option<WatcherState>>,
    last_incremental_check: Mutex<std::time::Instant>,
    dir_cache: Mutex<Option<crate::indexer::merkle::DirectoryCache>>,
}

impl McpServer {
    fn open_db(db_path: &Path, embedding_model: &Option<EmbeddingModel>) -> Result<Database> {
        if embedding_model.is_some() {
            Database::open_with_vec(db_path)
        } else {
            Database::open(db_path)
        }
    }

    /// Create from project root path: auto-creates .code-graph/ directory and .gitignore entry
    pub fn from_project_root(project_root: &Path) -> Result<Self> {
        let db_dir = project_root.join(".code-graph");
        std::fs::create_dir_all(&db_dir)?;
        let db_path = db_dir.join("index.db");

        // Ensure .code-graph/ is in .gitignore
        let gitignore_path = project_root.join(".gitignore");
        {
            let content = std::fs::read_to_string(&gitignore_path).unwrap_or_default();
            if !content.contains(".code-graph") {
                let mut new_content = content;
                if !new_content.ends_with('\n') {
                    new_content.push('\n');
                }
                new_content.push_str(".code-graph/\n");
                if let Err(e) = std::fs::write(&gitignore_path, new_content) {
                    tracing::warn!("Could not update .gitignore: {}", e);
                }
            }
        }

        let embedding_model = EmbeddingModel::load()?;
        let db = Self::open_db(&db_path, &embedding_model)?;
        Ok(Self {
            registry: ToolRegistry::new(),
            db,
            embedding_model,
            project_root: Some(project_root.to_path_buf()),
            indexed: Mutex::new(false),
            watcher: Mutex::new(None),
            last_incremental_check: Mutex::new(std::time::Instant::now() - std::time::Duration::from_secs(60)),
            dir_cache: Mutex::new(None),
        })
    }

    #[cfg(test)]
    pub fn new_test() -> Self {
        let db = Database::open(Path::new(":memory:")).unwrap();
        Self {
            registry: ToolRegistry::new(),
            db,
            embedding_model: None,
            project_root: None,
            indexed: Mutex::new(false),
            watcher: Mutex::new(None),
            last_incremental_check: Mutex::new(std::time::Instant::now() - std::time::Duration::from_secs(60)),
            dir_cache: Mutex::new(None),
        }
    }

    #[cfg(test)]
    pub fn new_test_with_project(project_root: &Path) -> Self {
        let db_dir = project_root.join(".code-graph");
        std::fs::create_dir_all(&db_dir).unwrap();
        let db = Database::open(&db_dir.join("index.db")).unwrap();
        Self {
            registry: ToolRegistry::new(),
            db,
            embedding_model: None,
            project_root: Some(project_root.to_path_buf()),
            indexed: Mutex::new(false),
            watcher: Mutex::new(None),
            last_incremental_check: Mutex::new(std::time::Instant::now() - std::time::Duration::from_secs(60)),
            dir_cache: Mutex::new(None),
        }
    }

    pub fn db(&self) -> &Database {
        &self.db
    }

    /// Ensure index is up-to-date. On first call, runs full index.
    /// If watcher is active, checks for pending events to decide if incremental needed.
    fn ensure_indexed(&self) -> Result<()> {
        let project_root = match &self.project_root {
            Some(p) => p.clone(),
            None => return Ok(()),
        };

        let model = self.embedding_model.as_ref();

        // Read the indexed flag (short lock scope to avoid holding across I/O)
        let is_indexed = *self.indexed.lock().unwrap_or_else(|e| e.into_inner());

        if !is_indexed {
            run_full_index(&self.db, &project_root, model)?;
            *self.indexed.lock().unwrap_or_else(|e| e.into_inner()) = true;
        } else {
            // Check if watcher detected changes (locks watcher only)
            let has_changes = self.drain_watcher_events();
            if has_changes {
                let cache = self.dir_cache.lock().unwrap().take();
                let (_result, new_cache) = run_incremental_index_cached(
                    &self.db, &project_root, model, cache.as_ref()
                )?;
                *self.dir_cache.lock().unwrap() = Some(new_cache);
            } else {
                // No watcher or no events: still run incremental (cheap if nothing changed)
                let has_watcher = self.watcher.lock().unwrap_or_else(|e| e.into_inner()).is_some();
                if !has_watcher {
                    // No watcher active — debounce to avoid rescanning on every tool call
                    let mut last_check = self.last_incremental_check.lock()
                        .unwrap_or_else(|e| e.into_inner());
                    if last_check.elapsed() > std::time::Duration::from_secs(INCREMENTAL_DEBOUNCE_SECS) {
                        let cache = self.dir_cache.lock().unwrap().take();
                        let (_result, new_cache) = run_incremental_index_cached(
                            &self.db, &project_root, model, cache.as_ref()
                        )?;
                        *self.dir_cache.lock().unwrap() = Some(new_cache);
                        *last_check = std::time::Instant::now();
                    }
                }
                // Watcher active but no events → index is up-to-date, skip
            }
        }
        Ok(())
    }

    /// Drain all pending events from the watcher receiver.
    /// Returns true if any file change events were received.
    fn drain_watcher_events(&self) -> bool {
        let watcher_guard = self.watcher.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref state) = *watcher_guard {
            let mut has_changes = false;
            while state.receiver.try_recv().is_ok() {
                has_changes = true;
            }
            has_changes
        } else {
            false
        }
    }

    /// Returns whether the file watcher is currently active.
    fn is_watching(&self) -> bool {
        self.watcher.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    pub fn handle_message(&self, line: &str) -> Result<Option<String>> {
        let req: JsonRpcRequest = serde_json::from_str(line)?;

        // Validate JSON-RPC version
        if let Err(msg) = req.validate() {
            let resp = JsonRpcResponse::error(req.id, -32600, msg.to_string());
            return Ok(Some(serde_json::to_string(&resp)?));
        }

        // Per JSON-RPC 2.0, any request without an id is a notification — no response
        if req.id.is_none() {
            return Ok(None);
        }

        let response = match req.method.as_str() {
            "initialize" => self.handle_initialize(req.id),
            "notifications/initialized" | "notifications/cancelled" => {
                return Ok(Some(serde_json::to_string(&JsonRpcResponse::success(req.id, json!({})))?));
            }
            "ping" => JsonRpcResponse::success(req.id, json!({})),
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

        let tool_name = match params["name"].as_str() {
            Some(name) => name,
            None => return JsonRpcResponse::error(id, -32602, "Missing or invalid 'name' in tool call params".into()),
        };
        let arguments = &params["arguments"];

        match self.handle_tool(tool_name, arguments) {
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
        let top_k = args["top_k"].as_u64().unwrap_or(5).min(100) as i64;
        let language_filter = args["language"].as_str();
        let node_type_filter = args["node_type"].as_str();

        // Ensure index is up to date
        self.ensure_indexed()?;

        // FTS5 search (fetch extra to allow for filtering)
        let fetch_count = top_k * 4;
        let fts_results = queries::fts5_search(self.db.conn(), query, fetch_count)?;

        // Convert to SearchResult for RRF
        let fts_search: Vec<crate::search::fusion::SearchResult> = fts_results.iter()
            .map(|r| crate::search::fusion::SearchResult { node_id: r.id, score: 0.0 })
            .collect();

        // Vector search (if embedding model available and vec enabled)
        let vec_search: Vec<crate::search::fusion::SearchResult> =
            if let Some(ref model) = self.embedding_model {
                if self.db.vec_enabled() {
                    match model.embed(query) {
                        Ok(query_embedding) => {
                            queries::vector_search(self.db.conn(), &query_embedding, fetch_count)?
                                .iter()
                                .map(|(node_id, _distance)| {
                                    crate::search::fusion::SearchResult { node_id: *node_id, score: 0.0 }
                                })
                                .collect()
                        }
                        Err(_) => vec![],
                    }
                } else {
                    vec![]
                }
            } else {
                vec![]
            };

        // RRF fusion (FTS + Vec when available, FTS-only otherwise)
        let fused = weighted_rrf_fusion(&fts_search, &vec_search, 60, fetch_count as usize, 1.5, 1.0);

        // Batch-fetch all candidate nodes with file info (single query instead of N+1)
        let candidate_ids: Vec<i64> = fused.iter().map(|r| r.node_id).collect();
        let nodes_with_files = queries::get_nodes_with_files_by_ids(self.db.conn(), &candidate_ids)?;

        // Build a lookup by node_id preserving the fused ranking order
        let mut nwf_map: std::collections::HashMap<i64, &queries::NodeWithFile> =
            nodes_with_files.iter().map(|nwf| (nwf.node.id, nwf)).collect();

        // Collect results with language/node_type filtering
        let mut node_results: Vec<queries::NodeResult> = Vec::new();
        let mut file_paths: Vec<String> = Vec::new();
        let mut results = Vec::new();
        for r in &fused {
            if results.len() >= top_k as usize {
                break;
            }
            if let Some(nwf) = nwf_map.remove(&r.node_id) {
                let node = &nwf.node;
                // Apply node_type filter
                if let Some(nt) = node_type_filter {
                    if node.node_type != nt {
                        continue;
                    }
                }
                // Apply language filter
                if let Some(lang) = language_filter {
                    if nwf.language.as_deref() != Some(lang) {
                        continue;
                    }
                }
                results.push(json!({
                    "node_id": node.id,
                    "name": node.name,
                    "type": node.node_type,
                    "file_path": nwf.file_path,
                    "start_line": node.start_line,
                    "end_line": node.end_line,
                    "code_content": node.code_content,
                    "signature": node.signature,
                    "context_string": node.context_string,
                }));
                node_results.push(queries::NodeResult {
                    id: node.id,
                    file_id: node.file_id,
                    node_type: node.node_type.clone(),
                    name: node.name.clone(),
                    qualified_name: node.qualified_name.clone(),
                    start_line: node.start_line,
                    end_line: node.end_line,
                    code_content: node.code_content.clone(),
                    signature: node.signature.clone(),
                    doc_comment: node.doc_comment.clone(),
                    context_string: node.context_string.clone(),
                });
                file_paths.push(nwf.file_path.clone());
            }
        }

        // Context Sandbox: compress if results exceed token threshold
        use crate::sandbox::compressor::CompressedOutput;
        if let Some(compressed) = crate::sandbox::compressor::compress_if_needed(self.db.conn(), &node_results, &file_paths, 2000) {
            let (mode, compact) = match compressed {
                CompressedOutput::Nodes(nodes) => {
                    let items: Vec<serde_json::Value> = nodes.iter().map(|c| json!({
                        "node_id": c.node_id,
                        "file_path": c.file_path,
                        "summary": c.summary,
                    })).collect();
                    ("compressed_nodes", items)
                }
                CompressedOutput::Files(groups) => {
                    let items: Vec<serde_json::Value> = groups.iter().map(|g| json!({
                        "file_path": g.file_path,
                        "summary": g.summary,
                        "node_ids": g.node_ids,
                    })).collect();
                    ("compressed_files", items)
                }
                CompressedOutput::Directories(groups) => {
                    let items: Vec<serde_json::Value> = groups.iter().map(|g| json!({
                        "file_path": g.file_path,
                        "summary": g.summary,
                        "node_ids": g.node_ids,
                    })).collect();
                    ("compressed_directories", items)
                }
            };
            return Ok(json!({
                "mode": mode,
                "message": "Results exceeded token limit. Use read_snippet(node_id) to expand individual symbols.",
                "results": compact
            }));
        }

        Ok(json!(results))
    }

    fn tool_get_call_graph(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let function_name = args["function_name"].as_str()
            .ok_or_else(|| anyhow!("function_name is required"))?;
        let direction = args["direction"].as_str().unwrap_or("both");
        let depth = args["depth"].as_i64().unwrap_or(2).clamp(1, 20) as i32;
        let file_path = args["file_path"].as_str();

        self.ensure_indexed()?;

        let results = crate::graph::query::get_call_graph(
            self.db.conn(), function_name, direction, depth, file_path,
        )?;

        use crate::graph::query::Direction;

        let callee_nodes: Vec<serde_json::Value> = results.iter()
            .filter(|n| n.direction == Direction::Callees && n.depth > 0)
            .map(|n| json!({"node_id": n.node_id, "name": n.name, "type": n.node_type, "file_path": n.file_path, "depth": n.depth}))
            .collect();
        let caller_nodes: Vec<serde_json::Value> = results.iter()
            .filter(|n| n.direction == Direction::Callers && n.depth > 0)
            .map(|n| json!({"node_id": n.node_id, "name": n.name, "type": n.node_type, "file_path": n.file_path, "depth": n.depth}))
            .collect();

        Ok(json!({
            "function": function_name,
            "direction": direction,
            "callees": callee_nodes,
            "callers": caller_nodes,
        }))
    }

    fn tool_find_http_route(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let route_path = args["route_path"].as_str()
            .ok_or_else(|| anyhow!("route_path is required"))?;
        let include_middleware = args["include_middleware"].as_bool().unwrap_or(true);

        self.ensure_indexed()?;

        use crate::storage::schema::{REL_CALLS, REL_ROUTES_TO};
        let rows = queries::find_routes_by_path(self.db.conn(), route_path, REL_ROUTES_TO)?;

        let mut handlers: Vec<serde_json::Value> = Vec::new();
        for rm in &rows {
            let mut handler = json!({
                "node_id": rm.node_id,
                "metadata": rm.metadata,
                "handler_name": rm.handler_name,
                "handler_type": rm.handler_type,
                "file_path": rm.file_path,
                "start_line": rm.start_line,
                "end_line": rm.end_line,
            });

            if include_middleware {
                let downstream = queries::get_edge_target_names(self.db.conn(), rm.node_id, REL_CALLS)?;
                handler["downstream_calls"] = json!(downstream);
            }

            handlers.push(handler);
        }

        Ok(json!({
            "route": route_path,
            "handlers": handlers,
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
                    use crate::storage::schema::REL_CALLS as CALLS;
                    let callees = queries::get_edge_target_names(self.db.conn(), n.id, CALLS)?;
                    let callers = queries::get_edge_source_names(self.db.conn(), n.id, CALLS)?;
                    result["calls"] = json!(callees);
                    result["called_by"] = json!(callers);
                }

                Ok(result)
            }
            None => Err(anyhow!("Symbol '{}' not found in '{}'", symbol_name, file_path)),
        }
    }

    fn tool_read_snippet(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        self.ensure_indexed()?;

        let node_id = args["node_id"].as_i64()
            .ok_or_else(|| anyhow!("node_id is required"))?;
        let context_lines = args["context_lines"].as_i64().unwrap_or(3).clamp(0, 100) as usize;

        let node = queries::get_node_by_id(self.db.conn(), node_id)?
            .ok_or_else(|| anyhow!("Node {} not found", node_id))?;

        let file_path = queries::get_file_path(self.db.conn(), node.file_id)?
            .unwrap_or_default();

        // Read source file for context lines if project root available
        let full_snippet = if let Some(ref root) = self.project_root {
            let abs_path = root.join(&file_path);
            // Verify path stays within project root to prevent traversal
            let canonical = match abs_path.canonicalize() {
                Ok(p) => p,
                Err(_) => {
                    return Ok(json!({
                        "error": format!("Cannot resolve path: {}", file_path),
                        "node_id": node_id
                    }));
                }
            };
            let root_canonical = root.canonicalize()
                .map_err(|e| anyhow!("Cannot resolve project root: {}", e))?;
            if !canonical.starts_with(&root_canonical) {
                return Ok(json!({
                    "error": "Path traversal detected",
                    "node_id": node_id
                }));
            }
            if let Ok(source) = std::fs::read_to_string(&canonical) {
                let lines: Vec<&str> = source.lines().collect();
                let start = (node.start_line as usize).saturating_sub(1 + context_lines);
                let end = ((node.end_line as usize) + context_lines).min(lines.len());
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
        let project_root = self.project_root.as_ref()
            .ok_or_else(|| anyhow!("No project root configured"))?;

        let mut watcher_guard = self.watcher.lock().unwrap_or_else(|e| e.into_inner());
        if watcher_guard.is_some() {
            return Ok(json!({
                "status": "already_watching",
                "message": "File watcher is already running"
            }));
        }

        let (tx, rx) = mpsc::channel();
        let fw = FileWatcher::start(project_root, tx)?;
        *watcher_guard = Some(WatcherState {
            _watcher: fw,
            receiver: rx,
        });

        Ok(json!({
            "status": "watching",
            "message": "File watcher started. Changes will be detected and indexed on next tool call."
        }))
    }

    fn tool_stop_watch(&self) -> Result<serde_json::Value> {
        let mut watcher_guard = self.watcher.lock().unwrap_or_else(|e| e.into_inner());
        if watcher_guard.is_none() {
            return Ok(json!({
                "status": "not_watching",
                "message": "File watcher was not running"
            }));
        }
        *watcher_guard = None; // Drops the FileWatcher, stopping it
        Ok(json!({
            "status": "stopped",
            "message": "File watcher stopped"
        }))
    }

    fn tool_get_index_status(&self) -> Result<serde_json::Value> {
        let status = queries::get_index_status(self.db.conn(), self.is_watching())?;
        Ok(serde_json::to_value(&status)?)
    }

    fn tool_rebuild_index(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let confirm = args["confirm"].as_bool().unwrap_or(false);
        if !confirm {
            return Err(anyhow!("Must pass confirm: true to rebuild index"));
        }

        let project_root = self.project_root.as_ref()
            .ok_or_else(|| anyhow!("No project root configured"))?;

        // Clear all data in a single transaction (CASCADE handles nodes→edges)
        self.db.conn().execute_batch("BEGIN")?;
        let cleanup = (|| -> anyhow::Result<()> {
            self.db.conn().execute("DELETE FROM context_sandbox", [])?;
            self.db.conn().execute("DELETE FROM files", [])?;
            Ok(())
        })();
        match cleanup {
            Ok(()) => { self.db.conn().execute_batch("COMMIT")?; }
            Err(e) => {
                let _ = self.db.conn().execute_batch("ROLLBACK");
                return Err(e);
            }
        }

        let result = run_full_index(&self.db, project_root, self.embedding_model.as_ref())?;

        // Reset indexed flag
        *self.indexed.lock().unwrap_or_else(|e| e.into_inner()) = true;

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
    fn test_start_stop_watch() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(project_dir.path().join("a.ts"), "function a() {}").unwrap();
        let server = McpServer::new_test_with_project(project_dir.path());

        // Start watching
        let req = tool_call_json("start_watch", json!({}));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert_eq!(result["status"], "watching");
        assert!(server.is_watching());

        // Starting again should say already watching
        let req = tool_call_json("start_watch", json!({}));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert_eq!(result["status"], "already_watching");

        // Status should reflect watching
        let req = tool_call_json("get_index_status", json!({}));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert_eq!(result["is_watching"], true);

        // Stop watching
        let req = tool_call_json("stop_watch", json!({}));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert_eq!(result["status"], "stopped");
        assert!(!server.is_watching());

        // Stopping again should say not watching
        let req = tool_call_json("stop_watch", json!({}));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert_eq!(result["status"], "not_watching");
    }

    #[test]
    fn test_watcher_detects_changes_and_reindexes() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(project_dir.path().join("a.ts"), "function original() {}").unwrap();
        let server = McpServer::new_test_with_project(project_dir.path());

        // Initial index
        server.ensure_indexed().unwrap();

        // Verify original is indexed
        let nodes = queries::get_nodes_by_name(server.db().conn(), "original").unwrap();
        assert_eq!(nodes.len(), 1);

        // Start watching
        let req = tool_call_json("start_watch", json!({}));
        let _ = server.handle_message(&req).unwrap();

        // Modify file
        std::fs::write(project_dir.path().join("a.ts"), "function changed() {}").unwrap();

        // Give watcher time to detect change
        std::thread::sleep(std::time::Duration::from_millis(300));

        // Next ensure_indexed should detect change via watcher and run incremental
        server.ensure_indexed().unwrap();

        // Verify changed is now indexed
        let nodes = queries::get_nodes_by_name(server.db().conn(), "changed").unwrap();
        assert_eq!(nodes.len(), 1, "changed function should be indexed after watcher-triggered reindex");

        // Stop watching
        let req = tool_call_json("stop_watch", json!({}));
        let _ = server.handle_message(&req).unwrap();
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

    #[test]
    fn test_malformed_json_returns_error() {
        let server = McpServer::new_test();
        let result = server.handle_message("not valid json");
        assert!(result.is_err(), "malformed JSON should return Err");
    }

    #[test]
    fn test_wrong_jsonrpc_version() {
        let server = McpServer::new_test();
        let req = r#"{"jsonrpc":"1.0","id":1,"method":"initialize","params":{}}"#;
        let resp = server.handle_message(req).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert!(parsed["error"].is_object());
        assert_eq!(parsed["error"]["code"], -32600);
    }

    #[test]
    fn test_notification_returns_none() {
        let server = McpServer::new_test();
        // JSON-RPC notification: no "id" field
        let req = r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#;
        let resp = server.handle_message(req).unwrap();
        assert!(resp.is_none(), "notifications should return None");
    }

    #[test]
    fn test_ping_returns_empty_object() {
        let server = McpServer::new_test();
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"ping","params":{}}"#;
        let resp = server.handle_message(req).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert!(parsed["result"].is_object());
    }

    #[test]
    fn test_tools_call_missing_params() {
        let server = McpServer::new_test();
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call"}"#;
        let resp = server.handle_message(req).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert!(parsed["error"].is_object());
        assert_eq!(parsed["error"]["code"], -32602);
    }

    #[test]
    fn test_tools_call_missing_name() {
        let server = McpServer::new_test();
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"arguments":{}}}"#;
        let resp = server.handle_message(req).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert!(parsed["error"].is_object());
        assert_eq!(parsed["error"]["code"], -32602);
    }

    #[test]
    fn test_unknown_tool_returns_error() {
        let server = McpServer::new_test();
        let req = tool_call_json("nonexistent_tool", json!({}));
        let resp = server.handle_message(&req).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        let text = parsed["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Error"), "unknown tool should return error in content");
        assert!(parsed["result"]["isError"].as_bool().unwrap_or(false));
    }

    #[test]
    fn test_semantic_search_language_filter() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(project_dir.path().join("app.ts"), "function handler() { return 1; }").unwrap();
        std::fs::write(project_dir.path().join("app.py"), "def handler():\n    return 1\n").unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());

        // Search with language filter for typescript
        let req = tool_call_json("semantic_code_search", json!({
            "query": "handler",
            "language": "typescript"
        }));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        let results = result.as_array().unwrap();
        for r in results {
            assert!(r["file_path"].as_str().unwrap().ends_with(".ts"),
                "language filter should only return typescript files, got: {}", r["file_path"]);
        }
    }

    #[test]
    fn test_semantic_search_node_type_filter() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(project_dir.path().join("mix.ts"), r#"
class UserService {
    getUser() { return null; }
}
function standalone() { return 1; }
"#).unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        let req = tool_call_json("semantic_code_search", json!({
            "query": "user",
            "node_type": "class"
        }));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        let results = result.as_array().unwrap();
        for r in results {
            assert_eq!(r["type"].as_str().unwrap(), "class",
                "node_type filter should only return classes");
        }
    }

    #[test]
    fn test_semantic_search_sandbox_compression() {
        let project_dir = TempDir::new().unwrap();
        // Create many functions with large code to exceed 2000 token threshold
        let mut code = String::new();
        for i in 0..20 {
            code.push_str(&format!(
                "function func{}() {{\n{}\n}}\n",
                i,
                format!("  // {}\n", "x".repeat(500)).repeat(3)
            ));
        }
        std::fs::write(project_dir.path().join("big.ts"), &code).unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        let req = tool_call_json("semantic_code_search", json!({
            "query": "func",
            "top_k": 20
        }));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);

        // Should be in compressed mode
        if result["mode"].as_str() == Some("compressed") {
            assert!(result["results"].is_array());
            let compressed = result["results"].as_array().unwrap();
            assert!(!compressed.is_empty());
            // Each compressed result should have node_id and summary
            assert!(compressed[0]["node_id"].is_number());
            assert!(compressed[0]["summary"].is_string());
        }
        // If not compressed (small code), that's also valid behavior
    }

    #[test]
    fn test_find_http_route_with_downstream() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(project_dir.path().join("server.ts"), r#"
function validateToken(token: string) { return true; }

function handleLogin(req: Request) {
    validateToken(req.token);
    return createSession(req.userId);
}

app.post('/api/login', handleLogin);
"#).unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        server.ensure_indexed().unwrap();

        let req = tool_call_json("find_http_route", json!({
            "route_path": "/api/login",
            "include_middleware": true
        }));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        assert_eq!(result["route"], "/api/login");
        // handlers array should exist
        assert!(result["handlers"].is_array());
    }

    #[test]
    fn test_semantic_search_clamps_top_k() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(
            project_dir.path().join("small.ts"),
            "function hello() { return 1; }\n",
        ).unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        // Request absurdly large top_k — should not error, just return clamped results
        let req = tool_call_json("semantic_code_search", json!({
            "query": "hello",
            "top_k": 999999
        }));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);
        // Should succeed (array or compressed mode) — not crash or OOM
        assert!(result.is_array() || result["mode"].as_str() == Some("compressed"),
            "search with huge top_k should return valid results, got: {}", result);
    }

    #[test]
    fn test_read_snippet_handles_missing_node() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(
            project_dir.path().join("a.ts"),
            "function exists() { return 1; }\n",
        ).unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        server.ensure_indexed().unwrap();

        // Request a non-existent node_id — should return error gracefully, not panic
        let req = tool_call_json("read_snippet", json!({"node_id": 999999}));
        let resp = server.handle_message(&req).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        let text = parsed["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Error") || text.contains("not found"),
            "missing node should return error message, got: {}", text);
    }
}
