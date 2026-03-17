use anyhow::Result;
use std::io::{self, BufRead, Read, Write};

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
            let format = args
                .iter()
                .position(|a| a == "--format")
                .and_then(|i| args.get(i + 1))
                .map(|s| s.as_str())
                .unwrap_or("oneline");
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
        Some(other) => {
            eprintln!("Unknown subcommand: {}", other);
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
    println!("    ast-search <query>  Structured search with --type/--returns/--params filters");
    println!("    callgraph <symbol>  Show call graph (callers/callees)");
    println!("    impact <symbol>     Impact analysis (callers, routes, risk level)");
    println!("    map                 Project architecture map (modules, deps, entry points)");
    println!("    overview <path>     Module overview (symbols grouped by file and type)");
    println!("    incremental-index   Run incremental index update");
    println!("    health-check        Query index status\n");
    println!("OPTIONS:");
    println!("    --json              JSON output (available on all commands)");
    println!("    --compact           Compact output (map)");
    println!("    --limit N           Limit results (search, ast-search; default: 20)");
    println!("    --type <type>       Filter by node type: fn, class, struct, enum, trait, ...");
    println!("    --returns <type>    Filter by return type (ast-search)");
    println!("    --params <text>     Filter by parameter text (ast-search)");
    println!("    --direction <dir>   callers, callees, or both (callgraph; default: both)");
    println!("    --depth N           Max traversal depth (callgraph, impact; default: 3)");
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

    // Enable MCP progress/log notifications via stdout
    server.set_notify_writer(Box::new(io::stdout()));

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut reader = stdin.lock();
    let mut buf = String::new();
    const MAX_MESSAGE_SIZE: usize = 10 * 1024 * 1024; // 10MB

    loop {
        buf.clear();
        let n = reader.by_ref().take((MAX_MESSAGE_SIZE + 1) as u64).read_line(&mut buf)?;
        if n == 0 {
            break; // EOF
        }
        if buf.trim().is_empty() {
            continue;
        }
        if buf.len() > MAX_MESSAGE_SIZE {
            // Drain remainder of the truncated line to prevent corrupting the next read.
            // Bound the drain to MAX_MESSAGE_SIZE to prevent OOM from unbounded input.
            if !buf.ends_with('\n') {
                // Drain until newline (line-aware) without UTF-8 String allocation
                let _ = reader.by_ref().take(MAX_MESSAGE_SIZE as u64)
                    .read_until(b'\n', &mut Vec::new());
            }
            let err_resp = serde_json::json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": {
                    "code": code_graph_mcp::mcp::protocol::JSONRPC_INVALID_REQUEST,
                    "message": format!("Message too large: {} bytes (max {})", buf.len(), MAX_MESSAGE_SIZE)
                }
            });
            writeln!(stdout, "{}", err_resp)?;
            stdout.flush()?;
            continue;
        }

        match server.handle_message(&buf) {
            Ok(Some(response)) => {
                writeln!(stdout, "{}", response)?;
                stdout.flush()?;
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
                writeln!(stdout, "{}", err_resp)?;
                stdout.flush()?;
            }
        }

        // Run startup indexing + auto-watch if triggered by notifications/initialized
        server.run_startup_tasks();
    }

    server.flush_metrics();
    tracing::info!("[session] Ended after {:.0}s", session_start.elapsed().as_secs_f64());

    Ok(())
}
