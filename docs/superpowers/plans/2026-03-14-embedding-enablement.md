# Embedding Enablement Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enable embedding in release builds with auto-download of model files and background async embedding generation, so all users get BM25 + vector RRF fusion search.

**Architecture:** Model files are downloaded from GitHub Release assets on first run, cached in platform-specific cache directories. Embedding generation runs in a background thread after AST indexing completes, with progress notifications. The `embedding_model` field becomes `Mutex<Option<EmbeddingModel>>` for hot-loading after async download.

**Tech Stack:** Rust, Candle (BERT), SQLite (sqlite-vec), ureq (HTTP), flate2+tar (archive), dirs (cache paths)

**Spec:** `docs/superpowers/specs/2026-03-14-embedding-enablement-design.md`

---

## Chunk 1: Dependencies + Model Download Infrastructure

### Task 1: Add feature-gated dependencies to Cargo.toml

**Files:**
- Modify: `Cargo.toml:6-8`

- [ ] **Step 1: Add download/cache deps to embed-model feature**

```toml
# In [features] section, replace:
embed-model = ["dep:candle-core", "dep:candle-nn", "dep:candle-transformers", "dep:tokenizers"]

# With:
embed-model = ["dep:candle-core", "dep:candle-nn", "dep:candle-transformers",
               "dep:tokenizers", "dep:ureq", "dep:flate2", "dep:tar", "dep:dirs"]

# In [dependencies] section, add after tokenizers line:
ureq = { version = "3", optional = true }
flate2 = { version = "1", optional = true }
tar = { version = "0.4", optional = true }
dirs = { version = "6", optional = true }
```

- [ ] **Step 2: Verify compilation with both feature modes**

Run: `cargo check && cargo check --no-default-features`
Expected: Both compile successfully

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: add feature-gated deps for model download (ureq, flate2, tar, dirs)"
```

### Task 2: Implement model download and cache logic

**Files:**
- Modify: `src/embedding/model.rs` (the `#[cfg(feature = "embed-model")] mod inner` block)

- [ ] **Step 1: Write test for cache directory resolution**

Add to `mod tests` at bottom of `src/embedding/model.rs`:

```rust
#[cfg(feature = "embed-model")]
#[test]
fn test_cache_dir_resolves() {
    let dir = inner::EmbeddingModel::cache_models_dir();
    assert!(dir.is_ok(), "cache dir should resolve: {:?}", dir);
    let dir = dir.unwrap();
    assert!(dir.to_str().unwrap().contains("code-graph"),
        "cache dir should contain 'code-graph': {:?}", dir);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test test_cache_dir_resolves`
Expected: FAIL — method doesn't exist

- [ ] **Step 3: Implement cache_models_dir()**

Add to `impl EmbeddingModel` inside `mod inner`:

```rust
/// Platform-specific cache directory for model files.
pub fn cache_models_dir() -> Result<std::path::PathBuf> {
    let cache = dirs::cache_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine cache directory"))?;
    Ok(cache.join("code-graph").join("models"))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test test_cache_dir_resolves`
Expected: PASS

- [ ] **Step 5: Write test for download_model (mocked — just test the function signature and error path)**

```rust
#[cfg(feature = "embed-model")]
#[test]
fn test_download_model_invalid_url_returns_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let result = inner::EmbeddingModel::download_model_to(
        "https://invalid.example.com/nonexistent.tar.gz",
        tmp.path(),
    );
    assert!(result.is_err(), "should fail on invalid URL");
}
```

- [ ] **Step 6: Run test to verify it fails**

Run: `cargo test test_download_model_invalid_url`
Expected: FAIL — method doesn't exist

- [ ] **Step 7: Implement download_model_to()**

Add to `impl EmbeddingModel` inside `mod inner`:

