use anyhow::Result;
use clap::Parser;
use std::io::{self, BufRead, Read, Write};
use std::sync::{Arc, Mutex};

/// Newtype wrapper around `Arc<Mutex<io::Stdout>>` so both the main loop
/// and `McpServer::send_notification` share a single, mutex-protected handle.
struct SharedStdout(Arc<Mutex<io::Stdout>>);

/// Lock the shared stdout, recovering from mutex poison. The handle is shared with
/// background notification writes (the file watcher / startup tasks); if one panics
/// while holding the lock, the mutex is poisoned and a plain `.lock().unwrap()` on
/// the main loop's write paths (which sit OUTSIDE the per-request catch_unwind) would
/// panic and tear down the long-lived stdio session (H3). A poisoned `Stdout` has no
/// broken invariant, so recovering the guard is safe and keeps the session alive.
fn lock_stdout(m: &Mutex<io::Stdout>) -> std::sync::MutexGuard<'_, io::Stdout> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Write for SharedStdout {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        lock_stdout(&self.0).write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        lock_stdout(&self.0).flush()
    }
}

/// EPIPE on stdout (reader of `cg map | head` hung up) is a normal end of
/// conversation, not an error — grep and stats already exit 0 silently
/// (`test_cli_grep_sigpipe_graceful`), but every other command either panicked
/// inside `println!` or surfaced `Error: Broken pipe (os error 32)` through the
/// anyhow return path. Two central hooks extend the same contract to all
/// commands without routing hundreds of print sites through a macro:
/// this panic hook catches the `println!` shape, and `exit_zero_on_epipe`
/// catches the `?`-propagated shape.
fn install_stdout_epipe_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info.payload();
        let msg = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(|s| s.as_str()))
            .unwrap_or("");
        // std's exact stdout-print failure message: "failed printing to
        // stdout: <io error>". Scoped to stdout so a genuine stderr failure
        // (e.g. ENOSPC on a redirect) or an unrelated panic still reports
        // normally. The io-error rendering differs per platform: Unix says
        // "Broken pipe"; Windows renders ERROR_NO_DATA (232) / ERROR_BROKEN_PIPE
        // (109) with a FormatMessage-LOCALIZED message — but std's own
        // "(os error N)" suffix is not localized, so match the codes.
        if msg.starts_with("failed printing to stdout")
            && (msg.contains("Broken pipe")
                || msg.contains("os error 232")
                || msg.contains("os error 109"))
        {
            std::process::exit(0);
        }
        default_hook(info);
    }));
}

/// Err-path half of the EPIPE contract: a `?`-propagated `BrokenPipe` io::Error
/// anywhere in the chain means the consumer went away mid-write — exit 0
/// silently instead of printing an error nobody is reading.
fn exit_zero_on_epipe(result: &Result<()>) {
    if let Err(e) = result {
        let epipe = e
            .chain()
            .filter_map(|cause| cause.downcast_ref::<io::Error>())
            .any(|ioe| ioe.kind() == io::ErrorKind::BrokenPipe);
        if epipe {
            std::process::exit(0);
        }
    }
}

