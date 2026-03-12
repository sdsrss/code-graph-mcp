# code-graph-mcp Maturity Improvement Plan

**Date**: 2026-03-13
**Version**: v0.3.0 → v0.6.0
**Approach**: Experience-driven (方案 B) — address daily-use pain points first
**Context**: Personal tool, mixed project sizes (small to large), medium effort budget

## Current State

| Metric | Value |
|--------|-------|
| Rust LOC | 7,545 |
| Tests | 150 (125 unit + 25 integration) |
| MCP Tools | 14 |
| Clippy warnings | 0 |
| CI/CD | 5-platform cross-compilation + auto npm publish |
| Maturity score | 7.5/10 |

**Pain points**: Indexing speed, search quality, stability

## Root Cause Analysis

### Stability

1. **No error boundary**: Tool handlers lack defensive error handling; a single file error can poison mutexes and corrupt state. Note: `catch_unwind` is incompatible with `panic = "abort"` in release profile (Cargo.toml:47), so the approach must be converting panic paths to `Result` returns (server.rs ensure_indexed)
2. **Unbounded memory**: `parsed_files: Vec<FileParsed>` collects all ASTs in memory; 100K files → OOM (pipeline.rs:258-379)
3. **Cache lost on failure**: `dir_cache.take()` empties cache before incremental indexing; failure means no restore. Only affects incremental path, not first full-index (server.rs:155)
4. **All-or-nothing transaction**: Single `unchecked_transaction` for all 3 phases; partial failure rolls back everything (pipeline.rs:237)

### Indexing Performance

1. **Sequential node INSERT**: One INSERT per node, no batching (pipeline.rs:332)
2. **Sequential edge INSERT**: One INSERT OR IGNORE per edge, nested O(src × tgt) loop (pipeline.rs:411)
3. **No true batch embedding**: `embed_batch()` exists at model.rs:120 but is just a sequential loop over `embed()`. Pipeline calls `embed()` per-node (pipeline.rs:453). Real batch inference (single tensor forward pass) not implemented
4. **Single large transaction**: Holds DB lock for all 3 phases, blocks reads (pipeline.rs:237)

### Search Quality

1. **Default FTS5 tokenizer**: No camelCase/PascalCase splitting; `validateToken` indexed as single token (queries.rs:741)
2. **RRF weight imbalance**: fts_weight=1.5, vec_weight=1.0, k=60; lexical results override semantic (server.rs:534)
3. **BM25 field weight skew**: name=10.0, code_content=1.0; empty function named "parse" beats actual parsing code (queries.rs:744)
4. **Context string order**: Graph metadata (callers/callees) before code; 512-token embedding limit drops actual code (context.rs)
5. **No type information**: Return types and parameter types not extracted from AST (treesitter.rs)

---

## Phase 1: Quick Stabilization (v0.4.0)

**Goal**: Stop crashes, reduce memory usage on large repos, improve insert throughput by 30-50%
**Effort**: 2-3 sessions

### 1.1 Stability Hardening

#### Defensive error handling in ensure_indexed

Convert remaining panic paths to `Result` returns (cannot use `catch_unwind` because release profile uses `panic = "abort"`). Specifically:
- Audit all `unwrap()` / `expect()` in indexing hot path → convert to `?` or `.unwrap_or_else`
- Ensure tree-sitter parse failures return `Err` (not panic)
- On indexing error: restore `indexed` flag to previous state, restore `dir_cache` from cloned snapshot, return JSON-RPC internal error

**Files**: `src/mcp/server.rs`, `src/indexer/pipeline.rs`, `src/parser/treesitter.rs`

#### Streaming batch indexing

Replace single-pass `parsed_files` collection with batched processing:
- Split file list into batches of 500
- Each batch: parse → insert nodes → insert edges → commit transaction
- Failed batch: log warning, skip, continue to next batch
- Phase 3 (context/embedding): runs after all batches committed

**Files**: `src/indexer/pipeline.rs`

#### Cache snapshot and restore

Before `dir_cache.take()`, clone the current cache. On indexing failure, restore from snapshot.

**Files**: `src/mcp/server.rs`

#### Per-file error isolation

In Phase 1 loop: wrap individual file parsing in Result match. On error, log warning with file path and continue.

**Files**: `src/indexer/pipeline.rs`

### 1.2 Indexing Performance

#### Batch node INSERT

Current code uses `INSERT ... RETURNING id` to get auto-generated node IDs for edge insertion. Two approaches:

- **Option A (recommended)**: Use `conn.prepare_cached()` for the existing single-row INSERT. This caches the prepared statement, eliminating repeated statement preparation overhead. ~80% of the benefit with minimal code change.
- **Option B**: Multi-row INSERT without RETURNING, then query back IDs by (file_id, name) composite key. More complex, better throughput for very large batches.