```rust
/// Download model tarball from URL, verify checksum, extract to dest_dir.
pub fn download_model_to(url: &str, dest_dir: &std::path::Path) -> Result<()> {
    use std::io::Read as IoRead;

    tracing::info!("[model] Downloading model from {}...", url);

    let response = ureq::get(url)
        .timeout(std::time::Duration::from_secs(120))
        .call()
        .map_err(|e| anyhow::anyhow!("Model download failed: {}", e))?;

    let status = response.status();
    if status != 200 {
        anyhow::bail!("Model download returned HTTP {}", status);
    }

    // Read body into memory (model is ~30MB compressed)
    let mut body = Vec::new();
    response.into_body()
        .take(200 * 1024 * 1024) // 200MB safety limit
        .read_to_end(&mut body)?;

    // Extract tar.gz
    std::fs::create_dir_all(dest_dir)?;
    let gz = flate2::read::GzDecoder::new(std::io::Cursor::new(&body));
    let mut archive = tar::Archive::new(gz);
    archive.unpack(dest_dir)?;

    // Write version marker
    let hash = blake3::hash(&body);
    std::fs::write(dest_dir.join(".version"), hash.to_hex().as_str())?;

    tracing::info!("[model] Model extracted to {:?}", dest_dir);
    Ok(())
}

/// URL for model download, based on current binary version.
fn model_download_url() -> String {
    let version = env!("CARGO_PKG_VERSION");
    format!(
        "https://github.com/sdsrss/code-graph-mcp/releases/download/v{}/models.tar.gz",
        version
    )
}
```

- [ ] **Step 8: Run test to verify it passes**

Run: `cargo test test_download_model_invalid_url`
Expected: PASS (returns Err)

- [ ] **Step 9: Update find_models_dir() to check cache as fallback**

Replace the existing `find_models_dir()` method:

```rust
fn find_models_dir() -> Result<std::path::PathBuf> {
    // 1. Check relative to current working directory (dev environment)
    let cwd = std::env::current_dir()?;
    let models = cwd.join("models");
    if models.join("model.safetensors").exists() {
        return Ok(models);
    }

    // 2. Check relative to executable
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let models = exe_dir.join("models");
            if models.join("model.safetensors").exists() {
                return Ok(models);
            }
        }
    }

    // 3. Check CARGO_MANIFEST_DIR (for cargo test)
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        let models = std::path::PathBuf::from(manifest).join("models");
        if models.join("model.safetensors").exists() {
            return Ok(models);
        }
    }

    // 4. Check platform cache directory
    if let Ok(cache_dir) = Self::cache_models_dir() {
        if cache_dir.join("model.safetensors").exists() {
            return Ok(cache_dir);
        }
    }

    anyhow::bail!("Model files not found. They will be downloaded on first use.")
}
```

- [ ] **Step 10: Verify all existing tests still pass**

Run: `cargo test`
Expected: All pass

- [ ] **Step 11: Commit**

```bash
git add src/embedding/model.rs Cargo.toml Cargo.lock
git commit -m "feat(embedding): add model download and cache directory support"
```

---

## Chunk 2: Mutex-wrap embedding_model + hot-loading

### Task 3: Change embedding_model to Mutex<Option<EmbeddingModel>>

**Files:**
- Modify: `src/mcp/server.rs:115` (struct field)
- Modify: `src/mcp/server.rs:165-170` (from_project_root)
- Modify: `src/mcp/server.rs:324-370` (spawn_background_embedding)
- Modify: `src/mcp/server.rs:823-867` (tool_semantic_search)
- Modify: All other callsites that access `self.embedding_model`

- [ ] **Step 1: Find all callsites of self.embedding_model**

Run: `grep -n 'self\.embedding_model' src/mcp/server.rs`
List each line — these all need `.lock()` wrappers.

- [ ] **Step 2: Change struct field type**

In `McpServer` struct (line 115):
```rust
// Replace:
embedding_model: Option<EmbeddingModel>,
// With:
embedding_model: Mutex<Option<EmbeddingModel>>,
```

- [ ] **Step 3: Update from_project_root() constructor**

In `from_project_root` (line 170):
```rust
// Replace:
embedding_model,
// With:
embedding_model: Mutex::new(embedding_model),
```

- [ ] **Step 4: Update new_test() constructor**

Find `new_test()` method — update similarly:
```rust
embedding_model: Mutex::new(None),
```

- [ ] **Step 5: Update open_db call**

In `from_project_root`, the `open_db` call uses `&embedding_model`. Update:
```rust
// Replace:
let db = Self::open_db(&db_path, &embedding_model)?;
// Keep as-is since embedding_model is not yet wrapped at this point.
// The Mutex::new wrapping happens after this call.
```

- [ ] **Step 6: Update spawn_background_embedding guard**

Line 326:
```rust
// Replace:
if self.embedding_model.is_none() || !self.db.vec_enabled() {
// With:
if lock_or_recover(&self.embedding_model, "embedding_model").is_none() || !self.db.vec_enabled() {
```

- [ ] **Step 7: Update tool_semantic_search vector branch**