fn main() -> Result<()> {
    install_stdout_epipe_panic_hook();
    let args: Vec<String> = std::env::args().collect();
    let subcommand = args.get(1).map(|s| s.as_str());

    // Funnel visibility: a model-initiated CLI query IS the conversion the deny
    // hook works toward — record it (best-effort, never creates .code-graph/;
    // hook-internal answer runs carry CODE_GRAPH_INTERNAL=1 and are skipped).
    if let Some(cmd) = subcommand.and_then(code_graph_mcp::cli::canonical_query_cmd) {
        if let Ok(root) = code_graph_mcp::cli::resolve_project_root() {
            code_graph_mcp::cli::record_cli_use(&root, cmd);
        }
    }

    // CLI subcommands (everything except serve) get a stderr tracing subscriber
    // too, so warn!/error! from the indexer is visible — RUST_LOG overrides the
    // "warn" default (feedback_tracing_invisible_in_cli). serve installs its own
    // ("info") inside run_serve; help/version need no logging.
    if !matches!(
        subcommand,
        Some("serve") | None | Some("--help" | "-h" | "help") | Some("--version" | "-V")
    ) {
        init_tracing("warn");
    }

    let result = match subcommand {
        Some("serve") | None => run_serve(),
        Some("--help" | "-h" | "help") => {
            print_help();
            Ok(())
        }
        Some("--version" | "-V") => {
            print_version();
            Ok(())
        }
        Some("incremental-index") => {
            // clap-migrated (audit #4): flags via clap; the git/index guard below
            // stays in main() (must precede indexing side effects, may skip entirely).
            let idx_args =
                code_graph_mcp::cli::IncrementalIndexArgs::parse_from(args.iter().skip(1));
            let quiet = idx_args.quiet;
            let no_embed = idx_args.no_embed;
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            // Silent bail when the resolved root has neither a .git anchor nor an
            // existing index. Without this guard the PostToolUse hook would create
            // .code-graph/ in multi-repo workspace parents (issue #8).
            // Interactive runs get a helpful message so users know *why* nothing
            // happened — silent exit-0 was indistinguishable from a real index.
            let has_git = project_root.join(".git").exists();
            let has_index = project_root
                .join(code_graph_mcp::domain::CODE_GRAPH_DIR)
                .join("index.db")
                .exists();
            if !has_git && !has_index {
                if !quiet {
                    eprintln!(
                        "[code-graph] Skipping index: no .git anchor or existing .code-graph/ at {}.\n  \
                         Run `git init` first, or cd into a git repository.",
                        project_root.display()
                    );
                }
                // This leg exits 0 without indexing anything, so under --json it
                // would otherwise leave stdout at 0 bytes on a SUCCESSFUL run —
                // the tier-1 hole the empty-result contract exists to close. Same
                // keys as a real run, zeroed, plus the reason the counters are 0;
                // a consumer that only reads files_indexed still parses.
                if idx_args.json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "mode": "skipped",
                            "files_indexed": 0,
                            "files_deleted": 0,
                            "nodes_created": 0,
                            "edges_created": 0,
                            "files_with_parse_errors": 0,
                            "elapsed_ms": 0,
                            "skipped": format!(
                                "no .git anchor or existing .code-graph/ at {}",
                                project_root.display()
                            ),
                        })
                    );
                }
                return Ok(());
            }
            code_graph_mcp::cli::cmd_incremental_index_opts(
                &project_root,
                quiet,
                no_embed,
                idx_args.json,
            )
        }
        Some("rebuild-index") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            let rebuild_args =
                code_graph_mcp::cli::RebuildIndexArgs::parse_from(args.iter().skip(1));
            code_graph_mcp::cli::cmd_rebuild_index(&project_root, rebuild_args)
        }
        Some("reindex") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            let reindex_args = code_graph_mcp::cli::ReindexArgs::parse_from(args.iter().skip(1));
            code_graph_mcp::cli::cmd_reindex(&project_root, reindex_args)
        }
        Some("health-check") => {
            // clap-migrated (audit #4): --json/--format duality normalized via the
            // HealthCheckArgs::resolved_format shim (--json wins, else --format,
            // else oneline), keeping cmd_health_check's `format: &str` contract.
            let hc_args = code_graph_mcp::cli::HealthCheckArgs::parse_from(args.iter().skip(1));
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            code_graph_mcp::cli::cmd_health_check_opts(
                &project_root,
                hc_args.resolved_format(),
                hc_args.deep,
            )
        }
        Some("grep") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            // parse_grep_args normalizes attached context forms (`-A2` → `-A 2`)
            // before clap, so grep's attached numeric syntax works despite the
            // pattern positional's allow_hyphen_values (see normalize_grep_argv).
            let grep_args = code_graph_mcp::cli::parse_grep_args(&args);
            code_graph_mcp::cli::cmd_grep(&project_root, grep_args)
        }
        // MCP tool names (e.g. `semantic_code_search`) accepted as aliases for
        // their CLI short forms. Reason: MCP `instructions` and adopted memory
        // both reference tools by MCP name; agents copy-pasting the MCP name
        // into Bash should not hit "Unknown subcommand". Note: `search` runs
        // FTS5 only — MCP `semantic_code_search` adds vector+RRF fusion.
        Some("search" | "semantic_code_search") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            let search_args = code_graph_mcp::cli::SearchArgs::parse_from(args.iter().skip(1));
            code_graph_mcp::cli::cmd_search(&project_root, search_args)
        }
        Some("ast-search" | "ast_search") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            let ast_search_args =
                code_graph_mcp::cli::AstSearchArgs::parse_from(args.iter().skip(1));
            code_graph_mcp::cli::cmd_ast_search(&project_root, ast_search_args)
        }
        Some("callgraph" | "get_call_graph") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            let callgraph_args =
                code_graph_mcp::cli::CallgraphArgs::parse_from(args.iter().skip(1));
            code_graph_mcp::cli::cmd_callgraph(&project_root, callgraph_args)
        }
        Some("impact" | "impact_analysis") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            let impact_args = code_graph_mcp::cli::ImpactArgs::parse_from(args.iter().skip(1));
            code_graph_mcp::cli::cmd_impact(&project_root, impact_args)
        }
        Some("map" | "project_map") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            let map_args = code_graph_mcp::cli::MapArgs::parse_from(args.iter().skip(1));
            code_graph_mcp::cli::cmd_map(&project_root, map_args)
        }
        Some("tour") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            let tour_args = code_graph_mcp::cli::TourArgs::parse_from(args.iter().skip(1));
            code_graph_mcp::cli::cmd_tour(&project_root, tour_args)
        }
        Some("overview" | "module_overview") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            let overview_args = code_graph_mcp::cli::OverviewArgs::parse_from(args.iter().skip(1));
            code_graph_mcp::cli::cmd_overview(&project_root, overview_args)
        }
        Some("show" | "get_ast_node") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            let show_args = code_graph_mcp::cli::ShowArgs::parse_from(args.iter().skip(1));
            code_graph_mcp::cli::cmd_show(&project_root, show_args)
        }
        Some("trace" | "trace_http_chain") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            let trace_args = code_graph_mcp::cli::TraceArgs::parse_from(args.iter().skip(1));
            code_graph_mcp::cli::cmd_trace(&project_root, trace_args)
        }
        Some("deps" | "dependency_graph") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            let deps_args = code_graph_mcp::cli::DepsArgs::parse_from(args.iter().skip(1));
            code_graph_mcp::cli::cmd_deps(&project_root, deps_args)
        }
        Some("similar" | "find_similar_code") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            let similar_args = code_graph_mcp::cli::SimilarArgs::parse_from(args.iter().skip(1));
            code_graph_mcp::cli::cmd_similar(&project_root, similar_args)
        }
        Some("refs" | "find_references") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            let refs_args = code_graph_mcp::cli::RefsArgs::parse_from(args.iter().skip(1));
            code_graph_mcp::cli::cmd_refs(&project_root, refs_args)
        }
        Some("dead-code" | "find_dead_code") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            let dead_code_args = code_graph_mcp::cli::DeadCodeArgs::parse_from(args.iter().skip(1));
            code_graph_mcp::cli::cmd_dead_code(&project_root, dead_code_args)
        }
        Some("affected") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            let affected_args = code_graph_mcp::cli::AffectedArgs::parse_from(args.iter().skip(1));
            code_graph_mcp::cli::cmd_affected(&project_root, affected_args)
        }
        Some("centrality") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            let centrality_args =
                code_graph_mcp::cli::CentralityArgs::parse_from(args.iter().skip(1));
            code_graph_mcp::cli::cmd_centrality(&project_root, centrality_args)
        }
        Some("cycles") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            let cycles_args = code_graph_mcp::cli::CyclesArgs::parse_from(args.iter().skip(1));
            code_graph_mcp::cli::cmd_cycles(&project_root, cycles_args)
        }
        Some("surprising") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            let surprising_args =
                code_graph_mcp::cli::SurprisingArgs::parse_from(args.iter().skip(1));
            code_graph_mcp::cli::cmd_surprising(&project_root, surprising_args)
        }
        Some("report") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            let report_args = code_graph_mcp::cli::ReportArgs::parse_from(args.iter().skip(1));
            code_graph_mcp::cli::cmd_report(&project_root, report_args)
        }
        Some("benchmark") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            let bench_args = code_graph_mcp::cli::BenchmarkArgs::parse_from(args.iter().skip(1));
            code_graph_mcp::cli::cmd_benchmark(&project_root, bench_args)
        }
        Some("stats") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            // clap-migrated (audit #4): parse this subcommand's args via clap,
            // then dispatch. skip(1) drops argv[0]; clap treats the next token
            // (the subcommand/alias name) as the binary-name slot and skips it.
            let stats_args = code_graph_mcp::cli::StatsArgs::parse_from(args.iter().skip(1));
            code_graph_mcp::cli::cmd_stats(&project_root, stats_args)
        }
        Some("outcome") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            let outcome_args =
                code_graph_mcp::outcome::OutcomeArgs::parse_from(args.iter().skip(1));
            code_graph_mcp::outcome::cmd_outcome(&project_root, outcome_args)
        }
        Some("doctor") => {
            // doctor/adopt/unadopt are JS-dispatched and bypass clap, so `--help`
            // would otherwise RUN them — and doctor's default repairs rewrite
            // ~/.claude/settings.json (adopt rewrites the project CLAUDE.md managed
            // block). `--help`/`-h` must be side-effect-free, so intercept it
            // before run_node_script.
            if wants_subcommand_help(&args) {
                print!(
                    "code-graph-mcp doctor \u{2014} diagnose and repair environment issues\n\n\
                     USAGE:\n    code-graph-mcp doctor [--check-only]\n\n\
                     By default doctor repairs detected issues (re-registers hooks in\n\
                     ~/.claude/settings.json, fixes stale binary/model paths). Pass\n\
                     --check-only to report issues without changing anything.\n"
                );
                Ok(())
            } else {
                // Pass the tail through VERBATIM. This used to filter argv down to
                // the single literal `--check-only`, which silently DROPPED every
                // other token — so `code-graph-mcp doctor --check-onlyy` reached
                // doctor.js with an empty argv, parsed as "no flags", and ran the
                // full repair pass while the user believed they had asked for the
                // read-only mode. doctor.js validates its own flags and exits 2 on
                // an unknown one; run_node_script propagates that exit code.
                //
                // Third site of the same parsing. doctor.js was fixed first,
                // `lifecycle.js doctor` was found second — and this one, the
                // surface users actually install via npx / cargo install / the
                // plugin, was still on the old idiom. Whichever entry point you
                // fix, enumerate the others before claiming the flag is handled.
                run_node_script("doctor.js", &args[2..])
            }
        }
        Some("adopt") => {
            if wants_subcommand_help(&args) {
                print!(
                    "code-graph-mcp adopt \u{2014} install the code-graph steering block into the project CLAUDE.md\n\n\
                     USAGE:\n    code-graph-mcp adopt\n\n\
                     Writes a managed block into this project's CLAUDE.md plus the\n\
                     .claude/plugin_code_graph_mcp.md detail doc, so Claude Code\n\
                     auto-loads the decision table. Run `code-graph-mcp unadopt` to\n\
                     remove it.\n"
                );
                Ok(())
            } else {
                reject_extra_args("adopt", &args)?;
                run_node_script("adopt.js", &[])
            }
        }
        Some("unadopt") => {
            if wants_subcommand_help(&args) {
                print!(
                    "code-graph-mcp unadopt \u{2014} remove the code-graph steering block + detail doc\n\n\
                     USAGE:\n    code-graph-mcp unadopt\n\n\
                     Reverses `code-graph-mcp adopt`: strips the managed block from\n\
                     CLAUDE.md and deletes the .claude/plugin_code_graph_mcp.md detail\n\
                     doc. Content outside the managed block is kept.\n"
                );
                Ok(())
            } else {
                reject_extra_args("unadopt", &args)?;
                run_node_script("adopt.js", &["unadopt".to_string()])
            }
        }
        Some("snapshot") => {
            // clap-migrated (audit #4): nested #[command(subcommand)] replaces the
            // hand-rolled args[2]/args[3] dispatch. clap owns the no-subcommand and
            // unknown-subcommand errors (exit 2). `inspect` stays project-root-free.
            let snapshot_args = code_graph_mcp::cli::SnapshotArgs::parse_from(args.iter().skip(1));
            match snapshot_args.command {
                code_graph_mcp::cli::SnapshotCommand::Create(create_args) => {
                    let project_root = code_graph_mcp::cli::resolve_project_root()?;
                    code_graph_mcp::cli::cmd_snapshot_create(&project_root, create_args)
                }
                code_graph_mcp::cli::SnapshotCommand::Inspect(inspect_args) => {
                    code_graph_mcp::cli::cmd_snapshot_inspect(inspect_args)
                }
            }
        }
        Some(other) => {
            eprintln!("Unknown subcommand: {}", other);
            if let Some(suggestion) = suggest_subcommand(other) {
                eprintln!("Did you mean '{}'?", suggestion);
            }
            eprintln!("Run 'code-graph-mcp --help' for available commands.");
            std::process::exit(1);
        }
    };

    // Tier-3 empty contract, error leg (audit 2026-08-02 P1-7): with --json,
    // ANY pre-handler bail (no index yet, path outside root, open failure)
    // used to leave stdout at 0 bytes — a machine consumer got a JSON parse
    // failure instead of an error object, on the single most common error
    // path a fresh checkout hits. One catch here covers every command and
    // every future bail site; commands that already emitted their own JSON
    // error object exit via std::process::exit and never reach this.
    // stderr keeps the human-readable line via anyhow's Termination below.
    // EPIPE check must run BEFORE the JSON error leg: the consumer is gone, so
    // emitting the error object would itself hit the closed pipe (and panic).
    exit_zero_on_epipe(&result);
    if let Err(e) = &result {
        if args.iter().skip(2).any(|a| a == "--json") {
            println!("{}", serde_json::json!({ "error": format!("{e:#}") }));
        }
    }
    result
}

