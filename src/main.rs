use anyhow::Result;
use std::io::{self, BufRead, Write};

use code_graph_mcp::mcp::server::McpServer;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .init();

    let project_root = std::env::current_dir()?;
    let db_dir = project_root.join(".code-graph");
    std::fs::create_dir_all(&db_dir)?;
    let db_path = db_dir.join("index.db");

    let server = McpServer::new(&db_path, Some(project_root.to_string_lossy().into()))?;

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        match server.handle_message(&line) {
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
                        "code": -32700,
                        "message": format!("Parse error: {}", e)
                    }
                });
                writeln!(stdout, "{}", err_resp)?;
                stdout.flush()?;
            }
        }
    }

    Ok(())
}
