# Embedding Enablement Design

## Overview

Enable the existing embedding functionality in release builds and implement auto-download of model files with background async embedding generation. This makes `semantic_code_search` use BM25 + vector RRF fusion and enables `find_similar_code` for all users.

## Background

- Embedding code is fully implemented (`src/embedding/`) using BERT via Candle (384-dim)
- Model files exist: `model.safetensors` (87MB), `tokenizer.json` (466KB), `config.json` (612B)
- RRF fusion search (`src/search/fusion.rs`) is implemented but unused in release
- Release builds use `--no-default-features`, excluding `embed-model` feature
- All users currently get FTS5-only (BM25) search

## Step 1: Model Auto-Download + Release Enablement

### Model File Distribution

- Host as `models.tar.gz` (~30MB compressed) attached to each GitHub Release
- Download URL: `https://github.com/sdsrss/code-graph-mcp/releases/download/v{VERSION}/models.tar.gz`

### Cache Location

- Linux: `~/.cache/code-graph/models/`
- macOS: `~/Library/Caches/code-graph/models/`
- Windows: `%LOCALAPPDATA%/code-graph/models/`
- Version tracking: `.version` file in models directory, triggers re-download on mismatch

### Download Flow

```
EmbeddingModel::load()
  ├── Check cache path for model.safetensors
  │     ├── Exists + version matches → load model
  │     ├── Exists + version mismatch → re-download
  │     └── Not found → download
  ├── Download (background, non-blocking)
  │     ├── Success → extract to cache, write .version, load model
  │     └── Failure → log warning, return Ok(None) → FTS5-only
  └── Model loaded → return Ok(Some(model))
```

### Graceful Degradation

At every stage, failure results in FTS5-only operation. No user-facing errors.

### Dependencies

- Add `ureq` (lightweight HTTP client, ~200KB) to Cargo.toml for model download
- Add `flate2` + `tar` for .tar.gz extraction (or use existing deps if available)

### Release Workflow Changes

```yaml
# release.yml
build:
  # Remove --no-default-features to enable embed-model
  run: cargo build --release --target ${{ matrix.target }}

# New job: package and upload model files
upload-models:
  run: tar czf models.tar.gz -C models .
  # Upload as Release asset alongside platform binaries
```

### CI Changes

```yaml
# ci.yml - add embed-model test matrix
- name: Test (with embedding)
  run: cargo test
- name: Test (without embedding)
  run: cargo test --no-default-features
```

## Step 2: Background Async Embedding Generation

### Architecture

```
Index complete (AST + BM25 ready)
    │
    ├── Tools immediately available (BM25-only search)
    │
    └── Background thread spawned (if model loaded)
         ├── Query: nodes LEFT JOIN node_vectors WHERE vector IS NULL
         ├── Batch generate: BATCH_SIZE = 32
         ├── Write each batch to node_vectors table
         ├── Send progress via MCP notifications/message
         └── On completion: search auto-upgrades to BM25 + Vector fusion
```

### Progressive Enhancement

The search function dynamically adapts based on available vectors:

```rust
// In tool_semantic_search
let fts_results = fts5_search(db, query, fetch_count)?;

let vec_results = if db.vec_enabled() && has_vectors(db) {
    let query_vec = model.embed(query)?;
    vector_search(db, &query_vec, fetch_count)?
} else {
    vec![]  // Degrades to BM25-only
};

let results = if vec_results.is_empty() {
    fts_results  // Pure BM25
} else {
    weighted_rrf_fusion(&fts_results, &vec_results, 60, top_k, 1.0, 1.0)
};
```

### Concurrency Control

- Use existing `embedding_in_progress: Arc<AtomicBool>` to prevent concurrent embedding threads
- Background thread checks this flag before starting
- Graceful shutdown: thread checks a stop signal between batches

### Incremental Updates

- File watcher triggers incremental index → only new/changed nodes need embedding
- `embed_and_store_batch` already handles this — just needs to be called after incremental index

### Status Reporting

`get_index_status` response adds:

```json
{
  "embedding_status": "in_progress",  // or "complete", "unavailable"
  "embedding_progress": "342/510",
  "model_available": true
}
```

## Files to Modify

| File | Change |
|------|--------|
| `Cargo.toml` | Add `ureq`, `flate2`, `tar` deps |
| `src/embedding/model.rs` | Add `download_model()`, cache path logic, version check |
| `src/mcp/server.rs` | Background embedding thread in `run_startup_tasks` |
| `src/mcp/server.rs` | `tool_semantic_search` vector fusion branch |
| `src/mcp/server.rs` | `get_index_status` embedding progress fields |
| `src/storage/queries.rs` | Add `count_nodes_without_vectors()` |
| `src/indexer/pipeline.rs` | Adjust `embed_and_store_batch` call site for async |
| `.github/workflows/release.yml` | Remove `--no-default-features`, add model upload |
| `.github/workflows/ci.yml` | Add embed-model test matrix |

## Degradation Matrix

| Condition | Search Mode | `find_similar_code` |
|-----------|-------------|---------------------|
| Model loaded + vectors ready | BM25 + Vector RRF | Available |
| Model loaded + vectors generating | BM25 + partial Vector | Available (partial) |
| Model downloading | BM25-only | Error: "model loading" |
| Model download failed | BM25-only (Porter) | Error: "model unavailable" |
| `--no-default-features` build | BM25-only (Porter) | Error: "not compiled" |

## Success Criteria

1. `cargo build --release` produces a binary with embedding support
2. First run auto-downloads model files without blocking tools
3. `semantic_code_search` uses RRF fusion once vectors are ready
4. `find_similar_code` works for all users with model available
5. Any failure in the embedding pipeline degrades gracefully to BM25
6. `get_index_status` reports embedding progress accurately