fn print_version() {
    println!("code-graph-mcp {}", env!("CARGO_PKG_VERSION"));
}

/// True if a JS-dispatched subcommand was invoked with `--help`/`-h`. Skips
/// argv[0] (binary) and argv[1] (subcommand name) so only the subcommand's own
/// flags are inspected. Lets doctor/adopt/unadopt honor `--help` without running
/// their side effects (settings.json / CLAUDE.md managed-block rewrites).
fn wants_subcommand_help(args: &[String]) -> bool {
    args.iter().skip(2).any(|a| a == "--help" || a == "-h")
}

/// Refuse a flag-taking-no-flags subcommand any argument beyond `--help`.
///
/// `adopt` / `unadopt` are JS-dispatched and `adopt.js` reads only `argv[2]` as
/// the action — it parses no flags at all, so anything else was silently
/// discarded and the command ran. `code-graph-mcp adopt --helpp` therefore WROTE
/// the user's CLAUDE.md, one keystroke from the side effect the `--help`
/// interception exists to prevent.
///
/// Fifth site of this idiom, found by the pre-tag review after four others were
/// closed: doctor.js, `lifecycle.js doctor`, main.rs's own doctor arm, and
/// `bin/cli.js`. The npm/npx surface was already covered by bin/cli.js; this is
/// the `cargo install` / direct-binary path.
///
/// Passing the tail through to adopt.js is NOT the fix here — it would make
/// `--helpp` the action argument.
fn reject_extra_args(sub: &str, args: &[String]) -> Result<()> {
    let extra: Vec<&String> = args.iter().skip(2).collect();
    if !extra.is_empty() {
        eprintln!(
            "code-graph-mcp {sub}: unknown argument(s): {}",
            extra
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        );
        eprintln!("Usage: code-graph-mcp {sub}");
        std::process::exit(2);
    }
    Ok(())
}

