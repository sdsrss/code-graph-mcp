use super::*;

/// CLI arguments for the `similar` subcommand (audit #4 clap migration).
#[derive(Parser, Debug)]
#[command(
    name = "code-graph-mcp similar",
    about = "Find semantically similar code (requires embeddings)"
)]
pub struct SimilarArgs {
    /// Symbol name (required unless --node-id is given)
    pub symbol: Option<String>,
    /// Look up by node ID instead of name
    #[arg(long = "node-id")]
    pub node_id: Option<i64>,
    // clamp(1,100) stays in the handler; clap parse-errors (exit 2) on non-numeric.
    // `--limit` is a hidden alias so users who learned `--limit` from search /
    // ast-search / centrality don't hit a cryptic "unexpected argument" (mirrors
    // SearchArgs, where `--top-k` aliases `--limit`, and MCP semantic_code_search,
    // which accepts both `top_k` and `limit`).
    /// Number of results (default: 5, max: 100); alias: --limit
    #[arg(long = "top-k", alias = "limit")]
    pub top_k: Option<i64>,
    /// Max cosine distance (default: 0.8)
    #[arg(long = "max-distance")]
    pub max_distance: Option<f64>,
    /// JSON output
    #[arg(long)]
    pub json: bool,
}

/// Find semantically similar code.
/// CLI equivalent of MCP `find_similar_code`.
pub fn cmd_similar(project_root: &Path, args: SimilarArgs) -> Result<()> {
    let top_k: i64 = args.top_k.unwrap_or(5).clamp(1, 100);
    let max_distance: f64 = args.max_distance.unwrap_or(0.8);
    let json_mode = args.json;
    let node_id_arg: Option<i64> = args.node_id;

    // Open with vec support for vector search — but as a READER. `similar` is a
    // passive consumer: reaching for the indexer constructor (`open_with_vec`)
    // made it wipe a version-lagging index to 0 nodes with nothing rebuilding it,
    // and it was the one read command bypassing CliContext's worktree fallback.
    let ctx = CliContext::open_with_vec(project_root)?;
    let db = &ctx.db;
    let conn = db.conn();

    if !db.vec_enabled() {
        // Disclosure object, not `[]`. This is the CAPABILITY-missing case: a
        // bare array under `2>/dev/null` says "no similar code exists", when the
        // truth is that similarity could not be computed at all. Middle tier of
        // the three-tier JSON contract (feedback_cli_json_empty_contract).
        if json_mode {
            println!(
                "{}",
                serde_json::json!({
                    "results": [],
                    "unavailable": "vector search (sqlite-vec extension not loaded)",
                })
            );
        }
        eprintln!("[code-graph] Vector search not available (sqlite-vec extension not loaded).");
        eprintln!("  To enable: build with `cargo build --release --features embed-model`.");
        eprintln!("  Alternative: use `code-graph-mcp search <query>` for text-based similarity.");
        return Ok(());
    }

    // Resolve to node_id: by --node-id or by positional symbol name. `target_label`
    // is what we display in error messages — symbol name when resolved by name,
    // "node_id N" when resolved by --node-id.
    let (node_id, target_label) = if let Some(nid) = node_id_arg {
        // Validate existence up-front — BEFORE the embedding checks below. The
        // symbol path already validates (get_first_node_id_by_name); the --node-id
        // path used not to, so a missing id fell through to the embedded_count==0
        // guard and reported a misleading "No embeddings found" instead of the
        // true cause. This check is embedding-independent → reachable and testable
        // in the default (no embed-model) build, and mirrors refs --node-id.
        if queries::get_node_by_id(conn, nid)?.is_none() {
            if json_mode {
                println!(
                    "{}",
                    serde_json::json!({ "error": "node_id not found", "node_id": nid })
                );
            }
            eprintln!("[code-graph] node_id {} not found in index", nid);
            std::process::exit(1);
        }
        (nid, format!("node_id {}", nid))
    } else {
        let symbol = args.symbol.as_deref()
            .filter(|s| !s.is_empty())
            .map(strip_qualified_prefix)
            .ok_or_else(|| anyhow::anyhow!(
                "Usage: code-graph-mcp similar <symbol> [--node-id N] [--top-k N] [--max-distance N] [--json]"
            ))?;
        // Ambiguity FIRST, through the shared resolver — the same verdict
        // `callgraph`/`impact`/`refs` give. `get_first_node_id_by_name` alone
        // meant `similar new` silently answered about ONE arbitrary definition
        // out of five while `callgraph new` reported the five-way ambiguity: same
        // repo, same word, one surface guessing and not saying so. A silent wrong
        // answer is the worst shape this CLI can produce (2026-08-16 audit §四).
        // `--node-id` is the documented escape hatch and is handled above.
        if let Some(cands) = crate::resolve::detect_ambiguity(conn, symbol)? {
            emit_exact_ambiguity(symbol, &cands, json_mode);
        }
        match queries::get_first_node_id_by_name(conn, symbol)? {
            Some(id) => (id, symbol.to_string()),
            None => {
                if json_mode {
                    println!(
                        "{}",
                        serde_json::json!({ "error": "Symbol not found", "symbol": symbol })
                    );
                }
                // All-digit positional is almost certainly a node_id mistakenly passed
                // without the flag — guide the user instead of "Symbol not found: 1010".
                if !symbol.is_empty() && symbol.chars().all(|c| c.is_ascii_digit()) {
                    eprintln!(
                        "[code-graph] Symbol not found: {} \u{2014} did you mean `code-graph-mcp similar --node-id {}`?",
                        symbol, symbol
                    );
                } else {
                    eprintln!("[code-graph] Symbol not found: {}", symbol);
                    hint_symbol_maybe_unindexed(symbol);
                }
                std::process::exit(1);
            }
        }
    };

    // Check embedding exists
    let (embedded_count, total_nodes) = queries::count_nodes_with_vectors(conn)?;
    if embedded_count == 0 {
        // Empty-JSON contract: every --json exit path must emit parseable stdout
        // (feedback_cli_json_empty_contract.md). This path (vec extension present
        // but no embeddings generated yet) is the only one in cmd_similar that was
        // missing it — a consumer piping stdout got an empty string → parse error.
        if json_mode {
            println!(
                "{}",
                serde_json::json!({
                    "error": "No embeddings found",
                    "symbol": target_label,
                    "embedded_count": embedded_count,
                    "total_nodes": total_nodes,
                })
            );
        }
        eprintln!(
            "[code-graph] No embeddings found ({}/{} nodes embedded).",
            embedded_count, total_nodes
        );
        // Tailor the remedy to THIS binary: telling an embed-model build to
        // rebuild with --features embed-model sends the user to fix a problem
        // they don't have (the missing step is just running the MCP server).
        if cfg!(feature = "embed-model") {
            eprintln!("  To enable: start the MCP server to generate embeddings.");
        } else {
            eprintln!("  To enable: build with `cargo build --release --features embed-model`,");
            eprintln!("  then restart the MCP server to generate embeddings.");
        }
        eprintln!("  Alternative: use `code-graph-mcp search <query>` for text-based similarity.");
        std::process::exit(1);
    }

    let embedding: Vec<f32> = {
        let bytes = match queries::get_node_embedding(conn, node_id) {
            Ok(b) => b,
            Err(_) => {
                // Node exists (validated above) but this one has no embedding yet —
                // embeddings still generating. Empty-JSON contract: emit [] under
                // --json instead of bailing with empty stdout.
                if json_mode {
                    println!("[]");
                }
                eprintln!(
                    "[code-graph] No embedding for {} ({}/{} nodes embedded \u{2014} embeddings still generating; try again shortly or pick a node with `--node-id` from `show {}`).",
                    target_label, embedded_count, total_nodes, target_label
                );
                std::process::exit(1);
            }
        };
        bytemuck::cast_slice(&bytes).to_vec()
    };

    // Over-fetch so self-exclusion + max_distance + test/module post-filters don't
    // silently starve top_k (vec0 KNN can't pre-filter on joined node columns). Parity
    // with the MCP twin tool_find_similar_code; the old `top_k + 1` fell short on any drop.
    let fetch_count = crate::domain::similar_fetch_count(top_k);
    let raw_results = queries::vector_search(conn, &embedding, fetch_count)?;

    // Collect filtered results
    let mut similar: Vec<(queries::NodeResult, String, f64)> = Vec::new();
    for (id, distance) in &raw_results {
        if *id == node_id || *distance > max_distance {
            continue;
        }
        let Some(node) = queries::get_node_by_id(conn, *id)? else {
            continue;
        };
        let fp = queries::get_file_path(conn, node.file_id)?.unwrap_or_default();
        if crate::domain::is_skippable_result(node.is_test, &node.node_type, &node.name, &fp) {
            continue;
        }
        similar.push((node, fp, *distance));
        if similar.len() >= top_k as usize {
            break;
        }
    }

    // Observability: post-filters (max_distance + test/module) can shrink results below
    // top_k even with over-fetch. Surface to stderr; stdout JSON stays a bare array.
    let cutoff_dropped = raw_results
        .iter()
        .filter(|(id, dist)| *id != node_id && *dist > max_distance)
        .count();
    if (similar.len() as i64) < top_k && cutoff_dropped > 0 {
        eprintln!(
            "[code-graph] {} result(s) within max_distance={} (< top_k={}); {} nearer candidate(s) exceeded the cutoff. Raise --max-distance to widen.",
            similar.len(), max_distance, top_k, cutoff_dropped
        );
    }

    // Query-time freshness (shared with show/refs/… via refresh_files_if_stale):
    // re-index any displayed file edited since indexing so the printed
    // start_line/end_line are post-edit. NOTE: unlike the other read commands we do
    // NOT re-run the vector search afterward — `ensure_file_indexed` re-indexes with
    // model=None, dropping the touched nodes' embeddings until backfill, so a re-run
    // of vector_search would lose exactly the just-edited rows. Instead we patch the
    // line numbers in place by matching name+file in the refreshed index, preserving
    // the similarity ranking and set.
    let files: Vec<String> = similar.iter().map(|(_, fp, _)| fp.clone()).collect();
    let outcome = refresh_files_if_stale(db, &ctx.project_root, &files);
    if outcome.any_changed {
        for (node, fp, _) in similar.iter_mut() {
            if let Ok(fresh) = queries::get_nodes_by_file_path(conn, fp) {
                if let Some(m) = fresh
                    .iter()
                    .find(|n| n.name == node.name && n.qualified_name == node.qualified_name)
                {
                    node.start_line = m.start_line;
                    node.end_line = m.end_line;
                }
            }
        }
    }
    outcome.disclose();

    let mut stdout = std::io::stdout().lock();

    if similar.is_empty() {
        if json_mode {
            writeln!(stdout, "[]")?;
        }
        eprintln!(
            "[code-graph] No similar code found for node_id: {}",
            node_id
        );
        return Ok(());
    }

    if json_mode {
        let json_results: Vec<serde_json::Value> = similar.iter().map(|(node, fp, distance)| {
            let similarity = 1.0 / (1.0 + distance);
            serde_json::json!({
                "node_id": node.id, "name": node.name, "type": node.node_type, "file_path": fp,
                "start_line": node.start_line, "similarity": (similarity * 10000.0).round() / 10000.0,
                "distance": (distance * 10000.0).round() / 10000.0,
            })
        }).collect();
        writeln!(stdout, "{}", serde_json::to_string(&json_results)?)?;
        return Ok(());
    }

    for (node, fp, distance) in &similar {
        let similarity = 1.0 / (1.0 + distance);
        writeln!(
            stdout,
            "{:.1}%  {} {}  {}:{}-{}",
            similarity * 100.0,
            node.node_type,
            node.qualified_name.as_deref().unwrap_or(&node.name),
            fp,
            node.start_line,
            node.end_line
        )?;
    }

    Ok(())
}

// --- refs subcommand ---
