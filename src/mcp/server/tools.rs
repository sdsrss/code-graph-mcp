//! Tool handlers split per family. Each child module adds an `impl McpServer`
//! block that contributes the `tool_*` method called by `handle_tool` in
//! `super::mod`. The dispatcher itself stays in `super::mod` next to the
//! JSON-RPC plumbing.
//!
//! v0.18.4 split: previously one 2354-line file. The split is mechanical (no
//! semantics changed) and bisectable — the matching commit "refactor(mcp):
//! split server/tools.rs into per-tool modules" is the diff target if you're
//! cherry-picking history.

mod advanced;
mod ast_node;
mod ast_search;
mod callgraph;
mod management;
mod overview;
mod project_map;
mod refs;
mod search;

/// Normalize a caller-supplied `file_path`/`path` tool argument to the `/`
/// spelling the index stores, at TOOL ENTRY — before the value is used either
/// as a freshness target or as an index lookup key.
///
/// Why entry and not inside the freshness helper: `ensure_file_fresh_opt`
/// normalizes internally, but its normalized value is local — it returns
/// `Result<()>`, so every caller went on to hand the RAW argument to
/// `get_nodes_by_file_path` / `get_call_graph_filtered` / `get_module_exports`.
/// An MCP client on Windows that echoes back `src\Foo.cs` therefore refreshed
/// the right file and then missed the index (which stores `src/Foo.cs`),
/// reporting `File 'src\Foo.cs' not found in index` for an indexed file — the
/// issue-#34 failure mode, half-fixed. Normalizing at entry also covers the
/// `should_skip_indexing` branch, where the freshness helper never runs at all.
///
/// MCP paths are root-relative by contract, so this is separator normalization
/// only — deliberately NOT `cli::normalize_user_path`, which additionally
/// resolves against the process cwd (see `indexer::pipeline` docs).
pub(super) fn normalize_path_arg(raw: &str) -> String {
    normalize_path_arg_on(raw, cfg!(windows))
}

/// Testable core of [`normalize_path_arg`]. `backslash_is_sep` is a parameter for
/// the same reason it is one in `merkle::normalize_rel_str_on` and
/// `cli::normalize_user_path_from_on`: without it the Windows branch of the MCP
/// entry point is reachable only from the `windows-latest` CI leg, and the audit
/// that found `find_dead_code` missing its normalization also found this — every
/// defect in this family so far has been pure string logic that a Linux leg could
/// have caught if anything had been able to call it.
pub(super) fn normalize_path_arg_on(raw: &str, backslash_is_sep: bool) -> String {
    crate::indexer::merkle::normalize_rel_str_on(raw, backslash_is_sep)
}

#[cfg(test)]
mod normalize_path_arg_tests {
    use super::normalize_path_arg_on;

    /// The MCP entry contract, asserted for BOTH platforms from any host.
    #[test]
    fn normalizes_windows_separators_only_where_backslash_is_one() {
        assert_eq!(
            normalize_path_arg_on(r"src\parser\mod.rs", true),
            "src/parser/mod.rs"
        );
        assert_eq!(normalize_path_arg_on("src//a.ts", true), "src/a.ts");
        assert_eq!(normalize_path_arg_on("src/a.ts", true), "src/a.ts");
        // On Unix `\` is a legal filename byte — rewriting it would build a key
        // that misses the indexed file, which is issue #34 in reverse.
        assert_eq!(
            normalize_path_arg_on(r"src/od\bc.rs", false),
            r"src/od\bc.rs"
        );
        assert_eq!(normalize_path_arg_on("src//a.ts", false), "src/a.ts");
    }
}
