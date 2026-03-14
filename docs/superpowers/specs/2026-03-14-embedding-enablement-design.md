# Embedding Enablement Design

## Overview

Enable the existing embedding functionality in release builds and implement auto-download of model files with background async embedding generation. This makes `semantic_code_search` use BM25 + vector RRF fusion and enables `find_similar_code` for all users.

## Background

- Embedding code is fully implemented (`src/embedding/`) using BERT via Candle (384-dim)
- Model files exist: `model.safetensors` (87MB), `tokenizer.json` (466KB), `config.json` (612B)
- RRF fusion search (`src/search/fusion.rs`) is implemented but unused in release
- Release builds use `--no-default-features`, excluding `embed-model` feature
- All users currently get FTS5-only (BM25 + Porter stemmer) search
- `spawn_background_embedding` already exists in `server.rs` with `Arc<AtomicBool>` concurrency control
- SQLite WAL mode with `busy_timeout = 5000ms` already configured for concurrent access

## Step 1: Model Auto-Download + Release Enablement

### Model File Distribution

- Host as `models.tar.gz` (~30MB compressed) attached to each GitHub Release
- Download URL: `https://github.com/sdsrss/code-graph-mcp/releases/download/v{VERSION}/models.tar.gz`
- Integrity: publish `models.tar.gz.sha256` alongside; verify after download
- Model source in CI: download BERT model from HuggingFace during Release workflow

### Cache Location

Use `dirs` crate for cross-platform cache directory resolution:
- Linux: `~/.cache/code-graph/models/`
- macOS: `~/Library/Caches/code-graph/models/`
- Windows: `%LOCALAPPDATA%/code-graph/models/`
- Handle `create_dir_all` failures gracefully (fall back to FTS5-only)

### Version Tracking

- Write `.version` file containing sha256 hash of `models.tar.gz` (not app version)
- Embed expected hash at compile time via `include_str!` or const
- Re-download only when hash mismatches (model changes are independent of app releases)

### Download Flow

Download and model loading are **separate concerns**:

```
Server initialization (from_project_root)
  ├── Check cache for model files
  │     ├── Found + hash matches → load model into Mutex → ready
  │     ├── Found + hash mismatch → spawn download thread
  │     └── Not found → spawn download thread
  ├── Spawn download thread (if needed)
  │     ├── Download with 60s timeout
  │     ├── Verify sha256 checksum
  │     ├── Extract to cache, write .version
  │     ├── Load model → update Mutex<Option<EmbeddingModel>>
  │     └── On any failure: log warning, clear flags, FTS5-only
  └── Server ready immediately (tools work in BM25-only mode)
```

### Model Hot-Loading

Change `embedding_model` field from `Option<EmbeddingModel>` to `Mutex<Option<EmbeddingModel>>`:
- All callsites that read `self.embedding_model` use `.lock()` instead
- Download thread can update the model after async download completes
- The `embedding_in_progress` AtomicBool flag must be cleared in **all** exit paths (success, timeout, error)

### Dependencies (feature-gated)

All download-related deps behind `embed-model` feature to avoid bloating `--no-default-features` builds:

```toml
[dependencies]
ureq = { version = "3", optional = true }        # HTTP download
flate2 = { version = "1", optional = true }       # gzip decompression
tar = { version = "0.4", optional = true }         # tarball extraction
dirs = { version = "6", optional = true }          # cross-platform cache dirs

[features]
embed-model = ["dep:candle-core", "dep:candle-nn", "dep:candle-transformers",
               "dep:tokenizers", "dep:ureq", "dep:flate2", "dep:tar", "dep:dirs"]
```

### Binary Size Impact

Enabling `embed-model` adds Candle + tokenizers + HTTP deps. Expected increase: ~10-15MB.
Accept this tradeoff — embedding is the core value-add. Users needing minimal builds can use `--no-default-features`.

### Release Workflow Changes

```yaml
# release.yml
build:
  # Remove --no-default-features to enable embed-model
  run: cargo build --release --target ${{ matrix.target }}

# New job: package and upload model files
upload-models:
  steps:
    # Download BERT model from HuggingFace (not stored in repo)
    - name: Download model
      run: |
        mkdir -p models
        cd models
        wget https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/model.safetensors
        wget https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/tokenizer.json
        wget https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/config.json
    - name: Package
      run: tar czf models.tar.gz -C models .
    - name: Checksum
      run: sha256sum models.tar.gz > models.tar.gz.sha256
    # Upload both as Release assets
```

### CI Changes

