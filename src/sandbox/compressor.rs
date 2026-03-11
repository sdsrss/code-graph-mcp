use std::collections::{BTreeMap, HashSet};

pub struct CompressedResult {
    pub node_id: i64,
    pub file_path: String,
    pub summary: String,
}

pub struct GroupedResult {
    pub file_path: String,
    pub summary: String,
    pub node_ids: Vec<i64>,
}

pub enum CompressedOutput {
    Nodes(Vec<CompressedResult>),
    Files(Vec<GroupedResult>),
    Directories(Vec<GroupedResult>),
}

/// Estimate token count for results (1 token ~ 3 chars for code).
/// context_string already includes name, signature, and code_content,
/// so we use it exclusively when available to avoid double-counting.
fn estimate_tokens(results: &[crate::storage::queries::NodeResult]) -> usize {
    let total_chars: usize = results.iter().map(|r| {
        r.context_string.as_ref().map_or_else(
            || r.code_content.len() + r.name.len() + r.signature.as_ref().map_or(0, |s| s.len()),
            |ctx| ctx.len(),
        )
    }).sum();
    total_chars / 3
}

/// Estimate token count for a JSON value (1 token ~ 3 chars for code)
pub fn estimate_json_tokens(value: &serde_json::Value) -> usize {
    match serde_json::to_string(value) {
        Ok(s) => s.len() / 3,
        Err(_) => 1, // conservative non-zero estimate on serialization failure
    }
}

/// Compress results if needed.
/// `file_paths` maps each result's index to its file path.
///
/// Returns multi-level compression based on token count:
/// - None: tokens <= threshold (no compression needed)
/// - Nodes (L1): tokens <= threshold * 3 (node summaries)
/// - Files (L2): tokens <= threshold * 8 (file groups)
/// - Directories (L3): tokens > threshold * 8 (directory groups)
pub fn compress_if_needed(
    results: &[crate::storage::queries::NodeResult],
    file_paths: &[String],
    token_threshold: usize,
) -> Option<CompressedOutput> {
    let tokens = estimate_tokens(results);
    if tokens <= token_threshold {
        None
    } else if tokens <= token_threshold * 3 {
        Some(CompressedOutput::Nodes(compress_results(results, file_paths)))
    } else if tokens <= token_threshold * 8 {
        Some(CompressedOutput::Files(compress_by_file(results, file_paths)))
    } else {
        Some(CompressedOutput::Directories(compress_by_directory(results, file_paths)))
    }
}

/// Compress results to summaries with node IDs for read_snippet expansion
pub fn compress_results(
    results: &[crate::storage::queries::NodeResult],
    file_paths: &[String],
) -> Vec<CompressedResult> {
    assert_eq!(results.len(), file_paths.len(), "results and file_paths must have same length");
    results
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let fp = file_paths.get(i).map(|s| s.as_str()).unwrap_or("?");
            let summary = format!(
                "{} {} in {} (lines {}-{}){}",
                r.node_type,
                r.name,
                fp,
                r.start_line,
                r.end_line,
                r.signature
                    .as_ref()
                    .map(|s| format!(" {}", s))
                    .unwrap_or_default(),
            );
            CompressedResult {
                node_id: r.id,
                file_path: fp.to_string(),
                summary,
            }
        })
        .collect()
}

/// Group results by file path, producing a summary per file.
pub fn compress_by_file(
    results: &[crate::storage::queries::NodeResult],
    file_paths: &[String],
) -> Vec<GroupedResult> {
    assert_eq!(results.len(), file_paths.len(), "results and file_paths must have same length");
    let mut groups: BTreeMap<String, (Vec<String>, Vec<i64>)> = BTreeMap::new();
    for (i, r) in results.iter().enumerate() {
        let fp = file_paths.get(i).map(|s| s.as_str()).unwrap_or("?");
        let entry = groups.entry(fp.to_string()).or_insert_with(|| (Vec::new(), Vec::new()));
        entry.0.push(format!("{} {}", r.node_type, r.name));
        entry.1.push(r.id);
    }
    groups
        .into_iter()
        .map(|(file_path, (symbols, node_ids))| {
            let n = symbols.len();
            let summary = format!("{}: [{}] ({} symbols)", file_path, symbols.join(", "), n);
            GroupedResult {
                file_path,
                summary,
                node_ids,
            }
        })
        .collect()
}

