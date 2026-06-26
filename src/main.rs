use anyhow::Result;
use clap::Parser;
use std::io::{self, BufRead, Read, Write};
use std::sync::{Arc, Mutex};

/// Newtype wrapper around `Arc<Mutex<io::Stdout>>` so both the main loop
/// and `McpServer::send_notification` share a single, mutex-protected handle.
struct SharedStdout(Arc<Mutex<io::Stdout>>);

impl Write for SharedStdout {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.lock().unwrap().flush()
    }
}

fn main() -> Result<()> {
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

    match subcommand {
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
            let idx_args = code_graph_mcp::cli::IncrementalIndexArgs::parse_from(args.iter().skip(1));
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
                return Ok(());
            }
            code_graph_mcp::cli::cmd_incremental_index(&project_root, quiet, no_embed)
        }
        Some("rebuild-index") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            let rebuild_args = code_graph_mcp::cli::RebuildIndexArgs::parse_from(args.iter().skip(1));
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
            code_graph_mcp::cli::cmd_health_check(&project_root, hc_args.resolved_format())
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
            let ast_search_args = code_graph_mcp::cli::AstSearchArgs::parse_from(args.iter().skip(1));
            code_graph_mcp::cli::cmd_ast_search(&project_root, ast_search_args)
        }
        Some("callgraph" | "get_call_graph") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            let callgraph_args = code_graph_mcp::cli::CallgraphArgs::parse_from(args.iter().skip(1));
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
            let centrality_args = code_graph_mcp::cli::CentralityArgs::parse_from(args.iter().skip(1));
            code_graph_mcp::cli::cmd_centrality(&project_root, centrality_args)
        }
        Some("cycles") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            let cycles_args = code_graph_mcp::cli::CyclesArgs::parse_from(args.iter().skip(1));
            code_graph_mcp::cli::cmd_cycles(&project_root, cycles_args)
        }
        Some("surprising") => {
            let project_root = code_graph_mcp::cli::resolve_project_root()?;
            let surprising_args = code_graph_mcp::cli::SurprisingArgs::parse_from(args.iter().skip(1));
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
        Some("doctor") => {
            // doctor/adopt/unadopt are JS-dispatched and bypass clap, so `--help`
            // would otherwise RUN them — and doctor's default repairs rewrite
            // ~/.claude/settings.json (adopt rewrites MEMORY.md). `--help`/`-h`
            // must be side-effect-free, so intercept it before run_node_script.
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
                run_node_script("doctor.js", &args.iter().filter(|a| a.as_str() == "--check-only").cloned().collect::<Vec<_>>())
            }
        }
        Some("adopt") => {
            if wants_subcommand_help(&args) {
                print!(
                    "code-graph-mcp adopt \u{2014} install the code-graph memory file + MEMORY.md sentinel\n\n\
                     USAGE:\n    code-graph-mcp adopt\n\n\
                     Writes plugin_code_graph_mcp.md and a sentinel block into this\n\
                     project's ~/.claude memory so Claude Code auto-loads the decision\n\
                     table. Run `code-graph-mcp unadopt` to remove it.\n"
                );
                Ok(())
            } else {
                run_node_script("adopt.js", &[])
            }
        }
        Some("unadopt") => {
            if wants_subcommand_help(&args) {
                print!(
                    "code-graph-mcp unadopt \u{2014} remove the code-graph memory file + sentinel\n\n\
                     USAGE:\n    code-graph-mcp unadopt\n\n\
                     Reverses `code-graph-mcp adopt`: deletes the memory file and the\n\
                     MEMORY.md sentinel block. User content outside the sentinel is kept.\n"
                );
                Ok(())
            } else {
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
    }
}

fn print_version() {
    println!("code-graph-mcp {}", env!("CARGO_PKG_VERSION"));
}

