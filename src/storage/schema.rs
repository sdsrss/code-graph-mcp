pub const SCHEMA_VERSION: i32 = 10;

// Meta keys stored in the `meta` table (added in v7).
pub const META_KEY_EMBEDDING_DIM: &str = "embedding_dim";
pub const META_KEY_EMBEDDING_MODEL: &str = "embedding_model";
/// Set before an index run's first batch commits and cleared once its
/// cross-file edges are durable. Present at startup ⇒ the previous run was
/// killed in between, so file hashes claim "indexed" for files whose edges were
/// only ever in memory — the next incremental escalates to a full re-index
/// (audit 2026-08-16 P1-2). No SCHEMA_VERSION bump: the `meta` table is v7 and
/// an absent key reads as "no run in flight", which is correct for every index
/// built before this key existed.
pub const META_KEY_INDEX_RUN_IN_FLIGHT: &str = "index_run_in_flight";

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
    format!(
        r#"
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
-- (file_id, name) serves "is this name defined in THIS file?", which
-- `bind_calls_to_imported_targets` asks once per candidate edge. It is the ONE
-- post-pass that asks it of `nodes`; the other two ask DIFFERENT questions of
-- the `cg_imports` temp table, so this index does nothing for either:
-- `prune_import_contradicted_call_edges` asks by name (`fid`, `nm`), served by
-- `cg_imports_fid_nm`, while `classify_edge_confidence` asks by target id
-- (`fid`, `tid`), served by `cg_imports_fid_tid` — and builds the temp table
-- with `with_name = false`, so the name index is never even created on its
-- path. Getting this right matters the next time someone weighs the write cost:
-- one wording here credited all three passes, and its correction then credited
-- both siblings to the name index. `idx_nodes_name` alone
-- makes that a name-bucket probe followed by a table fetch per row to test
-- file_id — and in real corpora the hot names (`get`, `run`, `__init__`) have
-- buckets hundreds of rows deep. The composite is COVERING for that predicate,
-- so the row fetch disappears entirely. Does NOT replace `idx_nodes_name`: a
-- name-only lookup cannot use a composite whose leading column is file_id.
CREATE INDEX IF NOT EXISTS idx_nodes_file_name ON nodes(file_id, name);

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
    metadata    TEXT,
    -- Resolution confidence (added by migrate_v8_to_v9, SCHEMA_VERSION 9):
    -- extracted | inferred | ambiguous. Assigned
    -- by classify_edge_confidence after Phase 2 + the pending sweep. Defaults to
    -- 'extracted' so the ~10 precise insert sites need no change; only cross-file
    -- by-name calls/references get downgraded. See domain::CONF_* .
    confidence  TEXT NOT NULL DEFAULT 'extracted'
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_edges_unique ON edges(source_id, target_id, relation, COALESCE(metadata, ''));
CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source_id);
CREATE INDEX IF NOT EXISTS idx_edges_target ON edges(target_id);
CREATE INDEX IF NOT EXISTS idx_edges_relation ON edges(relation);
CREATE INDEX IF NOT EXISTS idx_edges_source_rel ON edges(source_id, relation);
CREATE INDEX IF NOT EXISTS idx_edges_target_rel ON edges(target_id, relation);

-- Key-value metadata table (v7+). Stores embedding_dim / embedding_model so
-- a model swap is detected on open and the vec0 table gets rebuilt instead
-- of silently producing dimension-mismatch junk.
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);

-- Pending unresolved REL_CALLS edges (v8+). When Phase 2 can't resolve a call
-- against same-file or same-language candidates it would silently drop the
-- edge — but in incremental indexing this strands "B calls foo" rows whose
-- callee `foo` later gets added to a sibling file. This table buffers those
-- drops so a post-Phase-2 resolution sweep can claim them as edges once a
-- matching same-language target exists. ON DELETE CASCADE on source_id keeps
-- the table self-cleaning when callers are removed/reindexed.
CREATE TABLE IF NOT EXISTS pending_unresolved_calls (
    id              INTEGER PRIMARY KEY,
    source_id       INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    target_name     TEXT NOT NULL,
    source_language TEXT NOT NULL,
    metadata        TEXT,
    -- Failed-resolution sweep count (added by migrate_v9_to_v10, SCHEMA_VERSION
    -- 10). Rows reaching domain::PENDING_CALL_MAX_ATTEMPTS are evicted so
    -- never-resolvable external/builtin calls don't accumulate forever.
    attempts        INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_pending_target_lang ON pending_unresolved_calls(target_name, source_language);
CREATE INDEX IF NOT EXISTS idx_pending_source ON pending_unresolved_calls(source_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_pending_unique ON pending_unresolved_calls(source_id, target_name, source_language);
"#
    )
}

