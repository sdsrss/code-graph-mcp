use super::types::ToolDefinition;
use serde_json::json;

/// Expected tool count — update this when adding/removing tools.
/// Management tools (start_watch, stop_watch, get_index_status, rebuild_index)
/// are still callable via tools/call but hidden from tools/list to save tokens.
/// Merged tools (find_http_route → trace_http_chain, read_snippet → get_ast_node)
/// remain callable as aliases for backward compatibility.
pub const TOOL_COUNT: usize = 9;

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
                description: "Search code by meaning. Returns structured AST nodes (name, file, signature, type) ranked by relevance. Supports stemming.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search query" },
                        "top_k": { "type": "number", "description": "Results count (default 5)" },
                        "language": { "type": "string", "description": "Filter by language" },
                        "node_type": { "type": "string", "description": "Filter by node type" },
                        "compact": { "type": "boolean", "description": "Compact mode: signature+location only, no code (saves tokens)" }
                    },
                    "required": ["query"]
                }),
            },
            ToolDefinition {
                name: "get_call_graph".into(),
                description: "Caller/callee call chain with depth tracking. Static languages: high accuracy. Dynamic (JS/TS/Python): may have false edges.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "symbol_name": { "type": "string", "description": "Function/method name" },
                        "direction": { "type": "string", "enum": ["callers", "callees", "both"], "description": "Direction (default 'both')" },
                        "depth": { "type": "number", "description": "Max depth (default 2)" },
                        "file_path": { "type": "string", "description": "Disambiguate same-name functions" },
                        "compact": { "type": "boolean", "description": "Compact mode: name+file+depth only (saves tokens)" }
                    },
                    "required": ["symbol_name"]
                }),
            },
            ToolDefinition {
                name: "trace_http_chain".into(),
                description: "Trace HTTP route → handler → downstream calls. Also works as route finder (depth=1 for handler only).".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "route_path": { "type": "string", "description": "Route e.g. '/api/users' or 'POST /api/login'" },
                        "depth": { "type": "number", "description": "Call chain depth (default 3, use 1 for handler only)" },
                        "include_middleware": { "type": "boolean", "description": "Include middleware (default true)" }
                    },
                    "required": ["route_path"]
                }),
            },
            ToolDefinition {
                name: "get_ast_node".into(),
                description: "Get a symbol's type, signature, code, and callers/callees. Accepts file_path+symbol_name, node_id, or symbol_name alone (auto-resolves). Set context_lines to include surrounding source code.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string", "description": "File path (with symbol_name)" },
                        "symbol_name": { "type": "string", "description": "Symbol name (with file_path, or alone for auto-resolve)" },
                        "node_id": { "type": "number", "description": "Node ID (alternative to file_path+symbol_name)" },
                        "include_references": { "type": "boolean", "description": "Include callers/callees (default false)" },
                        "include_impact": { "type": "boolean", "description": "Include impact summary: risk level, caller count, affected files/routes (default false)" },
                        "context_lines": { "type": "number", "description": "Surrounding source lines to include (default 0, default 3 when using node_id)" }
                    },
                    "required": []
                }),
            },
            ToolDefinition {
                name: "project_map".into(),
                description: "Full project architecture: modules, cross-module dependencies, HTTP entry points, hot functions. Call first for overview.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "compact": { "type": "boolean", "description": "Compact mode: paths+counts+key_symbols, trimmed hot_functions (saves ~50% tokens)" }
                    },
                    "required": []
                }),
            },
            ToolDefinition {
                name: "impact_analysis".into(),
                description: "Blast radius of changing a symbol: affected callers, routes, files, and risk level. Call before modifying functions.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "symbol_name": { "type": "string", "description": "Symbol to analyze" },
                        "change_type": { "type": "string", "enum": ["signature", "behavior", "remove"], "description": "Change type (default 'behavior')" },
                        "depth": { "type": "number", "description": "Max depth (default 3)" }
                    },
                    "required": ["symbol_name"]
                }),
            },
            ToolDefinition {
                name: "module_overview".into(),
                description: "Module/file structure: exports, hot paths, file list, dependency summary.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File or directory path (e.g. 'src/auth/')" },
                        "compact": { "type": "boolean", "description": "Compact mode: name+type+callers only, no signatures (saves tokens)" }
                    },
                    "required": ["path"]
                }),
            },
            ToolDefinition {
                name: "dependency_graph".into(),
                description: "File-level import/export dependency map with recursive depth. Static languages: high accuracy. Dynamic: may have false edges.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string", "description": "File to analyze" },
                        "direction": { "type": "string", "enum": ["outgoing", "incoming", "both"], "description": "Direction (default 'both')" },
                        "depth": { "type": "number", "description": "Max depth (default 2)" }
                    },
                    "required": ["file_path"]
                }),
            },
            ToolDefinition {
                name: "find_similar_code".into(),
                description: "Find semantically similar code via embeddings. For duplicate detection and refactoring.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "symbol_name": { "type": "string", "description": "Symbol name (alternative to node_id)" },
                        "node_id": { "type": "number", "description": "Node ID (alternative to symbol_name)" },
                        "top_k": { "type": "number", "description": "Results count (default 5)" },
                        "max_distance": { "type": "number", "description": "Max distance (default 0.8)" }
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
    fn test_tool_count() {
        let registry = ToolRegistry::new();
        assert_eq!(registry.list_tools().len(), TOOL_COUNT);
    }

    #[test]
    fn test_tool_registry_has_all_tools() {
        let registry = ToolRegistry::new();
        let names: Vec<&str> = registry.list_tools().iter().map(|t| t.name.as_str()).collect();
        for expected in [
            "semantic_code_search", "get_call_graph", "trace_http_chain",
            "get_ast_node", "project_map", "impact_analysis",
            "module_overview", "dependency_graph", "find_similar_code",
        ] {
            assert!(names.contains(&expected), "missing tool: {}", expected);
        }
        // Merged tools should NOT be in the list
        assert!(!names.contains(&"find_http_route"));
        assert!(!names.contains(&"read_snippet"));
        // Management tools should NOT be in the list
        assert!(!names.contains(&"start_watch"));
        assert!(!names.contains(&"stop_watch"));
        assert!(!names.contains(&"get_index_status"));
        assert!(!names.contains(&"rebuild_index"));
    }

    #[test]
    fn test_descriptions_are_concise() {
        let registry = ToolRegistry::new();
        for tool in registry.list_tools() {
            assert!(tool.description.len() <= 200,
                "Tool {} description too long ({} chars): '{}'",
                tool.name, tool.description.len(), tool.description);
        }
    }
}
