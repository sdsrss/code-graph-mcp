# Phase 1: Stabilization + Indexing Performance

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop OOM crashes on large repos, isolate per-file errors, and improve insert throughput by 30-50%

**Architecture:** Convert the monolithic single-transaction index pipeline into a batched, error-isolated pipeline with prepared-statement caching. Add defensive error handling in the server's ensure_indexed path with cache snapshot/restore.

**Tech Stack:** Rust, rusqlite (prepare_cached), anyhow::Result, tracing

---

## File Structure

| File | Action | Responsibility |
|------|--------|---------------|
| `src/indexer/pipeline.rs` | Modify | Batch processing, per-file error isolation, split transactions, prepared-statement inserts |
| `src/mcp/server.rs` | Modify | Cache snapshot/restore on indexing failure in ensure_indexed |
| `src/storage/queries.rs` | Modify | Add `insert_node_cached`, `insert_edge_cached` using prepare_cached |
| `tests/integration.rs` | Modify | Add error-isolation and batch-boundary tests |

---

## Task 1: Prepared-statement node INSERT

**Files:**
- Modify: `src/storage/queries.rs:180-193`
- Test: `tests/integration.rs`

- [ ] **Step 1: Write the failing test**

Add to `tests/integration.rs`:

```rust
#[test]
fn test_insert_node_cached_returns_same_as_insert_node() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = Database::open(&tmp.path().join("test.db")).unwrap();

    let file_id = upsert_file(db.conn(), &FileRecord {
        path: "test.ts".into(),
        blake3_hash: "abc123".into(),
        last_modified: 0,
        language: Some("typescript".into()),
    }).unwrap();

    let id = insert_node_cached(db.conn(), &NodeRecord {
        file_id,
        node_type: "function".into(),
        name: "foo".into(),
        qualified_name: None,
        start_line: 1,
        end_line: 5,
        code_content: "function foo() {}".into(),
        signature: Some("foo()".into()),
        doc_comment: None,
        context_string: None,
    }).unwrap();

    assert!(id > 0);
    let nodes = get_nodes_by_name(db.conn(), "foo").unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].id, id);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --no-default-features test_insert_node_cached -- --nocapture`
Expected: FAIL — `insert_node_cached` not found

- [ ] **Step 3: Implement insert_node_cached**

Add to `src/storage/queries.rs` after the existing `insert_node` function:

```rust
/// Insert a node using a cached prepared statement for better throughput in loops.
/// Same semantics as insert_node, but avoids re-preparing the SQL on each call.
pub fn insert_node_cached(conn: &Connection, node: &NodeRecord) -> Result<i64> {
    let mut stmt = conn.prepare_cached(
        "INSERT INTO nodes (file_id, type, name, qualified_name, start_line, end_line, code_content, signature, doc_comment, context_string)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         RETURNING id"
    )?;
    let id: i64 = stmt.query_row(
        (
            node.file_id, &node.node_type, &node.name, &node.qualified_name,
            node.start_line, node.end_line, &node.code_content,
            &node.signature, &node.doc_comment, &node.context_string,
        ),
        |row| row.get(0),
    )?;
    Ok(id)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --no-default-features test_insert_node_cached -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/storage/queries.rs tests/integration.rs
git commit -m "perf(storage): add insert_node_cached with prepare_cached"
```

---

## Task 2: Prepared-statement edge INSERT

**Files:**
- Modify: `src/storage/queries.rs:220-227`
- Test: `tests/integration.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn test_insert_edge_cached_deduplicates() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = Database::open(&tmp.path().join("test.db")).unwrap();

    let file_id = upsert_file(db.conn(), &FileRecord {
        path: "test.ts".into(),
        blake3_hash: "abc".into(),
        last_modified: 0,
        language: Some("typescript".into()),
    }).unwrap();

    let n1 = insert_node_cached(db.conn(), &NodeRecord {
        file_id, node_type: "function".into(), name: "a".into(),
        qualified_name: None, start_line: 1, end_line: 2,
        code_content: "".into(), signature: None, doc_comment: None, context_string: None,
    }).unwrap();
    let n2 = insert_node_cached(db.conn(), &NodeRecord {
        file_id, node_type: "function".into(), name: "b".into(),
        qualified_name: None, start_line: 3, end_line: 4,
        code_content: "".into(), signature: None, doc_comment: None, context_string: None,
    }).unwrap();

    // First insert should succeed
    assert!(insert_edge_cached(db.conn(), n1, n2, "calls", None).unwrap());
    // Duplicate should be ignored
    assert!(!insert_edge_cached(db.conn(), n1, n2, "calls", None).unwrap());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --no-default-features test_insert_edge_cached -- --nocapture`
