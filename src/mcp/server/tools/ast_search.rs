//! `ast_search` — structural enumeration with type/returns/params filters.
//!
//! The pipeline (filter-aware candidate pool, Rust-side column filtering,
//! name-substring fallback) lives in [`crate::search::ast_query`] so this and
//! the CLI twin (`cmd_ast_search`) cannot answer the same query differently —
//! they did: the fallback existed here only (audit 2026-08-16 P1-8). This file
//! owns the MCP response shape and hint wording.
//!
//! Generic-fallback hint kicks in when zero hits + returns_filter has angle brackets.

use super::super::*;
use crate::search::ast_query::{run as run_ast_search, AstSearchParams};

impl McpServer {
    pub(in crate::mcp::server) fn tool_ast_search(
        &self,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        if !should_skip_indexing(args) {
            self.ensure_indexed()?;
        }

        let query = args["query"]
            .as_str()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());
        let type_filter = args["type"].as_str();
        let returns_filter = args["returns"].as_str();
        let params_filter = args["params"].as_str();
        let limit = args["limit"].as_u64().unwrap_or(20).clamp(1, 100) as usize;

        let has_filters =
            type_filter.is_some() || returns_filter.is_some() || params_filter.is_some();
        if query.is_none() && !has_filters {
            return Err(anyhow!(
                "Either query or at least one filter (type, returns, params) is required."
            ));
        }

        // Validate type up-front: unknown aliases normalize to an empty Vec,
        // which would silently filter every node away. Surface the typo so the
        // caller doesn't read "No results" and assume the index is empty.
        if let Some(tf) = type_filter {
            if crate::domain::normalize_type_filter(tf).is_empty() {
                return Err(anyhow!(
                    "Unknown type filter: '{}'. Valid: fn, class, struct, enum, trait, type, const, var",
                    tf
                ));
            }
        }

        let outcome = run_ast_search(
            self.db.conn(),
            &AstSearchParams {
                query,
                type_filter,
                returns_filter,
                params_filter,
                limit,
            },
        )?;

        if outcome.fts_empty {
            return Ok(json!({ "results": [], "count": 0, "message": "No results found." }));
        }

        let items: Vec<serde_json::Value> = outcome
            .results
            .iter()
            .map(|nwf| {
                let n = &nwf.node;
                json!({
                    "node_id": n.id,
                    "name": n.qualified_name.as_deref().unwrap_or(&n.name),
                    "type": n.node_type,
                    "file_path": nwf.file_path,
                    "start_line": n.start_line,
                    "end_line": n.end_line,
                    "signature": n.signature,
                    "return_type": n.return_type,
                    "param_types": n.param_types,
                })
            })
            .collect();

        let mut response = json!({
            "results": items,
            "count": items.len(),
        });
        if let Some(total) = outcome.matched_total {
            response["matched_total"] = json!(total);
        }

        // Truncation is a disclosure, not a nicety: "count: 20" is otherwise
        // indistinguishable from "20 matches exist" and the caller reports the
        // cut set as the complete answer. The remedy is to RAISE limit.
        if outcome.truncated {
            response["truncated"] = json!(true);
        }

        // Empty because the structural filters rejected everything: say what
        // happened. The REMEDY for this (and for truncation, and for the
        // name-substring fallback) comes from the shared ordered builder — this
        // file used to assign `hint` from three separate blocks and keep only
        // whichever ran last, disagreeing with the CLI twin about which that was.
        if items.is_empty() && outcome.dropped_by_filter > 0 {
            response["filtered_out"] = json!(outcome.dropped_by_filter);
            response["message"] = json!(format!(
                "No results — {} candidate(s) matched the query but not the filter.",
                outcome.dropped_by_filter
            ));
        }

        let hints = crate::search::ast_query::hints(
            &outcome,
            query,
            limit,
            crate::search::ast_query::HintStyle::Mcp,
        );
        if !hints.is_empty() {
            response["hint"] = json!(hints.join(" "));
        }

