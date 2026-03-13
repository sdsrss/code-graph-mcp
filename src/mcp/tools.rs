use super::types::ToolDefinition;
use serde_json::json;

/// Expected tool count — update this when adding/removing tools.
pub const TOOL_COUNT: usize = 14;

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
                description: "Search code by meaning, not just text matching. Returns structured AST nodes (name, file, signature, type, relations) ranked by semantic relevance. Delivers ~200 tokens of focused results vs ~3000 tokens from multiple Grep+Read calls. Use INSTEAD OF Grep when searching for concepts, patterns, or related code across the codebase.".into(),
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
                description: "Get the complete upstream/downstream call chain for any function in one call. Returns structured caller/callee trees with file locations and depth tracking. Replaces 5-10 rounds of Grep+Read to manually trace call relationships. Use INSTEAD OF Grep when you need to understand what calls a function or what a function calls.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "symbol_name": { "type": "string", "description": "Function/method name to trace" },
                        "direction": { "type": "string", "enum": ["callers", "callees", "both"], "description": "Direction (default 'both')" },
                        "depth": { "type": "number", "description": "Max recursion depth (default 2)" },
                        "file_path": { "type": "string", "description": "Disambiguate same-name functions" }
                    },
                    "required": ["symbol_name"]
                }),
            },
            ToolDefinition {
                name: "find_http_route".into(),
                description: "Find the backend handler function for an HTTP route path. Returns handler name, file, signature, and optionally the middleware chain. Use when you know the route (e.g. '/api/users') and need to find its implementation quickly.".into(),
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
                name: "trace_http_chain".into(),
                description: "Trace a complete HTTP request flow from route definition through handler to all downstream function calls, in a single call. Returns the full chain with middleware, handler, and nested callees. Use INSTEAD OF manually reading router files then handler files then service files one by one.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "route_path": { "type": "string", "description": "Route path e.g. '/api/users' or 'POST /api/login'" },
                        "depth": { "type": "number", "description": "Max call chain depth (default 3)" },
                        "include_middleware": { "type": "boolean", "description": "Include middleware chain (default true)" }
                    },
                    "required": ["route_path"]
                }),
            },
            ToolDefinition {
                name: "get_ast_node".into(),
                description: "Extract a specific symbol's metadata from a file: type, signature, qualified name, doc comment, and optionally all callers/callees. Returns ~100 tokens of structured data vs ~1000+ tokens from reading the entire file. Use INSTEAD OF Read when you only need one symbol's definition and relationships.".into(),
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
                description: "Expand a node_id into its full source code with surrounding context lines. Use after semantic_code_search or get_call_graph to read the actual code of specific results. Pairs with the structured results from other code-graph tools.".into(),
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
            ToolDefinition {
                name: "impact_analysis".into(),
                description: "Analyze the blast radius of changing a symbol. Returns all affected callers, routes, files, and a risk rating (LOW/MEDIUM/HIGH) in one call. Irreplaceable by Grep — computes transitive impact through the full call graph. Use BEFORE modifying any function to understand consequences.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "symbol_name": { "type": "string", "description": "Symbol name to analyze" },
                        "change_type": { "type": "string", "enum": ["signature", "behavior", "remove"], "description": "Type of change (default 'behavior')" },
                        "depth": { "type": "number", "description": "Max caller depth (default 3)" }
                    },
                    "required": ["symbol_name"]
                }),
            },
            ToolDefinition {
                name: "module_overview".into(),
                description: "Get a structured overview of a module or file: exports, dependencies, caller counts, and hot paths. Returns ~400 tokens of high-density insight vs ~5000 tokens from reading all files in the module. Use INSTEAD OF reading multiple files when you need to understand what a module does and how it connects.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File path or directory prefix (e.g. 'src/auth/' or 'src/auth/validator.ts')" }
                    },
                    "required": ["path"]
                }),
            },
            ToolDefinition {
                name: "dependency_graph".into(),
                description: "Map the import/export dependencies for any file. Shows what it depends on (outgoing imports) and what depends on it (incoming imports) at the file level. Use when understanding module boundaries, planning refactors, or checking for circular dependencies.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string", "description": "File path to analyze" },
                        "direction": { "type": "string", "enum": ["outgoing", "incoming", "both"], "description": "Direction (default 'both')" },
                        "depth": { "type": "number", "description": "Max depth (default 2)" }
                    },
                    "required": ["file_path"]
                }),
            },
            ToolDefinition {
                name: "find_similar_code".into(),
                description: "Find semantically similar code to a given function or class. Uses vector embeddings to find code with similar purpose even when naming differs. Requires embed-model feature. Use for finding duplicate logic, related implementations, or refactoring candidates.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "symbol_name": { "type": "string", "description": "Symbol name to find similar code for (alternative to node_id)" },
                        "node_id": { "type": "number", "description": "Node ID to find similar code for (alternative to symbol_name)" },
                        "top_k": { "type": "number", "description": "Number of results (default 5)" },
                        "max_distance": { "type": "number", "description": "Maximum distance threshold (default 0.5)" }
                    }
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
        assert!(names.contains(&"trace_http_chain"));
        assert!(names.contains(&"impact_analysis"));
        assert!(names.contains(&"module_overview"));
        assert!(names.contains(&"dependency_graph"));
        assert!(names.contains(&"find_similar_code"));
        assert_eq!(tools.len(), TOOL_COUNT);
    }

    #[test]
    fn test_tool_schema_has_description() {
        let registry = ToolRegistry::new();
        let tools = registry.list_tools();
        for tool in tools {
            assert!(!tool.description.is_empty(), "Tool {} has no description", tool.name);
        }
    }

    #[test]
    fn test_navigation_tools_have_competitive_descriptions() {
        let registry = ToolRegistry::new();
        let nav_tools = ["semantic_code_search", "get_call_graph", "trace_http_chain",
                         "find_http_route", "get_ast_node", "read_snippet"];
        for name in nav_tools {
            let tool = registry.list_tools().iter().find(|t| t.name == name)
                .unwrap_or_else(|| panic!("Tool {} not found", name));
            assert!(
                tool.description.contains("INSTEAD OF") || tool.description.contains("Use after") || tool.description.contains("Use when"),
                "Tool {} description lacks usage guidance: '{}'", name, tool.description
            );
        }
    }
}