Expected: FAIL — `insert_edge_cached` not found

- [ ] **Step 3: Implement insert_edge_cached**

Add to `src/storage/queries.rs` after `insert_edge`:

```rust
/// Insert an edge using a cached prepared statement. Returns true if new row inserted.
pub fn insert_edge_cached(conn: &Connection, source_id: i64, target_id: i64, relation: &str, metadata: Option<&str>) -> Result<bool> {
    let mut stmt = conn.prepare_cached(
        "INSERT OR IGNORE INTO edges (source_id, target_id, relation, metadata)
         VALUES (?1, ?2, ?3, ?4)"
    )?;
    let rows = stmt.execute((source_id, target_id, relation, metadata))?;
    Ok(rows > 0)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --no-default-features test_insert_edge_cached -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/storage/queries.rs tests/integration.rs
git commit -m "perf(storage): add insert_edge_cached with prepare_cached"
```

---

## Task 3: Switch pipeline to cached inserts

**Files:**
- Modify: `src/indexer/pipeline.rs:315-346` (Phase 1 node inserts)
- Modify: `src/indexer/pipeline.rs:407-414` (Phase 2 edge inserts)

- [ ] **Step 1: Replace insert_node with insert_node_cached in Phase 1**

In `pipeline.rs`, do a find-and-replace: `insert_node(` → `insert_node_cached(` on lines 315 and 332. No changes to arguments or logic — the function signature is identical.

Also update the import at the top of `pipeline.rs`: add `insert_node_cached` and `insert_edge_cached` to the `use crate::storage::queries::*;` (already covered by wildcard import, no action needed).

- [ ] **Step 2: Replace insert_edge with insert_edge_cached in Phase 2**

In `pipeline.rs`, do a find-and-replace: `insert_edge(` → `insert_edge_cached(` on line 411. No changes to arguments or logic.

- [ ] **Step 3: Run all tests**

Run: `cargo test --no-default-features`
Expected: All tests pass

- [ ] **Step 4: Commit**

```bash
git add src/indexer/pipeline.rs
git commit -m "perf(indexer): use prepared-statement cache for node and edge inserts"
```

---

## Task 4: Per-file error isolation in Phase 1

**Files:**
- Modify: `src/indexer/pipeline.rs:261-358`
- Test: `tests/integration.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn test_index_skips_unparseable_files_without_crashing() {
    let project_dir = tempfile::TempDir::new().unwrap();
    let db_dir = tempfile::TempDir::new().unwrap();

    // Create a valid TS file
    std::fs::write(project_dir.path().join("good.ts"), "function works() {}").unwrap();
    // Create a file with supported extension but binary content
    std::fs::write(project_dir.path().join("bad.ts"), &[0xFF, 0xFE, 0x00, 0x01]).unwrap();
    // Another valid file
    std::fs::write(project_dir.path().join("also_good.ts"), "function alsoWorks() {}").unwrap();

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    let result = run_full_index(&db, project_dir.path(), None).unwrap();

    // Bad file skipped, but good files indexed
    assert!(result.files_indexed >= 2, "Should index at least the 2 good files, got {}", result.files_indexed);
    let nodes = get_nodes_by_name(db.conn(), "works").unwrap();
    assert_eq!(nodes.len(), 1);
    let nodes2 = get_nodes_by_name(db.conn(), "alsoWorks").unwrap();
    assert_eq!(nodes2.len(), 1);
}
```

- [ ] **Step 2: Run test to check current behavior**

Run: `cargo test --no-default-features test_index_skips_unparseable -- --nocapture`
Expected: May already pass (binary content → `read_to_string` fails → `continue`). If it passes, the test is confirming existing behavior — still valuable as a regression test.

- [ ] **Step 3: Add explicit error logging for skipped files**