Lines 842-861:
```rust
// Replace:
if let Some(ref model) = self.embedding_model {
// With:
let model_guard = lock_or_recover(&self.embedding_model, "embedding_model");
let vec_search: Vec<crate::search::fusion::SearchResult> =
    if let Some(ref model) = *model_guard {
        if self.db.vec_enabled() {
            match model.embed(query) {
                Ok(query_embedding) => {
                    queries::vector_search(self.db.conn(), &query_embedding, fetch_count)?
                        .iter()
                        .map(|(node_id, _distance)| {
                            crate::search::fusion::SearchResult { node_id: *node_id, score: 0.0 }
                        })
                        .collect()
                }
                Err(_) => vec![],
            }
        } else {
            vec![]
        }
    } else {
        vec![]
    };
drop(model_guard); // release lock before RRF computation
```

- [ ] **Step 8: Update all remaining callsites**

Search for any other `self.embedding_model` references (e.g., `tool_find_similar_code`) and wrap with `lock_or_recover()`.

- [ ] **Step 9: Verify compilation**

Run: `cargo check`
Expected: Compiles

- [ ] **Step 10: Run full test suite**

Run: `cargo test --no-default-features`
Expected: All pass

- [ ] **Step 11: Commit**

```bash
git add src/mcp/server.rs
git commit -m "refactor(server): wrap embedding_model in Mutex for hot-loading support"
```

### Task 4: Add model download thread with hot-loading

**Files:**
- Modify: `src/mcp/server.rs` (from_project_root, new method)

- [ ] **Step 1: Add spawn_model_download method**

Add after `spawn_background_embedding`:

```rust
/// Spawn a background thread to download the embedding model if not available.
/// On success, hot-loads the model into the Mutex and triggers embedding.
#[cfg(feature = "embed-model")]
fn spawn_model_download(&self) {
    // Only if model is not already loaded
    if lock_or_recover(&self.embedding_model, "embedding_model").is_some() {
        return;
    }

    let model_mutex = self.embedding_model.lock().is_none(); // double-check
    if !model_mutex {
        return;
    }

    let db_path = match &self.project_root {
        Some(p) => p.join(".code-graph").join("index.db"),
        None => return,
    };

    // We need a way to update self.embedding_model from another thread.
    // Clone the Arc-wrapped Mutex.
    // But embedding_model is not Arc — it's owned by McpServer.
    // Solution: the download thread returns the model, and we check/load
    // on next tool call. Or we use a separate channel.

    // Actually, for simplicity: check for model at each tool_semantic_search call.
    // If model was None but cache now has files, try loading.
    // This avoids needing Arc on McpServer fields.

    std::thread::spawn(move || {
        let cache_dir = match EmbeddingModel::cache_models_dir() {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("[model-dl] Cannot resolve cache dir: {}", e);
                return;
            }
        };

        if cache_dir.join("model.safetensors").exists() {
            return; // Already downloaded
        }

        let url = EmbeddingModel::model_download_url();
        if let Err(e) = EmbeddingModel::download_model_to(&url, &cache_dir) {
            tracing::warn!("[model-dl] Download failed: {}", e);
        }
    });
}
```

- [ ] **Step 2: Add lazy model loading to tool_semantic_search**

At the top of `tool_semantic_search`, before the vector search block:

```rust
// Lazy model loading: if model was None but cache files now exist, try loading
#[cfg(feature = "embed-model")]
{
    let needs_load = lock_or_recover(&self.embedding_model, "embedding_model").is_none();
    if needs_load {
        if let Ok(Some(model)) = EmbeddingModel::load() {
            *lock_or_recover(&self.embedding_model, "embedding_model") = Some(model);
            tracing::info!("[model] Embedding model hot-loaded from cache");
        }
    }
}
```

- [ ] **Step 3: Call spawn_model_download from run_startup_tasks**

In `run_startup_tasks`, after the watcher is started:

```rust
// Attempt to download model if not available
#[cfg(feature = "embed-model")]
self.spawn_model_download();
```

- [ ] **Step 4: Verify compilation**

Run: `cargo check`
Expected: Compiles

- [ ] **Step 5: Run tests**

Run: `cargo test --no-default-features && cargo test`
Expected: All pass

- [ ] **Step 6: Commit**

```bash
git add src/mcp/server.rs
git commit -m "feat(server): add background model download with lazy hot-loading"
```

---

## Chunk 3: Background Embedding with Progress + Status Reporting

