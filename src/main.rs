use anyhow::Result;
use std::io::{self, BufRead, Read, Write};

use code_graph_mcp::mcp::server::McpServer;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .init();

    let project_root = std::env::current_dir()?;
    let server = McpServer::from_project_root(&project_root)?;

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut reader = stdin.lock();
    let mut buf = String::new();
    const MAX_MESSAGE_SIZE: usize = 10 * 1024 * 1024; // 10MB

    loop {
        buf.clear();
        let n = reader.by_ref().take((MAX_MESSAGE_SIZE + 1) as u64).read_line(&mut buf)?;
        if n == 0 { break; } // EOF
        if buf.trim().is_empty() {
            continue;
        }
        if buf.len() > MAX_MESSAGE_SIZE {
            let err_resp = serde_json::json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": {
                    "code": -32600,
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
                let (code, label) = if e.downcast_ref::<serde_json::Error>().is_some() {
                    (-32700, "Parse error")
                } else {
                    (-32603, "Internal error")
                };
                let err_resp = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": {
                        "code": code,
                        "message": format!("{}: {}", label, e)
                    }
                });
                writeln!(stdout, "{}", err_resp)?;
                stdout.flush()?;
            }
        }
    }

    Ok(())
}
