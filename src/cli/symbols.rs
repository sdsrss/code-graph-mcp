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
                // How many definitions there are: `suggestions` stops at
                // SUGGESTION_CAP, and a consumer picking one by line must know
                // when the one it needs may be past the list.
                "total": cands.len(),
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

/// The empty answer a command's file-selector miss is merged into, so one
/// parser reads the command's found, empty and miss outputs alike.
pub(crate) enum MissEnvelope {
    /// `callgraph`: `results: []`.
    Results,
    /// `impact`: the miss keys alone, as it has always printed them.
    Plain,
    /// `refs`: `references: []`, `by_relation: {}`, `total_references: 0`.
    References,
}

/// A `--file` that holds no definition of `name` (D#253, D#274): the sentence
/// MCP `get_call_graph` and `find_references` give, and a JSON envelope of
/// `error` (`File not found in index` / `Symbol not found in file`),
/// `symbol`, `file` and the `candidates` in other files, in the command's own
/// [`MissEnvelope`]. Shared by `callgraph`, `impact` and `refs`, which had
/// named one miss three ways. Exits 1.
pub(crate) fn emit_file_selector_miss(
    miss: &crate::resolve::FileSelectorMiss,
    name: &str,
    file: &str,
    json_mode: bool,
    envelope: MissEnvelope,
) -> ! {
    if json_mode {
        let mut out = serde_json::json!({
            "error": if miss.file_indexed {
                "Symbol not found in file"
            } else {
                "File not found in index"
            },
            "symbol": name,
            "file": file,
            "candidates": crate::resolve::candidates_to_json(&miss.elsewhere)
                .into_iter()
                .take(crate::resolve::SUGGESTION_CAP)
                .collect::<Vec<_>>(),
        });
        if miss.elsewhere.len() > crate::resolve::SUGGESTION_CAP {
            out["candidates_total"] = serde_json::json!(miss.elsewhere.len());
        }
        match envelope {
            MissEnvelope::Results => out["results"] = serde_json::json!([]),
            MissEnvelope::Plain => {}
            MissEnvelope::References => {
                out["references"] = serde_json::json!([]);
                out["by_relation"] = serde_json::json!({});
                out["total_references"] = serde_json::json!(0);
            }
        }
        println!("{out}");
    }
    eprintln!("[code-graph] {}", miss.message(name, file));
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

/// `--node-id` on `impact` / `callgraph` (Q4): the one definition to answer
/// for, and the identity it is re-found by after the command's own query-time
/// refresh. `nodes.id` is a rowid alias with no AUTOINCREMENT, so a re-index
/// reuses freed ids and the id alone can name another symbol afterwards
/// (SURF-16); the identity is `resolve::reresolve_node_by_identity`'s, shared
/// with `refs --node-id`, `show --node-id` and MCP `get_ast_node`.
///
/// An identity can be carried by several nodes (`#[cfg]` twins, overloads), so
/// the target also records its position among them, in source order, before
/// the refresh, and is re-found at the same position after it.
pub(crate) struct CliNodeTarget {
    pub(crate) id: i64,
    /// The id the caller passed, for messages after a re-resolution moved `id`.
    requested: i64,
    pub(crate) name: String,
    file_path: String,
    qualified_name: Option<String>,
    node_type: String,
    /// (position, size) of this node's identity group before any refresh.
    ordinal: (usize, usize),
}

impl CliNodeTarget {
    /// The node `nid` names, or exit 1: `{"error", "node_id"}` plus
    /// `extra_json` (a command's own envelope keys, e.g. `results: []`).
    pub(crate) fn lookup(
        conn: &rusqlite::Connection,
        nid: i64,
        json_mode: bool,
        extra_json: &serde_json::Value,
    ) -> Result<Self> {
        match queries::get_node_with_file_by_id(conn, nid)? {
            Some(nwf) => {
                let group = crate::resolve::identity_group_ids(
                    conn,
                    &nwf.file_path,
                    &nwf.node.name,
                    nwf.node.qualified_name.as_deref(),
                    &nwf.node.node_type,
                )?;
                let at = group.iter().position(|&id| id == nid).unwrap_or(0);
                Ok(Self {
                    id: nid,
                    requested: nid,
                    name: nwf.node.name,
                    file_path: nwf.file_path,
                    qualified_name: nwf.node.qualified_name,
                    node_type: nwf.node.node_type,
                    ordinal: (at, group.len()),
                })
            }
            None => Self::exit_missing(
                nid,
                "Node ID not found",
                &format!("node_id {nid} not found in index"),
                json_mode,
                extra_json,
            ),
        }
    }

    /// Re-find the node after a refresh re-indexed its file. Exits 1 when the
    /// definition is gone from the re-indexed source; the caller discloses the
    /// refresh first (`on_gone`), since this exits.
    pub(crate) fn reresolve(
        &mut self,
        conn: &rusqlite::Connection,
        json_mode: bool,
        extra_json: &serde_json::Value,
        on_gone: impl FnOnce(),
    ) -> Result<()> {
        let group = crate::resolve::identity_group_ids(
            conn,
            &self.file_path,
            &self.name,
            self.qualified_name.as_deref(),
            &self.node_type,
        )?;
        let (at, len) = self.ordinal;
        let found = match group.len() {
            0 => None,
            n if n == len => Some(group[at]),
            _ => {
                // The definitions sharing this identity were added or removed:
                // no position is the one the caller meant. Refuse, never guess.
                on_gone();
                Self::exit_missing(
                    self.requested,
                    "Node identity changed in the re-indexed source",
                    &format!(
                        "node_id {} ('{}' in {}) shares its name with {} definition(s) there now, {} before the re-index — look the node_id up again.",
                        self.requested, self.name, self.file_path, group.len(), len
                    ),
                    json_mode,
                    extra_json,
                )
            }
        };
        match found {
            Some(id) => {
                self.id = id;
                Ok(())
            }
            None => {
                on_gone();
                Self::exit_missing(
                    self.requested,
                    "Node no longer in the re-indexed source",
                    &format!(
                        "node_id {} ('{}' in {}) is no longer in the re-indexed source — nothing to report.",
                        self.requested, self.name, self.file_path
                    ),
                    json_mode,
                    extra_json,
                )
            }
        }
    }

    fn exit_missing(
        nid: i64,
        error: &str,
        message: &str,
        json_mode: bool,
        extra_json: &serde_json::Value,
    ) -> ! {
        if json_mode {
            let mut out = extra_json.clone();
            out["error"] = serde_json::json!(error);
            out["node_id"] = serde_json::json!(nid);
            println!("{}", out);
        }
        eprintln!("[code-graph] {}", message);
        std::process::exit(1);
    }
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
