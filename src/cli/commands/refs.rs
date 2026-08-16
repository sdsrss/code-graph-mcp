use super::*;

/// CLI arguments for the `refs` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp refs",
    about = "Find all references to a symbol (callers, importers, etc.)"
)]
pub struct RefsArgs {
    /// Symbol name (required unless --node-id is given)
    pub symbol: Option<String>,
    /// Look up by node ID (authoritative over --file)
    #[arg(long = "node-id")]
    pub node_id: Option<i64>,
    /// Disambiguate same-name symbols by file path
    #[arg(long)]
    pub file: Option<String>,
    // --relation stays an in-handler String validated at entry (before index open),
    // NOT a clap ValueEnum — so a bad --relation on a nonexistent symbol reports the
    // relation error (exit 1), not "symbol not found", and the message is preserved.
    #[arg(long, help = crate::domain::RELATION_FILTER_HELP)]
    pub relation: Option<String>,
    // Validated in-handler (not a clap ValueEnum) so a bad value reports a clear
    // tier error before symbol resolution, consistent with --relation.
    /// Minimum edge confidence: extracted (precise), inferred, ambiguous (default: show all)
    #[arg(long = "min-confidence")]
    pub min_confidence: Option<String>,
    /// Compact output
    #[arg(long)]
    pub compact: bool,
    /// JSON output
    #[arg(long)]
    pub json: bool,
}

/// Emit the refs not-found JSON envelope on stdout. Mirrors the success-case
/// envelope shape (object with `references`/`by_relation`) plus an `error` key,
/// so a single consumer parser handles found, empty, and not-found alike — and
/// every `--json` exit path produces parseable stdout (empty-JSON contract).
/// Used by all three not-found branches: symbol, --file miss, and --node-id miss.
pub(crate) fn print_refs_notfound_json(symbol: &str) {
    println!(
        "{}",
        serde_json::json!({
            "symbol": symbol,
            "total_references": 0,
            "by_relation": {},
            "references": [],
            "error": "Symbol not found",
        })
    );
}

