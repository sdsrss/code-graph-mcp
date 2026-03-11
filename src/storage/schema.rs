pub const SCHEMA_VERSION: i32 = 1;

// Relation type constants
pub const REL_CALLS: &str = "calls";
pub const REL_INHERITS: &str = "inherits";
pub const REL_IMPORTS: &str = "imports";
pub const REL_ROUTES_TO: &str = "routes_to";
pub const REL_IMPLEMENTS: &str = "implements";
pub const REL_EXPORTS: &str = "exports";

pub const CREATE_TABLES: &str = r#"
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
    context_string TEXT
);

CREATE INDEX IF NOT EXISTS idx_nodes_file ON nodes(file_id);
CREATE INDEX IF NOT EXISTS idx_nodes_type ON nodes(type);
CREATE INDEX IF NOT EXISTS idx_nodes_name ON nodes(name);

-- FTS5 virtual table
CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
    name, qualified_name, code_content, context_string, doc_comment,
    content='nodes', content_rowid='id'
);

-- FTS5 sync triggers
CREATE TRIGGER IF NOT EXISTS nodes_ai AFTER INSERT ON nodes BEGIN
    INSERT INTO nodes_fts(rowid, name, qualified_name, code_content, context_string, doc_comment)
    VALUES (new.id, new.name, new.qualified_name, new.code_content, new.context_string, new.doc_comment);
END;
CREATE TRIGGER IF NOT EXISTS nodes_ad AFTER DELETE ON nodes BEGIN
    INSERT INTO nodes_fts(nodes_fts, rowid, name, qualified_name, code_content, context_string, doc_comment)
    VALUES ('delete', old.id, old.name, old.qualified_name, old.code_content, old.context_string, old.doc_comment);
END;
CREATE TRIGGER IF NOT EXISTS nodes_au AFTER UPDATE ON nodes BEGIN
    INSERT INTO nodes_fts(nodes_fts, rowid, name, qualified_name, code_content, context_string, doc_comment)
    VALUES ('delete', old.id, old.name, old.qualified_name, old.code_content, old.context_string, old.doc_comment);
    INSERT INTO nodes_fts(rowid, name, qualified_name, code_content, context_string, doc_comment)
    VALUES (new.id, new.name, new.qualified_name, new.code_content, new.context_string, new.doc_comment);
END;

CREATE TABLE IF NOT EXISTS edges (
    id          INTEGER PRIMARY KEY,
    source_id   INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    target_id   INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    relation    TEXT NOT NULL,
    metadata    TEXT,
    UNIQUE(source_id, target_id, relation)
);

CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source_id);
CREATE INDEX IF NOT EXISTS idx_edges_target ON edges(target_id);
CREATE INDEX IF NOT EXISTS idx_edges_relation ON edges(relation);
CREATE INDEX IF NOT EXISTS idx_edges_source_rel ON edges(source_id, relation);
CREATE INDEX IF NOT EXISTS idx_edges_target_rel ON edges(target_id, relation);

CREATE TABLE IF NOT EXISTS context_sandbox (
    id          INTEGER PRIMARY KEY,
    query_hash  TEXT NOT NULL,
    summary     TEXT NOT NULL,
    pointers    TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_sandbox_query ON context_sandbox(query_hash);
"#;

pub fn create_vec_tables_sql() -> String {
    format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS node_vectors USING vec0(
            node_id INTEGER PRIMARY KEY,
            embedding float[{dim}]
        );

        CREATE TRIGGER IF NOT EXISTS nodes_vectors_ad AFTER DELETE ON nodes BEGIN
            DELETE FROM node_vectors WHERE node_id = old.id;
        END;",
        dim = crate::embedding::model::EMBEDDING_DIM,
    )
}
