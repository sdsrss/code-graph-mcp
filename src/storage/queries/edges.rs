use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashMap;

use super::helpers::{make_placeholders, MAX_IN_PARAMS};

/// Edge info tuple: (relation, direction, target_name, metadata).
/// Used by batch edge queries and context string builders.
pub type EdgeInfo = (String, String, String, Option<String>);

// --- Edge records ---

pub struct EdgeRecord {
    pub source_id: i64,
    pub target_id: i64,
    pub relation: String,
    pub metadata: Option<String>,
}

/// Pending row resolved into one or more concrete edges.
pub struct PendingCallRow {
    pub id: i64,
    pub source_id: i64,
    pub target_name: String,
    pub source_language: String,
    pub metadata: Option<String>,
}

pub struct IncomingReference {
    pub node_id: i64,
    pub name: String,
    pub node_type: String,
    pub file_path: String,
    pub start_line: i64,
    pub relation: String,
}

// --- Edge CRUD ---

/// Insert an edge, ignoring duplicates. Returns true if a new row was actually inserted.
pub fn insert_edge(conn: &Connection, source_id: i64, target_id: i64, relation: &str, metadata: Option<&str>) -> Result<bool> {
    conn.execute(
        "INSERT OR IGNORE INTO edges (source_id, target_id, relation, metadata)
         VALUES (?1, ?2, ?3, ?4)",
        (source_id, target_id, relation, metadata),
    )?;
    Ok(conn.changes() > 0)
}

/// Insert an edge using a cached prepared statement. Returns true if new row inserted.
pub fn insert_edge_cached(conn: &Connection, source_id: i64, target_id: i64, relation: &str, metadata: Option<&str>) -> Result<bool> {
    let mut stmt = conn.prepare_cached(
        "INSERT OR IGNORE INTO edges (source_id, target_id, relation, metadata)
         VALUES (?1, ?2, ?3, ?4)"
    )?;
    let rows = stmt.execute((source_id, target_id, relation, metadata))?;
    Ok(rows > 0)
}

/// Buffer a REL_CALLS edge that Phase 2 couldn't resolve against same-file
/// or same-language candidates so a later resolution sweep can claim it once
/// the callee is added. Idempotent via the unique index on
/// (source_id, target_name, source_language).
pub fn insert_pending_unresolved_call(
    conn: &Connection,
    source_id: i64,
    target_name: &str,
    source_language: &str,
    metadata: Option<&str>,
) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "INSERT OR IGNORE INTO pending_unresolved_calls (source_id, target_name, source_language, metadata)
         VALUES (?1, ?2, ?3, ?4)"
    )?;
    stmt.execute((source_id, target_name, source_language, metadata))?;
    Ok(())
}

/// Stream all pending rows. Caller does the per-row resolution + edge insert.
pub fn list_pending_unresolved_calls(conn: &Connection) -> Result<Vec<PendingCallRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, source_id, target_name, source_language, metadata FROM pending_unresolved_calls"
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(PendingCallRow {
            id: row.get(0)?,
            source_id: row.get(1)?,
            target_name: row.get(2)?,
            source_language: row.get(3)?,
            metadata: row.get(4)?,
        })
    })?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Drop a pending row by id (called after the row was successfully resolved).
pub fn delete_pending_unresolved_call(conn: &Connection, id: i64) -> Result<()> {
    let mut stmt = conn.prepare_cached("DELETE FROM pending_unresolved_calls WHERE id = ?1")?;
    stmt.execute([id])?;
    Ok(())
}

/// Diagnostic: number of buffered unresolved calls. Useful in tests + a future
/// `code-graph-mcp health-check` warning when the table grows unbounded.
pub fn count_pending_unresolved_calls(conn: &Connection) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pending_unresolved_calls",
        [],
        |row| row.get(0),
    )?;
    Ok(count)
}

pub fn get_edges_from(conn: &Connection, node_id: i64) -> Result<Vec<EdgeRecord>> {
    let mut stmt = conn.prepare(
        "SELECT source_id, target_id, relation, metadata FROM edges WHERE source_id = ?1"
    )?;
    let rows = stmt.query_map([node_id], |row| {
        Ok(EdgeRecord {
            source_id: row.get(0)?,
            target_id: row.get(1)?,
            relation: row.get(2)?,
            metadata: row.get(3)?,
        })
    })?;
    let results = rows.collect::<Result<Vec<_>, _>>()?;
    Ok(results)
}

