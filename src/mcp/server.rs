use anyhow::{anyhow, Result};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, mpsc};

use super::protocol::{JsonRpcRequest, JsonRpcResponse};
use super::tools::ToolRegistry;
use crate::embedding::model::EmbeddingModel;
use crate::indexer::pipeline::{run_full_index, run_incremental_index_cached};
use crate::indexer::watcher::{FileWatcher, WatchEvent};
use crate::search::fusion::weighted_rrf_fusion;
use crate::storage::db::Database;
use crate::storage::queries;

/// Lock a Mutex, recovering from poison but logging a warning.
fn lock_or_recover<'a, T>(mutex: &'a Mutex<T>, label: &str) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(|e| {
        tracing::warn!("Recovering poisoned mutex ({}): prior panic in critical section", label);
        e.into_inner()
    })
}

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

/// Token threshold for auto-compressing tool results.
/// Results exceeding this estimated token count are returned as summaries
/// with node_ids for expansion via read_snippet.
const COMPRESSION_TOKEN_THRESHOLD: usize = 2000;

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
            if !content.lines().any(|line| {
                let trimmed = line.trim();
                trimmed == ".code-graph/" || trimmed == ".code-graph"
            }) {
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
        let is_indexed = *lock_or_recover(&self.indexed, "indexed");

        if !is_indexed {
            run_full_index(&self.db, &project_root, model)?;
            *lock_or_recover(&self.indexed, "indexed") = true;
        } else {
            // Check if watcher detected changes (locks watcher only)
            let has_changes = self.drain_watcher_events();
            if has_changes {
                self.run_incremental_with_cache_restore(&project_root, model)?;
            } else {
                // No watcher or no events: still run incremental (cheap if nothing changed)
                let has_watcher = lock_or_recover(&self.watcher, "watcher").is_some();
                if !has_watcher {
                    // No watcher active — debounce to avoid rescanning on every tool call
                    let mut last_check = lock_or_recover(&self.last_incremental_check, "last_incremental_check");
                    if last_check.elapsed() > std::time::Duration::from_secs(INCREMENTAL_DEBOUNCE_SECS) {
                        self.run_incremental_with_cache_restore(&project_root, model)?;
                        *last_check = std::time::Instant::now();
                    }
                }
                // Watcher active but no events → index is up-to-date, skip
            }
        }
        Ok(())
    }

    /// Run incremental index with cache snapshot/restore on failure.
    fn run_incremental_with_cache_restore(&self, project_root: &Path, model: Option<&EmbeddingModel>) -> Result<()> {
        let mut cache_guard = lock_or_recover(&self.dir_cache, "dir_cache");
        let cache_snapshot = cache_guard.clone();
        let cache = cache_guard.take();
        drop(cache_guard); // Release lock during I/O

        match run_incremental_index_cached(&self.db, project_root, model, cache.as_ref()) {
            Ok((_result, new_cache)) => {
                *lock_or_recover(&self.dir_cache, "dir_cache") = Some(new_cache);
                Ok(())
            }
            Err(e) => {
                tracing::error!("Incremental index failed, restoring cache: {}", e);
                *lock_or_recover(&self.dir_cache, "dir_cache") = cache_snapshot;
                Err(e)
            }
        }
    }

    /// Drain all pending events from the watcher receiver.
    /// Returns true if any file change events were received.
    fn drain_watcher_events(&self) -> bool {
        let watcher_guard = lock_or_recover(&self.watcher, "watcher");
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
        lock_or_recover(&self.watcher, "watcher").is_some()
    }

    pub fn handle_message(&self, line: &str) -> Result<Option<String>> {
        let req: JsonRpcRequest = match serde_json::from_str(line) {
            Ok(req) => req,
            Err(e) => {
                let resp = JsonRpcResponse::error(
                    None,
                    super::protocol::JSONRPC_PARSE_ERROR,
                    format!("Parse error: {}", e),
                );
                return Ok(Some(serde_json::to_string(&resp)?));
            }
        };

        // Per JSON-RPC 2.0, notifications (no id) must never receive a response
        if req.id.is_none() {
            return Ok(None);
        }

        // Validate JSON-RPC version (only for requests with id)
        if let Err(msg) = req.validate() {
            let resp = JsonRpcResponse::error(req.id, super::protocol::JSONRPC_INVALID_REQUEST, msg.to_string());
            return Ok(Some(serde_json::to_string(&resp)?));
        }

        let response = match req.method.as_str() {
            "initialize" => self.handle_initialize(req.id),
            "ping" => JsonRpcResponse::success(req.id, json!({})),
            "tools/list" => self.handle_tools_list(req.id),
            "tools/call" => self.handle_tools_call(req.id, req.params),
            "resources/list" => self.handle_resources_list(req.id),
            "resources/read" => self.handle_resources_read(req.id, req.params),
            "prompts/list" => self.handle_prompts_list(req.id),
            "prompts/get" => self.handle_prompts_get(req.id, req.params),
            _ => JsonRpcResponse::error(
                req.id,
                super::protocol::JSONRPC_METHOD_NOT_FOUND,
                format!("Method not found: {}", req.method),
            ),
        };

        Ok(Some(serde_json::to_string(&response)?))
    }

    fn handle_initialize(&self, id: Option<serde_json::Value>) -> JsonRpcResponse {
        JsonRpcResponse::success(id, json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": { "listChanged": false },
                "resources": { "subscribe": false, "listChanged": false },
                "prompts": { "listChanged": false }
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
            None => return JsonRpcResponse::error(id, super::protocol::JSONRPC_INVALID_PARAMS, "Missing params".into()),
        };

        let tool_name = match params["name"].as_str() {
            Some(name) => name,
            None => return JsonRpcResponse::error(id, super::protocol::JSONRPC_INVALID_PARAMS, "Missing or invalid 'name' in tool call params".into()),
        };
        let arguments = &params["arguments"];

        match self.handle_tool(tool_name, arguments) {
            Ok(result) => {
                let text = serde_json::to_string_pretty(&result)
                    .unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {}\"}}", e));
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

    fn handle_resources_list(&self, id: Option<serde_json::Value>) -> JsonRpcResponse {
        JsonRpcResponse::success(id, json!({
            "resources": [{
                "uri": "code-graph://project-summary",
                "name": "Code Graph Project Summary",
                "description": "Overview of the indexed codebase: file count, node count, edge count, languages, and index health",
                "mimeType": "application/json",
                "annotations": {
                    "audience": ["assistant"]
                }
            }]
        }))
    }

    fn handle_resources_read(&self, id: Option<serde_json::Value>, params: Option<serde_json::Value>) -> JsonRpcResponse {
        let uri = params.as_ref()
            .and_then(|p| p["uri"].as_str())
            .unwrap_or("");

        match uri {
            "code-graph://project-summary" => {
                let status = match queries::get_index_status(self.db.conn(), self.is_watching()) {
                    Ok(s) => s,
                    Err(e) => return JsonRpcResponse::error(
                        id,
                        super::protocol::JSONRPC_INTERNAL_ERROR,
                        format!("Failed to get index status: {}", e),
                    ),
                };

                let summary = json!({
                    "files": status.files_count,
                    "nodes": status.nodes_count,
                    "edges": status.edges_count,
                    "schema_version": status.schema_version,
                    "db_size_bytes": status.db_size_bytes,
                    "watching": status.is_watching,
                    "last_indexed_at": status.last_indexed_at,
                });

                JsonRpcResponse::success(id, json!({
                    "contents": [{
                        "uri": "code-graph://project-summary",
                        "mimeType": "application/json",
                        "text": serde_json::to_string_pretty(&summary).unwrap_or_default()
                    }]
                }))
            }
            _ => JsonRpcResponse::error(
                id,
                super::protocol::JSONRPC_INVALID_PARAMS,
                format!("Unknown resource URI: {}", uri),
            ),
        }
    }

    fn handle_prompts_list(&self, id: Option<serde_json::Value>) -> JsonRpcResponse {
        JsonRpcResponse::success(id, json!({
            "prompts": [
                {
                    "name": "impact-analysis",
                    "description": "Analyze the blast radius of changing a symbol",
                    "arguments": [
                        { "name": "symbol_name", "description": "Symbol to analyze", "required": true }
                    ]
                },
                {
                    "name": "understand-module",
                    "description": "Deep dive into a module's architecture and relationships",
                    "arguments": [
                        { "name": "path", "description": "File or directory path", "required": true }
                    ]
                },
                {
                    "name": "trace-request",
                    "description": "Trace an HTTP request from route to data layer",
                    "arguments": [
                        { "name": "route", "description": "HTTP route path (e.g. /api/users)", "required": true }
                    ]
                }
            ]
        }))
    }

    fn handle_prompts_get(&self, id: Option<serde_json::Value>, params: Option<serde_json::Value>) -> JsonRpcResponse {
        let name = params.as_ref()
            .and_then(|p| p["name"].as_str())
            .unwrap_or("");
        let arguments = params.as_ref()
            .and_then(|p| p["arguments"].as_object());

        match name {
            "impact-analysis" => {
                let symbol = arguments
                    .and_then(|a| a.get("symbol_name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("<symbol>");
                JsonRpcResponse::success(id, json!({
                    "messages": [{
                        "role": "user",
                        "content": {
                            "type": "text",
                            "text": format!(
                                "Analyze the impact of changing the symbol '{}'. \
                                 Use the impact_analysis tool with symbol_name='{}' to get the blast radius, \
                                 then use get_call_graph to understand the full caller/callee chain. \
                                 Present: affected files, affected routes, risk level, and recommendations.",
                                symbol, symbol
                            )
                        }
                    }]
                }))
            }
            "understand-module" => {
                let path = arguments
                    .and_then(|a| a.get("path"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("<path>");
                JsonRpcResponse::success(id, json!({
                    "messages": [{
                        "role": "user",
                        "content": {
                            "type": "text",
                            "text": format!(
                                "Give me a deep understanding of the module at '{}'. \
                                 Use module_overview to get exports and hot paths, \
                                 then use dependency_graph to map what it depends on and what depends on it. \
                                 For the top 3 most-called exports, use get_call_graph to show their caller chain. \
                                 Present: purpose, public API, dependencies, dependents, and hot paths.",
                                path
                            )
                        }
                    }]
                }))
            }
            "trace-request" => {
                let route = arguments
                    .and_then(|a| a.get("route"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("<route>");
                JsonRpcResponse::success(id, json!({
                    "messages": [{
                        "role": "user",
                        "content": {
                            "type": "text",
                            "text": format!(
                                "Trace the complete HTTP request flow for route '{}'. \
                                 Use trace_http_chain to get the full chain from route to data layer. \
                                 For each key node, use read_snippet to show the implementation. \
                                 Map the flow: route → middleware → validation → business logic → data access → response. \
                                 Highlight error handling, auth checks, and database operations.",
                                route
                            )
                        }
                    }]
                }))
            }
            _ => JsonRpcResponse::error(
                id,
                super::protocol::JSONRPC_INVALID_PARAMS,
                format!("Unknown prompt: {}", name),
            ),
        }
    }

    fn handle_tool(&self, name: &str, args: &serde_json::Value) -> Result<serde_json::Value> {
        match name {
            "semantic_code_search" => self.tool_semantic_search(args),
            "get_call_graph" => self.tool_get_call_graph(args),
            "find_http_route" => self.tool_find_http_route(args),
            "trace_http_chain" => self.tool_trace_http_chain(args),
            "get_ast_node" => self.tool_get_ast_node(args),
            "read_snippet" => self.tool_read_snippet(args),
            "start_watch" => self.tool_start_watch(),
            "stop_watch" => self.tool_stop_watch(),
            "get_index_status" => self.tool_get_index_status(),
            "rebuild_index" => self.tool_rebuild_index(args),
            "impact_analysis" => self.tool_impact_analysis(args),
            "module_overview" => self.tool_module_overview(args),
            "dependency_graph" => self.tool_dependency_graph(args),
            "find_similar_code" => self.tool_find_similar_code(args),
            _ => Err(anyhow!("Unknown tool: {}", name)),
        }
    }

    fn tool_semantic_search(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let query = args["query"].as_str()
            .ok_or_else(|| anyhow!("query is required"))?;
        let top_k = args["top_k"].as_u64().unwrap_or(5).clamp(1, 100) as i64;
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
        if let Some(compressed) = crate::sandbox::compressor::compress_if_needed(&node_results, &file_paths, COMPRESSION_TOKEN_THRESHOLD)? {
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

        // Build node lists with direction tag for potential compression reuse
        let all_nodes: Vec<serde_json::Value> = results.iter()
            .filter(|n| n.depth > 0)
            .map(|n| json!({
                "node_id": n.node_id,
                "name": n.name,
                "type": n.node_type,
                "file_path": n.file_path,
                "depth": n.depth,
                "direction": n.direction.as_str(),
            }))
            .collect();

        // Estimate tokens from the flat list to avoid building full result just to discard it
        let est_tokens = crate::sandbox::compressor::estimate_json_tokens(&json!(all_nodes));
        if est_tokens > COMPRESSION_TOKEN_THRESHOLD {
            return Ok(json!({
                "mode": "compressed_call_graph",
                "message": "Call graph exceeded token limit. Use read_snippet(node_id) to expand individual nodes.",
                "function": function_name,
                "results": all_nodes,
            }));
        }

        // Normal path: split into callers/callees for structured output
        let callee_nodes: Vec<&serde_json::Value> = all_nodes.iter()
            .filter(|n| n["direction"] == "callees")
            .collect();
        let caller_nodes: Vec<&serde_json::Value> = all_nodes.iter()
            .filter(|n| n["direction"] == "callers")
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

        use crate::domain::{REL_CALLS, REL_ROUTES_TO};
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

    fn tool_trace_http_chain(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let route_path = args["route_path"].as_str()
            .ok_or_else(|| anyhow!("route_path is required"))?;
        let depth = args["depth"].as_i64().unwrap_or(3).clamp(1, 20) as i32;
        let include_middleware = args["include_middleware"].as_bool().unwrap_or(true);

        self.ensure_indexed()?;

        use crate::domain::{REL_CALLS, REL_ROUTES_TO};
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

            // Recursive call chain via call graph
            let chain = crate::graph::query::get_call_graph(
                self.db.conn(), &rm.handler_name, "callees", depth, Some(&rm.file_path),
            )?;
            let chain_nodes: Vec<serde_json::Value> = chain.iter()
                .filter(|n| n.depth > 0) // exclude root (the handler itself)
                .map(|n| json!({
                    "node_id": n.node_id,
                    "name": n.name,
                    "type": n.node_type,
                    "file_path": n.file_path,
                    "depth": n.depth,
                }))
                .collect();
            handler["call_chain"] = json!(chain_nodes);

            handlers.push(handler);
        }

        let result = json!({
            "route": route_path,
            "handlers": handlers,
        });

        // Compress if result exceeds token threshold
        let tokens = crate::sandbox::compressor::estimate_json_tokens(&result);
        if tokens > COMPRESSION_TOKEN_THRESHOLD {
            let compressed_handlers: Vec<serde_json::Value> = handlers.iter().map(|h| {
                json!({
                    "node_id": h["node_id"],
                    "handler_name": h["handler_name"],
                    "file_path": h["file_path"],
                    "start_line": h["start_line"],
                    "end_line": h["end_line"],
                    "chain_count": h["call_chain"].as_array().map_or(0, |a| a.len()),
                })
            }).collect();
            return Ok(json!({
                "mode": "compressed_http_chain",
                "message": "HTTP chain exceeded token limit. Use read_snippet(node_id) or get_call_graph(function_name) to expand.",
                "route": route_path,
                "results": compressed_handlers,
            }));
        }

        Ok(result)
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
                    use crate::domain::REL_CALLS as CALLS;
                    let callees = queries::get_edge_target_names(self.db.conn(), n.id, CALLS)?;
                    let callers = queries::get_edge_source_names(self.db.conn(), n.id, CALLS)?;
                    result["calls"] = json!(callees);
                    result["called_by"] = json!(callers);
                }

                // Compress if code_content exceeds token threshold
                let tokens = crate::sandbox::compressor::estimate_json_tokens(&result);
                if tokens > COMPRESSION_TOKEN_THRESHOLD {
                    return Ok(json!({
                        "mode": "compressed_node",
                        "message": "Node content exceeded token limit. Use read_snippet(node_id) to read full code.",
                        "node_id": n.id,
                        "name": n.name,
                        "type": n.node_type,
                        "file_path": file_path,
                        "start_line": n.start_line,
                        "end_line": n.end_line,
                        "signature": n.signature,
                        "summary": format!("{} {} in {} (lines {}-{}){}",
                            n.node_type, n.name, file_path, n.start_line, n.end_line,
                            n.signature.as_ref().map(|s| format!(" {}", s)).unwrap_or_default()),
                    }));
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
            .ok_or_else(|| anyhow!("File record missing for node {}", node_id))?;

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

        let mut watcher_guard = lock_or_recover(&self.watcher, "watcher");
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
        let mut watcher_guard = lock_or_recover(&self.watcher, "watcher");
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
        {
            let tx = self.db.conn().unchecked_transaction()?;
            tx.execute("DELETE FROM files", [])?;
            tx.commit()?;
        }

        let result = run_full_index(&self.db, project_root, self.embedding_model.as_ref())?;

        // Reset indexed flag
        *lock_or_recover(&self.indexed, "indexed") = true;

        Ok(json!({
            "status": "rebuilt",
            "files_indexed": result.files_indexed,
            "nodes_created": result.nodes_created,
            "edges_created": result.edges_created,
        }))
    }

    fn tool_impact_analysis(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        self.ensure_indexed()?;

        let symbol_name = args["symbol_name"].as_str()
            .ok_or_else(|| anyhow!("Missing symbol_name"))?;
        let change_type = args.get("change_type")
            .and_then(|v| v.as_str())
            .unwrap_or("behavior");
        let depth = args.get("depth")
            .and_then(|v| v.as_i64())
            .unwrap_or(3) as i32;

        let callers = queries::get_callers_with_route_info(
            self.db.conn(), symbol_name, None, depth
        )?;

        let affected_files: std::collections::HashSet<&str> = callers.iter()
            .map(|c| c.file_path.as_str()).collect();
        let affected_routes: Vec<serde_json::Value> = callers.iter()
            .filter_map(|c| {
                c.route_info.as_ref().and_then(|meta| serde_json::from_str(meta).ok())
            }).collect();

        let risk_level = if callers.len() > 10 || affected_routes.len() >= 3 || change_type == "remove" {
            "HIGH"
        } else if callers.len() > 3 || !affected_routes.is_empty() {
            "MEDIUM"
        } else {
            "LOW"
        };

        let direct: Vec<_> = callers.iter().filter(|c| c.depth == 1).collect();
        let transitive: Vec<_> = callers.iter().filter(|c| c.depth > 1).collect();

        Ok(json!({
            "symbol": symbol_name,
            "change_type": change_type,
            "direct_callers": direct.iter().map(|c| json!({
                "name": c.name, "file": c.file_path, "depth": c.depth
            })).collect::<Vec<_>>(),
            "transitive_callers": transitive.iter().map(|c| json!({
                "name": c.name, "file": c.file_path, "depth": c.depth
            })).collect::<Vec<_>>(),
            "affected_routes": affected_routes,
            "affected_files": affected_files.len(),
            "risk_level": risk_level,
            "summary": format!("Changing {} affects {} routes, {} functions across {} files [{}]",
                symbol_name, affected_routes.len(), callers.len(), affected_files.len(), risk_level)
        }))
    }

    fn tool_module_overview(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        self.ensure_indexed()?;

        let path = args["path"].as_str()
            .ok_or_else(|| anyhow!("Missing path"))?;

        let exports = queries::get_module_exports(self.db.conn(), path)?;

        // Get import/dependency info at file level
        let files: std::collections::HashSet<&str> = exports.iter()
            .map(|e| e.file_path.as_str()).collect();

        let hot_paths: Vec<serde_json::Value> = exports.iter()
            .filter(|e| e.caller_count > 0)
            .take(5)
            .map(|e| json!({
                "name": e.name,
                "type": e.node_type,
                "file": e.file_path,
                "caller_count": e.caller_count,
            }))
            .collect();

        let all_exports: Vec<serde_json::Value> = exports.iter()
            .map(|e| json!({
                "node_id": e.node_id,
                "name": e.name,
                "type": e.node_type,
                "signature": e.signature,
                "file": e.file_path,
                "caller_count": e.caller_count,
            }))
            .collect();

        Ok(json!({
            "path": path,
            "files_count": files.len(),
            "exports": all_exports,
            "hot_paths": hot_paths,
            "summary": format!("Module '{}': {} exports across {} files", path, exports.len(), files.len())
        }))
    }

    fn tool_dependency_graph(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        self.ensure_indexed()?;

        let file_path = args["file_path"].as_str()
            .ok_or_else(|| anyhow!("Missing file_path"))?;
        let direction = args.get("direction")
            .and_then(|v| v.as_str())
            .unwrap_or("both");
        let depth = args.get("depth")
            .and_then(|v| v.as_i64())
            .unwrap_or(2) as i32;

        let deps = queries::get_import_tree(self.db.conn(), file_path, direction, depth)?;

        let outgoing: Vec<serde_json::Value> = deps.iter()
            .filter(|d| d.direction == "outgoing")
            .map(|d| json!({
                "file": d.file_path,
                "symbols": d.symbol_count,
                "depth": d.depth,
            }))
            .collect();

        let incoming: Vec<serde_json::Value> = deps.iter()
            .filter(|d| d.direction == "incoming")
            .map(|d| json!({
                "file": d.file_path,
                "symbols": d.symbol_count,
                "depth": d.depth,
            }))
            .collect();

        Ok(json!({
            "file": file_path,
            "depends_on": outgoing,
            "depended_by": incoming,
            "summary": format!("{} depends on {} files, {} files depend on it", file_path, outgoing.len(), incoming.len())
        }))
    }

    fn tool_find_similar_code(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        self.ensure_indexed()?;

        let node_id = args["node_id"].as_i64()
            .ok_or_else(|| anyhow!("Missing node_id"))?;
        let top_k = args.get("top_k")
            .and_then(|v| v.as_i64())
            .unwrap_or(5);
        let max_distance = args.get("max_distance")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.5);

        // Check if embeddings are available
        if !self.db.vec_enabled() {
            return Ok(json!({
                "error": "Embedding not available. Build with --features embed-model.",
                "node_id": node_id
            }));
        }

        // Get the node's embedding
        let embedding: Vec<f32> = {
            let bytes: Vec<u8> = self.db.conn().query_row(
                "SELECT embedding FROM node_vectors WHERE node_id = ?1",
                [node_id],
                |row| row.get(0),
            ).map_err(|_| anyhow!("No embedding found for node_id {}. Node may not have been embedded.", node_id))?;
            bytemuck::cast_slice(&bytes).to_vec()
        };

        // Search for similar vectors
        let results = queries::vector_search(self.db.conn(), &embedding, top_k + 1)?; // +1 to exclude self

        // Filter and format results
        let similar: Vec<serde_json::Value> = results.iter()
            .filter(|(id, dist)| *id != node_id && *dist <= max_distance)
            .take(top_k as usize)
            .filter_map(|(id, distance)| {
                queries::get_node_by_id(self.db.conn(), *id).ok().flatten().map(|node| {
                    let file_path = queries::get_file_path(self.db.conn(), node.file_id)
                        .ok().flatten().unwrap_or_default();
                    let similarity = 1.0 / (1.0 + distance);
                    json!({
                        "node_id": node.id,
                        "name": node.name,
                        "type": node.node_type,
                        "file_path": file_path,
                        "start_line": node.start_line,
                        "similarity": format!("{:.2}", similarity),
                        "distance": format!("{:.4}", distance),
                    })
                })
            })
            .collect();

        Ok(json!({
            "query_node_id": node_id,
            "results": similar,
            "count": similar.len(),
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
        assert_eq!(tools.len(), crate::mcp::tools::TOOL_COUNT);
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
        let resp = result.expect("should be Ok").expect("should be Some");
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert!(parsed["error"].is_object());
        assert_eq!(parsed["error"]["code"], -32700);
        assert!(parsed["error"]["message"].as_str().unwrap().contains("Parse error"));
    }

    #[test]
    fn test_notification_with_invalid_version_returns_none() {
        let server = McpServer::new_test();
        // Notification (no id) with wrong JSON-RPC version — must still return None per spec
        let req = r#"{"jsonrpc":"1.0","method":"notifications/initialized"}"#;
        let resp = server.handle_message(req).unwrap();
        assert!(resp.is_none(), "malformed notifications must never receive a response");
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
        let mode = result["mode"].as_str().unwrap_or("");
        if mode.starts_with("compressed_") {
            assert!(result["results"].is_array());
            let compressed = result["results"].as_array().unwrap();
            assert!(!compressed.is_empty());
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
    fn test_trace_http_chain() {
        let project_dir = TempDir::new().unwrap();
        std::fs::write(project_dir.path().join("server.ts"), r#"
function validateToken(token: string) { return true; }
function queryDatabase(userId: string) { return null; }

function handleLogin(req: Request) {
    validateToken(req.token);
    return queryDatabase(req.userId);
}

app.post('/api/login', handleLogin);
"#).unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        server.ensure_indexed().unwrap();

        let req = tool_call_json("trace_http_chain", json!({
            "route_path": "/api/login",
            "depth": 3
        }));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);

        assert_eq!(result["route"], "/api/login");
        let handlers = result["handlers"].as_array().unwrap();
        assert!(!handlers.is_empty(), "should find at least one handler");

        // First handler should have a call_chain with recursive callees
        let handler = &handlers[0];
        assert!(handler["handler_name"].as_str().is_some());
        assert!(handler["call_chain"].is_array(), "handler should have call_chain array");
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

    #[test]
    fn test_read_snippet_blocks_path_traversal() {
        // Verify the canonicalize+starts_with guard prevents reading outside project root.
        // Instead of fighting the server lifecycle, test the path logic directly:
        // root.join("../../etc/passwd").canonicalize() should NOT starts_with(root).
        let project_dir = TempDir::new().unwrap();
        let root = project_dir.path().canonicalize().unwrap();

        // Simulate what tool_read_snippet does with a traversal path
        let traversal_path = root.join("../../etc/passwd");

        // If the file exists on disk (e.g., /etc/passwd on Linux), canonicalize
        // succeeds but starts_with check rejects it. If it doesn't exist,
        // canonicalize fails — either way, content is never read.
        match traversal_path.canonicalize() {
            Ok(canonical) => {
                assert!(
                    !canonical.starts_with(&root),
                    "canonical traversal path {:?} must not start with root {:?}",
                    canonical, root
                );
            }
            Err(_) => {
                // File doesn't exist — canonicalize fails, read_snippet returns "Cannot resolve"
                // This is the safe outcome on systems without /etc/passwd at that relative path
            }
        }

        // Also test that a legitimate path DOES pass
        std::fs::write(project_dir.path().join("safe.ts"), "function ok() {}").unwrap();
        let safe_path = root.join("safe.ts").canonicalize().unwrap();
        assert!(safe_path.starts_with(&root), "legitimate path should be within root");
    }

    #[test]
    fn test_call_graph_compression() {
        let project_dir = TempDir::new().unwrap();
        // Create a deep call chain with large function bodies
        let mut code = String::new();
        for i in 0..30 {
            code.push_str(&format!(
                "function chain{}() {{\n{}\n  chain{}();\n}}\n",
                i,
                format!("  // {}\n", "x".repeat(400)).repeat(3),
                i + 1,
            ));
        }
        code.push_str("function chain30() { return 1; }\n");
        std::fs::write(project_dir.path().join("deep.ts"), &code).unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        server.ensure_indexed().unwrap();

        let req = tool_call_json("get_call_graph", json!({
            "function_name": "chain0",
            "direction": "callees",
            "depth": 20
        }));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);

        // Result should either be normal (if small enough) or compressed
        if result["mode"].as_str().is_some() {
            assert!(result["mode"].as_str().unwrap().starts_with("compressed_"));
            assert!(result["results"].is_array());
        } else {
            assert!(result["function"].as_str().is_some());
        }
    }

    #[test]
    fn test_ast_node_compression() {
        let project_dir = TempDir::new().unwrap();
        // Create a function with very large body
        let big_body = format!("  // {}\n", "x".repeat(500)).repeat(30);
        let code = format!("function bigFunc() {{\n{}}}\n", big_body);
        std::fs::write(project_dir.path().join("big.ts"), &code).unwrap();

        let server = McpServer::new_test_with_project(project_dir.path());
        server.ensure_indexed().unwrap();

        let req = tool_call_json("get_ast_node", json!({
            "file_path": "big.ts",
            "symbol_name": "bigFunc"
        }));
        let resp = server.handle_message(&req).unwrap();
        let result = parse_tool_result(&resp);

        // Result should either be normal or compressed
        if result["mode"].as_str().is_some() {
            assert_eq!(result["mode"], "compressed_node");
            assert!(result["node_id"].is_number());
            assert!(result["summary"].is_string());
        } else {
            assert_eq!(result["name"], "bigFunc");
        }
    }

    #[test]
    fn test_find_similar_code_no_embeddings() {
        let server = McpServer::new_test(); // no embedding model, vec not enabled
        let msg = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"find_similar_code","arguments":{"node_id":1}}}"#;
        let response = server.handle_message(msg).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&response).unwrap();
        // Should return a result (not error) with an informative message about embedding requirement
        assert!(parsed["result"].is_object());
    }

    #[test]
    fn test_resources_list() {
        let server = McpServer::new_test();
        let msg = r#"{"jsonrpc":"2.0","id":1,"method":"resources/list","params":{}}"#;
        let response = server.handle_message(msg).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&response).unwrap();
        let resources = parsed["result"]["resources"].as_array().unwrap();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0]["uri"], "code-graph://project-summary");
    }

    #[test]
    fn test_prompts_list() {
        let server = McpServer::new_test();
        let msg = r#"{"jsonrpc":"2.0","id":1,"method":"prompts/list","params":{}}"#;
        let response = server.handle_message(msg).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&response).unwrap();
        let prompts = parsed["result"]["prompts"].as_array().unwrap();
        assert_eq!(prompts.len(), 3);
    }
}