Start with Option A; profile before pursuing Option B.

**Files**: `src/storage/queries.rs`, `src/indexer/pipeline.rs`

#### Batch edge INSERT

Collect edges per batch (up to 100 per multi-row INSERT OR IGNORE):
```sql
INSERT OR IGNORE INTO edges (source_id, target_id, relation, ...) VALUES ...
```

**Files**: `src/storage/queries.rs`, `src/indexer/pipeline.rs`

#### Implement true batch embedding (stretch goal)

Current `embed_batch()` is just a sequential loop — no actual performance gain. True batch inference requires:
- Tokenize all context strings, pad to uniform length
- Stack into a single tensor, run one forward pass through BERT
- This is non-trivial with Candle and may not be the bottleneck

**Decision**: Profile first. If embedding is <20% of total index time, skip this. If >20%, implement true batch inference.

**Files**: `src/embedding/model.rs`, `src/indexer/pipeline.rs`

#### Ensure WAL mode is set

Verify `PRAGMA journal_mode=WAL` is set during DB init (may already be present in schema.rs). WAL mode enables concurrent reads during writes, directly supporting split-transaction approach below.

**Files**: `src/storage/schema.rs`

#### Split transactions by phase

Each phase gets its own transaction:
- Phase 1 transaction: file parse + node insert (per batch)
- Phase 2 transaction: relation extraction + edge insert
- Phase 3 transaction: context build + embedding

Between phases, readers can access the database.

**Files**: `src/indexer/pipeline.rs`

### 1.1 + 1.2 Verification

- [ ] `cargo test --no-default-features` passes
- [ ] `cargo clippy -- -D warnings` clean
- [ ] Manual test: index this repo, verify no crash
- [ ] Manual test: index a large repo (1000+ files), verify memory stays <2GB
- [ ] Indexing time benchmark: compare before/after on 500-file repo

---

## Phase 2: Search Quality (v0.5.0)

**Goal**: Semantic search returns relevant results for natural language queries
**Effort**: 2-3 sessions

### 2.1 Code-aware FTS5 tokenizer

Implement camelCase/PascalCase splitting at index time:
- `validateToken` → "validate", "token", "validateToken" (keep original + parts)
- `get_user_by_id` → already split by underscore (no change needed)

**Approach**: The current FTS5 table uses `content='nodes'` with sync triggers (schema.rs:38-51). Directly modifying indexed text would break trigger sync. Instead:

1. Add a `name_tokens TEXT` column to `nodes` table (stores split form: "validate token validateToken")
2. Include `name_tokens` in FTS5 virtual table definition
3. Populate `name_tokens` during node insertion using a Rust splitting function
4. Coordinate this schema change with Phase 2.4 (type columns) — single migration

This avoids corrupting display data while enabling split-token search.

**Files**: `src/storage/schema.rs`, `src/storage/queries.rs`, new `src/search/tokenizer.rs`

### 2.2 RRF and BM25 weight tuning

Starting parameters (to be validated empirically):
```
fts_weight = 1.0
vec_weight = 1.2
k = 20
BM25 weights: name=5, qualified_name=3, code_content=2, context_string=2, doc_comment=1
```

Rationale:
- vec_weight > fts_weight: semantic matches should rank higher for natural language queries
- k=20: tighter ranking differentiation in top results (rank 1 vs 5 gap: 19% vs current 6.5%)
- name weight reduced from 10→5: still important but doesn't dominate

**Validation plan**: Create a test harness with 10-15 representative search queries and expected top-3 results. Run before/after weight changes. Tune iteratively. Note: k=20 is aggressive — may need to start at k=30 and lower gradually.

**Files**: `src/mcp/server.rs` (fusion call), `src/storage/queries.rs` (BM25 weights), new `tests/search_quality_test.rs`

### 2.3 Context string reordering

New composition order (code-first):
```
1. signature (always short, high value)
2. code_content (truncated to fit within 512-token budget)
3. doc_comment
4. node_type + name + file_path
5. callers/callees (fill remaining space, truncated last)
```

This ensures embedding model sees actual code semantics, not just graph metadata.

**Files**: `src/embedding/context.rs`

### 2.4 Type information extraction

Extract from tree-sitter AST:
- Return type annotations (TypeScript: `: ReturnType`, Python: `-> ReturnType`, Rust: `-> Type`)
- Parameter type annotations
- Store in new `return_type` and `param_types` columns on nodes table

Add to FTS5 index for type-aware search.

**Files**: `src/parser/treesitter.rs`, `src/storage/schema.rs`, `src/storage/queries.rs`