        // Generic-fallback hint: when returns_filter has angle brackets and zero hits,
        // retry with the inner-most type as a suggestion so the caller sees "did you mean Relation?"
        // rather than an empty response.
        if response["count"].as_u64().unwrap_or(0) == 0 {
            if let Some(rf) = returns_filter {
                if let Some(inner) = strip_outer_generic(rf) {
                    let normalized = type_filter.map(normalize_type_filter_mcp);
                    let type_refs: Option<Vec<&str>> = normalized
                        .as_ref()
                        .map(|v| v.iter().map(|s| s.as_str()).collect());
                    let retry = queries::get_nodes_with_files_by_filters(
                        self.db.conn(),
                        type_refs.as_deref(),
                        Some(&inner),
                        params_filter,
                        None,
                        100,
                    )?;
                    if !retry.is_empty() {
                        let n = retry.len();
                        let plural = if n == 1 { "" } else { "es" };
                        // PREPEND, don't overwrite: this is the most actionable
                        // sentence ("did you mean X"), but the filter/truncation
                        // remedies it used to clobber are still true.
                        let suggestion = format!(
                            "No match for returns='{}'. Substring '{}' has {} match{} — try that.",
                            rf, inner, n, plural
                        );
                        response["hint"] = json!(match response["hint"].as_str() {
                            Some(prior) if !prior.is_empty() => format!("{suggestion} {prior}"),
                            _ => suggestion,
                        });
                        let mut suggested = serde_json::Map::new();
                        suggested.insert("returns".to_string(), json!(inner));
                        if let Some(tf) = type_filter {
                            suggested.insert("type".to_string(), json!(tf));
                        }
                        if let Some(pf) = params_filter {
                            suggested.insert("params".to_string(), json!(pf));
                        }
                        if let Some(q) = query {
                            suggested.insert("query".to_string(), json!(q));
                        }
                        response["suggested_query"] = serde_json::Value::Object(suggested);
                    }
                }
            }
        }

        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Index where 40 `node_*` functions outrank 8 `Node*` structs in BM25, the
    /// shape the audit measured on this repo (`ast-search node --type struct`
    /// showed 3 of 39 matches at the default limit, 0 at limit 5).
    fn drown_project() -> tempfile::TempDir {
        let project = tempfile::TempDir::new().unwrap();
        let src = project.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let mut walk = String::new();
        for i in 0..40 {
            walk.push_str(&format!(
                "pub fn node_walk_{i:02}(node: &NodeRef) -> u32 {{\n    let node_id = node.node_id();\n    let node_depth = node.node_depth();\n    node_id + node_depth + node_id\n}}\n"
            ));
        }
        std::fs::write(src.join("walk.rs"), walk).unwrap();
        let mut types = String::new();
        for name in [
            "NodeAlpha",
            "NodeBravo",
            "NodeCharlie",
            "NodeDelta",
            "NodeEcho",
            "NodeFoxtrot",
            "NodeGolf",
            "NodeHotel",
        ] {
            types.push_str(&format!("pub struct {name} {{\n    pub id: u32,\n}}\n"));
        }
        std::fs::write(src.join("types.rs"), types).unwrap();
        std::fs::write(src.join("lib.rs"), "pub mod walk;\npub mod types;\n").unwrap();
        std::fs::write(
            project.path().join("Cargo.toml"),
            "[package]\nname = \"fixture_lib\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        project
    }

    fn indexed_server(project: &tempfile::TempDir) -> McpServer {
        let server = McpServer::new_test_with_project(project.path());
        crate::indexer::pipeline::run_full_index(&server.db, project.path(), None, None).unwrap();
        server
    }

    /// A filtered query must not report zero while matches sit below a fixed
    /// `limit * 4` cut, and a truncated answer must say it was truncated.
    #[test]
    fn type_filtered_query_returns_matches_and_discloses_truncation() {
        let project = drown_project();
        let server = indexed_server(&project);
        let out = server
            .tool_ast_search(&json!({
                "query": "node",
                "type": "struct",
                "limit": 5,
                "skip_indexing": true
            }))
            .unwrap();
        assert_eq!(
            out["count"], 5,
            "8 structs exist and 5 were asked for; got {out}"
        );
        assert_eq!(out["matched_total"], 8, "got {out}");
        assert_eq!(out["truncated"], true, "got {out}");
        assert!(
            out["hint"].as_str().unwrap_or("").contains("limit"),
            "the remedy is raising limit; got {out}"
        );
    }

    /// Default limit: every one of the 8 matches fits, so nothing is truncated
    /// and no hint is owed.
    #[test]
    fn type_filtered_query_at_default_limit_returns_every_match() {
        let project = drown_project();
        let server = indexed_server(&project);
        let out = server
            .tool_ast_search(&json!({"query": "node", "type": "struct", "skip_indexing": true}))
            .unwrap();
        assert_eq!(out["count"], 8, "all 8 Node* structs must come back: {out}");
        assert!(out.get("truncated").is_none(), "nothing was cut: {out}");
    }
}
