use anyhow::Result;
use rusqlite::Connection;

pub struct CompressedResult {
    pub node_id: i64,
    pub summary: String,
}

/// Check if results exceed token threshold (rough estimate: 1 token ~ 4 chars)
pub fn should_compress(
    results: &[crate::storage::queries::NodeResult],
    token_threshold: usize,
) -> bool {
    let total_chars: usize = results.iter().map(|r| r.code_content.len()).sum();
    total_chars / 4 > token_threshold
}

/// Compress results to summaries with node IDs for read_snippet expansion
pub fn compress_results(
    results: &[crate::storage::queries::NodeResult],
) -> Vec<CompressedResult> {
    results
        .iter()
        .map(|r| {
            let summary = format!(
                "{} {} (lines {}-{}){}",
                r.node_type,
                r.name,
                r.start_line,
                r.end_line,
                r.signature
                    .as_ref()
                    .map(|s| format!(" {}", s))
                    .unwrap_or_default(),
            );
            CompressedResult {
                node_id: r.id,
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
        let compressed = compress_results(&results);
        assert_eq!(compressed.len(), 2);
        assert_eq!(compressed[0].node_id, 1);
        assert!(compressed[0].summary.contains("foo"));
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
