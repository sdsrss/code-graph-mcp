use super::*;

/// What the post-fetch filters did with one FTS candidate.
///
/// `Noise` and `Filtered` are kept apart because they mean different things to
/// the user: noise (module/external placeholders, test symbols) is removed by
/// policy and is never what they asked for, while `Filtered` is their own
/// `--language`/`--node-type` doing its job. Their SUM is what says a pool was
/// consumed before `limit` was filled.
enum Candidate {
    Keep,
    Noise,
    Filtered,
}

/// The one place the post-fetch filters are decided.
///
/// Both the exhaustion tally and the render loop go through this. They used to
/// be one inline loop, and adding the tally beside it would have created exactly
/// the twin-that-drifts shape the SURF-03/04/05 findings are about.
fn classify_candidate(
    n: &queries::NodeResult,
    nwf_map: &std::collections::HashMap<i64, &queries::NodeWithFile>,
    language_filter: Option<&str>,
    normalized_node_types: &[&'static str],
) -> Candidate {
    let nwf = nwf_map.get(&n.id);
    // Skip <module>/<external> placeholders and test symbols, consistent with
    // MCP semantic_code_search (domain::is_skippable_result = the shared triad;
    // the CLI path previously omitted the <external> leg the MCP path applied).
    let fp = nwf.map(|nwf| nwf.file_path.as_str()).unwrap_or("");
    if crate::domain::is_skippable_result(n.is_test, &n.node_type, &n.name, fp) {
        return Candidate::Noise;
    }
    if let Some(lang) = language_filter {
        let lang_ok = nwf
            .and_then(|nwf| nwf.language.as_deref())
            .map(|l| l.eq_ignore_ascii_case(lang))
            .unwrap_or(false);
        if !lang_ok {
            return Candidate::Filtered;
        }
    }
    if !normalized_node_types.is_empty() && !normalized_node_types.iter().any(|t| n.node_type == *t)
    {
        return Candidate::Filtered;
    }
    Candidate::Keep
}

/// `(kept, dropped_by_filter, skipped_noise)` for a fetched pool.
///
/// Stops counting `kept` at `limit` exactly like the render loop, so "would a
/// wider pool help" is answered against the same number the user will see.
fn tally_pool(
    fts: &queries::FtsResult,
    nodes_with_files: &[queries::NodeWithFile],
    language_filter: Option<&str>,
    normalized_node_types: &[&'static str],
    limit: i64,
) -> (usize, usize, usize) {
    let map: std::collections::HashMap<i64, &queries::NodeWithFile> = nodes_with_files
        .iter()
        .map(|nwf| (nwf.node.id, nwf))
        .collect();
    let (mut kept, mut dropped, mut noise) = (0usize, 0usize, 0usize);
    for n in &fts.nodes {
        match classify_candidate(n, &map, language_filter, normalized_node_types) {
            Candidate::Noise => noise += 1,
            Candidate::Filtered => dropped += 1,
            Candidate::Keep => {
                kept += 1;
                if kept >= limit as usize {
                    break;
                }
            }
        }
    }
    (kept, dropped, noise)
}

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
    #[arg(long = "node-type", help = crate::domain::TYPE_FILTER_HELP_ARG)]
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
    // `--top-k` is an alias of `--limit` here (the mirror of `similar`, where the
    // canonical spelling is the other one) — name both so the message matches
    // whichever the caller typed.
    let limit: i64 = clamp_arg("--limit (alias --top-k)", args.limit.unwrap_or(20), 1, 100);

    // Validate --node-type up-front: unknown alias normalizes to an empty Vec
    // and silently filters every node away (see ast-search same fix).
    if let Some(ntf) = node_type_filter {
        if crate::domain::normalize_type_filter(ntf).is_empty() {
            anyhow::bail!(
                "Unknown node-type filter: '{}'. Valid: {}.{}",
                ntf,
                crate::domain::TYPE_FILTER_HELP,
                crate::domain::type_filter_note(ntf)
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
    // `fetch` is a parameter, not the captured `fetch_limit`: the exhaustion
    // retry below re-runs this with a wider pool.
    let run_query = |conn: &rusqlite::Connection,
                     fetch: i64|
     -> Result<(queries::FtsResult, Vec<queries::NodeWithFile>)> {
        let fts_result = queries::fts5_search(conn, query, fetch)?;
        let node_ids: Vec<i64> = fts_result.nodes.iter().map(|n| n.id).collect();
        let nodes_with_files = queries::get_nodes_with_files_by_ids(conn, &node_ids)?;
        Ok((fts_result, nodes_with_files))
    };
    let (mut fts_result, mut nodes_with_files) = run_query(conn, fetch_limit)?;
    // Normalize node_type filter for matching. Needed here because the
    // exhaustion retry below applies the same predicate to decide whether a
    // wider pool would help.
    let normalized_node_types: Vec<&'static str> = node_type_filter
        .map(normalize_type_filter)
        .unwrap_or_default();

    // ── Pool-exhaustion retry (parity with MCP semantic_code_search) ─────────
    // The post-fetch filters run in Rust, AFTER the SQL `LIMIT`, so a pool that
    // came back full can be consumed by them before `limit` is filled — the
    // matches are simply below the cut. The CLI reported that as "no results …
    // broaden or clear the filter", advice that cannot work, because the filter
    // was not what removed them (SURF-03; the MCP twin has widened once on
    // exhaustion since the 2026-08-16 audit P1-7).
    //
    // Widen only when all four hold, so every query that was NOT starved
    // retrieves exactly as before and pays nothing: under-filled, pool came back
    // full, something was actually dropped, and the wider pool is really wider.
    //
    // Ordered BEFORE the freshness resync below, not after. The resync is what
    // makes `start_line`/`end_line` post-edit, and it is bounded by the files of
    // the pool handed to it. Widening afterwards pulls in files it never checked
    // and returns their stale line numbers with `outcome.disclose()` already
    // printed — measured off by exactly the edit size, silently (pre-ship review
    // 2026-09-06).
    let mut effective_fetch = fetch_limit;
    let (kept_first, dropped_first, noise_first) = tally_pool(
        &fts_result,
        &nodes_with_files,
        language_filter,
        &normalized_node_types,
        limit,
    );
    let retry_fetch = crate::domain::search_retry_fetch_count(fetch_limit);
    if kept_first < limit as usize
        && fts_result.nodes.len() >= fetch_limit as usize
        && (dropped_first + noise_first) > 0
        && retry_fetch > fetch_limit
    {
        let (f, n) = run_query(conn, retry_fetch)?;
        let (kept_retry, _, _) = tally_pool(&f, &n, language_filter, &normalized_node_types, limit);
        // Keep the wider pool only if it actually recovered something: a pool
        // that is all noise must not cost a second fetch AND a changed answer.
        if kept_retry > kept_first {
            fts_result = f;
            nodes_with_files = n;
            effective_fetch = retry_fetch;
        }
    }

    // Re-index any matched file edited since indexing so start_line/end_line are
    // post-edit, then re-run once. Bounded by the fetched pool (`effective_fetch`
    // — the widened one whenever the retry above fired), not the whole index.
    let files: Vec<String> = nodes_with_files
        .iter()
        .map(|nwf| nwf.file_path.clone())
        .collect();
    let outcome = refresh_files_if_stale(&ctx.db, &ctx.project_root, &files);
    if outcome.any_changed {
        let (f, n) = run_query(conn, effective_fetch)?;
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

    // Filter by language, node_type, and skip test/module nodes (align with MCP behavior).
    // Count language/node_type drops separately so an over-selective filter that empties
    // the result set can say so (vs a generic "no results"), mirroring MCP's filter hint.
    // `skipped_noise` is counted too: it is the other half of "why is this short",
    // and the retry above is decided on their sum.
    let mut filtered_nodes: Vec<&queries::NodeResult> = Vec::new();
    let mut dropped_by_filter = 0usize;
    let mut skipped_noise = 0usize;
    for n in &fts_result.nodes {
        match classify_candidate(n, &nwf_map, language_filter, &normalized_node_types) {
            Candidate::Noise => {
                skipped_noise += 1;
                continue;
            }
            Candidate::Filtered => {
                dropped_by_filter += 1;
                continue;
            }
            Candidate::Keep => {}
        }
        filtered_nodes.push(n);
        if filtered_nodes.len() >= limit as usize {
            break;
        }
    }

    // Still short after the retry (or with no retry available) on a pool that came
    // back FULL: the answer is under-returned and stdout cannot say so on its own —
    // the success path is a bare JSON array, so `--json 2>/dev/null` shows a short
    // list that is byte-identical to a complete one.
    let pool_saturated = filtered_nodes.len() < limit as usize
        && fts_result.nodes.len() >= effective_fetch as usize
        && (dropped_by_filter + skipped_noise) > 0;

    if filtered_nodes.is_empty() {
        // TWO reasons an empty answer is not a true zero-hit, and both must be
        // disclosed IN-BAND (stdout), not only on stderr: under
        // `--json 2>/dev/null` a bare `[]` is byte-identical to "this repo has no
        // such code" and the LLM consumer reports exactly that (disclosure-gap
        // class, roadmap 2026-07-18 §1.1).
        //
        //   - the user's own language/node-type filter removed every match, or
        //   - the pool came back FULL and was consumed before the limit was
        //     filled, with the retry above already widened as far as it may go.
        //
        // The second one used to fall through to the plain-`[]` arm whenever the
        // drops were noise rather than the user's filter — i.e. exactly the case
        // where `[]` reads most strongly as absence, and the one where the user
        // has no filter to "broaden" as the other message suggests (pre-ship
        // review 2026-09-06). A true zero-hit still gets the plain `[]` below.
        let filter_removed_all = filtered && dropped_by_filter > 0;
        if filter_removed_all || pool_saturated {
            let filter_desc = format!(
                "language: {}{}",
                language_filter.unwrap_or("any"),
                node_type_filter
                    .map(|t| format!(", node-type: {t}"))
                    .unwrap_or_default()
            );
            // "Broaden or clear the filter" is the wrong instruction when the
            // filter is not what removed the match. On a pool that came back FULL
            // and was consumed before the limit, the cut did — and the retry above
            // has already widened as far as it is allowed to.
            let exhaustion_note = if pool_saturated {
                format!(
                    " The candidate pool ({} rows) came back full and was consumed before the limit was filled, so matches may sit below the fetch cut; a higher --limit widens it.",
                    fts_result.nodes.len()
                )
            } else {
                String::new()
            };
            let message = if filter_removed_all {
                format!(
                    "[code-graph] No results for: {} — {} candidate(s) matched the query but were removed by the active filter ({}). Broaden or clear the filter.{}",
                    query, dropped_by_filter, filter_desc, exhaustion_note
                )
            } else {
                format!(
                    "[code-graph] No results for: {} — every candidate the query matched was dropped as noise (test, module or external symbols).{}",
                    query, exhaustion_note
                )
            };
            if json_mode {
                let mut envelope = serde_json::json!({
                    "results": [],
                    "query": query,
                });
                if filter_removed_all {
                    envelope["filtered_out"] = serde_json::json!(dropped_by_filter);
                    envelope["filter"] = serde_json::json!(filter_desc);
                } else {
                    // Names the drop that actually emptied it, so the envelope is
                    // not just "empty, and something happened".
                    envelope["noise_skipped"] = serde_json::json!(skipped_noise);
                }
                if pool_saturated {
                    envelope["pool_saturated"] = serde_json::json!(true);
                }
                println!("{}", envelope);
                // Only the JSON path needs the prose on stderr: its stdout is a
                // machine envelope. In human mode the `println!` below IS the
                // message, and emitting it on both streams printed the same
                // finding twice to one terminal — in two slightly different
                // wordings, which reads as two separate problems.
                eprintln!("{message}");
            } else {
                println!("{message}");
            }
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

    // Same placement and the same reason as the OR-fallback note above: a SHORT
    // result array is byte-identical to a complete one, so the consumer that
    // cannot infer the shortfall from stdout is exactly the one reading `--json`.
    // Emitted only when the pool came back full AND was consumed — a query that
    // simply has three matches in the whole repo is complete, not truncated.
    if pool_saturated {
        eprintln!(
            "[code-graph] Note: returned {} of --limit {} — the candidate pool ({} rows) was consumed by filters/noise before the limit was filled. Matches may sit below the fetch cut; a higher --limit widens it.",
            filtered_nodes.len(),
            limit,
            fts_result.nodes.len()
        );
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