/// Check if a column exists on a table using PRAGMA table_info (safe from SQL injection).
fn column_exists(conn: &rusqlite::Connection, table: &str, column: &str) -> bool {
    // Validate table name against allowlist to prevent injection via PRAGMA
    const ALLOWED_TABLES: &[&str] = &["files", "nodes", "edges", "pending_unresolved_calls"];
    if !ALLOWED_TABLES.contains(&table) {
        tracing::warn!(
            "column_exists: table '{}' not in allowlist, add it to ALLOWED_TABLES",
            table
        );
        return false;
    }
    let sql = format!("PRAGMA table_info({})", table);
    match conn.prepare(&sql) {
        Ok(mut stmt) => {
            let found = stmt
                .query_map([], |row| row.get::<_, String>(1))
                .map(|rows| rows.filter_map(|r| r.ok()).any(|name| name == column))
                .unwrap_or(false);
            found
        }
        Err(_) => false,
    }
}

/// Add a column only if it doesn't already exist (idempotent ALTER TABLE).
fn add_column_if_not_exists(
    conn: &rusqlite::Connection,
    table: &str,
    column: &str,
    col_type: &str,
) -> anyhow::Result<()> {
    // Validate identifiers to prevent SQL injection
    fn is_valid_ident(s: &str) -> bool {
        !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    }
    fn is_valid_col_type(s: &str) -> bool {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ' ')
    }
    if !is_valid_ident(table) || !is_valid_ident(column) || !is_valid_col_type(col_type) {
        anyhow::bail!(
            "Invalid identifier in ALTER TABLE: table={}, column={}, type={}",
            table,
            column,
            col_type
        );
    }
    if !column_exists(conn, table, column) {
        conn.execute_batch(&format!(
            "ALTER TABLE {} ADD COLUMN {} {}",
            table, column, col_type
        ))?;
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
         DROP TABLE IF EXISTS nodes_fts;",
    )?;

    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
            name, qualified_name, code_content, context_string, doc_comment,
            name_tokens, return_type, param_types,
            content='nodes', content_rowid='id'
        );",
    )?;
    conn.execute_batch(FTS5_TRIGGERS)?;

    conn.execute_batch("INSERT INTO nodes_fts(nodes_fts) VALUES('rebuild');")?;

    tracing::info!("[schema] Migration complete. Re-index recommended for full type extraction.");
    Ok(())
}

/// Migrate from schema v2 to v3. Must be called within a transaction.
/// Changes edges UNIQUE constraint to include metadata (enables multiple route edges per file).
pub fn migrate_v2_to_v3(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    tracing::info!(
        "[schema] Migrating v2 → v3: updating edges unique constraint to include metadata"
    );

    // SQLite requires recreating the table to change constraints
    conn.execute_batch(
        "CREATE TABLE edges_new (
            id          INTEGER PRIMARY KEY,
            source_id   INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            target_id   INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            relation    TEXT NOT NULL,
            metadata    TEXT
        );
        -- Explicit column list (NOT `SELECT *`): if a crash left user_version at 2
        -- while a later migration had already added `edges.confidence` (6 columns),
        -- `SELECT *` fed 6 values into this 5-column table and failed with
        -- '5 columns but 6 values' — a permanent brick that is_corruption_error
        -- didn't match, so no self-heal fired. Naming the 5 v2 columns makes the
        -- re-run forward-compatible with any extra columns (M3).
        INSERT INTO edges_new (id, source_id, target_id, relation, metadata)
            SELECT id, source_id, target_id, relation, metadata FROM edges;
        DROP TABLE edges;
        ALTER TABLE edges_new RENAME TO edges;
        CREATE UNIQUE INDEX idx_edges_unique ON edges(source_id, target_id, relation, COALESCE(metadata, ''));
        CREATE INDEX idx_edges_source ON edges(source_id);
        CREATE INDEX idx_edges_target ON edges(target_id);
        CREATE INDEX idx_edges_relation ON edges(relation);
        CREATE INDEX idx_edges_source_rel ON edges(source_id, relation);
        CREATE INDEX idx_edges_target_rel ON edges(target_id, relation);"
    )?;

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
         DROP TABLE IF EXISTS nodes_fts;",
    )?;

    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
            name, qualified_name, code_content, context_string, doc_comment,
            name_tokens, return_type, param_types,
            content='nodes', content_rowid='id',
            tokenize='porter unicode61'
        );",
    )?;
    conn.execute_batch(FTS5_TRIGGERS)?;

    conn.execute_batch("INSERT INTO nodes_fts(nodes_fts) VALUES('rebuild');")?;

    tracing::info!("[schema] Migration v3→v4 complete.");
    Ok(())
}

