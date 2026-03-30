use anyhow::Result;
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
            let quiet = args.iter().any(|a| a == "--quiet");
            let project_root = std::env::current_dir()?;
            code_graph_mcp::cli::cmd_incremental_index(&project_root, quiet)
        }
        Some("health-check") => {
            // Support both --format json and --json for consistency with other commands
            let format = if args.iter().any(|a| a == "--json") {
                "json"
            } else {
                args.iter()
                    .position(|a| a == "--format")
                    .and_then(|i| args.get(i + 1))
                    .map(|s| s.as_str())
                    .unwrap_or("oneline")
            };
            let project_root = std::env::current_dir()?;
            code_graph_mcp::cli::cmd_health_check(&project_root, format)
        }
        Some("grep") => {
            let project_root = std::env::current_dir()?;
            code_graph_mcp::cli::cmd_grep(&project_root, &args)
        }
        Some("search") => {
            let project_root = std::env::current_dir()?;
            code_graph_mcp::cli::cmd_search(&project_root, &args)
        }
        Some("ast-search") => {
            let project_root = std::env::current_dir()?;
            code_graph_mcp::cli::cmd_ast_search(&project_root, &args)
        }
        Some("callgraph") => {
            let project_root = std::env::current_dir()?;
            code_graph_mcp::cli::cmd_callgraph(&project_root, &args)
        }
        Some("impact") => {
            let project_root = std::env::current_dir()?;
            code_graph_mcp::cli::cmd_impact(&project_root, &args)
        }
        Some("map") => {
            let project_root = std::env::current_dir()?;
            code_graph_mcp::cli::cmd_map(&project_root, &args)
        }
        Some("overview") => {
            let project_root = std::env::current_dir()?;
            code_graph_mcp::cli::cmd_overview(&project_root, &args)
        }
        Some("show") => {
            let project_root = std::env::current_dir()?;
            code_graph_mcp::cli::cmd_show(&project_root, &args)
        }
        Some("trace") => {
            let project_root = std::env::current_dir()?;
            code_graph_mcp::cli::cmd_trace(&project_root, &args)
        }
        Some("deps") => {
            let project_root = std::env::current_dir()?;
            code_graph_mcp::cli::cmd_deps(&project_root, &args)
        }
        Some("similar") => {
            let project_root = std::env::current_dir()?;
            code_graph_mcp::cli::cmd_similar(&project_root, &args)
        }
        Some("refs") => {
            let project_root = std::env::current_dir()?;
            code_graph_mcp::cli::cmd_refs(&project_root, &args)
        }
        Some("dead-code") => {
            let project_root = std::env::current_dir()?;
            code_graph_mcp::cli::cmd_dead_code(&project_root, &args)
        }
        Some("benchmark") => {
            let project_root = std::env::current_dir()?;
            code_graph_mcp::cli::cmd_benchmark(&project_root, &args)
        }
        Some("doctor") => {
            // Delegate to plugin's doctor.js
            let exe_dir = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

            let doctor_candidates = [
                // Dev mode: binary is in target/release/, doctor.js is in claude-plugin/scripts/
                exe_dir.join("../../claude-plugin/scripts/doctor.js"),
                // Installed via npm: doctor.js is alongside the binary's package
                exe_dir.join("../claude-plugin/scripts/doctor.js"),
            ];

            for candidate in &doctor_candidates {
                if candidate.exists() {
                    let mut cmd = std::process::Command::new("node");
                    cmd.arg(candidate);
                    if args.iter().any(|a| a == "--check-only") {
                        cmd.arg("--check-only");
                    }
                    let status = cmd.status().map_err(|e| {
                        if e.kind() == std::io::ErrorKind::NotFound {
                            anyhow::anyhow!("Node.js not found. Install Node.js to use 'code-graph-mcp doctor'.")
                        } else {
                            e.into()
                        }
                    })?;
                    std::process::exit(status.code().unwrap_or(1));
                }
            }

            eprintln!("doctor.js not found. Run directly: node claude-plugin/scripts/doctor.js");
            std::process::exit(1);
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

fn print_help() {
    print_version();
    println!("AST-based code graph with semantic search\n");
    println!("USAGE:");
    println!("    code-graph-mcp [COMMAND]\n");
    println!("COMMANDS:");
    println!("    serve               Start MCP JSON-RPC server on stdio (default)");
    println!("    grep <pattern> [path]");
    println!("                        AST-context grep (ripgrep + containing function/class)");
    println!("    search <query>      FTS5 semantic search by concept");
    println!("    ast-search [query]  Structured search with --type/--returns/--params filters");
    println!("    callgraph <symbol>  Show call graph (callers/callees)");
    println!("    impact <symbol>     Impact analysis (callers, routes, risk level)");
    println!("    show <symbol>       Show symbol details (code, type, signature)");
    println!("    map                 Project architecture map (modules, deps, entry points)");
    println!("    overview <path>     Module overview (symbols grouped by file and type)");
    println!("    deps <file>         File-level dependency graph");
    println!("    trace <route>       Trace HTTP route → handler → downstream calls");
    println!("    similar <symbol>    Find semantically similar code (requires embeddings)");
    println!("    refs <symbol>       Find all references to a symbol (callers, importers, etc.)");
    println!("    dead-code [path]    Find unused code (orphans and exported-unused symbols)");
    println!("    incremental-index   Run incremental index update");
    println!("    health-check        Query index status");
    println!("    doctor              Diagnose and repair environment issues");
    println!("    benchmark           Benchmark index speed, query latency, token savings\n");
    println!("OPTIONS:");
    println!("    --json              JSON output (available on all commands)");
    println!("    --compact           Compact output (search, callgraph, map, overview, deps, refs)");
    println!("    --limit N           Limit results (search, ast-search; default: 20)");
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
    println!("    --include-tests     Show test callers (callgraph, show; hidden by default)");
    println!("    --refs              Show callers/callees (show; alias: --include-refs)");
    println!("    --impact            Show impact summary (show; alias: --include-impact)");
    println!("    --context-lines N   Surrounding source lines (show; default: 0)");
    println!("    --min-lines N       Min lines to report (dead-code; default: 3)");
    println!("    --relation <type>   Filter: calls, imports, inherits, implements (refs)");
    println!("    -h, --help          Show this help message");
    println!("    -V, --version       Show version");
}

fn run_serve() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
        )
        .with_writer(io::stderr)
        .init();

    let project_root = std::env::current_dir()?;
    let server = code_graph_mcp::mcp::server::McpServer::from_project_root(&project_root)?;
    let session_start = std::time::Instant::now();

    tracing::info!("[session] Started v{}, project: {}", env!("CARGO_PKG_VERSION"), project_root.display());

    // Shared stdout handle: prevents interleaved JSON when background threads
    // send notifications concurrently with the main loop writing responses.
    let stdout_shared = Arc::new(Mutex::new(io::stdout()));

    // Enable MCP progress/log notifications via the same shared handle
    server.set_notify_writer(Box::new(SharedStdout(Arc::clone(&stdout_shared))));

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
    "dead-code", "incremental-index", "health-check", "doctor", "benchmark",
];

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
