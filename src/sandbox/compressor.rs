use anyhow::Result;
use rusqlite::Connection;

pub struct CompressedResult {
    pub node_id: i64,
    pub file_path: String,
    pub summary: String,
}

/// Compress results if needed, with opportunistic sandbox cleanup.
/// `file_paths` maps each result's index to its file path.
pub fn compress_if_needed(
    conn: &Connection,
    results: &[crate::storage::queries::NodeResult],
    file_paths: &[String],
    token_threshold: usize,
) -> Option<Vec<CompressedResult>> {
    let _ = cleanup_expired_sandbox(conn); // opportunistic cleanup
    if should_compress(results, token_threshold) {
        Some(compress_results(results, file_paths))
    } else {
        None
    }
}

/// Check if results exceed token threshold (estimate: 1 token ~ 3 chars for code)
pub fn should_compress(
    results: &[crate::storage::queries::NodeResult],
    token_threshold: usize,
) -> bool {
    let total_chars: usize = results.iter().map(|r| {
        r.code_content.len()
            + r.name.len()
            + r.signature.as_ref().map_or(0, |s| s.len())
            + r.context_string.as_ref().map_or(0, |s| s.len())
    }).sum();
    total_chars / 3 > token_threshold
}

/// Compress results to summaries with node IDs for read_snippet expansion
pub fn compress_results(
    results: &[crate::storage::queries::NodeResult],
    file_paths: &[String],
) -> Vec<CompressedResult> {
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

/// Clean up expired sandbox entries
pub fn cleanup_expired_sandbox(conn: &Connection) -> Result<()> {
    conn.execute(
        "DELETE FROM context_sandbox WHERE expires_at < unixepoch()",
        [],
    )?;
    Ok(())
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
    fn test_should_compress_small_results() {
        let results = vec![NodeResult {
            code_content: "short".into(),
            ..default_node()
        }];
        assert!(!should_compress(&results, 2000));
    }

    #[test]
    fn test_should_compress_large_results() {
        let results = vec![NodeResult {
            code_content: "x".repeat(9000),
            ..default_node()
        }];
        assert!(should_compress(&results, 2000));
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
    fn test_sandbox_cleanup_expired() {
        use crate::storage::db::Database;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let db = Database::open(&tmp.path().join("test.db")).unwrap();
        // Insert an expired entry (expires_at = 0 which is well in the past)
        db.conn()
            .execute(
                "INSERT INTO context_sandbox (query_hash, summary, pointers, created_at, expires_at) VALUES ('h', 's', '[]', 0, 0)",
                [],
            )
            .unwrap();
        cleanup_expired_sandbox(db.conn()).unwrap();
        let count: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM context_sandbox", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
}
