use super::types::ToolDefinition;
use crate::mcp::server::helpers::count_range_hint;
use serde_json::json;

/// Expected tool count — update this when adding/removing tools.
///
/// v0.18.4 fold: 5 niche tools (impact_analysis / dependency_graph /
/// find_similar_code / find_dead_code / trace_http_chain) collapsed into
/// flags on the core 7 — `get_ast_node include_similar / include_impact`,
/// `module_overview include_deps / include_dead`, `get_call_graph route_path`.
/// `impact_analysis` was since removed (the lone orphan — no advertised tool
/// delegated to it; full impact is CLI `impact --json`, compact via `get_ast_node
/// include_impact`). The other four standalone names still dispatch as the live
/// backends for their folded flags; CLI subcommands
/// (`code-graph-mcp impact|deps|similar|dead-code|trace`) keep the
/// out-of-MCP path open for Bash workflows.
///
/// Management tools (start_watch, stop_watch, get_index_status, rebuild_index)
/// are still callable via tools/call but hidden from tools/list to save tokens.
/// Legacy alias `read_snippet → get_ast_node` remains callable for backward
/// compatibility (it was always a same-shape rename, never a hidden tool).
pub const TOOL_COUNT: usize = 7;

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
                        "top_k": { "type": "number", "description": format!("Results count (default 20, {}). Alias: limit", count_range_hint("semantic_code_search", "top_k")) },
                        "limit": { "type": "number", "description": format!("Alias for top_k (default 20, {})", count_range_hint("semantic_code_search", "limit")) },
                        "language": { "type": "string", "description": "Filter by language" },
                        "node_type": { "type": "string", "description": "Filter by node type" },
                        "compact": { "type": "boolean", "description": "Compact mode: signature+location only, no code (saves tokens)" }
                    },
                    "required": ["query"]
                }),
            },
            ToolDefinition {
                name: "get_call_graph".into(),
                // No "(folds the old trace_http_chain)" here: `tools/list` never
                // advertises that name, so it is a call the client cannot offer.
                // Guarded by `tool_descriptions_and_instructions_name_no_unlisted_tool`.
                description: "Multi-hop call chain. Replaces N rounds of `grep \"X(\"` + Read. Pass route_path='GET /api/x' to trace HTTP handler → downstream.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "symbol_name": { "type": "string", "description": "Function/method name. A call must carry symbol_name or route_path (mutually exclusive); neither is refused." },
                        "route_path": { "type": "string", "description": "HTTP route like 'GET /api/users' — traces from matched route handler(s) down. Mutually exclusive with symbol_name." },
                        "direction": { "type": "string", "enum": ["callers", "callees", "both"], "description": "Direction (default 'both'); ignored when route_path is set (always 'callees')" },
                        "depth": { "type": "number", "description": format!("Max depth (default 3, {}); a larger value is clamped and the response says so in clamped_arguments", count_range_hint("get_call_graph", "depth")) },
                        "file_path": { "type": "string", "description": "Disambiguate same-name functions" },
                        "include_middleware": { "type": "boolean", "description": "For route_path mode: include downstream middleware/calls (default true)" },
                        "compact": { "type": "boolean", "description": "Compact mode: name+file+depth only (saves tokens)" },
                        "include_tests": { "type": "boolean", "description": "Include test callers (default false)" },
                        "min_confidence": { "type": "string", "enum": ["extracted", "inferred", "ambiguous"], "description": "Min edge confidence to FOLLOW (default 'inferred'): hides 'ambiguous' by-name fan-out — a method name shared by many defs that resolves to all of them (e.g. `.execute()` → every execute). Pass 'ambiguous' to include every edge; 'extracted' for same-file-precise only. `ambiguous_edges_hidden` in the response counts what was suppressed." }
                    },
                    // `callgraph.rs:95` rejects a call carrying neither
                    // `symbol_name` nor `route_path`, and the schema cannot say
                    // so — see `no_tool_publishes_an_anyof_the_client_drops`.
                    "required": []
                }),
            },
            ToolDefinition {
                name: "get_ast_node".into(),
                description: "ONE named symbol: signature + source + opt impact/refs/similar. Use BEFORE editing X to see signature + blast radius. Repo-wide index (LSP only handles open files).".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string", "description": "File path (with symbol_name)" },
                        "symbol_name": { "type": "string", "description": "Symbol name (with file_path, or alone for auto-resolve). A call must carry symbol_name or node_id — file_path alone names a file, not a symbol, and is refused." },
                        "node_id": { "type": "number", "description": "Node ID (alternative to file_path+symbol_name)" },
                        "include_references": { "type": "boolean", "description": "Include callers/callees (default false)" },
                        "include_tests": { "type": "boolean", "description": "Include test callers in references (default false)" },
                        "include_impact": { "type": "boolean", "description": "Include impact summary: risk level, caller count, affected files/routes (default false)" },
                        "min_confidence": { "type": "string", "enum": ["extracted", "inferred", "ambiguous"], "description": "For include_impact: min caller-edge confidence counted toward risk (default 'inferred'). Folds the ambiguous by-name fan-out (a name shared by many defs) out of the blast radius; pass 'ambiguous' to count every resolved caller. `impact.ambiguous_callers_excluded` discloses what was folded." },
                        "include_similar": { "type": "boolean", "description": "Include embedding-similar nodes (default false; requires embed-model + indexed embeddings)" },
                        "similar_top_k": { "type": "number", "description": format!("With include_similar: max similar results (default 5, {})", count_range_hint("get_ast_node", "similar_top_k")) },
                        "context_lines": { "type": "number", "description": format!("Surrounding source lines to include (default 0, default 3 when using node_id; {})", count_range_hint("get_ast_node", "context_lines")) },
                        "compact": { "type": "boolean", "description": "Compact mode: type+signature+location only, no code_content (saves tokens)" }
                    },
                    // `ast_node.rs:200` rejects a call carrying neither
                    // `symbol_name` nor `node_id` (a bare `file_path` is not
                    // enough — it names a file, not a symbol).
                    "required": []
                }),
            },
            ToolDefinition {
                name: "project_map".into(),
                // "SessionStart already injected" was false for most installs and
                // told the model it ALREADY had this map — steering it off a call
                // it should make. The SessionStart project_map dump has been OFF
                // by default since v0.17.0 (session-init.js `quietHooks`; opt in
                // with CODE_GRAPH_VERBOSE_HOOKS=1, and even then only for adopted
                // projects), and the shipped detail doc says so in as many words,
                // so the two steering surfaces contradicted each other (audit
                // 2026-08-29 DOC-01). Stated as a positive cue rather than a
                // "don't call this unless…" — negative steering measured 20pp
                // WORSE in this repo's own routing bench.
                description: "Architecture map (modules / deps / hot fns; include_centrality=chokepoints). Replaces Glob+Read of N top-level files. Use when orienting in an unfamiliar repo or after a major refactor.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        // No figure in the description itself, matching the five
                        // sibling `compact` descriptions in this file. The old
                        // "~50%" was never true of any implementation, and the
                        // note that replaced it (4305 B of 5445 B) was measured
                        // when this repo had 15 modules — a baked-in ratio goes
                        // stale on the next commit, let alone the next repo.
                        //
                        // Re-measured 2026-09-01 (35 modules): full envelope
                        // 11197 B, compact 9730 B — 13.1% saved. It was 8799 B
                        // (21.4%) until compact stopped dropping
                        // `entry_points.route` and `module_dependencies.imports`;
                        // restoring those two cost 931 B. That trade is
                        // deliberate: a map without the URLs is a wrong answer to
                        // "what is the HTTP surface", not a terse one. What
                        // compact still buys is `languages` and the 15→10
                        // hot_functions trim (disclosed via
                        // `hot_functions_truncated`); key_symbols stays so the
                        // map is discoverable without a second round-trip.
                        "compact": { "type": "boolean", "description": "Compact mode: paths+counts+key_symbols, trimmed hot_functions (saves tokens)" },
                        "include_centrality": { "type": "boolean", "description": "Include architectural chokepoints (betweenness centrality — functions on the most shortest call paths; high score = structural bridge). Default false." },
                        "centrality_limit": { "type": "number", "description": format!("With include_centrality: max ranked results (default 10, {})", count_range_hint("project_map", "centrality_limit")) }
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
                        "deps_depth": { "type": "number", "description": format!("With include_deps: max transitive depth (default 2, {})", count_range_hint("module_overview", "deps_depth")) },
                        "include_dead": { "type": "boolean", "description": "Include unreferenced symbols (orphans + exported-unused) under this path (default false). Macro/shell-invoked entry points are pre-filtered. Results are candidates to verify: receiver-method calls (obj.method()) and cross-file const/type uses are not edge-tracked, so a flagged symbol may still be used." },
                        "dead_min_lines": { "type": "number", "description": "With include_dead: min line count to flag (default 3)" }
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
                        "query": { "type": "string", "description": "Search text. A call must carry query or at least one of type / returns / params; a bare limit is refused." },
                        // Built from the shared vocabulary, not hand-listed: this
                        // copy advertised `module` to the model long after every
                        // surface began dropping module placeholders, so the tool
                        // published a filter value that could only ever return
                        // zero rows. `interface` stays named as the alias for
                        // `trait` (both normalize to the same pair).
                        "type": { "type": "string", "description": format!("Node type: {} (interface = trait)", crate::domain::TYPE_FILTER_HELP) },
                        "returns": { "type": "string", "description": "Return type substring filter" },
                        "params": { "type": "string", "description": "Parameter text substring filter" },
                        "limit": { "type": "number", "description": format!("Max results (default 20, {})", count_range_hint("ast_search", "limit")) }
                    },
                    // `ast_search.rs:46`: a query OR at least one typed
                    // filter. A bare `limit` is not a search.
                    "required": []
                }),
            },
            ToolDefinition {
                name: "find_references".into(),
                description: "Rename/remove audits — every site that imports/inherits/implements/calls a symbol. Repo-wide cross-language (LSP needs file open). Literals → Grep; 'who calls X?' → get_call_graph.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "symbol_name": { "type": "string", "description": "Symbol to find references for. A call must carry symbol_name or node_id; neither is refused." },
                        "node_id": { "type": "integer", "description": "Exact node from a prior suggestion — overrides symbol_name. Use to disambiguate same-name defs in one file." },
                        "file_path": { "type": "string", "description": "Disambiguate same-name symbols across files" },
                        "relation": { "type": "string", "enum": ["calls", "imports", "inherits", "implements", "references", "exports", "routes_to", "all"], "description": "Relation type filter (default 'all')" },
                        "include_tests": { "type": "boolean", "description": "Include references from test code (default true — tests are usage sites for rename audits). Set false to see production callers only." },
                        "min_confidence": { "type": "string", "enum": ["extracted", "inferred", "ambiguous"], "description": "Min edge confidence to KEEP. Default: no floor — every reference is returned, each tagged with its own `confidence` ('extracted' = same-file precise, 'inferred' = import-resolved, 'ambiguous' = by-name fan-out that may point at a same-named symbol elsewhere). Pass 'inferred' to drop the ambiguous tier; `confidence_filtered` in the response counts what was dropped." },
                        "compact": { "type": "boolean", "description": "Compact mode: name+file+relation+confidence+node_id only, no code or signature (saves tokens)" }
                    },
                    // `refs.rs:35` rejects a call carrying neither `symbol_name`
                    // nor `node_id`. This is the site audit 2026-08-29 CON-13
                    // named; that finding's "the six siblings all declare
                    // `required`" was off — FIVE do, `get_call_graph` had the
                    // identical omission, and two more declared `required: []`,
                    // which in JSON Schema says exactly as much as saying
                    // nothing.
                    //
                    // The accurate expression is `anyOf`, and it MEASURABLY
                    // cannot be published — see
                    // `no_tool_publishes_an_anyof_the_client_drops`. So the
                    // disjunction lives in the handler and in that guard, and
                    // the schema stays as honest as it is able to be: no
                    // argument is unconditionally required, which is true.
                    "required": []
                }),
            },
        ];

        debug_assert_eq!(
            tools.len(),
            TOOL_COUNT,
            "TOOL_COUNT ({}) does not match actual tool count ({}). Update TOOL_COUNT in tools.rs.",
            TOOL_COUNT,
            tools.len()
        );
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
        let names: Vec<&str> = registry
            .list_tools()
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        for expected in [
            "semantic_code_search",
            "get_call_graph",
            "get_ast_node",
            "project_map",
            "module_overview",
            "ast_search",
            "find_references",
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
        assert_eq!(
            live, expected,
            "tools/list must equal domain::LIVE_MCP_TOOLS"
        );
    }

    /// No tool may publish `anyOf` — MEASURED, not stylistic.
    ///
    /// Four tools enforce "one of these arguments" in their handler
    /// (audit 2026-08-29 CON-13), and `anyOf` is the accurate JSON Schema for
    /// that. It was published for one build, and the client DROPPED exactly
    /// those four tools while keeping the three without it: same server, same
    /// reconnect, and `tools/list` over stdio still carried all seven. So the
    /// schema reached the client and the client refused the tools — silently,
    /// which is the worst available failure for a tool surface (the model
    /// simply stops having them, with nothing to read).
    ///
    /// `"required": []` is therefore as honest as this schema can be: it says
    /// no single argument is unconditionally required, which is true. The
    /// disjunction itself lives in the handler, and
    /// `tools_list_shape_matches_what_each_handler_enforces` in
    /// tests/integration.rs pins that the handler still enforces it.
    ///
    /// If you are here because you want to express the constraint properly:
    /// re-run the experiment before assuming the client has changed. Publish
    /// it, reconnect, and check whether the tool is still callable.
    #[test]
    fn no_tool_publishes_an_anyof_the_client_drops() {
        let registry = ToolRegistry::new();
        let mut checked = 0usize;
        for tool in registry.list_tools() {
            let name = tool.name.as_str();
            assert!(
                tool.input_schema.get("anyOf").is_none(),
                "tool '{name}' publishes `anyOf`. Measured 2026-09-02: the client \
                 drops every tool whose schema carries it, so shipping this makes \
                 '{name}' vanish from the model's tool set with no error anywhere."
            );
            // The drift axis the CON-13 finding actually named: `required` was
            // present on five of seven tools and absent on two, for no reason.
            // An empty array and an absent key mean the same thing to a
            // validator, but not to the next author reading the file.
            assert!(
                tool.input_schema.get("required").is_some(),
                "tool '{name}' declares no `required` array; every sibling does"
            );
            checked += 1;
        }
        assert_eq!(
            checked,
            crate::domain::LIVE_MCP_TOOLS.len(),
            "vacuity floor: every listed tool must have been examined"
        );
    }

    #[test]
    fn test_descriptions_are_concise() {
        let registry = ToolRegistry::new();
        for tool in registry.list_tools() {
            assert!(
                tool.description.len() <= 200,
                "Tool {} description too long ({} chars): '{}'",
                tool.name,
                tool.description.len(),
                tool.description
            );
        }
    }
}
