use super::*;

/// Strip qualified name prefix (e.g. "McpServer.handle_message" -> "handle_message")
/// so users can copy-paste names from output and use them in lookups.
pub(crate) fn strip_qualified_prefix(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// CLI-side fuzzy name resolution — the shared implementation in
/// `crate::resolve`, so CLI `callgraph`/`refs` and the MCP tools cannot drift
/// into opposite answers for one input (audit 2026-06-03 #6; the hand-written
/// CLI copy this replaces was the same defect shape, and had zero tests).
pub(crate) use crate::resolve::FuzzyResolution as CliFuzzyResolution;

pub(crate) fn resolve_fuzzy_name_cli(
    conn: &rusqlite::Connection,
    name: &str,
) -> Result<CliFuzzyResolution> {
    crate::resolve::resolve_fuzzy(conn, name)
}

/// Emit the "ambiguous symbol" error in the same shape whether the command was
/// invoked with --json (one-line JSON) or default (human-readable stderr lines),
/// then exit(1). Shared by cmd_callgraph, cmd_impact when no file filter was
/// given and `crate::resolve::detect_ambiguity` returned candidates. The message
/// and JSON suggestion shape come from `crate::resolve` so the CLI and MCP give
/// identical verdicts on same-file overloads (audit 2026-06-03 #6).
pub(crate) fn emit_exact_ambiguity(
    symbol: &str,
    cands: &[queries::NameCandidate],
    json_mode: bool,
) -> ! {
    let message = crate::resolve::ambiguity_message(symbol, cands, crate::resolve::Surface::Cli);
    if json_mode {
        let sugg: Vec<serde_json::Value> = crate::resolve::candidates_to_json(cands)
            .into_iter()
            .take(5)
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "error": message,
                "suggestions": sugg,
            })
        );
    } else {
        eprintln!("[code-graph] {}", message);
        for c in cands.iter().take(5) {
            eprintln!(
                "  {} ({}) in {} [node_id {}]",
                c.name, c.node_type, c.file_path, c.node_id
            );
        }
    }
    std::process::exit(1);
}

/// Resolve a possibly-qualified symbol name (e.g. "Database.open") to a base name
/// and optional file path for disambiguation. When the user passes a qualified name,
/// we find the matching node and use its file_path as a filter so that downstream
/// queries (callgraph, impact, refs) pick the right symbol.
/// Returns (base_name, resolved_file_filter) where resolved_file_filter is Some only
/// if the qualified name resolved uniquely and no explicit --file was given.
pub(crate) fn resolve_qualified_symbol<'a>(
    conn: &rusqlite::Connection,
    raw_symbol: &'a str,
    explicit_file: Option<&'a str>,
) -> (&'a str, Option<String>) {
    // If user already provided --file, just strip the prefix and use their filter
    if explicit_file.is_some() {
        return (strip_qualified_prefix(raw_symbol), None);
    }
    // If the symbol contains '.', try qualified name resolution
    if raw_symbol.contains('.') {
        let base = strip_qualified_prefix(raw_symbol);
        if let Ok(nodes) = queries::get_nodes_by_name(conn, base) {
            let matched: Vec<_> = nodes
                .iter()
                .filter(|n| n.qualified_name.as_deref() == Some(raw_symbol))
                .collect();
            if matched.len() == 1 {
                if let Ok(Some(fp)) = queries::get_file_path(conn, matched[0].file_id) {
                    return (base, Some(fp));
                }
            }
        }
        return (base, None);
    }
    (raw_symbol, None)
}

// --- Output formatting ---

/// Format a node as a compact single line: `type QualifiedName  file:start-end  (params) -> return`
pub(crate) fn format_node_compact(node: &queries::NodeResult, file_path: &str) -> String {
    let mut out = String::with_capacity(128);
    // type prefix
    let short_type = match node.node_type.as_str() {
        "function" => "fn",
        "method" => "fn",
        "class" => "class",
        "struct" => "struct",
        "interface" => "iface",
        "trait" => "trait",
        "enum" => "enum",
        "type_alias" => "type",
        "constant" => "const",
        "variable" => "var",
        other => other,
    };
    out.push_str(short_type);
    out.push(' ');

    // name (prefer qualified)
    if let Some(ref qn) = node.qualified_name {
        out.push_str(qn);
    } else {
        out.push_str(&node.name);
    }

    // location
    out.push_str("  ");
    out.push_str(file_path);
    out.push(':');
    out.push_str(&node.start_line.to_string());
    out.push('-');
    out.push_str(&node.end_line.to_string());

    // signature parts. param_types is stored ALREADY parenthesized ("(a, b)") by the
    // parser — verified every non-empty param_types starts with '(' and ends with ')'
    // — so append it verbatim. Wrapping it in another pair printed "((a, b))" (and
    // "(())" for no-arg fns) in `show` / `search` / `ast_search` output.
    if let Some(ref params) = node.param_types {
        if !params.is_empty() {
            out.push_str("  ");
            out.push_str(params);
        }
    }
    if let Some(ref ret) = node.return_type {
        if !ret.is_empty() {
            out.push_str(" -> ");
            out.push_str(ret);
        }
    }
    out
}

// --- Subcommands ---