In `pipeline.rs` Phase 1 loop, change the `read_to_string` error handling at line 283:

```rust
let source = match std::fs::read_to_string(&abs_path) {
    Ok(s) => s,
    Err(e) => {
        tracing::warn!("Skipping file {}: {}", rel_path, e);
        continue;
    }
};

// Parse once — shared by Phase 1 (nodes) and Phase 2 (relations)
let tree = match parse_tree(&source, language) {
    Ok(t) => t,
    Err(e) => {
        tracing::warn!("Parse failed for {}: {}", rel_path, e);
        continue;
    }
};
```

- [ ] **Step 4: Run tests**

Run: `cargo test --no-default-features`
Expected: All pass

- [ ] **Step 5: Commit**

```bash
git add src/indexer/pipeline.rs tests/integration.rs
git commit -m "fix(indexer): add explicit error logging for skipped files"
```

---

## Task 4b: Audit unwrap()/expect() in indexing hot path

**Files:**
- Audit: `src/indexer/pipeline.rs`, `src/parser/treesitter.rs`, `src/storage/queries.rs`

- [ ] **Step 1: Search for unwrap/expect in non-test code**

Run: `grep -n 'unwrap()\|expect(' src/indexer/pipeline.rs src/parser/treesitter.rs src/storage/queries.rs | grep -v '#\[cfg(test)\]' | grep -v 'mod tests'`

Review each hit: if it's in the indexing hot path (not test code), convert to `?` or `.unwrap_or_else`.

Known safe cases to keep:
- `unwrap_or(0)` on timestamp (line 298) — already has fallback
- Any `unwrap()` inside `#[cfg(test)]` blocks

- [ ] **Step 2: Fix any findings and run tests**

Run: `cargo test --no-default-features && cargo clippy --no-default-features -- -D warnings`
Expected: All pass

- [ ] **Step 3: Commit (if any changes)**

```bash
git add src/indexer/pipeline.rs src/parser/treesitter.rs src/storage/queries.rs
git commit -m "fix(indexer): convert unwrap() to Result in indexing hot path"
```

If no changes needed, skip this commit and note "Audit complete: no unwrap() found in hot path."

---

## Task 5: Streaming batch indexing

**Files:**
- Modify: `src/indexer/pipeline.rs:222-464` (major refactor of `index_files`)
- Test: `tests/integration.rs`

- [ ] **Step 1: Write the failing test for batch boundary**

```rust
#[test]
fn test_batch_indexing_commits_partial_on_many_files() {
    let project_dir = tempfile::TempDir::new().unwrap();
    let db_dir = tempfile::TempDir::new().unwrap();

    // Create 10 valid files
    for i in 0..10 {
        std::fs::write(
            project_dir.path().join(format!("file{}.ts", i)),
            format!("function func{}() {{}}", i),
        ).unwrap();
    }

    let db = Database::open(&db_dir.path().join("index.db")).unwrap();
    let result = run_full_index(&db, project_dir.path(), None).unwrap();

    assert_eq!(result.files_indexed, 10);
    // Verify all functions exist
    for i in 0..10 {
        let nodes = get_nodes_by_name(db.conn(), &format!("func{}", i)).unwrap();
        assert_eq!(nodes.len(), 1, "func{} should exist", i);
    }
}
```

- [ ] **Step 2: Run test (should pass with current code)**

Run: `cargo test --no-default-features test_batch_indexing -- --nocapture`
Expected: PASS (baseline behavior)

- [ ] **Step 3: Refactor index_files to process in batches with bounded memory**

Replace the single-transaction `index_files` with a batched approach. **Critical design choice**: each batch must do all 3 phases (parse + insert nodes + extract relations + insert edges) and then drop the `Tree` and `source` data before proceeding to the next batch. This bounds peak memory to `BATCH_SIZE` files worth of ASTs instead of all files.

Key changes:

1. Introduce `const BATCH_SIZE: usize = 500;`
2. Each batch: Phase 0 (delete) + Phase 1 (parse + insert nodes) + Phase 2 (relations + insert edges), then drop batch data
3. After all batches: Phase 3 (context strings + embeddings) — this only needs node IDs, which are lightweight
4. Use a lightweight `FileIndexed` struct to carry only `file_id`, `node_ids`, `node_names`, `rel_path` across batches (no `Tree`, no `source`)