/// Group results by parent directory, producing a summary per directory.
pub fn compress_by_directory(
    results: &[crate::storage::queries::NodeResult],
    file_paths: &[String],
) -> Vec<GroupedResult> {
    assert_eq!(results.len(), file_paths.len(), "results and file_paths must have same length");
    let mut groups: BTreeMap<String, (HashSet<String>, Vec<i64>, usize)> = BTreeMap::new();
    for (i, r) in results.iter().enumerate() {
        let fp = file_paths.get(i).map(|s| s.as_str()).unwrap_or("?");
        let dir = match fp.rfind('/') {
            Some(pos) => &fp[..pos],
            None => ".",
        };
        let entry = groups.entry(dir.to_string()).or_insert_with(|| (HashSet::new(), Vec::new(), 0));
        entry.0.insert(fp.to_string());
        entry.1.push(r.id);
        entry.2 += 1;
    }
    groups
        .into_iter()
        .map(|(dir, (files, node_ids, symbol_count))| {
            let summary = format!("{}: {} files, {} symbols", dir, files.len(), symbol_count);
            GroupedResult {
                file_path: dir,
                summary,
                node_ids,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::queries::NodeResult;

    fn default_node() -> NodeResult {
        NodeResult {
            id: 0,
            file_id: 0,
            node_type: "function".into(),
            name: "default".into(),
            qualified_name: None,
            start_line: 1,
            end_line: 5,
            code_content: "".into(),
            signature: None,
            doc_comment: None,
            context_string: None,
        }
    }

    #[test]
    fn test_compress_returns_summaries_with_node_ids() {
        let results = vec![
            NodeResult {
                id: 1,
                name: "foo".into(),
                signature: Some("() -> i32".into()),
                code_content: "x".repeat(500),
                ..default_node()
            },
            NodeResult {
                id: 2,
                name: "bar".into(),
                signature: Some("(x: str) -> bool".into()),
                code_content: "y".repeat(500),
                ..default_node()
            },
        ];
        let file_paths = vec!["src/main.rs".to_string(), "src/lib.rs".to_string()];
        let compressed = compress_results(&results, &file_paths);
        assert_eq!(compressed.len(), 2);
        assert_eq!(compressed[0].node_id, 1);
        assert!(compressed[0].summary.contains("foo"));
        assert!(compressed[0].summary.contains("src/main.rs"));
        assert!(compressed[0].summary.contains("() -> i32"));
    }

    #[test]
    fn test_compress_by_file() {
        let results = vec![
            NodeResult {
                id: 1,
                name: "foo".into(),
                node_type: "function".into(),
                code_content: "x".repeat(100),
                start_line: 1,
                end_line: 5,
                ..default_node()
            },
            NodeResult {
                id: 2,
                name: "bar".into(),
                node_type: "function".into(),
                code_content: "x".repeat(100),
                start_line: 10,
                end_line: 15,
                ..default_node()
            },
            NodeResult {
                id: 3,
                name: "baz".into(),
                node_type: "class".into(),
                code_content: "x".repeat(100),
                start_line: 1,
                end_line: 20,
                ..default_node()
            },
        ];
        let file_paths = vec![
            "src/auth.ts".into(),
            "src/auth.ts".into(),
            "src/models.ts".into(),
        ];
        let compressed = compress_by_file(&results, &file_paths);
        assert_eq!(compressed.len(), 2);
        let auth_entry = compressed
            .iter()
            .find(|c| c.file_path == "src/auth.ts")
            .unwrap();
        assert!(auth_entry.summary.contains("foo"));
        assert!(auth_entry.summary.contains("bar"));
        assert!(auth_entry.node_ids.contains(&1));
        assert!(auth_entry.node_ids.contains(&2));
    }

    #[test]
    fn test_compress_by_directory() {
        let results = vec![
            NodeResult {
                id: 1,
                name: "a".into(),
                ..default_node()
            },
            NodeResult {
                id: 2,
                name: "b".into(),
                ..default_node()
            },
            NodeResult {
                id: 3,
                name: "c".into(),
                ..default_node()
            },
        ];
        let file_paths = vec![
            "src/auth/login.ts".into(),
            "src/auth/token.ts".into(),
            "src/models/user.ts".into(),
        ];
        let compressed = compress_by_directory(&results, &file_paths);
        assert_eq!(compressed.len(), 2);
        let auth_dir = compressed
            .iter()
            .find(|c| c.file_path.contains("auth"))
            .unwrap();
        assert!(auth_dir.summary.contains("2 files"));
    }

    #[test]
    fn test_estimate_tokens() {
        // Small content: should be below threshold
        let small = vec![NodeResult {
            code_content: "short".into(),
            ..default_node()
        }];
        assert!(estimate_tokens(&small) < 2000);

        // Large content: should exceed threshold
        let large = vec![NodeResult {
            code_content: "x".repeat(9000),
            ..default_node()
        }];
        assert!(estimate_tokens(&large) > 2000);
    }

}