fn print_help() {
    print_version();
    println!("AST-based code graph with semantic search\n");
    println!("USAGE:");
    println!("    code-graph-mcp [COMMAND]\n");
    println!("COMMANDS:");
    println!("    serve               Start MCP JSON-RPC server on stdio (default)");
    println!("    grep <pattern> [path]");
    println!("                        AST-context grep (ripgrep + containing function/class)");
    println!("    search <query>      FTS5 text search by concept (CLI is FTS-only;");
    println!("                        MCP `semantic_code_search` adds vector+RRF fusion)");
    println!("    ast-search [query]  Structured search with --type/--returns/--params filters");
    println!("    callgraph <symbol>  Show call graph (callers/callees)");
    println!("    impact <symbol>     Impact analysis (callers, routes, risk level)");
    println!(
        "    affected [files...] Changed files → test files to re-run (--stdin, --depth, --json)"
    );
    println!("    show <symbol>       Show symbol details (code, type, signature)");
    println!("    map                 Project architecture map (modules, deps, entry points)");
    println!("    tour [path]         Dependency-ordered reading order (where to start reading a repo/subtree)");
    println!("    overview <path>     Module overview (symbols grouped by file and type)");
    println!("    deps <file>         File-level dependency graph");
    println!("    trace <route>       Trace HTTP route → handler → downstream calls");
    println!("    similar <symbol>    Find semantically similar code (requires embeddings)");
    println!("    refs <symbol>       Find all references to a symbol (callers, importers, etc.)");
    println!("    dead-code [path]    Find unused code (orphans and exported-unused symbols)");
    println!(
        "    centrality          Rank architectural chokepoints (betweenness over the call graph)"
    );
    println!("    cycles              Detect circular import dependencies (file-level)");
    println!("    surprising          Surface unexpected cross-module couplings (uncertain edges)");
    println!("    report              Consolidated code-health report (summary + all analyses)");
    println!("    incremental-index   Run incremental index update");
    println!(
        "    rebuild-index       Drop and rebuild the index from scratch (requires --confirm)"
    );
    println!("    reindex [--from-snapshot]");
    println!("                        Incremental index refresh. With --from-snapshot, drop the");
    println!("                        index and refetch the published snapshot (full rebuild if");
    println!(
        "                        unavailable). For an unconditional rebuild use rebuild-index."
    );
    println!("    health-check        Query index status");
    println!("                        (Note: file watcher start/stop is MCP-only — see start_watch/stop_watch tools)");
    println!("    doctor              Diagnose and repair environment issues");
    println!("    benchmark           Benchmark index speed, query latency, token savings");
    println!("    stats               Aggregate session metrics from .code-graph/usage.jsonl");
    println!("                        (which tools you used, search/index activity)");
    println!("    outcome             Retrieval adoption from session transcripts (field-MRR; read-only)");
    println!("    adopt               Install the steering block into the project CLAUDE.md + detail doc");
    println!("    unadopt             Remove the steering block + detail doc");
    println!("    snapshot create --out <path> [--include-embeddings] [--root <dir>] [--quiet]");
    println!("                        Build a portable graph snapshot. Auto zstd-compresses");
    println!("                        when --out ends in .db.zst; otherwise writes raw .db");
    println!("    snapshot inspect <file>");
    println!("                        Print snapshot metadata as JSON (accepts .db or .db.zst)\n");
    println!("OPTIONS:");
    println!("    --json              JSON output (query/analysis commands; not the index");
    println!("                        commands, doctor, adopt or unadopt)");
    println!(
        "    --compact           Compact output (search, callgraph, map, overview, deps, refs)"
    );
    println!("    --limit N           Limit results (search/ast-search default: 20; centrality default: 15; similar default: 5, alias of --top-k)");
    println!("    --language <lang>   Filter by language (search)");
    println!("    --node-type <type>  Filter by node type (search)");
    println!("    --type <type>       Filter by node type: fn, class, struct, enum, trait, ...");
    println!("    --returns <type>    Filter by return type (ast-search)");
    println!("    --params <text>     Filter by parameter text (ast-search)");
    println!(
        "    --direction <dir>   callers|callees|both (callgraph), outgoing|incoming|both (deps)"
    );
    println!("    --depth N           Max traversal depth (callgraph, impact, deps; default: 3)");
    println!(
        "    --file <path>       Disambiguate same-name symbols (callgraph, impact, show, refs)"
    );
    println!("    --node-id N         Lookup by node ID (show, similar)");
    println!("    --change-type <t>   signature, behavior, or remove (impact; default: behavior)");
    println!("    --include-tests     Show test callers / include test symbols (callgraph, show, centrality; hidden by default)");
    println!("    --refs              Show callers/callees (show; alias: --include-refs)");
    println!("    --impact            Show impact summary (show; alias: --include-impact)");
    println!("    --context-lines N   Surrounding source lines (show; default: 0)");
    println!("    --min-lines N       Min lines to report (dead-code; default: 3)");
    println!("    --ignore <prefix>   Exclude path prefix (dead-code; repeatable; default: claude-plugin/, benches/)");
    println!("    --no-ignore         Disable default --ignore prefixes (dead-code)");
    println!("    --relation <type>   Filter: calls, imports, inherits, implements (refs)");
    println!("    --min-confidence <t> Min edge confidence: extracted|inferred|ambiguous (refs; default: all)");
    println!("    --last N            Limit to last N sessions (stats; default: all)");
    println!(
        "    --deep              Run PRAGMA quick_check regardless of index size (health-check)"
    );
    println!(
        "    --force             Replace the index even while another process holds the index lock"
    );
    println!("                        (rebuild-index, reindex --from-snapshot; that process's pending writes are lost)");
    println!("    -h, --help          Show this help message");
    println!("    -V, --version       Show version");
}