```rust
/// Batch size for streaming indexing. Each batch processes all 3 phases
/// then drops heavyweight data (ASTs, source strings) before the next batch.
const BATCH_SIZE: usize = 500;

/// Lightweight post-batch record — no Tree or source string.
struct FileIndexed {
    rel_path: String,
    file_id: i64,
    node_ids: Vec<i64>,
    node_names: Vec<String>,
}

fn index_files(
    db: &Database,
    root: &Path,
    files: &[String],
    hashes: &HashMap<String, String>,
    model: Option<&EmbeddingModel>,
    delete_paths: &[String],
) -> Result<IndexResult> {
    let mut total_nodes_created = 0usize;
    let mut total_edges_created = 0usize;
    let mut all_indexed: Vec<FileIndexed> = Vec::new(); // lightweight, no Tree/source

    // Phase 0: Delete removed files
    if !delete_paths.is_empty() {
        let tx = db.conn().unchecked_transaction()?;
        delete_files_by_paths(db.conn(), delete_paths)?;
        tx.commit()?;
    }

    // Process files in batches — each batch does Phase 1 + Phase 2
    for (batch_idx, batch) in files.chunks(BATCH_SIZE).enumerate() {
        let tx = db.conn().unchecked_transaction()?;

        // --- Phase 1: Parse + insert nodes (same as existing lines 261-358) ---
        let mut batch_parsed: Vec<FileParsed> = Vec::new();
        for rel_path in batch {
            // ... existing file read/parse/insert logic ...
            // On error: log warning and continue
            // On success: push to batch_parsed
        }

        // --- Phase 2: Extract relations + insert edges (same as existing lines 360-417) ---
        // Build name_to_ids from this batch + query existing names from DB
        let mut name_to_ids: HashMap<String, Vec<i64>> = HashMap::new();
        for pf in &batch_parsed {
            for (id, name) in pf.node_ids.iter().zip(pf.node_names.iter()) {
                name_to_ids.entry(name.clone()).or_default().push(*id);
            }
        }
        let indexed_file_ids: Vec<i64> = batch_parsed.iter().map(|pf| pf.file_id).collect();
        let existing = get_node_names_excluding_files(db.conn(), &indexed_file_ids)?;
        for (name, id) in &existing {
            name_to_ids.entry(name.clone()).or_default().push(*id);
        }
        for ids in name_to_ids.values_mut() { ids.sort(); ids.dedup(); }

        for pf in &batch_parsed {
            let relations = extract_relations_from_tree(&pf.tree, &pf.source, &pf.language);
            // ... existing edge resolution logic (lines 382-417) ...
        }

        tx.commit()?;

        tracing::info!("[index] batch {}: {}/{} files processed ({} nodes, {} edges)",
            batch_idx + 1, all_indexed.len() + batch_parsed.len(), files.len(),
            total_nodes_created, total_edges_created);

        // Convert to lightweight records — drops Tree and source string
        for pf in batch_parsed {
            all_indexed.push(FileIndexed {
                rel_path: pf.rel_path,
                file_id: pf.file_id,
                node_ids: pf.node_ids,
                node_names: pf.node_names,
            });
            // pf.tree and pf.source are dropped here — memory freed
        }
    }

    // Phase 3: Build context strings + embeddings (single transaction, lightweight)
    {
        let tx = db.conn().unchecked_transaction()?;
        let all_node_ids: Vec<i64> = all_indexed.iter()
            .flat_map(|fi| fi.node_ids.iter().copied()).collect();
        let all_edges = get_edges_batch(db.conn(), &all_node_ids)?;
        let all_node_details: HashMap<i64, NodeResult> = {
            let nodes = get_nodes_with_files_by_ids(db.conn(), &all_node_ids)?;
            nodes.into_iter().map(|nwf| (nwf.node.id, nwf.node)).collect()
        };

        for fi in &all_indexed {
            for (idx, &node_id) in fi.node_ids.iter().enumerate() {
                // ... existing context build + embed logic (lines 429-454) ...
            }
        }
        tx.commit()?;
    }

    Ok(IndexResult {
        files_indexed: all_indexed.len(),
        nodes_created: total_nodes_created,
        edges_created: total_edges_created,
    })
}
```

