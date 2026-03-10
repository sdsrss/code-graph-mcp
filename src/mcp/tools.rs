use super::types::ToolDefinition;
use serde_json::json;

pub struct ToolRegistry {
    tools: Vec<ToolDefinition>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        let tools = vec![
            ToolDefinition {
                name: "semantic_code_search".into(),
                description: "Semantic code search. Hybrid BM25 full-text + vector semantic + graph relations, returns most relevant AST nodes.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Natural language search query" },
                        "top_k": { "type": "number", "description": "Number of results to return (default 5)" },
                        "language": { "type": "string", "description": "Filter by language" },
                        "node_type": { "type": "string", "description": "Filter by node type" }
                    },
                    "required": ["query"]
                }),
            },
            ToolDefinition {
                name: "get_call_graph".into(),
                description: "Query upstream/downstream call chain for a function. Recursive CTE traversal of knowledge graph with cycle detection.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "function_name": { "type": "string", "description": "Function name to trace" },
                        "direction": { "type": "string", "enum": ["callers", "callees", "both"], "description": "Direction (default 'both')" },
                        "depth": { "type": "number", "description": "Max recursion depth (default 2)" },
                        "file_path": { "type": "string", "description": "Disambiguate same-name functions" }
                    },
                    "required": ["function_name"]
                }),
            },
            ToolDefinition {
                name: "find_http_route".into(),
                description: "Trace from route path to backend handler function.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "route_path": { "type": "string", "description": "Route path e.g. '/api/users' or 'POST /api/login'" },
                        "include_middleware": { "type": "boolean", "description": "Include middleware chain (default true)" }
                    },
                    "required": ["route_path"]
                }),
            },
            ToolDefinition {
                name: "get_ast_node".into(),
                description: "Extract a specific code symbol from a file by name.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string", "description": "File path" },
                        "symbol_name": { "type": "string", "description": "Symbol name" },
                        "include_references": { "type": "boolean", "description": "Include references (default false)" }
                    },
                    "required": ["file_path", "symbol_name"]
                }),
            },
            ToolDefinition {
                name: "read_snippet".into(),
                description: "Read original code snippet by node_id. Pairs with semantic_code_search Context Sandbox.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "number", "description": "Node ID from nodes table" },
                        "context_lines": { "type": "number", "description": "Lines of surrounding context (default 3)" }
                    },
                    "required": ["node_id"]
                }),
            },
            ToolDefinition {
                name: "start_watch".into(),
                description: "Start file system real-time watcher for incremental indexing.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                }),
            },
            ToolDefinition {
                name: "stop_watch".into(),
                description: "Stop file system real-time watcher.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                }),
            },
            ToolDefinition {
                name: "get_index_status".into(),
                description: "Query index status and health information.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                }),
            },
            ToolDefinition {
                name: "rebuild_index".into(),
                description: "Force full index rebuild. Deletes and recreates .code-graph/index.db.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "confirm": { "type": "boolean", "description": "Must be true to confirm rebuild" }
                    },
                    "required": ["confirm"]
                }),
            },
        ];

        Self { tools }
    }

    pub fn list_tools(&self) -> &[ToolDefinition] {
        &self.tools
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_registry_lists_all_tools() {
        let registry = ToolRegistry::new();
        let tools = registry.list_tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"semantic_code_search"));
        assert!(names.contains(&"get_call_graph"));
        assert!(names.contains(&"find_http_route"));
        assert!(names.contains(&"get_ast_node"));
        assert!(names.contains(&"read_snippet"));
        assert!(names.contains(&"start_watch"));
        assert!(names.contains(&"stop_watch"));
        assert!(names.contains(&"get_index_status"));
        assert!(names.contains(&"rebuild_index"));
        assert_eq!(tools.len(), 9);
    }

    #[test]
    fn test_tool_schema_has_description() {
        let registry = ToolRegistry::new();
        let tools = registry.list_tools();
        for tool in tools {
            assert!(!tool.description.is_empty(), "Tool {} has no description", tool.name);
        }
    }
}
