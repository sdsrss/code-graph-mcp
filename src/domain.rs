// Shared domain constants used across modules.
// Relation constants, embedding dimensions, and other cross-cutting concerns
// live here to avoid layer violations (e.g., parser importing from storage).

// -- Relation types --
pub const REL_CALLS: &str = "calls";
pub const REL_INHERITS: &str = "inherits";
pub const REL_IMPORTS: &str = "imports";
pub const REL_ROUTES_TO: &str = "routes_to";
pub const REL_IMPLEMENTS: &str = "implements";
pub const REL_EXPORTS: &str = "exports";

// -- Index version --
// Bump this when parser/indexer logic changes in a way that produces different
// nodes or edges for the same source files. The server will detect a mismatch
// and automatically clear + rebuild the index.
// This is separate from SCHEMA_VERSION (which tracks table structure changes).
pub const INDEX_VERSION: i32 = 2;

// -- Embedding --
pub const EMBEDDING_DIM: usize = 384;

// -- Parsing limits --
pub const MAX_AST_DEPTH: usize = 64;
pub const MAX_RELATION_DEPTH: usize = 256;
pub const PARSE_TIMEOUT_MS: u64 = 5000; // 5 seconds max per file parse

// -- Indexing limits --
pub const MAX_FILE_SIZE: u64 = 1_048_576; // 1 MB
pub const MAX_CODE_CONTENT_LEN: usize = 4096; // 4KB max stored per node

// -- Edge resolution noise filter --
// Common standard-library method/trait names that produce false-positive call edges
// when resolved cross-file by name alone (without type context).
// These are skipped for cross-file `calls` edge creation.
pub const CROSS_FILE_CALL_NOISE: &[&str] = &[
    "new", "default", "from", "into", "as_str", "to_string", "clone",
    "fmt", "display", "drop", "try_from", "try_into",
    "as_ref", "as_mut", "borrow", "borrow_mut", "deref", "deref_mut",
    "eq", "ne", "cmp", "partial_cmp", "hash",
    "serialize", "deserialize",
    "next", "iter", "into_iter",
    "build", "builder",
    "len", "is_empty",
    "unwrap", "unwrap_or", "unwrap_or_else", "unwrap_or_default",
    "expect", "ok", "err", "map", "map_err", "and_then",
    "or_else", "filter", "flatten",
    "push", "pop", "insert", "remove", "contains", "get",
    "to_owned", "to_vec", "collect", "join",
];
