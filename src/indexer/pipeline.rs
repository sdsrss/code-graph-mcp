use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

use crate::embedding::context::{build_context_string, NodeContext};
use crate::indexer::merkle::{compute_diff, hash_file, scan_directory};
use crate::parser::relations::extract_relations;
use crate::parser::treesitter::parse_code;
use crate::storage::db::Database;
use crate::storage::queries::*;
use crate::utils::config::detect_language;

pub struct IndexResult {
    pub files_indexed: usize,
    pub nodes_created: usize,
    pub edges_created: usize,
}

pub fn run_full_index(db: &Database, project_root: &Path) -> Result<IndexResult> {
    let current_hashes = scan_directory(project_root)?;
    let files: Vec<String> = current_hashes.keys().cloned().collect();
    index_files(db, project_root, &files, &current_hashes)
}

pub fn run_incremental_index(db: &Database, project_root: &Path) -> Result<IndexResult> {
    let stored_hashes = get_all_file_hashes(db.conn())?;
    let current_hashes = scan_directory(project_root)?;
    let diff = compute_diff(&stored_hashes, &current_hashes);

    // Phase 0: Clean up deleted files (CASCADE handles nodes + edges)
    if !diff.deleted_files.is_empty() {
        delete_files_by_paths(db.conn(), &diff.deleted_files)?;
    }

    // Index changed + new files
    let to_index: Vec<String> = [diff.new_files, diff.changed_files].concat();
    index_files(db, project_root, &to_index, &current_hashes)
}