pub fn migrate_v4_to_v5(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    tracing::info!("[schema] Migrating v4 → v5: adding is_test column to nodes");
    add_column_if_not_exists(conn, "nodes", "is_test", "INTEGER NOT NULL DEFAULT 0")?;
    tracing::info!("[schema] Migration v4→v5 complete.");
    Ok(())
}

pub fn migrate_v5_to_v6(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    tracing::info!("[schema] Migrating v5 -> v6: adding index on qualified_name");
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_nodes_qualified_name ON nodes(qualified_name);",
    )?;
    tracing::info!("[schema] Migration v5->v6 complete.");
    Ok(())
}

/// Migrate v6 → v7: adds `meta` key-value table used to record
/// embedding_dim / embedding_model. Dim-mismatch detection (drop + rebuild
/// node_vectors) is handled post-migration in `Database::open_impl_inner`
/// so it fires on every open, not just during the one-shot migration.
pub fn migrate_v6_to_v7(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    tracing::info!("[schema] Migrating v6 -> v7: adding meta table for embedding-dim guard");
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (
            key   TEXT PRIMARY KEY NOT NULL,
            value TEXT NOT NULL
        );",
    )?;
    tracing::info!("[schema] Migration v6->v7 complete.");
    Ok(())
}

pub fn migrate_v7_to_v8(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    tracing::info!("[schema] Migrating v7 -> v8: adding pending_unresolved_calls for incremental edge re-resolution");
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS pending_unresolved_calls (
            id              INTEGER PRIMARY KEY,
            source_id       INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
            target_name     TEXT NOT NULL,
            source_language TEXT NOT NULL,
            metadata        TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_pending_target_lang ON pending_unresolved_calls(target_name, source_language);
        CREATE INDEX IF NOT EXISTS idx_pending_source ON pending_unresolved_calls(source_id);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_pending_unique ON pending_unresolved_calls(source_id, target_name, source_language);"
    )?;
    tracing::info!("[schema] Migration v7->v8 complete.");
    Ok(())
}

/// v8 -> v9: add `edges.confidence` (resolution confidence tier). Without this,
/// `CREATE TABLE IF NOT EXISTS edges` is a no-op on an existing table, so an
/// upgraded DB would keep a column-less `edges` and crash with
/// `no such column: confidence` on the next index pass (Phase 2e UPDATE) or
/// `refs`/`find_references` query. Raw guarded ALTER rather than
/// `add_column_if_not_exists` because the quoted `DEFAULT 'extracted'` fails that
/// helper's identifier validator. The DEFAULT backfills existing rows to
/// 'extracted'; the next index pass reclassifies via classify_edge_confidence.
pub fn migrate_v8_to_v9(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    tracing::info!(
        "[schema] Migrating v8 -> v9: adding edges.confidence (resolution confidence tier)"
    );
    // The `edges` table may not exist yet when migrating a contentless older DB
    // (migrations run BEFORE create_tables_sql, which then creates `edges` WITH
    // the column). Only ALTER an existing `edges` table that lacks the column —
    // ALTERing a not-yet-created table would error and fail the whole open.
    let edges_exists: bool = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='edges'",
        [],
        |r| r.get::<_, i64>(0),
    )? > 0;
    if edges_exists && !column_exists(conn, "edges", "confidence") {
        conn.execute_batch(
            "ALTER TABLE edges ADD COLUMN confidence TEXT NOT NULL DEFAULT 'extracted'",
        )?;
    }
    tracing::info!("[schema] Migration v8->v9 complete.");
    Ok(())
}

/// v9 -> v10: add `pending_unresolved_calls.attempts` (failed-sweep counter for
/// bounded pending-row retention). Same seam as v8->v9: `CREATE TABLE IF NOT
/// EXISTS` is a no-op on an upgraded DB, so without a guarded ALTER the column
/// never appears and the sweep's attempts UPDATE crashes with
/// `no such column: attempts`. The table may not exist yet on a contentless
/// older DB (migrations run BEFORE create_tables_sql) — skip then; create_tables
/// builds it WITH the column.
pub fn migrate_v9_to_v10(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    tracing::info!("[schema] Migrating v9 -> v10: adding pending_unresolved_calls.attempts (bounded retention)");
    let table_exists: bool = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='pending_unresolved_calls'",
        [],
        |r| r.get::<_, i64>(0),
    )? > 0;
    if table_exists {
        add_column_if_not_exists(
            conn,
            "pending_unresolved_calls",
            "attempts",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
    }
    tracing::info!("[schema] Migration v9->v10 complete.");
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
        END;

        CREATE TABLE IF NOT EXISTS embedding_cache (
            context_hash BLOB PRIMARY KEY,
            embedding    BLOB NOT NULL
        ) WITHOUT ROWID;",
        dim = crate::domain::EMBEDDING_DIM,
    )
}