// --- Graph query helpers ---

pub fn get_edge_target_names(conn: &Connection, source_id: i64, relation: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT n.name FROM edges e JOIN nodes n ON n.id = e.target_id
         WHERE e.source_id = ?1 AND e.relation = ?2"
    )?;
    let rows = stmt.query_map(rusqlite::params![source_id, relation], |row| {
        row.get::<_, String>(0)
    })?;
    let results = rows.collect::<Result<Vec<_>, _>>()?;
    Ok(results)
}

/// Batch-fetch edge target names for multiple source IDs in one query.
/// Returns a map from source_id to list of target names.
pub fn get_edge_target_names_batch(conn: &Connection, source_ids: &[i64], relation: &str) -> Result<HashMap<i64, Vec<String>>> {
    let mut result: HashMap<i64, Vec<String>> = HashMap::new();
    if source_ids.is_empty() {
        return Ok(result);
    }
    for chunk in source_ids.chunks(MAX_IN_PARAMS) {
        let placeholders = make_placeholders(2, chunk.len());
        let sql = format!(
            "SELECT e.source_id, n.name FROM edges e JOIN nodes n ON n.id = e.target_id
             WHERE e.source_id IN ({}) AND e.relation = ?1",
            placeholders
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut params: Vec<&dyn rusqlite::types::ToSql> = vec![&relation as &dyn rusqlite::types::ToSql];
        for id in chunk {
            params.push(id as &dyn rusqlite::types::ToSql);
        }
        let rows = stmt.query_map(params.as_slice(), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (src_id, name) = row?;
            result.entry(src_id).or_default().push(name);
        }
    }
    Ok(result)
}

pub fn get_edge_source_names(conn: &Connection, target_id: i64, relation: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT n.name FROM edges e JOIN nodes n ON n.id = e.source_id
         WHERE e.target_id = ?1 AND e.relation = ?2"
    )?;
    let rows = stmt.query_map(rusqlite::params![target_id, relation], |row| {
        row.get::<_, String>(0)
    })?;
    let results = rows.collect::<Result<Vec<_>, _>>()?;
    Ok(results)
}

