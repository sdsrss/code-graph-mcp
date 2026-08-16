use super::*;

/// CLI arguments for the `search` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp search",
    about = "FTS5 text search by concept (CLI is FTS-only; MCP adds vector+RRF fusion)"
)]
pub struct SearchArgs {
    /// Search query (concept keywords)
    pub query: String,
    /// JSON output
    #[arg(long)]
    pub json: bool,
    /// Compact output
    #[arg(long)]
    pub compact: bool,
    /// Filter by language
    #[arg(long)]
    pub language: Option<String>,
    /// Filter by node type: fn, class, struct, enum, trait, type, const, var
    #[arg(long = "node-type")]
    pub node_type: Option<String>,
    // --limit and --top-k are the same arg (alias); supplying both is a clap
    // duplicate-arg error. clamp(1,100) stays in the handler; clap parse-errors
    // (exit 2) on a non-numeric value, replacing the old warn+fallback.
    /// Limit results (default: 20, max: 100); alias: --top-k
    #[arg(long, alias = "top-k")]
    pub limit: Option<i64>,
}

/// FTS5 semantic search.
///
/// Output format:
/// ```text
/// fn McpServer::handle_tool_call  src/mcp/server.rs:350-420  (name: &str, params: Value) -> Result<Value>
/// ```
pub fn cmd_search(project_root: &Path, args: SearchArgs) -> Result<()> {
    // clap accepts an empty-string positional (e.g. an unset `search "$X"`);
    // preserve the non-empty query guard with the exact Usage string.
    let query = args.query.as_str();
    if query.is_empty() {
        anyhow::bail!("Usage: code-graph-mcp search <query> [--json] [--limit N] [--top-k N] [--language <lang>] [--compact]");
    }

    let json_mode = args.json;
    let compact = args.compact;
    let node_type_filter = args.node_type.as_deref();
    let limit: i64 = args.limit.unwrap_or(20).clamp(1, 100);

    // Validate --node-type up-front: unknown alias normalizes to an empty Vec
    // and silently filters every node away (see ast-search same fix).
    if let Some(ntf) = node_type_filter {
        if crate::domain::normalize_type_filter(ntf).is_empty() {
            anyhow::bail!(
                "Unknown node-type filter: '{}'. Valid: fn, class, struct, enum, trait, type, const, var",
                ntf
            );
        }
    }

    // Validate --language up-front and normalize to canonical case: an unknown
    // language matches no node's stored `language` field and would otherwise be
    // reported as a too-narrow filter ("Broaden or clear") rather than a bad value.
    // Parity with --node-type above and MCP semantic_code_search.
    let language_filter = match args.language.as_deref() {
        Some(lf) => Some(crate::utils::config::canonical_language(lf).ok_or_else(|| {
            anyhow::anyhow!(
                "Unknown language filter: '{}'. Valid: {}",
                lf,
                crate::utils::config::SUPPORTED_LANGUAGES.join(", ")
            )
        })?),
        None => None,
    };

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    // Over-fetch so post-fetch filtering can still return `limit` results. The filter
    // below ALWAYS drops <module>/test symbols, and a language/node-type filter can drop
    // far more — a selective filter over a minority language/type silently under-returns.
    // Widen the pool when a filter is active (shared policy with MCP semantic_code_search
    // via search_fetch_count); the unfiltered value stays (limit*4).max(20).
    let filtered = language_filter.is_some() || node_type_filter.is_some();
    let fetch_limit = crate::domain::search_fetch_count(limit, filtered);
    // FTS5 + file join, wrapped so a query-time freshness resync can re-run it
    // against the refreshed index (parity with show/refs/… via refresh_files_if_stale).
    let run_query =
        |conn: &rusqlite::Connection| -> Result<(queries::FtsResult, Vec<queries::NodeWithFile>)> {
            let fts_result = queries::fts5_search(conn, query, fetch_limit)?;
            let node_ids: Vec<i64> = fts_result.nodes.iter().map(|n| n.id).collect();
            let nodes_with_files = queries::get_nodes_with_files_by_ids(conn, &node_ids)?;
            Ok((fts_result, nodes_with_files))
        };
    let (mut fts_result, mut nodes_with_files) = run_query(conn)?;
    // Re-index any matched file edited since indexing so start_line/end_line are
    // post-edit, then re-run once. Bounded by the fetched pool (fetch_limit), not
    // the whole index.
    let files: Vec<String> = nodes_with_files
        .iter()
        .map(|nwf| nwf.file_path.clone())
        .collect();
    let outcome = refresh_files_if_stale(&ctx.db, &ctx.project_root, &files);
    if outcome.any_changed {
        let (f, n) = run_query(conn)?;
        fts_result = f;
        nodes_with_files = n;
    }
    outcome.disclose();

    if fts_result.nodes.is_empty() {
        // Tier-2 disclosure when the query never reached SQL (single characters,
        // stop words): a bare `[]` here says "this repo has no such code", and the
        // truth is that nothing was searched for (2026-08-16 audit §四).
        if let Some(reason) = fts_result.empty_reason {
            if json_mode {
                println!(
                    "{}",
                    serde_json::json!({
                        "results": [],
                        "query": query,
                        "not_searched": reason,
                    })
                );
            }
            eprintln!(
                "[code-graph] Nothing was searched for: {reason}. \
                 Query: {query} — try a longer or more specific term."
            );
            return Ok(());
        }
        if json_mode {
            println!("[]");
        }
        eprintln!("[code-graph] No results for: {}", query);
        // Hint: if query looks like code syntax, suggest ast-search
        if query.contains('(')
            || query.contains(')')
            || query.contains("->")
            || query.contains("::")
            || query.contains('<')
        {
            // Replace non-word chars with spaces, collapse multiple spaces, extract clean keywords
            let clean: String = query
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() || c == '_' {
                        c
                    } else {
                        ' '
                    }
                })
                .collect();
            let keywords: Vec<&str> = clean.split_whitespace().collect();
            if !keywords.is_empty() {
                eprintln!("  Tip: For structural queries, try: code-graph-mcp ast-search --type fn --returns \"{}\"",
                    keywords.join(" "));
            }
        }
        return Ok(());
    }

    // Build id->NodeWithFile map preserving FTS rank order
    let nwf_map: std::collections::HashMap<i64, &queries::NodeWithFile> = nodes_with_files
        .iter()
        .map(|nwf| (nwf.node.id, nwf))
        .collect();

    // Normalize node_type filter for matching
    let normalized_node_types: Vec<&'static str> = node_type_filter
        .map(normalize_type_filter)
        .unwrap_or_default();

    // Filter by language, node_type, and skip test/module nodes (align with MCP behavior).
    // Count language/node_type drops separately so an over-selective filter that empties
    // the result set can say so (vs a generic "no results"), mirroring MCP's filter hint.
    let mut filtered_nodes: Vec<&queries::NodeResult> = Vec::new();
    let mut dropped_by_filter = 0usize;
    for n in &fts_result.nodes {
        // Skip <module>/<external> placeholders and test symbols, consistent with
        // MCP semantic_code_search (domain::is_skippable_result = the shared triad;
        // the CLI path previously omitted the <external> leg the MCP path applied).
        let fp = nwf_map
            .get(&n.id)
            .map(|nwf| nwf.file_path.as_str())
            .unwrap_or("");
        if crate::domain::is_skippable_result(n.is_test, &n.node_type, &n.name, fp) {
            continue;
        }
        if let Some(lang) = language_filter {
            let lang_ok = nwf_map
                .get(&n.id)
                .and_then(|nwf| nwf.language.as_deref())
                .map(|l| l.eq_ignore_ascii_case(lang))
                .unwrap_or(false);
            if !lang_ok {
                dropped_by_filter += 1;
                continue;
            }
        }
        if !normalized_node_types.is_empty()
            && !normalized_node_types.iter().any(|t| n.node_type == *t)
        {
            dropped_by_filter += 1;
            continue;
        }
        filtered_nodes.push(n);
        if filtered_nodes.len() >= limit as usize {
            break;
        }
    }

    if filtered_nodes.is_empty() {
        if filtered && dropped_by_filter > 0 {
            // Matches existed but the language/node_type filter removed them all — the
            // index has hits, just not of this language/type. Disclose IN-BAND
            // (stdout), not only stderr: under `--json 2>/dev/null` a bare `[]`
            // is byte-identical to a true zero-hit and the LLM consumer reports
            // "no such code" (disclosure-gap class, roadmap 2026-07-18 §1.1).
            // True zero-hit keeps the plain `[]` / stderr shape below.
            let filter_desc = format!(
                "language: {}{}",
                language_filter.unwrap_or("any"),
                node_type_filter
                    .map(|t| format!(", node-type: {t}"))
                    .unwrap_or_default()
            );
            if json_mode {
                println!(
                    "{}",
                    serde_json::json!({
                        "results": [],
                        "query": query,
                        "filtered_out": dropped_by_filter,
                        "filter": filter_desc,
                    })
                );
            } else {
                println!(
                    "[code-graph] No results for: {} — {} candidate(s) matched but were removed by the active filter ({}). Broaden or clear the filter.",
                    query, dropped_by_filter, filter_desc
                );
            }
            eprintln!(
                "[code-graph] No results for: {} — {} candidate(s) matched the query but were removed by the active filter ({}). Broaden or clear the filter.",
                query, dropped_by_filter, filter_desc
            );
        } else {
            if json_mode {
                println!("[]");
            }
            eprintln!(
                "[code-graph] No results for: {} (language: {})",
                query,
                language_filter.unwrap_or("any")
            );
            // The FTS-only disclosure below used to live only on the SUCCESS
            // path, so the one outcome where it changes what the user should do
            // next — zero hits, which the vector channel might well have found —
            // was the one that never saw it (2026-08-16 audit §四).
            if !json_mode {
                eprintln!("[code-graph] Tip: CLI search is FTS5-only — a concept the index has under different words needs MCP semantic_code_search.");
            }
        }
        return Ok(());
    }

    // Build file_path map from filtered results
    let file_map: std::collections::HashMap<i64, &str> = nodes_with_files
        .iter()
        .map(|nwf| (nwf.node.id, nwf.file_path.as_str()))
        .collect();

    let mut stdout = std::io::stdout().lock();

    // Emitted BEFORE the `--json` early return below, not after it. The notice
    // used to sit at the end of the human-render path, so the one consumer that
    // cannot infer the degradation from the output — a script reading a bare JSON
    // array — was the one never told that these are the broader OR results rather
    // than the AND match it asked for (2026-08-16 audit §四). stderr, so the
    // stdout JSON stays a clean array.
    if fts_result.or_fallback {
        eprintln!("[code-graph] Note: AND match insufficient, showing OR results (broader match).");
    }

    if json_mode {
        let results: Vec<serde_json::Value> = filtered_nodes
            .iter()
            .map(|n| {
                let fp = file_map.get(&n.id).copied().unwrap_or("?");
                serde_json::json!({
                    "node_id": n.id,
                    "type": n.node_type,
                    "name": n.qualified_name.as_deref().unwrap_or(&n.name),
                    "file_path": fp,
                    "start_line": n.start_line,
                    "end_line": n.end_line,
                    "signature": n.signature,
                    "return_type": n.return_type,
                    "param_types": n.param_types,
                })
            })
            .collect();
        writeln!(stdout, "{}", serde_json::to_string(&results)?)?;
        return Ok(());
    }

    for node in &filtered_nodes {
        let fp = file_map.get(&node.id).copied().unwrap_or("?");
        if compact {
            let name = node.qualified_name.as_deref().unwrap_or(&node.name);
            writeln!(
                stdout,
                "{}  {}:{}-{}",
                name, fp, node.start_line, node.end_line
            )?;
        } else {
            writeln!(stdout, "{}", format_node_compact(node, fp))?;
        }
    }

    if !json_mode {
        eprintln!("[code-graph] Tip: CLI search is FTS5-only. For vector+RRF hybrid recall use MCP semantic_code_search.");
    }

    Ok(())
}

// --- ast-search subcommand ---