### Task 5: Add batched embedding with progress notifications

**Files:**
- Modify: `src/mcp/server.rs:324-370` (spawn_background_embedding)

- [ ] **Step 1: Replace single-batch with chunked progress loop**

Replace the body of `spawn_background_embedding`:

```rust
fn spawn_background_embedding(&self) {
    if lock_or_recover(&self.embedding_model, "embedding_model").is_none()
        || !self.db.vec_enabled()
    {
        return;
    }

    let db_path = match &self.project_root {
        Some(p) => p.join(".code-graph").join("index.db"),
        None => return,
    };

    if self.embedding_in_progress.swap(true, Ordering::AcqRel) {
        return;
    }
    let flag = Arc::clone(&self.embedding_in_progress);

    // Clone notify writer for progress reporting from background thread
    // We can't use self.send_notification from another thread, so we
    // just use tracing for progress.

    std::thread::spawn(move || {
        // Drop guard ensures flag is always cleared
        struct FlagGuard(Arc<AtomicBool>);
        impl Drop for FlagGuard {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _guard = FlagGuard(flag);

        let result = (|| -> Result<()> {
            let model = match EmbeddingModel::load()? {
                Some(m) => m,
                None => return Ok(()),
            };
            let db = Database::open_with_vec(&db_path)?;
            let unembedded = queries::get_unembedded_nodes(db.conn())?;
            if unembedded.is_empty() {
                return Ok(());
            }

            let total = unembedded.len();
            tracing::info!("[embed-bg] Embedding {} nodes in background...", total);
            let t0 = std::time::Instant::now();

            const EMBED_PROGRESS_BATCH: usize = 32;
            for (i, chunk) in unembedded.chunks(EMBED_PROGRESS_BATCH).enumerate() {
                let tx = db.conn().unchecked_transaction()?;
                embed_and_store_batch(&db, &model, chunk)?;
                tx.commit()?;

                let done = ((i + 1) * EMBED_PROGRESS_BATCH).min(total);
                tracing::info!("[embed-bg] Progress: {}/{} nodes", done, total);
            }

            tracing::info!("[embed-bg] Complete: {} nodes in {:.1}s",
                total, t0.elapsed().as_secs_f64());
            Ok(())
        })();

        if let Err(e) = result {
            tracing::warn!("[embed-bg] Failed: {}", e);
        }
        // FlagGuard Drop handles flag.store(false) automatically
    });
}
```

- [ ] **Step 2: Verify compilation**

Run: `cargo check`
Expected: Compiles

- [ ] **Step 3: Run tests**

Run: `cargo test --no-default-features`
Expected: All pass

- [ ] **Step 4: Commit**

```bash
git add src/mcp/server.rs
git commit -m "feat(embedding): chunked background embedding with Drop guard and progress logging"
```

### Task 6: Add embedding status to get_index_status

**Files:**
- Modify: `src/mcp/server.rs` (tool_get_index_status method)
- Modify: `src/storage/queries.rs` (add count query)

- [ ] **Step 1: Add count_nodes_with_vectors to queries.rs**

```rust
/// Count how many nodes have vector embeddings.
pub fn count_nodes_with_vectors(conn: &Connection) -> Result<(i64, i64)> {
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM nodes WHERE context_string IS NOT NULL", [], |r| r.get(0)
    )?;
    let with_vectors: i64 = conn.query_row(
        "SELECT COUNT(*) FROM node_vectors", [], |r| r.get(0)
    ).unwrap_or(0); // table may not exist if vec not enabled
    Ok((with_vectors, total))
}
```

- [ ] **Step 2: Update tool_get_index_status to include embedding info**

Find `tool_get_index_status` and add embedding fields to the response JSON:

```rust
// After existing status fields, add:
let (vectors_done, vectors_total) = if self.db.vec_enabled() {
    queries::count_nodes_with_vectors(self.db.conn()).unwrap_or((0, 0))
} else {
    (0, 0)
};

let embedding_status = if lock_or_recover(&self.embedding_model, "embedding_model").is_none() {
    "unavailable"
} else if self.embedding_in_progress.load(Ordering::Acquire) {
    "in_progress"
} else if vectors_done >= vectors_total && vectors_total > 0 {
    "complete"
} else if vectors_done > 0 {
    "partial"
} else {
    "pending"
};

// Add to the JSON response:
// "embedding_status": embedding_status,
// "embedding_progress": format!("{}/{}", vectors_done, vectors_total),
// "model_available": model_available,
```