**Schema migration** (coordinated with 2.1 camelCase tokenizer — single migration):

1. Bump `SCHEMA_VERSION` from 1 to 2 in `schema.rs`
2. On startup, check `PRAGMA user_version`:
   - If version 1: run migration within a transaction
   - If version 0 (fresh): create schema v2 directly
3. Migration steps:
   - `ALTER TABLE nodes ADD COLUMN return_type TEXT`
   - `ALTER TABLE nodes ADD COLUMN param_types TEXT`
   - `ALTER TABLE nodes ADD COLUMN name_tokens TEXT` (for 2.1)
   - `DROP TABLE IF EXISTS nodes_fts`
   - Recreate FTS5 table with new columns included
   - `INSERT INTO nodes_fts(nodes_fts) VALUES('rebuild')` to repopulate
   - `PRAGMA user_version = 2`
4. After migration: log "Schema upgraded to v2, re-index recommended for full type extraction"
5. Backward compatibility: detect old schema → auto-migrate, no interactive prompt (stdio MCP server has no interactive prompt mechanism)

### Phase 2 Verification

- [ ] Test: search "validate token" finds `validateToken` function
- [ ] Test: search "check user access" ranks semantic match above lexical-only match
- [ ] Test: search "returns boolean" finds typed functions
- [ ] All existing tests pass
- [ ] Manual A/B comparison on real codebase

---

## Phase 3: Infrastructure (v0.6.0)

**Goal**: Prevent regressions, enable debugging, minimal docs for self-use
**Effort**: 1-2 sessions

### 3.1 Test coverage expansion

| Category | Tests to add |
|----------|-------------|
| Boundary | Empty file, binary file, >1MB file, 0-function file, deeply nested AST |
| Error recovery | Index failure → re-index, DB lock contention, malformed tree-sitter output |
| Scale | 1000+ file indexing end-to-end (can be slow, marked `#[ignore]`) |
| Performance baseline | criterion.rs benchmarks for index/search/call_graph |

CI addition: `cargo-llvm-cov` step with ≥75% coverage gate.

**Files**: `tests/`, `.github/workflows/ci.yml`, new `benches/`

### 3.2 Observability

Lightweight, stderr-only (no external dependencies):

- **Tool call timing**: Log elapsed time for each `handle_tool` invocation
- **Index progress**: `[index] Phase 1: 150/500 files parsed (2.3s)`
- **Incremental summary**: `[incremental] 3 files changed, 12 nodes updated, 0.8s`
- **Optional verbose**: `RUST_LOG=debug` for SQL query timing

**Files**: `src/mcp/server.rs`, `src/indexer/pipeline.rs`

### 3.3 Documentation

| Document | Scope |
|----------|-------|
| `ARCHITECTURE.md` | Data flow diagram, module responsibilities, key design decisions (RRF choice, Merkle tree, why sqlite-vec) |
| Rustdoc on pub API | `pipeline.rs`, `queries.rs`, `fusion.rs` public functions |

Keep minimal — this is a personal tool, not a community project.

**Files**: `ARCHITECTURE.md`, inline rustdoc comments

### Phase 3 Verification

- [ ] `cargo llvm-cov` reports ≥75%
- [ ] `cargo bench` runs without error
- [ ] ARCHITECTURE.md exists and is accurate
- [ ] CI pipeline includes coverage step

---

## Version Roadmap

```
v0.3.0 (current)
  │
  ├── Phase 1: Stabilization + Indexing Performance
  │   ├── 1.1 Panic isolation, streaming batches, cache restore, per-file error isolation
  │   └── 1.2 Batch INSERT, embed_batch, split transactions
  │
  v0.4.0
  │
  ├── Phase 2: Search Quality
  │   ├── 2.1 camelCase FTS5 tokenizer
  │   ├── 2.2 RRF + BM25 weight tuning
  │   ├── 2.3 Context string reordering (code-first)
  │   └── 2.4 Type information extraction
  │
  v0.5.0
  │
  ├── Phase 3: Infrastructure
  │   ├── 3.1 Boundary/error/scale tests + coverage CI
  │   ├── 3.2 Tool timing + index progress + incremental summary
  │   └── 3.3 ARCHITECTURE.md + rustdoc
  │
  v0.6.0
```

## Success Criteria

| Metric | Current | Target (v0.6.0) |
|--------|---------|-----------------|
| Crash on 1000+ file repo | Possible (OOM) | Never |
| Index throughput (500 files) | ~baseline | 30-50% faster |
| Memory on large repo | Unbounded | <2GB |
| Search: camelCase match | No | Yes |
| Search: semantic ranking | FTS-biased | Balanced |
| Test coverage | Unknown | ≥75% |
| Maturity score | 7.5/10 | 9/10 |
