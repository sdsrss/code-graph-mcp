use std::path::{Path, PathBuf};
use anyhow::Result;
use serde_json::json;
use super::McpServer;

/// Registry of named project servers for multi-project MCP mode.
///
/// Populated from `CODE_GRAPH_PROJECTS=alias1=/path1:alias2=/path2`.
/// The first entry is the default project (used when no `project` param is given).
pub struct ProjectRegistry {
    /// Ordered list of (alias, server) pairs; index 0 is the default.
    projects: Vec<(String, McpServer)>,
}

impl ProjectRegistry {
    /// Parse `CODE_GRAPH_PROJECTS` and create a registry.
    ///
    /// Format: `alias1=/abs/path1:alias2=/abs/path2`
    ///
    /// Returns `Ok(None)` if the env var is unset or empty.
    pub fn from_env() -> Result<Option<Self>> {
        let env = match std::env::var("CODE_GRAPH_PROJECTS").ok() {
            Some(s) if !s.is_empty() => s,
            _ => return Ok(None),
        };

        let mut projects: Vec<(String, McpServer)> = Vec::new();
        for entry in env.split(':') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let (alias, path_str) = entry.split_once('=').ok_or_else(|| {
                anyhow::anyhow!(
                    "Invalid CODE_GRAPH_PROJECTS entry '{}'. Expected format: alias=path",
                    entry
                )
            })?;
            let alias = alias.trim().to_string();
            if alias.is_empty() {
                anyhow::bail!("Empty alias in CODE_GRAPH_PROJECTS entry: '{}'", entry);
            }
            if projects.iter().any(|(a, _)| a == &alias) {
                anyhow::bail!("Duplicate alias '{}' in CODE_GRAPH_PROJECTS", alias);
            }
            let path = PathBuf::from(path_str.trim());
            tracing::info!("[multi-project] loading project '{}' at {}", alias, path.display());
            let server = McpServer::from_project_root(&path)?;
            projects.push((alias, server));
        }

        if projects.is_empty() {
            anyhow::bail!("CODE_GRAPH_PROJECTS is set but contains no valid entries");
        }

        Ok(Some(Self { projects }))
    }

    /// Return the project root of the first (default) project.
    pub fn default_root(&self) -> &Path {
        self.projects[0].1.project_root.as_deref().unwrap_or(Path::new("."))
    }

    /// Look up a project server by alias.
    ///
    /// `None` → first (default) project.
    pub fn get(&self, alias: Option<&str>) -> Result<&McpServer> {
        match alias {
            None => Ok(&self.projects[0].1),
            Some(a) => self
                .projects
                .iter()
                .find(|(key, _)| key == a)
                .map(|(_, srv)| srv)
                .ok_or_else(|| {
                    let known: Vec<&str> =
                        self.projects.iter().map(|(k, _)| k.as_str()).collect();
                    anyhow::anyhow!(
                        "Unknown project alias '{}'. Known aliases: {}",
                        a,
                        known.join(", ")
                    )
                }),
        }
    }

    /// JSON listing of all registered projects.
    pub fn list_projects_json(&self) -> serde_json::Value {
        let entries: Vec<_> = self
            .projects
            .iter()
            .enumerate()
            .map(|(i, (alias, server))| {
                json!({
                    "alias": alias,
                    "path": server.project_root
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                    "is_default": i == 0,
                })
            })
            .collect();
        json!({ "projects": entries })
    }

    /// Remove and return the default (first) project's McpServer.
    ///
    /// The caller takes ownership of the primary server to run the stdio loop.
    /// The registry retains all other projects for tool-call routing.
    pub fn take_default(&mut self) -> Result<McpServer> {
        if self.projects.is_empty() {
            anyhow::bail!("ProjectRegistry is empty — no default project to take");
        }
        let (alias, server) = self.projects.remove(0);
        tracing::info!("[multi-project] default project '{}' promoted to outer server", alias);
        Ok(server)
    }

    /// Set a shared notification writer on every project server in the registry.
    pub fn set_notify_writers(&mut self, make_writer: impl Fn() -> Box<dyn std::io::Write + Send>) {
        for (_, server) in &mut self.projects {
            server.set_notify_writer(make_writer());
        }
    }
}
