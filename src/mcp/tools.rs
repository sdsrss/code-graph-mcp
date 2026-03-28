use super::types::ToolDefinition;
use serde_json::json;

/// Expected tool count — update this when adding/removing tools.
/// Management tools (start_watch, stop_watch, get_index_status, rebuild_index)
/// are still callable via tools/call but hidden from tools/list to save tokens.
/// Merged tools (find_http_route → trace_http_chain, read_snippet → get_ast_node)
/// remain callable as aliases for backward compatibility.
pub const TOOL_COUNT: usize = 12;

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
                description: "Search code by concept, not exact text. Use when: you know what code does but not its name, or grep returns noise. Returns AST nodes ranked by relevance.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search query" },
                        "top_k": { "type": "number", "description": "Results count (default 20). Alias: limit" },
                        "limit": { "type": "number", "description": "Alias for top_k" },
                        "language": { "type": "string", "description": "Filter by language" },
                        "node_type": { "type": "string", "description": "Filter by node type" },
                        "compact": { "type": "boolean", "description": "Compact mode: signature+location only, no code (saves tokens)" }
                    },
                    "required": ["query"]
                }),
            },
            ToolDefinition {
                name: "get_call_graph".into(),
                description: "Call chain for a function. Use when: tracing who calls it / what it calls, understanding flow before modifying. Recursive with depth tracking.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "symbol_name": { "type": "string", "description": "Function/method name" },
                        "direction": { "type": "string", "enum": ["callers", "callees", "both"], "description": "Direction (default 'both')" },
                        "depth": { "type": "number", "description": "Max depth (default 3)" },
                        "file_path": { "type": "string", "description": "Disambiguate same-name functions" },
                        "compact": { "type": "boolean", "description": "Compact mode: name+file+depth only (saves tokens)" },
                        "include_tests": { "type": "boolean", "description": "Include test callers (default false)" }
                    },
                    "required": ["symbol_name"]
                }),
            },
            ToolDefinition {
                name: "trace_http_chain".into(),
                description: "Trace HTTP route to handler and downstream calls. Use when: debugging API endpoints or finding which handler serves a route. depth=1 for handler only.".into(),
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
                description: "Get symbol details: type, signature, code, references, impact. Use when: inspecting a function/class before editing it. Accepts symbol_name, node_id, or file_path+symbol_name.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string", "description": "File path (with symbol_name)" },
                        "symbol_name": { "type": "string", "description": "Symbol name (with file_path, or alone for auto-resolve)" },
                        "node_id": { "type": "number", "description": "Node ID (alternative to file_path+symbol_name)" },
                        "include_references": { "type": "boolean", "description": "Include callers/callees (default false)" },
                        "include_tests": { "type": "boolean", "description": "Include test callers in references (default false)" },
                        "include_impact": { "type": "boolean", "description": "Include impact summary: risk level, caller count, affected files/routes (default false)" },
                        "context_lines": { "type": "number", "description": "Surrounding source lines to include (default 0, default 3 when using node_id)" },
                        "compact": { "type": "boolean", "description": "Compact mode: type+signature+location only, no code_content (saves tokens)" }
                    },
                    "required": []
                }),
            },
            ToolDefinition {
                name: "project_map".into(),
                description: "Project architecture map. Use when: starting work on unfamiliar code, finding which module owns functionality, or needing cross-module dependency overview.".into(),
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
                description: "Blast radius before modifying code. Use when: about to change/rename/remove a function — shows risk level, affected callers, routes, and files.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "symbol_name": { "type": "string", "description": "Symbol to analyze" },
                        "file_path": { "type": "string", "description": "Disambiguate same-name symbols" },
                        "change_type": { "type": "string", "enum": ["signature", "behavior", "remove"], "description": "Change type (default 'behavior')" },
                        "depth": { "type": "number", "description": "Max depth (default 3)" }
                    },
                    "required": ["symbol_name"]
                }),
            },
            ToolDefinition {
                name: "module_overview".into(),
                description: "Module structure and symbols. Use when: exploring a directory/module you haven't seen, or finding the right file to edit. Shows exports, hot paths, files.".into(),
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
                description: "File-level import/dependency map. Use when: understanding dependencies before splitting/moving files, checking import chains, or finding circular deps.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string", "description": "File to analyze" },
                        "direction": { "type": "string", "enum": ["outgoing", "incoming", "both"], "description": "Direction (default 'both')" },
                        "depth": { "type": "number", "description": "Max depth (default 2)" },
                        "compact": { "type": "boolean", "description": "Compact mode: paths+counts only, no symbol details (saves tokens)" }
                    },
                    "required": ["file_path"]
                }),
            },
            ToolDefinition {
                name: "find_similar_code".into(),
                description: "Find semantically similar functions. Use when: looking for duplicate logic to extract, consistent refactoring, or related patterns. Requires embeddings.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "symbol_name": { "type": "string", "description": "Symbol name (provide this OR node_id)" },
                        "node_id": { "type": "number", "description": "Node ID (provide this OR symbol_name)" },
                        "top_k": { "type": "number", "description": "Results count (default 5)" },
                        "max_distance": { "type": "number", "description": "Max distance (default 0.8)" }
                    },
                    "required": []
                }),
            },
            ToolDefinition {
                name: "ast_search".into(),
                description: "Structural code search by type/return/params. Use when: finding all functions returning a type, or querying code structure that grep can't express.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search text (optional if filters provided)" },
                        "type": { "type": "string", "description": "Node type: fn, class, struct, enum, interface, type, const, var, module" },
                        "returns": { "type": "string", "description": "Return type substring filter" },
                        "params": { "type": "string", "description": "Parameter text substring filter" },
                        "limit": { "type": "number", "description": "Max results (default 20)" }
                    },
                    "required": []
                }),
            },
            ToolDefinition {
                name: "find_references".into(),
                description: "All references to a symbol. Use when: checking if safe to rename/remove, or finding all usage points before refactoring. Shows callers, importers, inheritors.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "symbol_name": { "type": "string", "description": "Symbol to find references for" },
                        "file_path": { "type": "string", "description": "Disambiguate same-name symbols" },
                        "relation": { "type": "string", "enum": ["calls", "imports", "inherits", "implements", "all"], "description": "Relation type filter (default 'all')" },
                        "compact": { "type": "boolean", "description": "Compact mode: name+file+relation+node_id only, no code or signature (saves tokens)" }
                    },
                    "required": ["symbol_name"]
                }),
            },
            ToolDefinition {
                name: "find_dead_code".into(),
                description: "Find unused code (orphans, exported-unused). Use when: cleaning up codebase, finding safe-to-delete functions, or reviewing for unused exports.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Directory/file path filter (e.g. 'src/auth/')" },
                        "node_type": { "type": "string", "enum": ["fn", "class", "struct", "enum", "interface", "type", "const", "var"], "description": "Filter by node type" },
                        "include_tests": { "type": "boolean", "description": "Include test symbols (default false)" },
                        "min_lines": { "type": "integer", "description": "Minimum lines to report (default 3)" },
                        "compact": { "type": "boolean", "description": "Compact mode: name+file+line only, no code (default true)" }
                    },
                    "required": []
                }),
            },
        ];

        debug_assert_eq!(tools.len(), TOOL_COUNT,
            "TOOL_COUNT ({}) does not match actual tool count ({}). Update TOOL_COUNT in tools.rs.",
            TOOL_COUNT, tools.len());
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
            "ast_search", "find_references", "find_dead_code",
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