/// Install a global stderr tracing subscriber (idempotent via `try_init`).
/// `RUST_LOG` overrides `default_level`. CLI subcommands call this with "warn"
/// so indexer warnings (parse-skip counts, FTS5-only fallback, embedding
/// dim-mismatch wipe) are visible — previously only `run_serve` installed a
/// subscriber, so every tracing event on a CLI path was silently dropped
/// (feedback_tracing_invisible_in_cli). serve keeps the "info" default.
fn init_tracing(default_level: &str) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_level)),
        )
        .with_writer(io::stderr)
        .try_init();
}

fn run_serve() -> Result<()> {
    init_tracing("info");

    // P0.1 — non-project cwd guard (Rust counterpart to mcp-launcher.js's
    // isNonProjectCwd gate). When the binary is invoked directly — bypassing the
    // JS launcher, e.g. a dev `.mcp.json` or a global MCP config pointing at the
    // binary — in a dir with no project marker, serve a 0-tool stub: no database,
    // no embedding model, no `.code-graph/`, no NOISY instructions. Otherwise the
    // plugin half-activates in throwaway dirs (the ~2035 headless /tmp `claude -p`
    // calls). CODE_GRAPH_FORCE_PLUGIN_MCP=1 overrides, same as the launcher.
    let force_plugin = std::env::var("CODE_GRAPH_FORCE_PLUGIN_MCP").ok().as_deref() == Some("1");
    let cwd = std::env::current_dir()?;
    if !force_plugin && code_graph_mcp::cli::is_non_project_cwd(&cwd) {
        eprintln!(
            "[code-graph] non-project cwd (no .git/manifest); serving 0 tools, \
             no index created. Set CODE_GRAPH_FORCE_PLUGIN_MCP=1 to override."
        );
        let stdin = io::stdin();
        let stdout = io::stdout();
        code_graph_mcp::cli::serve_non_project_stub(stdin.lock(), stdout.lock())?;
        return Ok(());
    }

    let project_root = code_graph_mcp::cli::resolve_project_root()?;
    let server = code_graph_mcp::mcp::server::McpServer::from_project_root(&project_root)?;
    let session_start = std::time::Instant::now();

    tracing::info!(
        "[session] Started v{}, project: {}",
        env!("CARGO_PKG_VERSION"),
        project_root.display()
    );

    // Shared stdout handle: prevents interleaved JSON when background threads
    // send notifications concurrently with the main loop writing responses.
    let stdout_shared = Arc::new(Mutex::new(io::stdout()));

    // Enable MCP progress/log notifications via the same shared handle
    server.set_notify_writer(Box::new(SharedStdout(Arc::clone(&stdout_shared))));

    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let mut byte_buf: Vec<u8> = Vec::new();
    const MAX_MESSAGE_SIZE: usize = 10 * 1024 * 1024; // 10MB

    loop {
        byte_buf.clear();
        // Read raw bytes, not `read_line`: when a message's 10 MiB `take` boundary
        // splits a multi-byte UTF-8 char, `read_line`'s UTF-8 validation returns
        // Err(InvalidData) and the `?` propagates OUTSIDE the per-request
        // catch_unwind below — a single oversized CJK request would kill the whole
        // long-lived session (H3). `read_until` + lossy decode tolerate it.
        let n = reader
            .by_ref()
            .take(MAX_MESSAGE_SIZE as u64)
            .read_until(b'\n', &mut byte_buf)?;
        if n == 0 {
            break; // EOF
        }
        // Oversized: hit the `take` cap with no terminating newline. Drain the rest
        // of the line, reject with a JSON-RPC error, and keep serving. Checked on
        // the raw byte buffer before decoding to avoid a huge lossy allocation.
        if byte_buf.len() >= MAX_MESSAGE_SIZE && byte_buf.last() != Some(&b'\n') {
            let oversized_len = byte_buf.len();
            // Free the oversized buffer before draining to avoid 2x peak allocation
            byte_buf.clear();
            byte_buf.shrink_to(1024);
            // Drain until newline (line-aware), discarding the bytes. LOOP: a
            // single `take(MAX)` only consumes one MAX-sized chunk, so a line
            // larger than 2xMAX would leave a tail that gets misparsed as the
            // next message. Keep reading MAX-sized chunks until the terminating
            // newline is consumed or EOF is reached, so arbitrarily large lines
            // are fully drained.
            let mut sink: Vec<u8> = Vec::new();
            loop {
                sink.clear();
                let drained = reader
                    .by_ref()
                    .take(MAX_MESSAGE_SIZE as u64)
                    .read_until(b'\n', &mut sink)
                    .unwrap_or(0);
                // EOF (nothing left) or we consumed through the newline → done.
                if drained == 0 || sink.last() == Some(&b'\n') {
                    break;
                }
            }
            let err_resp = serde_json::json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": {
                    "code": code_graph_mcp::mcp::protocol::JSONRPC_INVALID_REQUEST,
                    "message": format!("Message too large: {} bytes (max {})", oversized_len, MAX_MESSAGE_SIZE)
                }
            });
            {
                let mut out = lock_stdout(&stdout_shared);
                writeln!(out, "{}", err_resp)?;
                out.flush()?;
            }
            continue;
        }

        // Lossily decode: a `take`-truncated or otherwise malformed multi-byte
        // sequence becomes U+FFFD rather than killing the session (H3). Well-formed
        // JSON-RPC lines are unaffected.
        let buf = String::from_utf8_lossy(&byte_buf);
        if buf.trim().is_empty() {
            continue;
        }

        // Isolate per-request panics: a single handler panic must not tear down the
        // long-lived stdio session. catch_unwind converts it to a JSON-RPC internal
        // error (request id unknown post-unwind) so the loop continues. No
        // currently-reachable panic on the request path — defense-in-depth.
        let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            server.handle_message(&buf)
        })) {
            Ok(r) => r,
            Err(panic) => {
                let msg = panic
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "handler panicked".to_string());
                tracing::error!("Panic handling message: {}", msg);
                Ok(Some(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": null,
                        "error": {
                            "code": code_graph_mcp::mcp::protocol::JSONRPC_INTERNAL_ERROR,
                            "message": format!("Internal error (panic): {}", msg)
                        }
                    })
                    .to_string(),
                ))
            }
        };

        match result {
            Ok(Some(response)) => {
                let mut out = lock_stdout(&stdout_shared);
                writeln!(out, "{}", response)?;
                out.flush()?;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!("Error handling message: {}", e);
                let err_resp = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": {
                        "code": code_graph_mcp::mcp::protocol::JSONRPC_INTERNAL_ERROR,
                        "message": format!("Internal error: {}", e)
                    }
                });
                let mut out = lock_stdout(&stdout_shared);
                writeln!(out, "{}", err_resp)?;
                out.flush()?;
            }
        }

        // Run startup indexing + auto-watch if triggered by notifications/initialized.
        // Isolated behind catch_unwind, mirroring the per-request guard above:
        // run_startup_tasks holds the most panic-prone code (indexing, watcher
        // spawn) yet executes every loop iteration OUTSIDE the request guard, so
        // an unwinding panic here would tear down the long-lived session. These
        // are background housekeeping tasks — on panic, log (dual-write) and keep
        // serving; the in-flight request was already answered above.
        if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            server.run_startup_tasks();
        })) {
            let msg = panic
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "startup task panicked".to_string());
            eprintln!("[code-graph] Panic in startup tasks (continuing): {}", msg);
            tracing::error!("Panic in startup tasks (continuing): {}", msg);
        }
    }

    server.flush_metrics();
    tracing::info!(
        "[session] Ended after {:.0}s",
        session_start.elapsed().as_secs_f64()
    );

    Ok(())
}