**Memory profile**: Peak memory now bounded to `BATCH_SIZE` × (Tree + source) during Phase 1+2, plus lightweight `FileIndexed` records. For 100K files with BATCH_SIZE=500: peak ~500 ASTs in memory at any time, vs all 100K previously.

**Note**: `get_node_names_excluding_files` is queried per-batch (includes nodes from prior batches already committed to DB). This is correct because prior batches are committed, so their nodes are queryable. Cross-batch relation resolution works through this DB query.

**Note for incremental path**: `BATCH_SIZE = 500` has no effect on typical incremental indexes (1-5 files). This is by design.

- [ ] **Step 4: Run all tests**

Run: `cargo test --no-default-features`
Expected: All 25+ tests pass

- [ ] **Step 5: Run clippy**

Run: `cargo clippy --no-default-features -- -D warnings`
Expected: No warnings

- [ ] **Step 6: Commit**

```bash
git add src/indexer/pipeline.rs
git commit -m "perf(indexer): batch file processing with per-batch transactions"
```

---

## Task 6: Cache snapshot and restore in ensure_indexed

**Files:**
- Modify: `src/mcp/server.rs:136-180`

- [ ] **Step 1: Write the regression test**

Note: This is a regression test verifying ensure_indexed works across full→incremental transitions. True failure injection (e.g., read-only filesystem) is environment-dependent and fragile in CI, so we test the safe path and verify the snapshot/restore code compiles correctly.

```rust
#[test]
fn test_ensure_indexed_survives_full_then_incremental() {
    let project_dir = tempfile::TempDir::new().unwrap();
    std::fs::write(project_dir.path().join("a.ts"), "function foo() {}").unwrap();

    let server = McpServer::new_test_with_project(project_dir.path());

    // First call: full index
    let req = make_tool_request("get_index_status", json!({}));
    let resp = server.handle_message(&serde_json::to_string(&req).unwrap()).unwrap().unwrap();
    assert!(resp.contains("files_count"));

    // Modify file to trigger incremental
    std::fs::write(project_dir.path().join("a.ts"), "function bar() {}").unwrap();

    // Second call: incremental path (exercises cache take/restore)
    let resp2 = server.handle_message(&serde_json::to_string(&req).unwrap()).unwrap().unwrap();
    assert!(resp2.contains("files_count"));
}
```

- [ ] **Step 2: Modify ensure_indexed for cache snapshot/restore**

In `server.rs`, modify the incremental index calls (lines 156 and 168):

```rust
// Snapshot-then-take pattern for cache safety
if has_changes {
    let mut cache_guard = lock_or_recover(&self.dir_cache, "dir_cache");
    let cache_snapshot = cache_guard.clone(); // Clone before take
    let cache = cache_guard.take();
    drop(cache_guard); // Release lock during I/O

    match run_incremental_index_cached(&self.db, &project_root, model, cache.as_ref()) {
        Ok((_result, new_cache)) => {
            *lock_or_recover(&self.dir_cache, "dir_cache") = Some(new_cache);
        }
        Err(e) => {
            tracing::error!("Incremental index failed, restoring cache: {}", e);
            *lock_or_recover(&self.dir_cache, "dir_cache") = cache_snapshot;
            return Err(e);
        }
    }
}
```

Apply the same pattern to the debounced incremental path (lines 168-173).

- [ ] **Step 3: DirectoryCache must implement Clone**

Check if `DirectoryCache` already implements Clone. If not, add `#[derive(Clone)]` to it in `src/indexer/merkle.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test --no-default-features`
Expected: All pass

- [ ] **Step 5: Commit**

```bash
git add src/mcp/server.rs src/indexer/merkle.rs
git commit -m "fix(server): snapshot and restore dir_cache on index failure"
```

---

## Task 7: Index progress logging

**Files:**
- Modify: `src/indexer/pipeline.rs`

- [ ] **Step 1: Add progress logging to Phase 1 batch loop**

Already partially done in Task 5. Ensure each batch logs:

```rust
tracing::info!(
    "[index] Phase 1: {}/{} files parsed ({} nodes)",
    all_parsed_files.len(), files.len(), total_nodes_created
);
```

