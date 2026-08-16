use anyhow::Result;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::{Parser, Subcommand};

use crate::domain::{CODE_GRAPH_DIR, NO_METRICS_SENTINEL};
use crate::indexer::merkle::normalize_path_display_on;
use crate::storage::db::Database;
use crate::storage::queries;
use crate::utils::paths::home_dir;

pub mod commands;
pub mod freshness;
pub mod grep;
pub mod health;
pub mod index_ops;
pub mod paths;
pub mod symbols;
pub mod usage;

pub use commands::*;
pub(crate) use freshness::*;
pub use grep::*;
pub use health::*;
pub use index_ops::*;
pub use paths::*;
pub(crate) use symbols::*;
pub use usage::*;

/// Minimal JSON-RPC loop that answers `initialize` / `tools/list` with an empty
/// catalog and rejects everything else, WITHOUT opening a database, loading the
/// embedding model, or creating `.code-graph/`. Mirrors the JS launcher's
/// `serveEmptyMcpStub`. Driven by `run_serve` when `is_non_project_cwd` holds
/// and `CODE_GRAPH_FORCE_PLUGIN_MCP` is unset.
pub fn serve_non_project_stub<R: BufRead, W: Write>(
    mut reader: R,
    mut writer: W,
) -> std::io::Result<()> {
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break; // EOF
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let method = match req.get("method").and_then(|m| m.as_str()) {
            Some(m) => m,
            None => continue,
        };
        // JSON-RPC notifications (no `id`) get no response.
        let id = match req.get("id") {
            Some(id) => id.clone(),
            None => continue,
        };
        let response = match method {
            "initialize" => serde_json::json!({
                "jsonrpc": "2.0", "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": {
                        "name": "code-graph-mcp (non-project stub)",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }
            }),
            "tools/list" => {
                serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": [] } })
            }
            "resources/list" => {
                serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": { "resources": [] } })
            }
            "prompts/list" => {
                serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": { "prompts": [] } })
            }
            "ping" => serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
            _ => serde_json::json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32601, "message": "method not found (non-project stub mode)" }
            }),
        };
        writeln!(writer, "{}", response)?;
        writer.flush()?;
    }
    Ok(())
}

/// Lightweight CLI context for subcommands called by hooks.
/// Does NOT load the embedding model (too slow for 5-10s hook timeouts).
pub struct CliContext {
    pub db: Database,
    pub project_root: PathBuf,
}

impl CliContext {
    pub fn open(project_root: &Path) -> Result<Self> {
        Self::open_inner(project_root, false)
    }

    /// Same reader contract as [`CliContext::open`], but with the sqlite-vec
    /// tables brought up for the one read command that needs vector search
    /// (`similar`). Kept distinct from `Database::open_with_vec` on purpose:
    /// that constructor also revalidates (wipes) on `INDEX_VERSION` mismatch,
    /// which a read command must never do.
    pub fn open_with_vec(project_root: &Path) -> Result<Self> {
        Self::open_inner(project_root, true)
    }

    fn open_inner(project_root: &Path, with_vec: bool) -> Result<Self> {
        // Read-side worktree fallback (D#106, roadmap §2.2 — Rust mirror of the
        // v0.99.0 project-root.js fix): a linked worktree with no OWN index
        // reads the main checkout's index instead of erroring/cold-building.
        // Own index wins (checked first inside effective_read_root); write side
        // (index/serve/rebuild) does not go through CliContext and still builds
        // a local index. Paths/line numbers in answers are the main checkout's,
        // same contract as the JS hooks/statusline side.
        let project_root = &effective_read_root(project_root);
        let db_path = project_root.join(CODE_GRAPH_DIR).join("index.db");
        if !db_path.exists() {
            anyhow::bail!(
                "No index found at {}. Run: code-graph-mcp incremental-index",
                db_path.display()
            );
        }
        cleanup_legacy_db_files(&project_root.join(CODE_GRAPH_DIR));
        // CLI commands behind CliContext are READERS (grep, show, callgraph,
        // health-check, …). Open non-destructively so a status poll or one-off
        // query never triggers the INDEX_VERSION wipe — only an explicit indexer
        // (reindex / incremental-index / server startup) clears + rebuilds.
        let db = if with_vec {
            Database::open_nondestructive_with_vec(&db_path)?
        } else {
            Database::open_nondestructive(&db_path)?
        };
        Ok(Self {
            db,
            project_root: project_root.to_path_buf(),
        })
    }

    /// Try to open, returning None if no index exists (for grep fallback).
    pub fn try_open(project_root: &Path) -> Option<Self> {
        // Same read-side worktree fallback as open() above.
        let project_root = &effective_read_root(project_root);
        let db_path = project_root.join(CODE_GRAPH_DIR).join("index.db");
        if !db_path.exists() {
            return None;
        }
        cleanup_legacy_db_files(&project_root.join(CODE_GRAPH_DIR));
        Database::open_nondestructive(&db_path).ok().map(|db| Self {
            db,
            project_root: project_root.to_path_buf(),
        })
    }
}

// --- Argument helpers ---

#[cfg(test)]
mod tests;