const SUBCOMMANDS: &[&str] = &[
    "serve",
    "grep",
    "search",
    "ast-search",
    "callgraph",
    "impact",
    "show",
    "map",
    "overview",
    "deps",
    "trace",
    "similar",
    "refs",
    "dead-code",
    "incremental-index",
    "rebuild-index",
    "reindex",
    "health-check",
    "doctor",
    "centrality",
    "cycles",
    "surprising",
    "report",
    "benchmark",
    "stats",
    "outcome",
    "adopt",
    "unadopt",
    "snapshot",
    // MCP tool names accepted as aliases (see dispatch above). Listed here so
    // typo-suggester picks the closer alias for inputs like "project_mapp".
    "project_map",
    "module_overview",
    "get_ast_node",
    "find_references",
    "get_call_graph",
    "impact_analysis",
    "find_similar_code",
    "dependency_graph",
    "trace_http_chain",
    "find_dead_code",
    "ast_search",
    "semantic_code_search",
];

/// Locate and exec a node script under claude-plugin/scripts/.
/// Searches both dev (target/release/) and installed (npm package) layouts.
///
/// SAFETY: `script` MUST be a hard-coded literal. Never pass user input —
/// the value is concatenated into a filesystem path and exec'd via node.
fn run_node_script(script: &str, extra_args: &[String]) -> Result<()> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    // Lookup order:
    //   1. $_FIND_BINARY_ROOT (set by bin/cli.js npm wrapper → main pkg root)
    //   2. exe_dir/../../claude-plugin/scripts/  (dev mode: target/release/)
    //   3. exe_dir/../claude-plugin/scripts/     (legacy fallback)
    //
    // Rationale: npm platform-pkg layout keeps the binary in
    // node_modules/@sdsrs/code-graph-<plat>/ but claude-plugin/ lives in the
    // sibling main pkg node_modules/@sdsrs/code-graph/. Relative-from-exe
    // cannot reach it; env var set by cli.js bridges the two.
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(root) = std::env::var("_FIND_BINARY_ROOT") {
        candidates.push(
            std::path::PathBuf::from(root)
                .join("claude-plugin")
                .join("scripts")
                .join(script),
        );
    }
    candidates.push(exe_dir.join(format!("../../claude-plugin/scripts/{}", script)));
    candidates.push(exe_dir.join(format!("../claude-plugin/scripts/{}", script)));

    for candidate in &candidates {
        if candidate.exists() {
            let mut cmd = std::process::Command::new("node");
            cmd.arg(candidate);
            for a in extra_args {
                cmd.arg(a);
            }
            let status = cmd.status().map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    anyhow::anyhow!("Node.js not found. Install Node.js to use this subcommand.")
                } else {
                    e.into()
                }
            })?;
            std::process::exit(status.code().unwrap_or(1));
        }
    }

    eprintln!("{} not found. Looked in:", script);
    for c in &candidates {
        eprintln!("  {}", c.display());
    }
    eprintln!("Tip: set _FIND_BINARY_ROOT to the main npm pkg dir, or run directly: node claude-plugin/scripts/{}", script);
    std::process::exit(1);
}