- [ ] **Step 2: Add progress logging to Phase 2**

```rust
tracing::info!(
    "[index] Phase 2: {} edges created across {} files",
    total_edges_created, all_parsed_files.len()
);
```

- [ ] **Step 3: Add progress logging to Phase 3**

```rust
tracing::info!(
    "[index] Phase 3: context strings built for {} nodes",
    all_node_ids.len()
);
```

- [ ] **Step 4: Add incremental summary**

In `run_incremental_index` and `run_incremental_index_cached`, after `index_files` returns:

```rust
if result.files_indexed > 0 || !deleted_files.is_empty() {
    tracing::info!(
        "[incremental] {} files changed, {} deleted, {} nodes, {} edges, {:.1}s",
        result.files_indexed, deleted_files.len(),
        result.nodes_created, result.edges_created,
        start.elapsed().as_secs_f64()
    );
}
```

Add `let start = std::time::Instant::now();` at the top of each function.

- [ ] **Step 5: Run tests + clippy**

Run: `cargo test --no-default-features && cargo clippy --no-default-features -- -D warnings`
Expected: All pass, no warnings

- [ ] **Step 6: Commit**

```bash
git add src/indexer/pipeline.rs
git commit -m "feat(indexer): add phase progress and incremental summary logging"
```

---

## Task 8: Tool call timing in server

**Files:**
- Modify: `src/mcp/server.rs` (handle_tool method)

- [ ] **Step 1: Add timing to handle_tool**

Find the `handle_tool` method and wrap the dispatch:

```rust
fn handle_tool(&self, name: &str, args: &serde_json::Value) -> Result<serde_json::Value> {
    let start = std::time::Instant::now();
    let result = match name {
        // ... existing dispatch ...
    };
    let elapsed = start.elapsed();
    if elapsed.as_millis() > 100 {
        tracing::info!("[tool] {} completed in {:.1}s", name, elapsed.as_secs_f64());
    } else {
        tracing::debug!("[tool] {} completed in {}ms", name, elapsed.as_millis());
    }
    result
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --no-default-features`
Expected: All pass

- [ ] **Step 3: Commit**

```bash
git add src/mcp/server.rs
git commit -m "feat(server): add tool call timing to handle_tool"
```

---

## Task 9: Final verification

- [ ] **Step 1: Run full test suite**

Run: `cargo test --no-default-features`
Expected: All tests pass

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --no-default-features -- -D warnings`
Expected: No warnings

- [ ] **Step 3: Build release**

Run: `cargo build --release --no-default-features`
Expected: Builds successfully

- [ ] **Step 4: Manual smoke test on this repo**

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}' | ./target/release/code-graph-mcp serve 2>/dev/null | head -1
```
Expected: JSON response with server capabilities

- [ ] **Step 5: Tag release**

```bash
git tag v0.4.0-rc1
```

---

## Phase 2 Preview (separate plan)

Phase 2 (Search Quality, v0.5.0) will be planned separately. Key tasks:
1. `src/search/tokenizer.rs` — camelCase/PascalCase splitting function
2. `src/storage/schema.rs` — Schema v2 migration (name_tokens, return_type, param_types columns + FTS5 rebuild)
3. `src/storage/queries.rs` — BM25 weight adjustment + FTS5 column update
4. `src/mcp/server.rs` — RRF parameter tuning (k=20, fts=1.0, vec=1.2)
5. `src/embedding/context.rs` — Reorder context string (code-first)
6. `src/parser/treesitter.rs` — Extract return_type and param_types from AST
7. `tests/search_quality_test.rs` — Validation harness with expected rankings

## Phase 3 Preview (separate plan)

Phase 3 (Infrastructure, v0.6.0) will be planned separately. Key tasks:
1. Boundary tests (empty/binary/large files)
2. Error recovery tests (index failure → re-index)
3. `benches/` — criterion.rs benchmarks for index/search/call_graph
4. `.github/workflows/ci.yml` — cargo-llvm-cov coverage step
5. `ARCHITECTURE.md` — Data flow, module responsibilities, design decisions
6. Rustdoc on pub API in pipeline.rs, queries.rs, fusion.rs