fn index_files(
    db: &Database,
    root: &Path,
    files: &[String],
    hashes: &HashMap<String, String>,
) -> Result<IndexResult> {
    let mut nodes_created = 0usize;
    let mut edges_created = 0usize;

    // Collect all parsed data first
    struct FileParsed {
        file_id: i64,
        rel_path: String,
        node_ids: Vec<i64>,
        node_names: Vec<String>,
    }

    let mut parsed_files: Vec<FileParsed> = Vec::new();

    // Phase 1: Parse files, insert nodes
    for rel_path in files {
        let abs_path = root.join(rel_path);
        let language = match detect_language(rel_path) {
            Some(lang) => lang,
            None => continue, // Skip unsupported file types
        };

        let hash = match hashes.get(rel_path.as_str()) {
            Some(h) => h.clone(),
            None => hash_file(&abs_path)?,
        };

        let source = match std::fs::read_to_string(&abs_path) {
            Ok(s) => s,
            Err(_) => continue,
        };

        // Delete old nodes for this file if it was previously indexed
        let file_id = upsert_file(db.conn(), &FileRecord {
            path: rel_path.clone(),
            blake3_hash: hash,
            last_modified: std::fs::metadata(&abs_path)
                .map(|m| m.modified().ok())
                .ok()
                .flatten()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            language: Some(language.to_string()),
        })?;

        // Delete old nodes for this file (they'll be re-created)
        delete_nodes_by_file(db.conn(), file_id)?;

        let parsed_nodes = match parse_code(&source, language) {
            Ok(nodes) => nodes,
            Err(_) => continue, // Skip files that fail to parse
        };

        let mut node_ids = Vec::new();
        let mut node_names = Vec::new();

        for pn in &parsed_nodes {
            let node_id = insert_node(db.conn(), &NodeRecord {
                file_id,
                node_type: pn.node_type.clone(),
                name: pn.name.clone(),
                qualified_name: pn.qualified_name.clone(),
                start_line: pn.start_line as i64,
                end_line: pn.end_line as i64,
                code_content: pn.code_content.clone(),
                signature: pn.signature.clone(),
                doc_comment: pn.doc_comment.clone(),
                context_string: None, // Phase 3 will fill this
            })?;
            node_ids.push(node_id);
            node_names.push(pn.name.clone());
            nodes_created += 1;
        }

        parsed_files.push(FileParsed {
            file_id,
            rel_path: rel_path.clone(),
            node_ids,
            node_names,
        });
    }

    // Phase 2: Extract relations, insert edges
    // Build a lookup from node name -> node_ids (for cross-file resolution)
    let mut name_to_ids: HashMap<String, Vec<i64>> = HashMap::new();
    for pf in &parsed_files {
        for (id, name) in pf.node_ids.iter().zip(pf.node_names.iter()) {
            name_to_ids.entry(name.clone()).or_default().push(*id);
        }
    }
    // Also include existing nodes not being re-indexed
    let all_existing = get_all_node_names(db.conn())?;
    for (name, id) in &all_existing {
        name_to_ids.entry(name.clone()).or_default().push(*id);
    }
    // Deduplicate
    for ids in name_to_ids.values_mut() {
        ids.sort();
        ids.dedup();
    }

    for pf in &parsed_files {
        let abs_path = root.join(&pf.rel_path);
        let language = match detect_language(&pf.rel_path) {
            Some(lang) => lang,
            None => continue,
        };
        let source = match std::fs::read_to_string(&abs_path) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let relations = match extract_relations(&source, language) {
            Ok(r) => r,
            Err(_) => continue,
        };

        for rel in &relations {
            // Find source node ID
            let source_ids = pf.node_names.iter()
                .zip(pf.node_ids.iter())
                .filter(|(name, _)| *name == &rel.source_name)
                .map(|(_, id)| *id)
                .collect::<Vec<_>>();

            let target_ids = name_to_ids.get(&rel.target_name)
                .cloned()
                .unwrap_or_default();

            for &src_id in &source_ids {
                for &tgt_id in &target_ids {
                    if src_id != tgt_id {
                        insert_edge(db.conn(), src_id, tgt_id, &rel.relation, None)?;
                        edges_created += 1;
                    }
                }
            }
        }
    }

    // Phase 3: Build context strings and update nodes
    for pf in &parsed_files {
        for (idx, &node_id) in pf.node_ids.iter().enumerate() {
            let node_name = &pf.node_names[idx];

            // Get callees (this node calls -> targets)
            let callees = get_edge_target_names(db.conn(), node_id, "calls")?;
            // Get callers (other nodes -> call this node)
            let callers = get_edge_source_names(db.conn(), node_id, "calls")?;
            // Get inheritance
            let inherits = get_edge_target_names(db.conn(), node_id, "inherits")?;
            // Get routes
            let routes = get_edge_target_names(db.conn(), node_id, "routes_to")?;

            // Get node details for signature/doc
            let nodes = get_nodes_by_name(db.conn(), node_name)?;
            let node_detail = nodes.iter().find(|n| n.id == node_id);

            let ctx = build_context_string(&NodeContext {
                node_type: node_detail.map(|n| n.node_type.clone()).unwrap_or_default(),
                name: node_name.clone(),
                file_path: pf.rel_path.clone(),
                signature: node_detail.and_then(|n| n.signature.clone()),
                routes,
                callees,
                callers,
                inherits,
                doc_comment: node_detail.and_then(|n| n.doc_comment.clone()),
            });

            update_context_string(db.conn(), node_id, &ctx)?;
        }
    }

    Ok(IndexResult {
        files_indexed: parsed_files.len(),
        nodes_created,
        edges_created,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::fs;

    #[test]
    fn test_full_index_pipeline() {
        let project_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();

        fs::create_dir_all(project_dir.path().join("src")).unwrap();
        fs::write(project_dir.path().join("src/auth.ts"), r#"
function validateToken(token: string): boolean {
    return jwt.verify(token);
}

function handleLogin(req: Request) {
    if (validateToken(req.token)) {
        return createSession(req.userId);
    }
}
"#).unwrap();

        let db = Database::open(&db_dir.path().join("index.db")).unwrap();
        let result = run_full_index(&db, project_dir.path()).unwrap();

        assert!(result.files_indexed > 0);
        assert!(result.nodes_created > 0);
        assert!(result.edges_created > 0);

        // Verify nodes are in DB
        let nodes = get_nodes_by_name(db.conn(), "handleLogin").unwrap();
        assert_eq!(nodes.len(), 1);

        // Verify edges: handleLogin → calls → validateToken
        let edges = get_edges_from(db.conn(), nodes[0].id).unwrap();
        assert!(edges.iter().any(|e| e.relation == "calls"), "should have call edges");

        // Verify context string was built
        assert!(nodes[0].context_string.is_some(), "context string should be set after Phase 3");
    }

    #[test]
    fn test_incremental_index() {
        let project_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        let db = Database::open(&db_dir.path().join("index.db")).unwrap();

        // Initial index
        fs::write(project_dir.path().join("a.ts"), "function foo() {}").unwrap();
        run_full_index(&db, project_dir.path()).unwrap();

        // Modify file
        fs::write(project_dir.path().join("a.ts"), "function bar() {}").unwrap();

        // Incremental index
        let result = run_incremental_index(&db, project_dir.path()).unwrap();
        assert_eq!(result.files_indexed, 1);

        let foo = get_nodes_by_name(db.conn(), "foo").unwrap();
        assert_eq!(foo.len(), 0);
        let bar = get_nodes_by_name(db.conn(), "bar").unwrap();
        assert_eq!(bar.len(), 1);
    }

    #[test]
    fn test_deleted_file_cleanup() {
        let project_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();
        let db = Database::open(&db_dir.path().join("index.db")).unwrap();

        fs::write(project_dir.path().join("a.ts"), "function foo() {}").unwrap();
        run_full_index(&db, project_dir.path()).unwrap();

        fs::remove_file(project_dir.path().join("a.ts")).unwrap();
        run_incremental_index(&db, project_dir.path()).unwrap();

        let foo = get_nodes_by_name(db.conn(), "foo").unwrap();
        assert_eq!(foo.len(), 0);
    }
}
