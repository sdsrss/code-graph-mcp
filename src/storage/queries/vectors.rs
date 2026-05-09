use anyhow::Result;
use rusqlite::Connection;

pub fn insert_node_vector(conn: &Connection, node_id: i64, embedding: &[f32]) -> Result<()> {
    let bytes: &[u8] = bytemuck::cast_slice(embedding);
    conn.execute(
        "INSERT OR REPLACE INTO node_vectors(node_id, embedding) VALUES (?1, ?2)",
        rusqlite::params![node_id, bytes],
    )?;
    Ok(())
}

/// Batch insert vectors using a single prepared statement.
/// For best performance, caller should wrap in a transaction (avoids per-statement fsync).
pub fn insert_node_vectors_batch(conn: &Connection, vectors: &[(i64, Vec<f32>)]) -> Result<()> {
    if vectors.is_empty() {
        return Ok(());
    }
    // vec0 virtual tables do not support INSERT OR REPLACE, so delete first.
    let mut del_stmt = conn.prepare_cached(
        "DELETE FROM node_vectors WHERE node_id = ?1"
    )?;
    let mut ins_stmt = conn.prepare_cached(
        "INSERT INTO node_vectors(node_id, embedding) VALUES (?1, ?2)"
    )?;
    for (node_id, embedding) in vectors {
        let bytes: &[u8] = bytemuck::cast_slice(embedding);
        del_stmt.execute(rusqlite::params![node_id])?;
        ins_stmt.execute(rusqlite::params![node_id, bytes])?;
    }
    Ok(())
}

pub fn vector_search(conn: &Connection, query_embedding: &[f32], limit: i64) -> Result<Vec<(i64, f64)>> {
    let bytes: &[u8] = bytemuck::cast_slice(query_embedding);
    let mut stmt = conn.prepare(
        "SELECT node_id, distance FROM node_vectors WHERE embedding MATCH ?1 ORDER BY distance LIMIT ?2"
    )?;
    let rows = stmt.query_map(rusqlite::params![bytes, limit], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
    })?;
    let results = rows.collect::<Result<Vec<_>, _>>()?;
    Ok(results)
}

pub fn get_node_embedding(conn: &Connection, node_id: i64) -> Result<Vec<u8>> {
    let bytes: Vec<u8> = conn.query_row(
        "SELECT embedding FROM node_vectors WHERE node_id = ?1",
        [node_id],
        |row| row.get(0),
    )?;
    Ok(bytes)
}

// --- Unembedded nodes ---

/// Get (node_id, context_string) for nodes that have context strings but no vectors.
/// Returns at most `limit` rows per call to bound memory usage.
pub fn get_unembedded_nodes(conn: &Connection, limit: usize) -> Result<Vec<(i64, String)>> {
    // Priority: embed hot-path nodes first (most referenced = highest value for search)
    // Uses LEFT JOIN + GROUP BY instead of correlated subquery for better performance
    let mut stmt = conn.prepare(
        "SELECT n.id, n.context_string
         FROM nodes n
         LEFT JOIN node_vectors nv ON n.id = nv.node_id
         LEFT JOIN edges e ON e.target_id = n.id
         WHERE nv.node_id IS NULL AND n.context_string IS NOT NULL
         GROUP BY n.id
         ORDER BY COUNT(e.target_id) DESC
         LIMIT ?1"
    )?;
    let rows = stmt.query_map([limit as i64], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Count nodes with embeddings vs total embeddable nodes.
/// Returns (with_vectors, total_embeddable).
pub fn count_nodes_with_vectors(conn: &Connection) -> Result<(i64, i64)> {
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM nodes WHERE context_string IS NOT NULL", [], |r| r.get(0)
    )?;
    // node_vectors table may not exist when embed-model feature is disabled; return 0 in that case
    let with_vectors: i64 = conn.query_row(
        "SELECT COUNT(*) FROM node_vectors", [], |r| r.get(0)
    ).unwrap_or(0);
    Ok((with_vectors, total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::files::{upsert_file, FileRecord};
    use super::super::helpers::test_db;
    use super::super::nodes::{insert_node, NodeRecord};

    #[test]
    fn test_get_unembedded_nodes_priority_order() {
        // Verify that get_unembedded_nodes returns nodes ordered by edge reference count (most referenced first)
        let (db, _tmp) = test_db();
        let conn = db.conn();
        let fid = upsert_file(conn, &FileRecord {
            path: "t.ts".into(), blake3_hash: "h".into(), last_modified: 1, language: None,
        }).unwrap();

        // Create 3 nodes with context strings
        let nid1 = insert_node(conn, &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "popular".into(),
            qualified_name: None, start_line: 1, end_line: 5,
            code_content: "function popular() {}".into(),
            signature: None, doc_comment: None, context_string: Some("function popular".into()),
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();
        let nid2 = insert_node(conn, &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "moderate".into(),
            qualified_name: None, start_line: 10, end_line: 15,
            code_content: "function moderate() {}".into(),
            signature: None, doc_comment: None, context_string: Some("function moderate".into()),
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();
        let nid3 = insert_node(conn, &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "lonely".into(),
            qualified_name: None, start_line: 20, end_line: 25,
            code_content: "function lonely() {}".into(),
            signature: None, doc_comment: None, context_string: Some("function lonely".into()),
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();

        // Create a caller node (no context string so it won't appear in results)
        let caller = insert_node(conn, &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "caller".into(),
            qualified_name: None, start_line: 30, end_line: 35,
            code_content: "function caller() {}".into(),
            signature: None, doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();

        // "popular" gets 3 incoming edges, "moderate" gets 1, "lonely" gets 0
        for _ in 0..3 {
            // Use different callers for unique edges - but we only have one caller node
            // Use different relations to make them unique
            conn.execute(
                "INSERT OR IGNORE INTO edges (source_id, target_id, relation) VALUES (?1, ?2, ?3)",
                rusqlite::params![caller, nid1, "calls"],
            ).unwrap();
        }
        // Add additional edges with different metadata to make them unique
        conn.execute(
            "INSERT INTO edges (source_id, target_id, relation, metadata) VALUES (?1, ?2, 'calls', 'a')",
            rusqlite::params![caller, nid1],
        ).unwrap();
        conn.execute(
            "INSERT INTO edges (source_id, target_id, relation, metadata) VALUES (?1, ?2, 'calls', 'b')",
            rusqlite::params![caller, nid1],
        ).unwrap();
        conn.execute(
            "INSERT INTO edges (source_id, target_id, relation) VALUES (?1, ?2, 'calls')",
            rusqlite::params![caller, nid2],
        ).unwrap();

        // Create vec tables for the LEFT JOIN to work
        conn.execute_batch(&crate::storage::schema::create_vec_tables_sql()).unwrap();

        let results = get_unembedded_nodes(conn, 10).unwrap();
        assert_eq!(results.len(), 3, "should return all 3 nodes with context strings");

        // First result should be "popular" (most referenced: 3 edges)
        assert_eq!(results[0].0, nid1, "most referenced node should be first");
        // Second should be "moderate" (1 edge)
        assert_eq!(results[1].0, nid2, "moderately referenced node should be second");
        // Third should be "lonely" (0 edges)
        assert_eq!(results[2].0, nid3, "unreferenced node should be last");
    }
}
