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
            .take(crate::resolve::SUGGESTION_CAP)
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
        for c in cands.iter().take(crate::resolve::SUGGESTION_CAP) {
            eprintln!(
                "  {} ({}) in {} [node_id {}]",
                c.name, c.node_type, c.file_path, c.node_id
            );
        }
    }
    std::process::exit(1);
}

/// Which JSON envelope a command wraps its fuzzy-ambiguity candidates in.
///
/// ARC-01: `callgraph` and `refs` are the only two fuzzy-Ambiguous sites, and the
/// two hand-written copies had already drifted into different key names and
/// different envelope shapes for one concept. Both spellings are PUBLISHED CLI
/// contract (project CLAUDE.md), so this is parameterised to keep each command's
/// bytes exactly as they are — unifying the keys would break anyone parsing
/// either one. The sibling exact-ambiguity path shared its renderer from the
/// start (`emit_exact_ambiguity` above); this is the leg that stayed forked, and
/// the drift grew precisely there.
pub(crate) enum FuzzyEnvelope {
    /// `callgraph`: `{"results": [], "error": …, "candidates": [...]}`
    ResultsAndCandidates,
    /// `refs`: `{"error": …, "suggestions": [...]}`
    Suggestions,
}

/// Emit the "ambiguous symbol" error for a FUZZY (not exact) match in the calling
/// command's own envelope, then exit(1).
///
/// `json_suffix` / `human_suffix` are appended to the shared
/// `Ambiguous symbol 'X': N matches` stem; they differ per command and per
/// surface, so they stay explicit at the call site rather than being derived.
pub(crate) fn emit_fuzzy_ambiguity(
    symbol: &str,
    cands: &[queries::NameCandidate],
    json_mode: bool,
    envelope: FuzzyEnvelope,
    json_suffix: &str,
    human_suffix: &str,
) -> ! {
    let stem = format!("Ambiguous symbol '{}': {} matches", symbol, cands.len());
    // SURF-34: this stem is built here rather than through `ambiguity_message`,
    // so it needs the same cap disclosure — the list below it is
    // `take(SUGGESTION_CAP)` on both arms. It goes AFTER the suffix, never
    // between: both suffixes continue the sentence (". Did you mean:"), so a
    // note spliced in front of them reads as a broken one.
    let capped = crate::resolve::suggestion_cap_note(cands.len());
    if json_mode {
        let sugg: Vec<serde_json::Value> = crate::resolve::candidates_to_json(cands)
            .into_iter()
            .take(crate::resolve::SUGGESTION_CAP)
            .collect();
        let error = format!("{stem}{json_suffix}{capped}");
        let payload = match envelope {
            FuzzyEnvelope::ResultsAndCandidates => serde_json::json!({
                "results": [],
                "error": error,
                "candidates": sugg,
            }),
            FuzzyEnvelope::Suggestions => serde_json::json!({
                "error": error,
                "suggestions": sugg,
            }),
        };
        println!("{}", payload);
    } else {
        eprintln!("[code-graph] {}{}", stem, human_suffix);
        for c in cands.iter().take(crate::resolve::SUGGESTION_CAP) {
            eprintln!(
                "  {} ({}) in {} [node_id {}]",
                c.name, c.node_type, c.file_path, c.node_id
            );
        }
        // After the list, not before it: on this arm the reader learns the list
        // was cut at the point where it ended.
        if !capped.is_empty() {
            eprintln!("[code-graph]{}", capped);
        }
    }
    std::process::exit(1);
}

/// The lookup chosen for a CLI symbol after qualified-name selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CliSymbolLookup {
    /// An exact `qualified_name` match. Traversals must retain the qualifier.
    ExactQualified,
    /// A bare-name lookup, including the historical dotted-to-bare fallback.
    Bare,
}

/// Shared symbol selection consumed by `refs`, `callgraph`, and `impact`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CliSymbolSelection {
    pub(crate) lookup_name: String,
    pub(crate) bare_name: String,
    pub(crate) file_filter: Option<String>,
    pub(crate) lookup: CliSymbolLookup,
}

/// A qualified CLI lookup can fail before command-specific querying begins.
pub(crate) enum CliSymbolSelectionError {
    /// More than one exact qualified definition survived the optional file filter.
    Ambiguous(Vec<queries::NameCandidate>),
    /// `--file` makes a missing qualifier a strict miss.
    QualifiedNotFound,
}

/// Select a CLI symbol with exact-qualified precedence and compatible fallback.
///
/// Exact qualified matches are filtered by `explicit_file`. One match keeps the
/// qualified spelling for traversal; multiple matches are ambiguous. With no
/// exact match, dotted input falls back to its final bare component only when
/// no file filter was supplied. Bare input keeps the normal name lookup.
pub(crate) fn select_cli_symbol(
    conn: &rusqlite::Connection,
    raw_symbol: &str,
    explicit_file: Option<&str>,
) -> Result<std::result::Result<CliSymbolSelection, CliSymbolSelectionError>> {
    let bare_name = strip_qualified_prefix(raw_symbol).to_string();
    if raw_symbol.contains('.') {
        let matches =
            crate::resolve::selectable_qualified_definitions(conn, raw_symbol, explicit_file)?;
        if matches.len() > 1 {
            let candidates = matches
                .into_iter()
                .map(|candidate| queries::NameCandidate {
                    name: candidate.node.name,
                    file_path: candidate.file_path,
                    node_type: candidate.node.node_type,
                    node_id: candidate.node.id,
                    start_line: candidate.node.start_line,
                })
                .collect();
            return Ok(Err(CliSymbolSelectionError::Ambiguous(candidates)));
        }
        if matches.into_iter().next().is_some() {
            return Ok(Ok(CliSymbolSelection {
                lookup_name: raw_symbol.to_string(),
                bare_name,
                // The exact qualifier is already the selector. Retain only a
                // file filter the user supplied; caching the matched path here
                // would make a freshness refresh follow a pre-refresh path.
                file_filter: explicit_file.map(str::to_string),
                lookup: CliSymbolLookup::ExactQualified,
            }));
        }
        if explicit_file.is_some() {
            return Ok(Err(CliSymbolSelectionError::QualifiedNotFound));
        }
    }
    Ok(Ok(CliSymbolSelection {
        lookup_name: if raw_symbol.contains('.') {
            bare_name.clone()
        } else {
            raw_symbol.to_string()
        },
        bare_name,
        file_filter: explicit_file.map(str::to_string),
        lookup: CliSymbolLookup::Bare,
    }))
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