fn suggest_subcommand(input: &str) -> Option<&'static str> {
    let input_lower = input.to_lowercase();
    let mut best: Option<(&str, usize)> = None;
    for &cmd in SUBCOMMANDS {
        let d = levenshtein_small(&input_lower, cmd);
        let threshold = if cmd.len() <= 4 { 1 } else { 2 };
        if d <= threshold && (best.is_none() || d < best.unwrap().1) {
            best = Some((cmd, d));
        }
    }
    best.map(|(cmd, _)| cmd)
}

fn levenshtein_small(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (m, n) = (a.len(), b.len());
    let mut prev = (0..=n).collect::<Vec<_>>();
    let mut curr = vec![0; n + 1];
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

#[cfg(test)]
mod tests {
    use super::*;

    // L4: the shared stdout handle is written from background notification threads;
    // if one panics while holding the lock, the mutex is poisoned and the main loop's
    // write paths (outside the per-request catch_unwind) would panic on `.unwrap()`
    // and kill the long-lived session. lock_stdout must recover the guard instead.
    #[test]
    fn lock_stdout_recovers_from_poisoned_mutex() {
        let m = Arc::new(Mutex::new(io::stdout()));
        let m2 = Arc::clone(&m);
        // Poison the mutex: a thread panics while holding the lock. join() swallows
        // the panic so the test process survives.
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("poison the stdout mutex");
        })
        .join();
        assert!(
            m.lock().is_err(),
            "mutex must be poisoned for this test to mean anything"
        );
        // The plain `.lock().unwrap()` the old code used would panic here; lock_stdout
        // recovers the guard and returns normally.
        let _guard = lock_stdout(&m);
    }
}
