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

/// How to find the reference targets AGAIN once a refresh has renumbered the
/// index (SURF-16, audit 2026-09-07).
///
/// `nodes.id` is a bare `INTEGER PRIMARY KEY` — a rowid alias with no
/// AUTOINCREMENT — so the incremental re-index `refs` runs on its own result set
/// deletes the file's rows and the re-insert REUSES the freed ids. Any id
/// resolved before `refresh_files_if_stale` can therefore name a DIFFERENT
/// symbol after it, and re-running the rollup with those ids answers about that
/// other symbol under the name resolution settled on first. `show --node-id`
/// already re-resolves by identity (CON-10); this is the same rule for `refs`.
enum RefsTarget {
    /// `--node-id`. Identity is (file_path, name, qualified_name, type) — the
    /// key [`crate::resolve::reresolve_node_by_identity`] uses, shared with
    /// `show --node-id` and MCP `get_ast_node` so the three cannot disagree
    /// about which symbol an id survived as.
    Node {
        file_path: String,
        qualified_name: Option<String>,
        node_type: String,
    },
    /// `--node-id` naming a row with no `files` row. The node_id arm reaches
    /// those deliberately (it looks the id up unjoined), and they have no file
    /// path to re-resolve against — but a file refresh cannot free an id that
    /// belongs to no file either, so the caller's id stays exactly valid.
    Orphan(i64),
    /// A name resolution already settled, optionally scoped to one file. Cheap
    /// to redo, and redoing it is what keeps the answer attached to the name.
    Name { file_path: Option<String> },
}