```yaml
# ci.yml - test both modes
- name: Test (with embedding, compilation only)
  run: cargo test
  # Note: embed-model tests use `if let Some(model)` guards,
  # so they verify compilation but skip runtime tests without model files
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
         ├── Query: SELECT nodes without vectors (get_unembedded_nodes)
         ├── Batch: EMBED_PROGRESS_BATCH = 32 nodes per iteration
         ├── For each batch:
         │     ├── Build context strings
         │     ├── model.embed_batch() (BATCH_CHUNK = 8 internal)
         │     ├── insert_node_vectors_batch() to DB
         │     └── Send MCP progress notification
         └── On completion: search auto-upgrades to BM25 + Vector fusion
```

### Batch Size Naming Convention

Three different batch concepts, explicitly named to avoid confusion:
- `EMBED_PROGRESS_BATCH = 32`: nodes per progress-reporting iteration (new, in server.rs)
- `EmbeddingModel::BATCH_CHUNK = 8`: inference chunk size for padding optimization (existing, model.rs)
- `BATCH_SIZE = 500`: file processing batch in indexing pipeline (existing, pipeline.rs)

### Progressive Enhancement

The search function dynamically adapts based on available vectors:

```rust
// In tool_semantic_search
let fts_results = fts5_search(db, query, fetch_count)?;

// Embed query only if model is available
let vec_results = match self.embedding_model.lock().as_ref() {
    Some(model) if db.vec_enabled() && has_vectors(db) => {
        let query_vec = model.embed(query)?;
        vector_search(db.conn(), &query_vec, fetch_count)?
    }
    _ => vec![]  // Degrades to BM25-only
};

let results = if vec_results.is_empty() {
    fts_results
} else {
    weighted_rrf_fusion(&fts_results, &vec_results, 60, top_k, 1.0, 1.0)
};
```

### Concurrency Control

- Use existing `embedding_in_progress: Arc<AtomicBool>` to prevent concurrent threads
- Background thread clears flag in **all** exit paths (success, error, panic via Drop guard)
- SQLite concurrency: WAL mode + `busy_timeout = 5000ms` handles concurrent reads/writes
  between main thread and background embedding thread

### Progress Reporting

Modify `spawn_background_embedding` to batch with progress callbacks:

```rust
fn spawn_background_embedding(&self) {
    // ... existing setup ...
    let total = unembedded.len();
    for (i, chunk) in unembedded.chunks(EMBED_PROGRESS_BATCH).enumerate() {
        embed_and_store_batch(&bg_db, &model, chunk)?;
        self.send_notification("notifications/message", json!({
            "level": "info",
            "logger": "code-graph",
            "data": format!("Embedding {}/{} nodes...",
                (i + 1) * EMBED_PROGRESS_BATCH.min(total), total)
        }));
    }
}
```

### Status Reporting

`get_index_status` response adds (additive, backward-compatible):

```json
{
  "embedding_status": "in_progress",
  "embedding_progress": "342/510",
  "model_available": true
}
```

### Incremental Updates

- File watcher → incremental index → new/changed nodes only
- After incremental index, check for unembedded nodes and spawn embedding if needed
- Existing `embed_and_store_batch` handles this — just called from a different site

## Files to Modify

| File | Change |
|------|--------|
| `Cargo.toml` | Add `ureq`, `flate2`, `tar`, `dirs` (feature-gated) |
| `src/embedding/model.rs` | Add `download_model()`, cache path, sha256 verify, `find_models_dir` cache fallback |
| `src/mcp/server.rs` | `embedding_model` → `Mutex<Option<EmbeddingModel>>`, download thread, batched embedding with progress |
| `src/mcp/server.rs` | `tool_semantic_search` lock model + vector fusion branch |
| `src/mcp/server.rs` | `get_index_status` embedding progress fields |
| `src/storage/queries.rs` | Add `count_nodes_with_vectors()` for progress tracking |
| `.github/workflows/release.yml` | Remove `--no-default-features`, add model download + upload |
| `.github/workflows/ci.yml` | Add embed-model compilation test |

## Degradation Matrix

| Condition | Search Mode | `find_similar_code` |
|-----------|-------------|---------------------|
| Model loaded + vectors ready | BM25 + Vector RRF | Available |
| Model loaded + vectors generating | BM25 + partial Vector | Available (partial) |
| Model downloading | BM25-only (Porter) | Error: "model loading" |
| Model download failed | BM25-only (Porter) | Error: "model unavailable" |
| `--no-default-features` build | BM25-only (Porter) | Error: "not compiled" |

## Success Criteria

1. `cargo build --release` produces a binary with embedding support (~20-25MB)
2. First run auto-downloads model files without blocking tools
3. `semantic_code_search` uses RRF fusion once vectors are ready
4. `find_similar_code` works for all users with model available
5. Any failure in the embedding pipeline degrades gracefully to BM25 (Porter)
6. `get_index_status` reports embedding progress accurately
7. `embedding_in_progress` flag is cleared in all exit paths
8. Model download verifies sha256 checksum before extraction
