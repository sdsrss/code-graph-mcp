use super::types::ToolDefinition;
use serde_json::json;

/// Expected tool count — update this when adding/removing tools.
///
/// v0.18.4 fold: 5 niche tools (impact_analysis / dependency_graph /
/// find_similar_code / find_dead_code / trace_http_chain) collapsed into
/// flags on the core 7 — `get_ast_node include_similar / include_impact`,
/// `module_overview include_deps / include_dead`, `get_call_graph route_path`.
/// The standalone tool names no longer dispatch; CLI subcommands
/// (`code-graph-mcp impact|deps|similar|dead-code|trace`) keep the
/// out-of-MCP path open for Bash workflows.
///
/// Management tools (start_watch, stop_watch, get_index_status, rebuild_index)
/// are still callable via tools/call but hidden from tools/list to save tokens.
/// Legacy alias `read_snippet → get_ast_node` remains callable for backward
/// compatibility (it was always a same-shape rename, never a hidden tool).
pub const TOOL_COUNT: usize = 8;

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
                description: "Concept search (vector + FTS RRF). Use INSTEAD OF multi-round Grep when query is fuzzy / no exact symbol. Named symbol → get_ast_node; known module path → module_overview.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search query" },
                        "top_k": { "type": "number", "description": "Results count (default 20). Alias: limit" },
                        "limit": { "type": "number", "description": "Alias for top_k" },
                        "language": { "type": "string", "description": "Filter by language" },
                        "node_type": { "type": "string", "description": "Filter by node type" },
                        "compact": { "type": "boolean", "description": "Compact mode: signature+location only, no code (saves tokens)" },
                        "project": { "type": "string", "description": "Project alias (from CODE_GRAPH_PROJECTS). Omit for default project." }
                    },
                    "required": ["query"]
                }),
            },
            ToolDefinition {
                name: "get_call_graph".into(),
                description: "Multi-hop call chain. Replaces N rounds of `grep \"X(\"` + Read. Pass route_path='GET /api/x' to trace HTTP handler → downstream (folds the old trace_http_chain).".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "symbol_name": { "type": "string", "description": "Function/method name (mutually exclusive with route_path)" },
                        "route_path": { "type": "string", "description": "HTTP route like 'GET /api/users' — traces from matched route handler(s) down. Mutually exclusive with symbol_name." },
                        "direction": { "type": "string", "enum": ["callers", "callees", "both"], "description": "Direction (default 'both'); ignored when route_path is set (always 'callees')" },
                        "depth": { "type": "number", "description": "Max depth (default 3)" },
                        "file_path": { "type": "string", "description": "Disambiguate same-name functions" },
                        "include_middleware": { "type": "boolean", "description": "For route_path mode: include downstream middleware/calls (default true)" },
                        "compact": { "type": "boolean", "description": "Compact mode: name+file+depth only (saves tokens)" },
                        "include_tests": { "type": "boolean", "description": "Include test callers (default false)" },
                        "project": { "type": "string", "description": "Project alias (from CODE_GRAPH_PROJECTS). Omit for default project." }
                    }
                }),
            },
            ToolDefinition {
                name: "get_ast_node".into(),
                description: "ONE named symbol: signature + source + opt impact/refs/similar. Use BEFORE editing X to see signature + blast radius. Repo-wide index (LSP only handles open files).".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string", "description": "File path (with symbol_name)" },
                        "symbol_name": { "type": "string", "description": "Symbol name (with file_path, or alone for auto-resolve)" },
                        "node_id": { "type": "number", "description": "Node ID (alternative to file_path+symbol_name)" },
                        "include_references": { "type": "boolean", "description": "Include callers/callees (default false)" },
                        "include_tests": { "type": "boolean", "description": "Include test callers in references (default false)" },
                        "include_impact": { "type": "boolean", "description": "Include impact summary: risk level, caller count, affected files/routes (default false)" },
                        "include_similar": { "type": "boolean", "description": "Include embedding-similar nodes (default false; requires embed-model + indexed embeddings)" },
                        "similar_top_k": { "type": "number", "description": "With include_similar: max similar results (default 5)" },
                        "context_lines": { "type": "number", "description": "Surrounding source lines to include (default 0, default 3 when using node_id)" },
                        "compact": { "type": "boolean", "description": "Compact mode: type+signature+location only, no code_content (saves tokens)" },
                        "project": { "type": "string", "description": "Project alias (from CODE_GRAPH_PROJECTS). Omit for default project." }
                    },
                    "required": []
                }),
            },
            ToolDefinition {
                name: "project_map".into(),
                description: "Architecture map (modules / deps / hot fns). Replaces Glob+Read of N top-level files. SessionStart already injected; recall only after major refactor or rebuild-index.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "compact": { "type": "boolean", "description": "Compact mode: paths+counts+key_symbols, trimmed hot_functions (saves ~50% tokens)" },
                        "project": { "type": "string", "description": "Project alias (from CODE_GRAPH_PROJECTS). Omit for default project." }
                    },
                    "required": []
                }),
            },
            ToolDefinition {
                name: "module_overview".into(),
                description: "Symbols in a directory or file, grouped by type + caller count. Replaces Glob + Read×N for big dirs / huge files. Single file: include_deps=dep graph, include_dead=unreferenced.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File or directory path (e.g. 'src/auth/')" },
                        "compact": { "type": "boolean", "description": "Compact mode: name+type+callers only, no signatures (saves tokens)" },
                        "include_deps": { "type": "boolean", "description": "When path is a single file: include outgoing/incoming file dependencies (default false)" },
                        "deps_direction": { "type": "string", "enum": ["outgoing", "incoming", "both"], "description": "With include_deps: direction filter (default 'both')" },
                        "deps_depth": { "type": "number", "description": "With include_deps: max transitive depth (default 2)" },
                        "include_dead": { "type": "boolean", "description": "Include unreferenced symbols (orphans + exported-unused) under this path (default false). Macro/shell-invoked entry points are pre-filtered. Results are candidates to verify: receiver-method calls (obj.method()) and cross-file const/type uses are not edge-tracked, so a flagged symbol may still be used." },
                        "dead_min_lines": { "type": "number", "description": "With include_dead: min line count to flag (default 3)" },
                        "project": { "type": "string", "description": "Project alias (from CODE_GRAPH_PROJECTS). Omit for default project." }
                    },
                    "required": ["path"]
                }),
            },
            ToolDefinition {
                name: "ast_search".into(),
                description: "Enumerate symbols by typed filters (type/returns/params) Grep can't express. Use for 'all fns returning Result<T>' / 'all structs implementing X'. ONE known symbol → get_ast_node.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search text (optional if filters provided)" },
                        "type": { "type": "string", "description": "Node type: fn, class, struct, enum, interface, type, const, var, module" },
                        "returns": { "type": "string", "description": "Return type substring filter" },
                        "params": { "type": "string", "description": "Parameter text substring filter" },
                        "limit": { "type": "number", "description": "Max results (default 20)" },
                        "project": { "type": "string", "description": "Project alias (from CODE_GRAPH_PROJECTS). Omit for default project." }
                    },
                    "required": []
                }),
            },
            ToolDefinition {
                name: "find_references".into(),
                description: "Rename/remove audits — every site that imports/inherits/implements/calls a symbol. Repo-wide cross-language (LSP needs file open). Literals → Grep; 'who calls X?' → get_call_graph.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "symbol_name": { "type": "string", "description": "Symbol to find references for" },
                        "node_id": { "type": "integer", "description": "Exact node from a prior suggestion — overrides symbol_name. Use to disambiguate same-name defs in one file." },
                        "file_path": { "type": "string", "description": "Disambiguate same-name symbols across files" },
                        "relation": { "type": "string", "enum": ["calls", "imports", "inherits", "implements", "references", "all"], "description": "Relation type filter (default 'all')" },
                        "include_tests": { "type": "boolean", "description": "Include references from test code (default true — tests are usage sites for rename audits). Set false to see production callers only." },
                        "compact": { "type": "boolean", "description": "Compact mode: name+file+relation+node_id only, no code or signature (saves tokens)" },
                        "project": { "type": "string", "description": "Project alias (from CODE_GRAPH_PROJECTS). Omit for default project." }
                    }
                }),
            },
            ToolDefinition {
                name: "invariance_check".into(),
                description: "Cross-spoke invariance verification: type audit + SDK/litellm/proxy/test ratchets. Compact output. Use get_skill('invariance-tower') for full context.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": ["status", "audit", "ratchet", "drift"], "description": "status: dashboard (5 gates, pass/fail). audit: verify 22 invariant types exist at expected paths. ratchet: cross-ref gaps against SDK/litellm/proxy sources. drift: compare audit against previous snapshot. Default: status." },
                        "layer": { "type": "string", "enum": ["wire", "harness", "settings"], "description": "Scope audit to one invariance layer (only for action=audit)." },
                        "name": { "type": "string", "enum": ["sdk-types", "litellm-params", "proxy-models", "test-titles"], "description": "Run one specific ratchet (only for action=ratchet). Omit for all four." },
                        "top": { "type": "number", "description": "Max gap entries to return per ratchet (default 5, only for action=ratchet)." }
                    }
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
            "semantic_code_search", "get_call_graph",
            "get_ast_node", "project_map",
            "module_overview",
            "ast_search", "find_references",
            "invariance_check",
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
        // Niche tools hidden from tools/list (callable by name)
        assert!(!names.contains(&"trace_http_chain"));
        assert!(!names.contains(&"impact_analysis"));
        assert!(!names.contains(&"dependency_graph"));
        assert!(!names.contains(&"find_similar_code"));
        assert!(!names.contains(&"find_dead_code"));

        // Drift guard: the tools/list surface must equal domain::LIVE_MCP_TOOLS.
        // `cli::cmd_stats` uses that const to flag legacy/folded tool names in
        // usage.jsonl, so the two cannot be allowed to drift apart.
        let live: std::collections::BTreeSet<&str> = names.iter().copied().collect();
        let expected: std::collections::BTreeSet<&str> =
            crate::domain::LIVE_MCP_TOOLS.iter().copied().collect();
        assert_eq!(live, expected, "tools/list must equal domain::LIVE_MCP_TOOLS");
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