impl RefsTarget {
    /// Re-find the target ids for `symbol` against the current index state.
    /// Empty means the symbol is gone from the re-indexed source.
    fn resolve(&self, conn: &rusqlite::Connection, symbol: &str) -> Result<Vec<i64>> {
        Ok(match self {
            RefsTarget::Node {
                file_path,
                qualified_name,
                node_type,
            } => crate::resolve::reresolve_node_by_identity(
                conn,
                file_path,
                symbol,
                qualified_name.as_deref(),
                node_type,
            )?
            .map(|c| vec![c.node.id])
            .unwrap_or_default(),
            RefsTarget::Orphan(id) => vec![*id],
            RefsTarget::Name {
                file_path: Some(fp),
            } => queries::get_nodes_by_file_path(conn, fp)?
                .into_iter()
                .filter(|n| n.name == symbol)
                .map(|n| n.id)
                .collect(),
            RefsTarget::Name { file_path: None } => queries::get_node_ids_by_name(conn, symbol)?
                .into_iter()
                .map(|(id, _)| id)
                .collect(),
        })
    }
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
    let (mut target_ids, symbol, target): (Vec<i64>, String, RefsTarget) = if let Some(nid) =
        node_id_arg
    {
        // Deliberately the UNJOINED lookup: a node whose `files` row is missing
        // still answers here, and the joined variant would turn it into a miss.
        // The identity for the post-refresh re-resolution comes from the joined
        // query separately, and its absence is what `Orphan` records.
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
        let target = match queries::get_node_with_file_by_id(conn, nid)? {
            Some(nwf) => RefsTarget::Node {
                file_path: nwf.file_path,
                qualified_name: nwf.node.qualified_name,
                node_type: nwf.node.node_type,
            },
            None => RefsTarget::Orphan(nid),
        };
        (vec![nid], node.name, target)
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
            let matched: Vec<&queries::NodeResult> =
                nodes.iter().filter(|n| n.name == base).collect();
            if matched.is_empty() {
                // Empty-JSON contract: emit a parseable envelope, not empty stdout.
                if json_mode {
                    print_refs_notfound_json(base);
                }
                eprintln!("[code-graph] Symbol '{}' not found in file '{}'.", base, fp);
                std::process::exit(1);
            }
            // SURF-17 (audit 2026-09-07): a file selector cannot split same-file
            // overloads, so merging them produced ONE reference total for TWO
            // symbols — silently, while MCP `find_references` refused the very
            // same input as ambiguous. That is the 2026-06-03 #6 shape (one
            // input, two surfaces, opposite verdicts) `crate::resolve` exists to
            // prevent, and the bare-name arm below has carried this gate since
            // audit 2026-08-02 P1-6. `--node-id` is the escape hatch, and the
            // shared message names it.
            if matched.len() > 1 {
                let cands: Vec<queries::NameCandidate> = matched
                    .iter()
                    .map(|n| queries::NameCandidate {
                        name: n.name.clone(),
                        file_path: fp.to_string(),
                        node_type: n.node_type.clone(),
                        node_id: n.id,
                        start_line: n.start_line,
                    })
                    .collect();
                emit_exact_ambiguity(base, &cands, json_mode);
            }
            (
                matched.iter().map(|n| n.id).collect(),
                base.to_string(),
                RefsTarget::Name {
                    file_path: Some(fp.to_string()),
                },
            )
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
                            RefsTarget::Name { file_path: None },
                        )
                    }
                    CliFuzzyResolution::Ambiguous(cands) => {
                        // ARC-01: shared renderer, refs's published envelope
                        // (`suggestions`, no `results` key). Both suffixes name the
                        // disambiguation flags, but the JSON one spells out
                        // "to disambiguate" and the stderr one does not — kept
                        // verbatim, since both strings are already shipped.
                        crate::cli::symbols::emit_fuzzy_ambiguity(
                            base,
                            &cands,
                            json_mode,
                            crate::cli::symbols::FuzzyEnvelope::Suggestions,
                            ". Specify --file or --node-id to disambiguate.",
                            ". Specify --file or --node-id.",
                        );
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
                    RefsTarget::Name { file_path: None },
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
    // The rule itself lives in `resolve::rollup_incoming_references`, shared with
    // MCP `find_references` (ARC-03). `skip_tests: false` — `refs` shows every
    // usage site, because a rename has to reach the tests too.
    //
    // The ids are passed in rather than captured, because the refresh below
    // invalidates them: see [`RefsTarget`].
    let build_refs = |conn: &rusqlite::Connection,
                      ids: &[i64]|
     -> Result<(Vec<queries::IncomingReference>, usize)> {
        let rollup = crate::resolve::rollup_incoming_references(
            conn,
            ids,
            relation_filter,
            min_confidence,
            false,
        )?;
        Ok((rollup.refs, rollup.confidence_filtered))
    };
    let (mut all_refs, mut conf_filtered) = build_refs(conn, &target_ids)?;
    let files: Vec<String> = all_refs.iter().map(|r| r.file_path.clone()).collect();
    let outcome = refresh_files_if_stale(&ctx.db, &ctx.project_root, &files);
    if outcome.any_changed {
        // SURF-16: the re-index just freed and reused node ids, so `target_ids`
        // may now point at whatever inherited them. Look the targets up again
        // BEFORE re-running the rollup — re-running it with the stale ids is
        // what answered `zeta_b` under the name `helper`.
        target_ids = target.resolve(conn, symbol)?;
        if target_ids.is_empty() {
            outcome.disclose();
            // Empty-JSON contract, same envelope as the other not-found exits.
            if json_mode {
                print_refs_notfound_json(symbol);
            }
            eprintln!(
                "[code-graph] '{}' is no longer in the re-indexed source — nothing to report.",
                symbol
            );
            std::process::exit(1);
        }
        let (a, c) = build_refs(conn, &target_ids)?;
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
                        crate::domain::display_node_name(&r.name),
                        r.file_path,
                        tag(&r.confidence)
                    )?;
                } else {
                    writeln!(
                        stdout,
                        "  [{}] {} ({}:{}){}",
                        r.relation,
                        crate::domain::display_node_name(&r.name),
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
