use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::storage::db::Database;
use crate::storage::queries;

/// Lightweight CLI context for subcommands called by hooks.
/// Does NOT load the embedding model (too slow for 5-10s hook timeouts).
pub struct CliContext {
    pub db: Database,
    pub project_root: PathBuf,
}

impl CliContext {
    pub fn open(project_root: &Path) -> Result<Self> {
        let db_path = project_root.join(".code-graph").join("index.db");
        if !db_path.exists() {
            anyhow::bail!(
                "No index found at {}. Run the MCP server first to create the index.",
                db_path.display()
            );
        }
        let db = Database::open(&db_path)?;
        Ok(Self {
            db,
            project_root: project_root.to_path_buf(),
        })
    }
}

/// Run incremental index update.
/// If `quiet` is true, suppress non-error output.
pub fn cmd_incremental_index(project_root: &Path, quiet: bool) -> Result<()> {
    let ctx = CliContext::open(project_root)?;

    // Use run_incremental_index without a model (no embedding for short-lived CLI)
    use crate::indexer::pipeline::run_incremental_index;
    let stats = run_incremental_index(&ctx.db, &ctx.project_root, None, None)?;

    if !quiet {
        eprintln!(
            "Incremental index: {} files updated, {} nodes created",
            stats.files_indexed, stats.nodes_created
        );
    }
    Ok(())
}

/// Run health check and print status.
pub fn cmd_health_check(project_root: &Path, format: &str) -> Result<()> {
    let ctx = CliContext::open(project_root)?;
    let conn = ctx.db.conn();
    let status = queries::get_index_status(conn, false)?;

    let expected_schema = crate::storage::schema::SCHEMA_VERSION;
    let schema_ok = status.schema_version == expected_schema;
    let has_data = status.nodes_count > 0 && status.files_count > 0;
    let healthy = schema_ok && has_data;

    match format {
        "json" => {
            let mut json = serde_json::json!({
                "healthy": healthy,
                "nodes": status.nodes_count,
                "edges": status.edges_count,
                "files": status.files_count,
                "watching": false,
                "schema_version": status.schema_version,
            });
            if !schema_ok {
                json["issue"] = serde_json::json!(format!(
                    "schema version mismatch: got {}, expected {}",
                    status.schema_version, expected_schema
                ));
            } else if !has_data {
                json["issue"] = serde_json::json!("index is empty");
            }
            println!("{}", json);
        }
        _ => {
            if healthy {
                println!(
                    "OK: {} nodes, {} edges, {} files",
                    status.nodes_count, status.edges_count, status.files_count
                );
            } else if !schema_ok {
                println!(
                    "UNHEALTHY: schema version mismatch (got {}, expected {})",
                    status.schema_version, expected_schema
                );
            } else {
                println!("UNHEALTHY: index is empty");
            }
        }
    }
    Ok(())
}