/// Find all references to a symbol. CLI equivalent of MCP `find_references`.
pub fn cmd_refs(project_root: &Path, args: RefsArgs) -> Result<()> {
    let explicit_file_owned: Option<String> = match args.file.as_deref() {
        Some(f) => Some(normalize_user_path(project_root, f)?),
        None => None,
    };
    let explicit_file = explicit_file_owned.as_deref();
    // Validate + case-normalize --relation at command entry — before opening the
    // index and before symbol resolution — so a nonexistent symbol with a bad
    // --relation reports the relation error, not "symbol not found".
    // normalize_relation canonicalizes case. feedback-enum-validate-at-entry.
    let relation: Option<&'static str> = match args.relation.as_deref() {
        None => None,
        Some(r) => match crate::domain::normalize_relation(r) {
            Some(rel) => Some(rel),
            None => anyhow::bail!(
                "--relation must be one of: {} (got '{}')",
                crate::domain::relation_filter_vocab_list(),
                r
            ),
        },
    };
    // Validate --min-confidence at entry (before index open), mirroring --relation,
    // so a typo'd tier errors loudly instead of silently passing all rows.
    let min_confidence: Option<&'static str> =
        crate::domain::parse_min_confidence(args.min_confidence.as_deref(), "--min-confidence")?;
    let json_mode = args.json;
    let compact = args.compact;
    let node_id_arg: Option<i64> = args.node_id;

    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();

    // Resolve to (target_ids, symbol_name) — prefer --node-id for same-file multi-def disambiguation.
    // When --node-id is given, it is authoritative: --file is ignored (matches MCP find_references).
    if node_id_arg.is_some() && explicit_file.is_some() {
        eprintln!("[code-graph] Note: --file is ignored when --node-id is given (node_id is authoritative).");
    }
    let (target_ids, symbol): (Vec<i64>, String) = if let Some(nid) = node_id_arg {
        let node = match queries::get_node_by_id(conn, nid)? {
            Some(n) => n,
            None => {
                // Empty-JSON contract: emit a parseable envelope, not empty stdout.
                if json_mode {
                    print_refs_notfound_json(&format!("node_id {}", nid));
                }
                eprintln!("[code-graph] node_id {} not found in index", nid);
                std::process::exit(1);
            }
        };
        (vec![nid], node.name)
    } else {
        let raw_symbol = args.symbol.as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!(
                format!("Usage: code-graph-mcp refs <symbol> [--node-id N] [--file path] [--relation {}] [--min-confidence extracted|inferred|ambiguous] [--compact] [--json]", crate::domain::RELATION_FILTER_VOCAB.join("|"))
            ))?;
        let (base, resolved_file) = resolve_qualified_symbol(conn, raw_symbol, explicit_file);
        let file_path = explicit_file.or(resolved_file.as_deref());

        if let Some(fp) = file_path {
            let nodes = queries::get_nodes_by_file_path(conn, fp)?;
            let matched: Vec<i64> = nodes
                .iter()
                .filter(|n| n.name == base)
                .map(|n| n.id)
                .collect();
            if matched.is_empty() {
                // Empty-JSON contract: emit a parseable envelope, not empty stdout.
                if json_mode {
                    print_refs_notfound_json(base);
                }
                eprintln!("[code-graph] Symbol '{}' not found in file '{}'.", base, fp);
                std::process::exit(1);
            }
            (matched, base.to_string())
        } else {
            // Exact-name ambiguity guard — shared with callgraph/impact and the
            // MCP twin via crate::resolve so every surface gives ONE answer for
            // one input (audit 2026-08-02 P1-6: refs was the third consumer and
            // skipped this gate, silently MERGING all same-name definitions'
            // references into a single total while callgraph/MCP errored
            // Ambiguous on the same symbol — the 2026-06-03 #6 shape).
            if let Some(cands) = crate::resolve::detect_ambiguity(conn, base)? {
                emit_exact_ambiguity(base, &cands, json_mode);
            }
            let ids = queries::get_node_ids_by_name(conn, base)?;
            if ids.is_empty() {
                // Fuzzy auto-resolve: unique match → promote; multi → suggest; none → bail
                match resolve_fuzzy_name_cli(conn, base)? {
                    CliFuzzyResolution::Unique(resolved) => {
                        let resolved_ids = queries::get_node_ids_by_name(conn, &resolved)?;
                        (
                            resolved_ids.into_iter().map(|(id, _)| id).collect(),
                            resolved,
                        )
                    }
                    CliFuzzyResolution::Ambiguous(cands) => {
                        if json_mode {
                            let sugg: Vec<serde_json::Value> = cands.iter().take(5).map(|c| serde_json::json!({
                                "name": c.name, "file_path": c.file_path,
                                "type": c.node_type, "node_id": c.node_id, "start_line": c.start_line,
                            })).collect();
                            println!(
                                "{}",
                                serde_json::json!({
                                    "error": format!("Ambiguous symbol '{}': {} matches. Specify --file or --node-id to disambiguate.", base, cands.len()),
                                    "suggestions": sugg,
                                })
                            );
                        } else {
                            eprintln!("[code-graph] Ambiguous symbol '{}': {} matches. Specify --file or --node-id.", base, cands.len());
                            for c in cands.iter().take(5) {
                                eprintln!(
                                    "  {} ({}) in {} [node_id {}]",
                                    c.name, c.node_type, c.file_path, c.node_id
                                );
                            }
                        }
                        std::process::exit(1);
                    }
                    CliFuzzyResolution::NotFound => {
                        // Match the success-case envelope shape (object with
                        // references/by_relation), not a bare `[]`. Object-success
                        // commands (callgraph/trace/deps) all emit an object on the
                        // empty/error path so one parser handles both — refs was the
                        // outlier returning `[]`, which broke `.references` access.
                        if json_mode {
                            print_refs_notfound_json(base);
                        }
                        eprintln!("[code-graph] Symbol not found: {}", base);
                        hint_symbol_maybe_unindexed(base);
                        std::process::exit(1);
                    }
                }
            } else {
                (
                    ids.into_iter().map(|(id, _)| id).collect(),
                    base.to_string(),
                )
            }
        }
    };
    // Intentional shadow: downstream paths want &str. Do NOT "simplify" into a
    // single binding — the tuple above must own the String so `get_node_by_id`'s
    // return doesn't get dropped across the .as_str() borrow.
    let symbol = symbol.as_str();

    // `relation` is already canonicalized by `normalize_relation` above, which
    // only ever yields a `RELATION_FILTER_VOCAB` member or "all" — so this maps
    // through the same vocabulary instead of re-listing it. The old hand-written
    // arms were the second of the two places `exports`/`routes_to` had to be added
    // and the reason adding them anywhere else would not have been enough.
    let relation_filter: Option<&'static str> = match relation {
        Some("all") | None => None,
        Some(r) => crate::domain::normalize_relation(r).filter(|rel| *rel != "all"),
    };

    // Build the deduped reference set. Wrapped in a closure so a query-time
    // freshness resync can re-run it against the refreshed index (parity with
    // show/overview/… via refresh_files_if_stale) — after re-indexing an edited
    // source file its referencing symbol's start_line is post-edit.
    // Dedup key is (name, file_path, relation) — it does NOT include the target,
    // so two edges from the same source to DIFFERENT same-name targets collapse to
    // one row. When their confidence differs, show the LOWEST (most conservative)
    // tier: the displayed confidence must not understate a hidden sibling's
    // ambiguity (L1 — surfacing low confidence is the whole point of the feature).
    let build_refs =
        |conn: &rusqlite::Connection| -> Result<(Vec<queries::IncomingReference>, usize)> {
            let mut all_refs: Vec<queries::IncomingReference> = Vec::new();
            let mut seen: std::collections::HashMap<(String, String, String), usize> =
                std::collections::HashMap::new();
            let mut conf_filtered = 0usize;
            for target_id in &target_ids {
                let refs = queries::get_incoming_references(conn, *target_id, relation_filter)?;
                for r in refs {
                    // --min-confidence: drop refs below the requested tier (default: keep all).
                    if let Some(min) = min_confidence {
                        if crate::domain::confidence_rank(&r.confidence)
                            < crate::domain::confidence_rank(min)
                        {
                            conf_filtered += 1;
                            continue;
                        }
                    }
                    let key = (r.name.clone(), r.file_path.clone(), r.relation.clone());
                    match seen.get(&key) {
                        Some(&idx) => {
                            // Keep the worst-case (lowest) confidence among deduped siblings.
                            if crate::domain::confidence_rank(&r.confidence)
                                < crate::domain::confidence_rank(&all_refs[idx].confidence)
                            {
                                all_refs[idx].confidence = r.confidence;
                            }
                        }
                        None => {
                            seen.insert(key, all_refs.len());
                            all_refs.push(r);
                        }
                    }
                }
            }
            Ok((all_refs, conf_filtered))
        };
    let (mut all_refs, mut conf_filtered) = build_refs(conn)?;
    let files: Vec<String> = all_refs.iter().map(|r| r.file_path.clone()).collect();
    let outcome = refresh_files_if_stale(&ctx.db, &ctx.project_root, &files);
    if outcome.any_changed {
        let (a, c) = build_refs(conn)?;
        all_refs = a;
        conf_filtered = c;
    }
    outcome.disclose();

    if json_mode {
        let items: Vec<serde_json::Value> = all_refs
            .iter()
            .map(|r| {
                if compact {
                    serde_json::json!({
                        "name": r.name,
                        "file_path": r.file_path,
                        "start_line": r.start_line,
                        "relation": r.relation,
                        "confidence": r.confidence,
                        "node_id": r.node_id,
                    })
                } else {
                    serde_json::json!({
                        "node_id": r.node_id,
                        "name": r.name,
                        "type": r.node_type,
                        "file_path": r.file_path,
                        "start_line": r.start_line,
                        "relation": r.relation,
                        "confidence": r.confidence,
                    })
                }
            })
            .collect();
        // Group counts by relation, mirroring MCP find_references envelope
        let mut by_relation: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        for r in &all_refs {
            *by_relation.entry(r.relation.clone()).or_insert(0) += 1;
        }
        let mut envelope = serde_json::json!({
            "symbol": symbol,
            "total_references": items.len(),
            "by_relation": by_relation,
            "references": items,
        });
        // Machine surface must not be LESS informative than the human one:
        // human mode prints the hidden count below, and the sibling commands
        // disclose theirs in-band (callgraph ambiguous_edges_hidden, impact
        // ambiguous_callers_excluded, ast-search filtered_out) — audit
        // 2026-08-02 MED-1.
        if conf_filtered > 0 {
            envelope["confidence_filtered"] = serde_json::json!(conf_filtered);
        }
        outcome.attach_partial(&mut envelope);
        println!("{}", serde_json::to_string_pretty(&envelope)?);
    } else {
        let mut stdout = std::io::stdout().lock();
        // Annotate only non-extracted edges so precise refs stay visually clean;
        // inferred/ambiguous are the ones worth scrutiny (by-name cross-file).
        let tag = |c: &str| -> String {
            if c == crate::domain::CONF_EXTRACTED {
                String::new()
            } else {
                format!(" ~{c}")
            }
        };
        if all_refs.is_empty() {
            writeln!(stdout, "No references found for '{}'.", symbol)?;
        } else {
            writeln!(stdout, "{} references to '{}':", all_refs.len(), symbol)?;
            for r in &all_refs {
                if compact {
                    writeln!(
                        stdout,
                        "  [{}] {} {}{}",
                        r.relation,
                        r.name,
                        r.file_path,
                        tag(&r.confidence)
                    )?;
                } else {
                    writeln!(
                        stdout,
                        "  [{}] {} ({}:{}){}",
                        r.relation,
                        r.name,
                        r.file_path,
                        r.start_line,
                        tag(&r.confidence)
                    )?;
                }
            }
        }
        if conf_filtered > 0 {
            writeln!(
                stdout,
                "({} lower-confidence ref(s) hidden by --min-confidence)",
                conf_filtered
            )?;
        }
    }

    Ok(())
}

// --- dead-code subcommand ---
