pub const SCHEMA_VERSION: i32 = 6;

/// FTS5 sync trigger SQL — single source of truth.
/// Used by CREATE_TABLES (fresh init) and migrations that recreate the FTS5 table.
const FTS5_TRIGGERS: &str = "
CREATE TRIGGER IF NOT EXISTS nodes_ai AFTER INSERT ON nodes BEGIN
    INSERT INTO nodes_fts(rowid, name, qualified_name, code_content, context_string, doc_comment, name_tokens, return_type, param_types)
    VALUES (new.id, new.name, new.qualified_name, new.code_content, new.context_string, new.doc_comment, new.name_tokens, new.return_type, new.param_types);
END;
CREATE TRIGGER IF NOT EXISTS nodes_ad AFTER DELETE ON nodes BEGIN
    INSERT INTO nodes_fts(nodes_fts, rowid, name, qualified_name, code_content, context_string, doc_comment, name_tokens, return_type, param_types)
    VALUES ('delete', old.id, old.name, old.qualified_name, old.code_content, old.context_string, old.doc_comment, old.name_tokens, old.return_type, old.param_types);
END;
CREATE TRIGGER IF NOT EXISTS nodes_au AFTER UPDATE ON nodes BEGIN
    INSERT INTO nodes_fts(nodes_fts, rowid, name, qualified_name, code_content, context_string, doc_comment, name_tokens, return_type, param_types)
    VALUES ('delete', old.id, old.name, old.qualified_name, old.code_content, old.context_string, old.doc_comment, old.name_tokens, old.return_type, old.param_types);
    INSERT INTO nodes_fts(rowid, name, qualified_name, code_content, context_string, doc_comment, name_tokens, return_type, param_types)
    VALUES (new.id, new.name, new.qualified_name, new.code_content, new.context_string, new.doc_comment, new.name_tokens, new.return_type, new.param_types);
END;
";

/// Build the full CREATE_TABLES SQL at runtime by concatenating the static parts.
/// This avoids duplicating FTS5 trigger definitions.
pub fn create_tables_sql() -> String {
    format!(r#"
CREATE TABLE IF NOT EXISTS files (
    id          INTEGER PRIMARY KEY,
    path        TEXT NOT NULL UNIQUE,
    blake3_hash TEXT NOT NULL,
    last_modified INTEGER NOT NULL,
    language    TEXT,
    indexed_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS nodes (
    id          INTEGER PRIMARY KEY,
    file_id     INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    type        TEXT NOT NULL,
    name        TEXT NOT NULL,
    qualified_name TEXT,
    start_line  INTEGER NOT NULL,
    end_line    INTEGER NOT NULL,
    code_content TEXT NOT NULL,
    signature   TEXT,
    doc_comment TEXT,
    context_string TEXT,
    name_tokens TEXT,
    return_type TEXT,
    param_types TEXT,
    is_test     INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_nodes_file ON nodes(file_id);
CREATE INDEX IF NOT EXISTS idx_nodes_type ON nodes(type);
CREATE INDEX IF NOT EXISTS idx_nodes_name ON nodes(name);
CREATE INDEX IF NOT EXISTS idx_nodes_qualified_name ON nodes(qualified_name);

-- FTS5 virtual table (v4: porter stemmer for better natural-language search)
CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
    name, qualified_name, code_content, context_string, doc_comment,
    name_tokens, return_type, param_types,
    content='nodes', content_rowid='id',
    tokenize='porter unicode61'
);

{FTS5_TRIGGERS}

CREATE TABLE IF NOT EXISTS edges (
    id          INTEGER PRIMARY KEY,
    source_id   INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    target_id   INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    relation    TEXT NOT NULL,
    metadata    TEXT
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_edges_unique ON edges(source_id, target_id, relation, COALESCE(metadata, ''));
CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source_id);
CREATE INDEX IF NOT EXISTS idx_edges_target ON edges(target_id);
CREATE INDEX IF NOT EXISTS idx_edges_relation ON edges(relation);
CREATE INDEX IF NOT EXISTS idx_edges_source_rel ON edges(source_id, relation);
CREATE INDEX IF NOT EXISTS idx_edges_target_rel ON edges(target_id, relation);
"#)
}

/// Check if a column exists on a table using PRAGMA table_info (safe from SQL injection).
fn column_exists(conn: &rusqlite::Connection, table: &str, column: &str) -> bool {
    // Validate table name against allowlist to prevent injection via PRAGMA
    const ALLOWED_TABLES: &[&str] = &["files", "nodes", "edges"];
    if !ALLOWED_TABLES.contains(&table) {
        tracing::warn!("column_exists: table '{}' not in allowlist, add it to ALLOWED_TABLES", table);
        return false;
    }
    let sql = format!("PRAGMA table_info({})", table);
    match conn.prepare(&sql) {
        Ok(mut stmt) => {
            let found = stmt.query_map([], |row| row.get::<_, String>(1))
                .map(|rows| rows.filter_map(|r| r.ok()).any(|name| name == column))
                .unwrap_or(false);
            found
        }
        Err(_) => false,
    }
}

/// Add a column only if it doesn't already exist (idempotent ALTER TABLE).
fn add_column_if_not_exists(conn: &rusqlite::Connection, table: &str, column: &str, col_type: &str) -> anyhow::Result<()> {
    if !column_exists(conn, table, column) {
        conn.execute_batch(&format!("ALTER TABLE {} ADD COLUMN {} {}", table, column, col_type))?;
    }
    Ok(())
}

/// Migrate from schema v1 to v2. Must be called within a transaction.
pub fn migrate_v1_to_v2(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    tracing::info!("[schema] Migrating v1 → v2: adding name_tokens, return_type, param_types");

    add_column_if_not_exists(conn, "nodes", "name_tokens", "TEXT")?;
    add_column_if_not_exists(conn, "nodes", "return_type", "TEXT")?;
    add_column_if_not_exists(conn, "nodes", "param_types", "TEXT")?;

    conn.execute_batch(
        "DROP TRIGGER IF EXISTS nodes_ai;
         DROP TRIGGER IF EXISTS nodes_ad;
         DROP TRIGGER IF EXISTS nodes_au;
         DROP TABLE IF EXISTS nodes_fts;"
    )?;

    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
            name, qualified_name, code_content, context_string, doc_comment,
            name_tokens, return_type, param_types,
            content='nodes', content_rowid='id'
        );"
    )?;
    conn.execute_batch(FTS5_TRIGGERS)?;

    conn.execute_batch("INSERT INTO nodes_fts(nodes_fts) VALUES('rebuild');")?;

    conn.pragma_update(None, "user_version", 2)?;

    tracing::info!("[schema] Migration complete. Re-index recommended for full type extraction.");
    Ok(())
}