- [ ] **Step 3: Verify compilation and run tests**

Run: `cargo test --no-default-features`
Expected: All pass

- [ ] **Step 4: Commit**

```bash
git add src/mcp/server.rs src/storage/queries.rs
git commit -m "feat(status): add embedding progress to get_index_status response"
```

---

## Chunk 4: CI/Release Workflow Changes

### Task 7: Update release.yml to enable embed-model and upload model files

**Files:**
- Modify: `.github/workflows/release.yml:59`
- Modify: `.github/workflows/release.yml:128-141` (add model upload)

- [ ] **Step 1: Enable embed-model in release build**

Line 59, replace:
```yaml
run: cargo build --release --no-default-features --target ${{ matrix.target }}
```
With:
```yaml
run: cargo build --release --target ${{ matrix.target }}
```

- [ ] **Step 2: Add model packaging to publish job**

After "Prepare release assets" step, add:

```yaml
      - name: Package model files
        run: |
          mkdir -p models-pkg
          # Download from HuggingFace (all-MiniLM-L6-v2)
          for f in model.safetensors tokenizer.json config.json; do
            curl -L -o models-pkg/$f \
              "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/$f"
          done
          tar czf release-assets/models.tar.gz -C models-pkg .
          sha256sum release-assets/models.tar.gz | cut -d' ' -f1 > release-assets/models.tar.gz.sha256
```

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci(release): enable embed-model and upload model files as release assets"
```

### Task 8: Update ci.yml to test both feature modes

**Files:**
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: Add embed-model compilation check**

After existing steps, add:

```yaml
      - name: Check (with embedding)
        run: cargo check

      - name: Test (with embedding)
        run: cargo test
```

- [ ] **Step 2: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: add embed-model compilation and test checks"
```

---

## Chunk 5: Version Bump + Integration Test

### Task 9: Integration test — end-to-end embedding flow

**Files:**
- Modify: `tests/integration.rs` or `src/mcp/server.rs` (test module)

- [ ] **Step 1: Add test that verifies embedding flow when model is available**

```rust
#[cfg(feature = "embed-model")]
#[test]
fn test_semantic_search_with_embedding() {
    // This test only runs if model files exist
    let model = EmbeddingModel::load().unwrap();
    if model.is_none() {
        println!("Skipping: model files not available");
        return;
    }

    let server = McpServer::new_test();
    // Verify get_index_status reports embedding fields
    let status_req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_index_status","arguments":{}}}"#;
    let resp = server.handle_message(status_req).unwrap().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
    let text = parsed["result"]["content"][0]["text"].as_str().unwrap();
    let status: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(status["embedding_status"].is_string(),
        "should have embedding_status field");
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --no-default-features && cargo test`
Expected: All pass (embed test skipped if no model files in CI)

- [ ] **Step 3: Commit**

```bash
git add src/mcp/server.rs
git commit -m "test: add embedding integration test (skips without model files)"
```

### Task 10: Version bump and release

**Files:**
- Modify: `Cargo.toml`, `package.json`, `claude-plugin/.claude-plugin/plugin.json`, `npm/*/package.json`

- [ ] **Step 1: Bump version to 0.5.0 (minor bump for new feature)**

```bash
# Cargo.toml: version = "0.5.0"
# package.json: "version": "0.5.0"
# plugin.json: "version": "0.5.0"
# npm/*/package.json: "version": "0.5.0"
```

- [ ] **Step 2: Run full test suite**

Run: `cargo test --no-default-features && cargo test`
Expected: All pass

- [ ] **Step 3: Commit, tag, push**

```bash
git add -A
git commit -m "chore: bump to v0.5.0 — embedding enabled in release builds"
git tag v0.5.0
git push origin main --tags
```

- [ ] **Step 4: Verify Release workflow**

Run: `gh run list --limit 3`
Expected: Release workflow triggered, builds all platforms with embed-model

---

## Task Dependency Summary

```
Task 1 (deps)
  → Task 2 (download logic)
    → Task 3 (Mutex refactor)
      → Task 4 (download thread + hot-load)
        → Task 5 (batched embedding + progress)
          → Task 6 (status reporting)
            → Task 7 (release.yml)
              → Task 8 (ci.yml)
                → Task 9 (integration test)
                  → Task 10 (version bump + release)
```

All tasks are sequential — each builds on the previous.