/// True if a JS-dispatched subcommand was invoked with `--help`/`-h`. Skips
/// argv[0] (binary) and argv[1] (subcommand name) so only the subcommand's own
/// flags are inspected. Lets doctor/adopt/unadopt honor `--help` without running
/// their side effects (settings.json / MEMORY.md rewrites).
fn wants_subcommand_help(args: &[String]) -> bool {
    args.iter().skip(2).any(|a| a == "--help" || a == "-h")
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
    println!("    affected [files...] Changed files → test files to re-run (--stdin, --depth, --json)");
    println!("    show <symbol>       Show symbol details (code, type, signature)");
    println!("    map                 Project architecture map (modules, deps, entry points)");
    println!("    overview <path>     Module overview (symbols grouped by file and type)");
    println!("    deps <file>         File-level dependency graph");
    println!("    trace <route>       Trace HTTP route → handler → downstream calls");
    println!("    similar <symbol>    Find semantically similar code (requires embeddings)");
    println!("    refs <symbol>       Find all references to a symbol (callers, importers, etc.)");
    println!("    dead-code [path]    Find unused code (orphans and exported-unused symbols)");
    println!("    centrality          Rank architectural chokepoints (betweenness over the call graph)");
    println!("    cycles              Detect circular import dependencies (file-level)");
    println!("    surprising          Surface unexpected cross-module couplings (uncertain edges)");
    println!("    report              Consolidated code-health report (summary + all analyses)");
    println!("    incremental-index   Run incremental index update");
    println!("    rebuild-index       Drop and rebuild the index from scratch (requires --confirm)");
    println!("    reindex [--from-snapshot]");
    println!("                        Reset index; with --from-snapshot, refetches the");
    println!("                        published snapshot (else falls back to full index)");
    println!("    health-check        Query index status");
    println!("                        (Note: file watcher start/stop is MCP-only — see start_watch/stop_watch tools)");
    println!("    doctor              Diagnose and repair environment issues");
    println!("    benchmark           Benchmark index speed, query latency, token savings");
    println!("    stats               Aggregate session metrics from .code-graph/usage.jsonl");
    println!("                        (which tools you used, search/index activity)");
    println!("    adopt               Install plugin_code_graph_mcp.md memory + MEMORY.md sentinel");
    println!("    unadopt             Remove the memory file + sentinel block");
    println!("    snapshot create --out <path> [--include-embeddings] [--root <dir>] [--quiet]");
    println!("                        Build a portable graph snapshot. Auto zstd-compresses");
    println!("                        when --out ends in .db.zst; otherwise writes raw .db");
    println!("    snapshot inspect <file>");
    println!("                        Print snapshot metadata as JSON (accepts .db or .db.zst)\n");
    println!("OPTIONS:");
    println!("    --json              JSON output (available on all commands)");
    println!("    --compact           Compact output (search, callgraph, map, overview, deps, refs)");
    println!("    --limit N           Limit results (search, ast-search default: 20; centrality default: 15)");
    println!("    --language <lang>   Filter by language (search)");
    println!("    --node-type <type>  Filter by node type (search)");
    println!("    --type <type>       Filter by node type: fn, class, struct, enum, trait, ...");
    println!("    --returns <type>    Filter by return type (ast-search)");
    println!("    --params <text>     Filter by parameter text (ast-search)");
    println!("    --direction <dir>   callers|callees|both (callgraph), outgoing|incoming|both (deps)");
    println!("    --depth N           Max traversal depth (callgraph, impact, deps; default: 3)");
    println!("    --file <path>       Disambiguate same-name symbols (callgraph, impact, show, refs)");
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
    // Multi-project mode: CODE_GRAPH_PROJECTS=alias1=/path1:alias2=/path2
    // When set, bypass the cwd guard (explicit config implies intent) and route
    // tool calls by the `project` parameter instead of using cwd.
    let multi_project_registry =
        code_graph_mcp::mcp::server::registry::ProjectRegistry::from_env()?;

    let force_plugin = std::env::var("CODE_GRAPH_FORCE_PLUGIN_MCP").ok().as_deref() == Some("1")
        || multi_project_registry.is_some();
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

    // In multi-project mode, the registry already holds a McpServer per project
    // (each with its own DB, index lock, watcher). We take ownership of the
    // default project's server as the "outer" server that runs the stdio loop,
    // and attach the full registry for tool-call routing. This avoids the
    // double-initialization bug where a second from_project_root() for the same
    // path would lose the flock → become secondary → ensure_indexed() no-ops.
    // Shared stdout — used by both the serve loop and notification writers.
    let stdout_shared = Arc::new(Mutex::new(io::stdout()));

    let (server, session_start, project_display) = if let Some(mut registry) = multi_project_registry {
        let default_root = registry.default_root().to_path_buf();
        let display = default_root.display().to_string();
        let start = std::time::Instant::now();

        // Wire stdout notifications to ALL registry servers (fixes silent
        // progress drops on delegated projects).
        let stdout_clone = Arc::clone(&stdout_shared);
        registry.set_notify_writers(move || Box::new(SharedStdout(Arc::clone(&stdout_clone))));

        // Take the default project's server out of the registry and use it as
        // the outer server. The registry (now missing one entry for the default)
        // is attached so tool calls with explicit `project` param still route.
        // Calls without `project` go to the outer server directly — which IS the
        // default project's primary server, holding the flock.
        let mut srv = registry.take_default()?;
        srv.set_notify_writer(Box::new(SharedStdout(Arc::clone(&stdout_shared))));
        srv.attach_registry(registry);
        (srv, start, display)
    } else {
        let project_root = code_graph_mcp::cli::resolve_project_root()?;
        let srv = code_graph_mcp::mcp::server::McpServer::from_project_root(&project_root)?;
        let display = project_root.display().to_string();
        let start = std::time::Instant::now();
        srv.set_notify_writer(Box::new(SharedStdout(Arc::clone(&stdout_shared))));
        (srv, start, display)
    };

    tracing::info!("[session] Started v{}, project: {}", env!("CARGO_PKG_VERSION"), project_display);

    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let mut buf = String::new();
    const MAX_MESSAGE_SIZE: usize = 10 * 1024 * 1024; // 10MB

    loop {
        buf.clear();
        let n = reader.by_ref().take(MAX_MESSAGE_SIZE as u64).read_line(&mut buf)?;
        if n == 0 {
            break; // EOF
        }
        if buf.trim().is_empty() {
            continue;
        }
        if buf.len() >= MAX_MESSAGE_SIZE && !buf.ends_with('\n') {
            let oversized_len = buf.len();
            let needs_drain = !buf.ends_with('\n');
            // Free the oversized buffer before draining to avoid 2x peak allocation
            buf.clear();
            buf.shrink_to(1024);
            if needs_drain {
                // Drain until newline (line-aware) without UTF-8 String allocation
                let _ = reader.by_ref().take(MAX_MESSAGE_SIZE as u64)
                    .read_until(b'\n', &mut Vec::new());
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
                let mut out = stdout_shared.lock().unwrap();
                writeln!(out, "{}", err_resp)?;
                out.flush()?;
            }
            continue;
        }

        match server.handle_message(&buf) {
            Ok(Some(response)) => {
                let mut out = stdout_shared.lock().unwrap();
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
                let mut out = stdout_shared.lock().unwrap();
                writeln!(out, "{}", err_resp)?;
                out.flush()?;
            }
        }

        // Run startup indexing + auto-watch if triggered by notifications/initialized
        server.run_startup_tasks();
    }

    server.flush_metrics();
    tracing::info!("[session] Ended after {:.0}s", session_start.elapsed().as_secs_f64());

    Ok(())
}

const SUBCOMMANDS: &[&str] = &[
    "serve", "grep", "search", "ast-search", "callgraph", "impact",
    "show", "map", "overview", "deps", "trace", "similar", "refs",
    "dead-code", "incremental-index", "rebuild-index", "reindex", "health-check", "doctor",
    "centrality", "cycles", "surprising", "report", "benchmark", "stats", "adopt", "unadopt", "snapshot",
    // MCP tool names accepted as aliases (see dispatch above). Listed here so
    // typo-suggester picks the closer alias for inputs like "project_mapp".
    "project_map", "module_overview", "get_ast_node", "find_references",
    "get_call_graph", "impact_analysis", "find_similar_code", "dependency_graph",
    "trace_http_chain", "find_dead_code", "ast_search", "semantic_code_search",
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
            for a in extra_args { cmd.arg(a); }
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
