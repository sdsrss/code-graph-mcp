use anyhow::Result;
use std::io::{self, BufRead, Read, Write};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let subcommand = args.get(1).map(|s| s.as_str());

    match subcommand {
        Some("serve") | None => run_serve(),
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
        Some(other) => {
            eprintln!(
                "Unknown subcommand: {}. Usage: code-graph-mcp [serve|incremental-index|health-check]",
                other
            );
            std::process::exit(1);
        }
    }
}

fn run_serve() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .init();

    let project_root = std::env::current_dir()?;
    let server = code_graph_mcp::mcp::server::McpServer::from_project_root(&project_root)?;

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
                let mut discard = String::new();
                let _ = reader.by_ref().take(MAX_MESSAGE_SIZE as u64).read_line(&mut discard);
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
    }

    Ok(())
}