/// Like get_edge_target_names but also returns the file path for each target node.
pub fn get_edge_targets_with_files(conn: &Connection, source_id: i64, relation: &str) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT n.name, COALESCE(f.path, '') FROM edges e
         JOIN nodes n ON n.id = e.target_id
         LEFT JOIN files f ON f.id = n.file_id
         WHERE e.source_id = ?1 AND e.relation = ?2"
    )?;
    let rows = stmt.query_map(rusqlite::params![source_id, relation], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Like get_edge_source_names but also returns the file path for each source node.
pub fn get_edge_sources_with_files(conn: &Connection, target_id: i64, relation: &str) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT n.name, COALESCE(f.path, '') FROM edges e
         JOIN nodes n ON n.id = e.source_id
         LEFT JOIN files f ON f.id = n.file_id
         WHERE e.target_id = ?1 AND e.relation = ?2"
    )?;
    let rows = stmt.query_map(rusqlite::params![target_id, relation], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Find all incoming references (source nodes) pointing to a target node, with file paths.
/// Optionally filter by relation type. Returns structured reference info.
pub fn get_incoming_references(
    conn: &Connection,
    target_id: i64,
    relation_filter: Option<&str>,
) -> Result<Vec<IncomingReference>> {
    let sql = if relation_filter.is_some() {
        "SELECT n.id, n.name, n.type, f.path, n.start_line, e.relation
         FROM edges e
         JOIN nodes n ON n.id = e.source_id
         LEFT JOIN files f ON f.id = n.file_id
         WHERE e.target_id = ?1 AND e.relation = ?2
         ORDER BY f.path, n.start_line"
    } else {
        "SELECT n.id, n.name, n.type, f.path, n.start_line, e.relation
         FROM edges e
         JOIN nodes n ON n.id = e.source_id
         LEFT JOIN files f ON f.id = n.file_id
         WHERE e.target_id = ?1
         ORDER BY e.relation, f.path, n.start_line"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = if let Some(rel) = relation_filter {
        stmt.query_map(rusqlite::params![target_id, rel], map_incoming_ref)?
    } else {
        stmt.query_map(rusqlite::params![target_id], map_incoming_ref)?
    };
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn map_incoming_ref(row: &rusqlite::Row) -> rusqlite::Result<IncomingReference> {
    Ok(IncomingReference {
        node_id: row.get(0)?,
        name: row.get(1)?,
        node_type: row.get(2)?,
        file_path: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
        start_line: row.get(4)?,
        relation: row.get(5)?,
    })
}

/// Batch-fetch all edge info for a set of node IDs, grouped by node_id.
/// Each entry is an [`EdgeInfo`] tuple: (relation, direction, target_name, metadata).
/// Direction is "out" for outgoing edges (source=node), "in" for incoming edges (target=node).
pub fn get_edges_batch(conn: &Connection, node_ids: &[i64]) -> Result<HashMap<i64, Vec<EdgeInfo>>> {
    if node_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut result: HashMap<i64, Vec<EdgeInfo>> = HashMap::new();

    for chunk in node_ids.chunks(MAX_IN_PARAMS) {
        let placeholders = make_placeholders(1, chunk.len());
        let params: Vec<&dyn rusqlite::types::ToSql> = chunk.iter().map(|id| id as &dyn rusqlite::types::ToSql).collect();

        // Outgoing edges: node is source
        let sql_out = format!(
            "SELECT e.source_id, e.relation, n.name, e.metadata FROM edges e JOIN nodes n ON n.id = e.target_id WHERE e.source_id IN ({})",
            placeholders
        );
        let mut stmt = conn.prepare(&sql_out)?;
        let rows = stmt.query_map(params.as_slice(), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, Option<String>>(3)?))
        })?;
        for row in rows {
            let (source_id, relation, name, metadata) = row?;
            result.entry(source_id).or_default().push((relation, "out".into(), name, metadata));
        }

        // Incoming edges: node is target
        let sql_in = format!(
            "SELECT e.target_id, e.relation, n.name, e.metadata FROM edges e JOIN nodes n ON n.id = e.source_id WHERE e.target_id IN ({})",
            placeholders
        );
        let mut stmt = conn.prepare(&sql_in)?;
        let rows = stmt.query_map(params.as_slice(), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, Option<String>>(3)?))
        })?;
        for row in rows {
            let (target_id, relation, name, metadata) = row?;
            result.entry(target_id).or_default().push((relation, "in".into(), name, metadata));
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::files::{delete_files_by_paths, upsert_file, FileRecord};
    use super::super::helpers::test_db;
    use super::super::nodes::{insert_node, NodeRecord};

    #[test]
    fn test_insert_edge_and_cascade_delete() {
        let (db, _tmp) = test_db();
        let fid = upsert_file(db.conn(), &FileRecord {
            path: "t.ts".into(), blake3_hash: "h".into(), last_modified: 1, language: None,
        }).unwrap();
        let n1 = insert_node(db.conn(), &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "a".into(),
            qualified_name: None, start_line: 1, end_line: 5,
            code_content: "fn a(){}".into(), signature: None,
            doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();
        let n2 = insert_node(db.conn(), &NodeRecord {
            file_id: fid, node_type: "function".into(), name: "b".into(),
            qualified_name: None, start_line: 6, end_line: 10,
            code_content: "fn b(){}".into(), signature: None,
            doc_comment: None, context_string: None,
            name_tokens: None, return_type: None, param_types: None, is_test: false,
        }).unwrap();

        insert_edge(db.conn(), n1, n2, "calls", None).unwrap();

        let edges = get_edges_from(db.conn(), n1).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].relation, "calls");

        // Delete the file → CASCADE deletes nodes → CASCADE deletes edges
        delete_files_by_paths(db.conn(), &["t.ts".into()]).unwrap();
        let edges_after = get_edges_from(db.conn(), n1).unwrap();
        assert_eq!(edges_after.len(), 0);
    }
}