/// Migrate from schema v2 to v3. Must be called within a transaction.
/// Changes edges UNIQUE constraint to include metadata (enables multiple route edges per file).
pub fn migrate_v2_to_v3(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    tracing::info!("[schema] Migrating v2 → v3: updating edges unique constraint to include metadata");

    // SQLite requires recreating the table to change constraints
    conn.execute_batch(
        "CREATE TABLE edges_new (
            id          INTEGER PRIMARY KEY,
            source_id   INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            target_id   INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            relation    TEXT NOT NULL,
            metadata    TEXT
        );
        INSERT INTO edges_new SELECT * FROM edges;
        DROP TABLE edges;
        ALTER TABLE edges_new RENAME TO edges;
        CREATE UNIQUE INDEX idx_edges_unique ON edges(source_id, target_id, relation, COALESCE(metadata, ''));
        CREATE INDEX idx_edges_source ON edges(source_id);
        CREATE INDEX idx_edges_target ON edges(target_id);
        CREATE INDEX idx_edges_relation ON edges(relation);
        CREATE INDEX idx_edges_source_rel ON edges(source_id, relation);
        CREATE INDEX idx_edges_target_rel ON edges(target_id, relation);"
    )?;

    conn.pragma_update(None, "user_version", 3)?;
    tracing::info!("[schema] Migration v2→v3 complete.");
    Ok(())
}

/// Migrate from schema v3 to v4. Must be called within a transaction.
/// Rebuilds FTS5 table with `porter unicode61` tokenizer for stemmed search.
pub fn migrate_v3_to_v4(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    tracing::info!("[schema] Migrating v3 → v4: enabling porter stemmer for FTS5");

    conn.execute_batch(
        "DROP TRIGGER IF EXISTS nodes_ai;
         DROP TRIGGER IF EXISTS nodes_ad;
         DROP TRIGGER IF EXISTS nodes_au;
         DROP TABLE IF EXISTS nodes_fts;"
    )?;

    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
            name, qualified_name, code_content, context_string, doc_comment,
            name_tokens, return_type, param_types,
            content='nodes', content_rowid='id',
            tokenize='porter unicode61'
        );"
    )?;
    conn.execute_batch(FTS5_TRIGGERS)?;

    conn.execute_batch("INSERT INTO nodes_fts(nodes_fts) VALUES('rebuild');")?;

    conn.pragma_update(None, "user_version", 4)?;
    tracing::info!("[schema] Migration v3→v4 complete.");
    Ok(())
}

pub fn migrate_v4_to_v5(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    tracing::info!("[schema] Migrating v4 → v5: adding is_test column to nodes");
    add_column_if_not_exists(conn, "nodes", "is_test", "INTEGER NOT NULL DEFAULT 0")?;
    conn.pragma_update(None, "user_version", 5)?;
    tracing::info!("[schema] Migration v4→v5 complete.");
    Ok(())
}

pub fn migrate_v5_to_v6(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    tracing::info!("[schema] Migrating v5 -> v6: adding index on qualified_name");
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_nodes_qualified_name ON nodes(qualified_name);"
    )?;
    conn.pragma_update(None, "user_version", 6)?;
    tracing::info!("[schema] Migration v5->v6 complete.");
    Ok(())
}

pub fn create_vec_tables_sql() -> String {
    format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS node_vectors USING vec0(
            node_id INTEGER PRIMARY KEY,
            embedding float[{dim}]
        );

        CREATE TRIGGER IF NOT EXISTS nodes_vectors_ad AFTER DELETE ON nodes BEGIN
            DELETE FROM node_vectors WHERE node_id = old.id;
        END;",
        dim = crate::domain::EMBEDDING_DIM,
    )
}
