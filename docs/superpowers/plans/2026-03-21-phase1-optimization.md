# Phase 1 Optimization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add dead code detection tool, benchmark CLI command, README performance data, and 6 new language parsers (C#, Kotlin, Ruby, PHP, Swift, Dart) to reach 16-language support.

**Architecture:** New `find_dead_code` MCP tool queries the existing edges table for zero-reference symbols with Orphan/Exported-Unused classification. Benchmark command follows existing `cmd_*` pattern in `src/cli.rs`. Language expansion adds Tree-sitter grammars with per-language node/relation extraction rules grouped by family (JVM, Script, Mobile).

**Tech Stack:** Rust, SQLite (rusqlite), Tree-sitter, serde_json

**Spec:** `docs/superpowers/specs/2026-03-21-phase1-optimization-design.md`

---

## File Map

### New files
- None (all changes in existing files)

### Modified files

| File | Changes |
|------|---------|
| `Cargo.toml:32-42` | Add 6 tree-sitter grammar dependencies |
| `src/domain.rs:21` | Bump INDEX_VERSION 4→5 |
| `src/utils/config.rs:1-21` | Add 6 language extension mappings |
| `src/parser/languages.rs:3-18` | Add 6 language→Language mappings |
| `src/parser/treesitter.rs` | Add node extraction for 6 languages |
| `src/parser/relations.rs` | Add relation extraction for 6 languages |
| `src/storage/queries.rs` | Add `find_dead_code()` query |
| `src/mcp/tools.rs:9,23-173,187-213` | Register `find_dead_code`, bump TOOL_COUNT, update test |
| `src/mcp/server/tools.rs` | Implement `tool_find_dead_code()` |
| `src/mcp/server/mod.rs` | Add dispatch for `find_dead_code` |
| `src/cli.rs` | Add `cmd_benchmark()` |
| `src/main.rs:8-89` | Wire `"benchmark"` subcommand |
| `README.md` | Add performance headline section |
| `tests/parser_deep_test.rs` | Add tests for 6 new languages |

---

## Task 1: `find_dead_code` Query Function

**Files:**
- Modify: `src/storage/queries.rs` (append after line ~1061)

- [ ] **Step 1: Write the test for find_dead_code query**

Add to `src/storage/queries.rs` in the `#[cfg(test)]` module:

```rust
#[test]
fn test_find_dead_code() {
    let (db, _tmp) = test_db();
    let conn = db.conn();

    // Insert two files
    conn.execute("INSERT INTO files (id, path, hash, language) VALUES (1, 'src/main.rs', 'h1', 'rust')", []).unwrap();
    conn.execute("INSERT INTO files (id, path, hash, language) VALUES (2, 'src/lib.rs', 'h2', 'rust')", []).unwrap();

    // Insert nodes: a called function, an orphan function, an exported-unused function
    conn.execute(
        "INSERT INTO nodes (id, file_id, name, type, start_line, end_line, code_content, is_test) VALUES (1, 1, 'main', 'function', 1, 10, 'fn main() {}', 0)",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO nodes (id, file_id, name, type, start_line, end_line, code_content, is_test) VALUES (2, 1, 'used_fn', 'function', 12, 25, 'fn used_fn() {}', 0)",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO nodes (id, file_id, name, type, start_line, end_line, code_content, is_test) VALUES (3, 1, 'orphan_fn', 'function', 30, 50, 'fn orphan_fn() {}', 0)",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO nodes (id, file_id, name, type, start_line, end_line, code_content, is_test) VALUES (4, 2, 'exported_unused', 'function', 1, 20, 'pub fn exported_unused() {}', 0)",
        [],
    ).unwrap();
    // A module node (should be excluded)
    conn.execute(
        "INSERT INTO nodes (id, file_id, name, type, start_line, end_line, code_content, is_test) VALUES (5, 1, '<module>', 'module', 1, 100, '', 0)",
        [],
    ).unwrap();
    // A test function (should be excluded by default)
    conn.execute(
        "INSERT INTO nodes (id, file_id, name, type, start_line, end_line, code_content, is_test) VALUES (6, 1, 'test_something', 'function', 60, 70, 'fn test_something() {}', 1)",
        [],
    ).unwrap();
    // A route handler (should be excluded)
    conn.execute(
        "INSERT INTO nodes (id, file_id, name, type, start_line, end_line, code_content, is_test) VALUES (7, 1, 'handle_login', 'function', 80, 95, 'fn handle_login() {}', 0)",
        [],
    ).unwrap();

    // used_fn is called by main
    conn.execute(
        "INSERT INTO edges (source_id, target_id, relation) VALUES (1, 2, 'calls')", [],
    ).unwrap();
    // exported_unused is exported by module
    conn.execute(
        "INSERT INTO edges (source_id, target_id, relation) VALUES (5, 4, 'exports')", [],
    ).unwrap();
    // handle_login has routes_to self-edge
    conn.execute(
        "INSERT INTO edges (source_id, target_id, relation, metadata) VALUES (7, 7, 'routes_to', '{\"method\":\"POST\",\"path\":\"/login\"}')", [],
    ).unwrap();

    // Query: find dead code with min_lines=3, exclude tests
    let results = find_dead_code(conn, None, None, false, 3).unwrap();
    let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();

    // orphan_fn (21 lines, no edges) should be found
    assert!(names.contains(&"orphan_fn"), "orphan should be found, got: {:?}", names);
    // exported_unused (20 lines, has exports edge but no callers) should be found
    assert!(names.contains(&"exported_unused"), "exported-unused should be found, got: {:?}", names);
    // main, used_fn, module, test_something, handle_login should NOT be found
    assert!(!names.contains(&"main"), "main entry should be excluded");
    assert!(!names.contains(&"used_fn"), "called function should be excluded");
    assert!(!names.contains(&"<module>"), "module should be excluded");
    assert!(!names.contains(&"test_something"), "test should be excluded by default");
    assert!(!names.contains(&"handle_login"), "route handler should be excluded");

    // Verify include_tests=true includes test functions
    let results_with_tests = find_dead_code(conn, None, None, true, 3).unwrap();
    let test_names: Vec<&str> = results_with_tests.iter().map(|r| r.name.as_str()).collect();
    assert!(test_names.contains(&"test_something"), "test should be included when include_tests=true");

    // Verify Orphan vs Exported-Unused classification
    let orphan = results.iter().find(|r| r.name == "orphan_fn").unwrap();
    assert!(!orphan.has_export_edge, "orphan_fn should not have export edge");
    let exported = results.iter().find(|r| r.name == "exported_unused").unwrap();
    assert!(exported.has_export_edge, "exported_unused should have export edge");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib test_find_dead_code -- --nocapture 2>&1 | tail -20`
Expected: FAIL — `find_dead_code` function not found

- [ ] **Step 3: Implement `find_dead_code()` query and `DeadCodeResult` struct**

Add to `src/storage/queries.rs` (before the test module):

```rust
#[derive(Debug)]
pub struct DeadCodeResult {
    pub id: i64,
    pub name: String,
    pub node_type: String,
    pub start_line: u32,
    pub end_line: u32,
    pub file_path: String,
    pub code_content: String,
    pub has_export_edge: bool, // true = Exported-Unused, false = Orphan
}

/// Find symbols with zero incoming usage edges (calls/imports/inherits/implements).
/// Excludes: module nodes, route handlers (routes_to self-edge), and optionally test nodes.
/// Returns results sorted by line count descending (largest dead code first).
pub fn find_dead_code(
    conn: &Connection,
    path_prefix: Option<&str>,
    node_type: Option<&str>,
    include_tests: bool,
    min_lines: u32,
) -> Result<Vec<DeadCodeResult>> {
    // Map shorthand node_type values; "fn" matches both function and method
    let type_values: Option<Vec<&str>> = node_type.map(|t| match t {
        "fn" => vec!["function", "method"],
        other => vec![other],
    });

    let test_filter = if include_tests { "" } else { " AND n.is_test = 0" };
    let path_filter = if path_prefix.is_some() { " AND f.path LIKE :path_pattern" } else { "" };
    let type_filter = if let Some(ref vals) = type_values {
        if vals.len() == 1 { " AND n.type = :node_type" }
        else { " AND n.type IN (:node_type, :node_type2)" }
    } else { "" };

    let sql = format!(
        "SELECT n.id, n.name, n.type, n.start_line, n.end_line, f.path, n.code_content,
                EXISTS(SELECT 1 FROM edges ex WHERE ex.target_id = n.id AND ex.relation = 'exports') as has_export
         FROM nodes n
         JOIN files f ON n.file_id = f.id
         WHERE n.type != 'module'
           AND n.name != '<module>'
           AND n.name != 'main'
           AND (n.end_line - n.start_line + 1) >= :min_lines
           {test_filter}
           {type_filter}
           {path_filter}
           AND NOT EXISTS (
               SELECT 1 FROM edges e
               WHERE e.target_id = n.id
               AND e.relation IN ('calls', 'imports', 'inherits', 'implements')
           )
           AND NOT EXISTS (
               SELECT 1 FROM edges e
               WHERE e.source_id = n.id
               AND e.relation = 'routes_to'
           )
         ORDER BY (n.end_line - n.start_line) DESC"
    );

    let mut stmt = conn.prepare(&sql)?;

    // Build named params dynamically
    let min_lines_i64 = min_lines as i64;
    let path_pattern = path_prefix.map(|p| format!("{}%", p));
    let mut params: Vec<(&str, &dyn rusqlite::types::ToSql)> = vec![
        (":min_lines", &min_lines_i64),
    ];
    if let Some(ref vals) = type_values {
        params.push((":node_type", &vals[0]));
        if vals.len() > 1 {
            params.push((":node_type2", &vals[1]));
        }
    }
    if let Some(ref pp) = path_pattern {
        params.push((":path_pattern", pp));
    }

    let rows = stmt.query_map(params.as_slice(), |row| {
        Ok(DeadCodeResult {
            id: row.get(0)?,
            name: row.get(1)?,
            node_type: row.get(2)?,
            start_line: row.get(3)?,
            end_line: row.get(4)?,
            file_path: row.get(5)?,
            code_content: row.get(6)?,
            has_export_edge: row.get(7)?,
        })
    })?;

    rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
}
```

Note: The implementor should adapt the parameterized query approach to match the exact patterns used elsewhere in `queries.rs`. The key logic is: two NOT EXISTS subqueries (one for usage edges on target_id, one for routes_to on source_id), plus dynamic filters for test/path/type. The `has_export_edge` check is a post-query step using a simple EXISTS query per result.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib test_find_dead_code -- --nocapture 2>&1 | tail -20`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/storage/queries.rs
git commit -m "feat(storage): add find_dead_code query with Orphan/Exported-Unused classification"
```

---

## Task 2: `find_dead_code` MCP Tool Registration & Implementation

**Files:**
- Modify: `src/mcp/tools.rs:9,23-173,187-213`
- Modify: `src/mcp/server/tools.rs` (append after `tool_find_references`)
- Modify: `src/mcp/server/mod.rs` (add dispatch case)

- [ ] **Step 1: Register the tool in `src/mcp/tools.rs`**

Change line 9:
```rust
pub const TOOL_COUNT: usize = 12;
```

Add to the vec! in `ToolRegistry::new()` (after the `find_references` entry, before the closing `]`):

```rust
ToolDefinition {
    name: "find_dead_code".into(),
    description: "Find unreferenced symbols (dead code) with smart classification. Returns two tiers: Orphan (completely isolated, no callers/exports) and Exported-Unused (exported but unused within the project). Excludes entry points, route handlers, trait implementations, and test functions by default.".into(),
    input_schema: json!({
        "type": "object",
        "properties": {
            "scope": {
                "type": "string",
                "description": "Scan scope: 'project' (full) or 'module' (directory-scoped)",
                "default": "project",
                "enum": ["project", "module"]
            },
            "path": {
                "type": "string",
                "description": "Directory path filter (required when scope='module')"
            },
            "node_type": {
                "type": "string",
                "description": "Filter by symbol type",
                "enum": ["fn", "class", "struct", "interface", "enum", "type", "const"]
            },
            "include_tests": {
                "type": "boolean",
                "description": "Include test symbols in results (default: false)",
                "default": false
            },
            "min_lines": {
                "type": "integer",
                "description": "Minimum line count to include (default: 3, skips trivial declarations)",
                "default": 3
            },
            "compact": {
                "type": "boolean",
                "description": "Compact output (default: true)",
                "default": true
            }
        }
    }),
},
```

Update `test_tool_registry_has_all_tools` — add `"find_dead_code"` to the expected list.

- [ ] **Step 2: Implement `tool_find_dead_code` in `src/mcp/server/tools.rs`**

Append after `tool_find_references`:

```rust
pub(super) fn tool_find_dead_code(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
    let scope = args["scope"].as_str().unwrap_or("project");
    let path = args["path"].as_str();
    let node_type = args["node_type"].as_str();
    let include_tests = args["include_tests"].as_bool().unwrap_or(false);
    let min_lines = args["min_lines"].as_u64().unwrap_or(3) as u32;
    let compact = args["compact"].as_bool().unwrap_or(true);

    if !should_skip_indexing(args) {
        self.ensure_indexed()?;
    }
    let conn = self.db.conn();

    let path_prefix = if scope == "module" { path } else { None };

    let results = crate::storage::queries::find_dead_code(
        conn, path_prefix, node_type, include_tests, min_lines,
    )?;

    // Classify into Orphan and Exported-Unused
    let mut orphans = Vec::new();
    let mut exported_unused = Vec::new();

    for r in &results {
        // Visibility heuristic for non-JS/TS languages
        let is_exported = r.has_export_edge
            || r.code_content.starts_with("pub ")
            || r.code_content.starts_with("pub(")
            || (r.name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
                && r.file_path.ends_with(".go"));

        if is_exported {
            exported_unused.push(r);
        } else {
            orphans.push(r);
        }
    }

    let mut output = String::new();
    let total_orphan_lines: u32 = orphans.iter().map(|r| r.end_line - r.start_line + 1).sum();
    let total_exported_lines: u32 = exported_unused.iter().map(|r| r.end_line - r.start_line + 1).sum();

    if !orphans.is_empty() {
        output.push_str(&format!("ORPHAN ({} symbols, {} lines) — completely unreferenced:\n", orphans.len(), total_orphan_lines));
        for r in &orphans {
            let lines = r.end_line - r.start_line + 1;
            if compact {
                output.push_str(&format!("  {} [{}] {}:{}-{} ({} lines)\n",
                    r.name, r.node_type, r.file_path, r.start_line, r.end_line, lines));
            } else {
                output.push_str(&format!("  {} [{}] {}:{}-{} ({} lines)\n    {}\n",
                    r.name, r.node_type, r.file_path, r.start_line, r.end_line, lines,
                    r.code_content.lines().next().unwrap_or("")));
            }
        }
    }

    if !exported_unused.is_empty() {
        if !output.is_empty() { output.push('\n'); }
        output.push_str(&format!("EXPORTED-UNUSED ({} symbols, {} lines) — exported/public but no internal callers:\n",
            exported_unused.len(), total_exported_lines));
        for r in &exported_unused {
            let lines = r.end_line - r.start_line + 1;
            if compact {
                output.push_str(&format!("  {} [{}] {}:{}-{} ({} lines)\n",
                    r.name, r.node_type, r.file_path, r.start_line, r.end_line, lines));
            } else {
                output.push_str(&format!("  {} [{}] {}:{}-{} ({} lines)\n    {}\n",
                    r.name, r.node_type, r.file_path, r.start_line, r.end_line, lines,
                    r.code_content.lines().next().unwrap_or("")));
            }
        }
    }

    if orphans.is_empty() && exported_unused.is_empty() {
        output.push_str("No dead code found with current filters.");
    } else {
        output.push_str(&format!("\nSummary: {} orphan ({} lines), {} exported-unused ({} lines)",
            orphans.len(), total_orphan_lines, exported_unused.len(), total_exported_lines));
    }

    Ok(json!({
        "content": [{"type": "text", "text": output}]
    }))
}
```

- [ ] **Step 3: Add dispatch in `src/mcp/server/mod.rs`**

Find the tool dispatch match block and add:

```rust
"find_dead_code" => self.tool_find_dead_code(args),
```

- [ ] **Step 4: Verify build compiles**

Run: `cargo check 2>&1 | tail -10`
Expected: no errors

- [ ] **Step 5: Run tool registry test**

Run: `cargo test --lib test_tool_registry 2>&1 | tail -10`
Expected: PASS (TOOL_COUNT=12, find_dead_code in list)

- [ ] **Step 6: Verify no-default-features build**

Run: `cargo check --no-default-features 2>&1 | tail -10`
Expected: no errors (dead code tool uses no feature-gated code)

- [ ] **Step 7: Commit**

```bash
git add src/mcp/tools.rs src/mcp/server/tools.rs src/mcp/server/mod.rs
git commit -m "feat(mcp): add find_dead_code tool with Orphan/Exported-Unused classification"
```

---

## Task 3: `benchmark` CLI Subcommand

**Files:**
- Modify: `src/cli.rs` (append `cmd_benchmark`)
- Modify: `src/main.rs:8-89` (add dispatch)

- [ ] **Step 1: Implement `cmd_benchmark` in `src/cli.rs`**

Append after `cmd_refs`:

```rust
pub fn cmd_benchmark(project_root: &Path, args: &[String]) -> Result<()> {
    let json_mode = has_flag(args, "--json") || has_flag(args, "--format");

    use std::time::Instant;
    use crate::storage::db::Database;

    let data_dir = project_root.join(".code-graph");
    std::fs::create_dir_all(&data_dir)?;

    // === Phase 1: Full index timing (use temp DB to avoid destroying user's index) ===
    eprintln!("Benchmarking full index...");
    let bench_db_path = data_dir.join("benchmark-temp.db");
    if bench_db_path.exists() {
        std::fs::remove_file(&bench_db_path)?;
    }
    let bench_db = Database::open(&bench_db_path)?;

    let t0 = Instant::now();
    let _result = crate::indexer::pipeline::run_full_index(&bench_db, project_root, None, None)?;
    let full_index_ms = t0.elapsed().as_millis();

    let conn = bench_db.conn();
    let file_count: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
    let node_count: i64 = conn.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))?;
    let edge_count: i64 = conn.query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))?;
    let files_per_sec = if full_index_ms > 0 { file_count * 1000 / full_index_ms as i64 } else { 0 };

    // === Phase 2: Incremental index timing (re-run on same DB = no-change detection) ===
    eprintln!("Benchmarking incremental index...");
    let t1 = Instant::now();
    let _result2 = crate::indexer::pipeline::run_full_index(&bench_db, project_root, None, None)?;
    let incr_index_ms = t1.elapsed().as_millis();

    // === Phase 3: Query latency ===
    eprintln!("Benchmarking queries...");
    let mut search_times = Vec::new();
    let test_queries = ["main", "function", "handler", "config", "error"];
    for q in &test_queries {
        let t = Instant::now();
        let _ = crate::storage::queries::fts5_search(conn, q, 10);
        search_times.push(t.elapsed().as_micros());
    }
    search_times.sort();
    let search_p50 = search_times.get(search_times.len() / 2).copied().unwrap_or(0);
    let search_p99 = search_times.last().copied().unwrap_or(0);

    // === Phase 4: DB size ===
    let db_size = std::fs::metadata(&bench_db_path)?.len();
    let db_size_mb = db_size as f64 / (1024.0 * 1024.0);

    // Clean up benchmark DB
    drop(bench_db);
    let _ = std::fs::remove_file(&bench_db_path);

    // === Phase 5: Token savings estimate ===
    let avg_node_chars: f64 = conn.query_row(
        "SELECT AVG(LENGTH(code_content)) FROM nodes WHERE type != 'module'", [], |r| r.get(0)
    ).unwrap_or(200.0);
    let avg_tokens_per_result = avg_node_chars / 3.0;
    let cg_tokens_per_task = avg_tokens_per_result * 10.0;
    let grep_tokens_per_task = 36000.0;
    let reduction = if cg_tokens_per_task > 0.0 { grep_tokens_per_task / cg_tokens_per_task } else { 0.0 };

    if json_mode {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
            "full_index_ms": full_index_ms,
            "incremental_index_ms": incr_index_ms,
            "files": file_count,
            "nodes": node_count,
            "edges": edge_count,
            "files_per_second": files_per_sec,
            "search_p50_us": search_p50,
            "search_p99_us": search_p99,
            "db_size_mb": format!("{:.1}", db_size_mb),
            "token_reduction_factor": format!("{:.1}x", reduction),
        }))?);
    } else {
        println!("=== code-graph-mcp benchmark ===\n");
        println!("Index:");
        println!("  Full index:        {} files in {:.1}s ({} files/s)",
            file_count, full_index_ms as f64 / 1000.0, files_per_sec);
        println!("  Incremental:       {}ms (no changes)", incr_index_ms);
        println!("  Database:          {:.1}MB ({} nodes, {} edges)", db_size_mb, node_count, edge_count);
        println!("\nQuery latency:");
        println!("  FTS search P50:    {}us", search_p50);
        println!("  FTS search P99:    {}us", search_p99);
        println!("\nToken savings (estimated):");
        println!("  Avg tokens/result: {:.0}", avg_tokens_per_result);
        println!("  vs grep+read:      {:.1}x fewer tokens", reduction);
    }

    Ok(())
}
```

Note: The benchmark creates a temporary `benchmark-temp.db` to avoid destroying the user's existing index. It uses `Database::open` directly (not `CliContext::open` which requires existing DB). After benchmarking, the temp DB is deleted. The `conn` reference from `bench_db` may not be valid after `drop(bench_db)` — the implementor should move the token savings calculation before the drop, or use the user's real DB for that query.

- [ ] **Step 2: Wire dispatch in `src/main.rs`**

Add in the match block (before `Some(other) =>`):

```rust
Some("benchmark") => {
    let project_root = std::env::current_dir()?;
    code_graph_mcp::cli::cmd_benchmark(&project_root, &args)
}
```

- [ ] **Step 3: Verify build compiles**

Run: `cargo check 2>&1 | tail -10`
Expected: no errors

- [ ] **Step 4: Test benchmark on this project**

Run: `cargo run -- benchmark 2>&1 | tail -20`
Expected: Benchmark output with index timing, query latency, and token savings

- [ ] **Step 5: Test JSON output**

Run: `cargo run -- benchmark --json 2>&1 | tail -20`
Expected: Valid JSON output

- [ ] **Step 6: Commit**

```bash
git add src/cli.rs src/main.rs
git commit -m "feat(cli): add benchmark subcommand with index speed, query latency, token savings"
```

---

## Task 4: README Performance Headline

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Run benchmark to get real numbers**

Run: `cargo run --release -- benchmark 2>&1`
Record: files/s, P50, P99, token reduction factor

- [ ] **Step 2: Update README with performance section**

Add after the existing efficiency comparison table (find the section that shows tool call reduction):

```markdown
### Performance Benchmarks

| Metric | Value |
|--------|-------|
| Indexing speed | **[X] files/second** (single-threaded, release build) |
| Incremental re-index | **<[Y]ms** (no-change detection via BLAKE3 Merkle tree) |
| FTS search P50 | **<[Z]ms** |
| Database overhead | **~[W]MB** per 1000 files |
| Token savings | **[N]x fewer tokens** per code understanding task vs grep+read |

Run `code-graph-mcp benchmark` on your own project to measure.
```

Replace `[X]`, `[Y]`, `[Z]`, `[W]`, `[N]` with actual benchmark numbers from Step 1.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs: add performance benchmarks section to README"
```

---

## Task 5: C# Language Support

**Files:**
- Modify: `Cargo.toml:32-42`
- Modify: `src/utils/config.rs:1-21`
- Modify: `src/parser/languages.rs:3-18`
- Modify: `src/parser/treesitter.rs`
- Modify: `src/parser/relations.rs`
- Modify: `tests/parser_deep_test.rs`

- [ ] **Step 1: Write parser test for C#**

Add to `tests/parser_deep_test.rs`:

```rust
#[test]
fn test_csharp_parsing() {
    let code = r#"
using System;
namespace MyApp {
    public class UserService : IUserService {
        public async Task<User> GetUser(int id) {
            return await db.FindAsync(id);
        }
        private void Helper() {}
    }
    public interface IUserService {
        Task<User> GetUser(int id);
    }
}
"#;
    let nodes = parse_code(code, "csharp").unwrap();
    let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"UserService"), "C# class should be parsed, got: {:?}", names);
    assert!(names.contains(&"GetUser"), "C# method should be parsed, got: {:?}", names);
    assert!(names.contains(&"IUserService"), "C# interface should be parsed, got: {:?}", names);

    let relations = extract_relations(code, "csharp").unwrap();
    let inherits: Vec<&str> = relations.iter()
        .filter(|r| r.relation == "implements")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(inherits.contains(&"IUserService"), "C# implements should be extracted, got: {:?}", inherits);

    let imports: Vec<&str> = relations.iter()
        .filter(|r| r.relation == "imports")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(imports.contains(&"System"), "C# using should be extracted as import, got: {:?}", imports);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test test_csharp_parsing 2>&1 | tail -10`
Expected: FAIL — `parse_code("csharp")` returns error or None

- [ ] **Step 3: Add tree-sitter-c-sharp dependency**

Add to `Cargo.toml` after line 42 (after `tree-sitter-css`):
```toml
tree-sitter-c-sharp = "0.23.1"
```

- [ ] **Step 4: Add language detection**

In `src/utils/config.rs`, add to the match (before `_ => None`):
```rust
"cs" => Some("csharp"),
```

- [ ] **Step 5: Add language mapping**

In `src/parser/languages.rs`, add to the match (before `_ => None`):
```rust
"csharp" => Some(tree_sitter_c_sharp::LANGUAGE.into()),
```

- [ ] **Step 6: Add node extraction for C#**

In `src/parser/treesitter.rs`, find the match block in `extract_nodes`. Most C# nodes already match existing patterns (`class_declaration`, `interface_declaration`, `enum_declaration`, `method_declaration`). Verify which tree-sitter-c-sharp node kinds are used:

- `class_declaration` — already handled
- `interface_declaration` — already handled
- `method_declaration` — already handled
- `enum_declaration` — already handled
- `struct_declaration` — may need adding if different from existing `struct_item` (Rust)
- `namespace_declaration` — skip (not a code symbol)
- `constructor_declaration` — add as function type

Add any missing patterns. For `constructor_declaration`:
```rust
"constructor_declaration" => {
    if let Some(mut parsed) = extract_function_node(&node, source, "function", parent_class) {
        parsed.is_test = node_is_test;
        results.push(parsed);
    }
}
```

Note: The implementor must explore the tree-sitter-c-sharp AST (run `tree-sitter parse` on a sample C# file or check the grammar repository) to discover exact node kinds. C# node kinds likely include: `class_declaration`, `interface_declaration`, `struct_declaration`, `enum_declaration`, `method_declaration`, `constructor_declaration`, `property_declaration`.

- [ ] **Step 7: Add relation extraction for C#**

In `src/parser/relations.rs`, add C#-specific import handling:

```rust
// C# using directives: using System; using System.Collections.Generic;
"using_directive" => {
    if language == "csharp" {
        // Walk children to find the namespace identifier
        for i in 0..node.named_child_count() {
            if let Some(child) = node.named_child(i) {
                if child.kind() == "qualified_name" || child.kind() == "identifier" {
                    let name = node_text(&child, source).to_string();
                    if !name.is_empty() {
                        results.push(ParsedRelation {
                            source_name: "<module>".into(),
                            target_name: name,
                            relation: REL_IMPORTS.into(),
                            metadata: None,
                        });
                    }
                }
            }
        }
    }
}
```

For C# class inheritance with `:` syntax (`class Dog : Animal, IWalkable`), the tree-sitter node is `base_list` containing `simple_base_type` children. Add:

```rust
"base_list" => {
    if language == "csharp" {
        for i in 0..node.named_child_count() {
            if let Some(child) = node.named_child(i) {
                let base_name = node_text(&child, source).to_string();
                if !base_name.is_empty() {
                    // Heuristic: names starting with I are interfaces
                    let rel = if base_name.starts_with('I') && base_name.len() > 1
                        && base_name.chars().nth(1).map(|c| c.is_uppercase()).unwrap_or(false) {
                        REL_IMPLEMENTS
                    } else {
                        REL_INHERITS
                    };
                    results.push(ParsedRelation {
                        source_name: current_scope.unwrap_or("<module>").into(),
                        target_name: base_name,
                        relation: rel.into(),
                        metadata: None,
                    });
                }
            }
        }
    }
}
```

Note: Exact node kinds must be verified against the tree-sitter-c-sharp grammar. The implementor should create a test C# file and examine the parse tree.

- [ ] **Step 8: Run test to verify it passes**

Run: `cargo test test_csharp_parsing 2>&1 | tail -20`
Expected: PASS

- [ ] **Step 9: Verify full build**

Run: `cargo check 2>&1 | tail -5`
Expected: no errors

- [ ] **Step 10: Commit**

```bash
git add Cargo.toml src/utils/config.rs src/parser/languages.rs src/parser/treesitter.rs src/parser/relations.rs tests/parser_deep_test.rs
git commit -m "feat(parser): add C# language support — classes, interfaces, using imports, inheritance"
```

---

## Task 6: Kotlin Language Support

**Files:** Same 6 files as Task 5

- [ ] **Step 1: Write parser test for Kotlin**

Add to `tests/parser_deep_test.rs`:

```rust
#[test]
fn test_kotlin_parsing() {
    let code = r#"
import kotlinx.coroutines.flow.Flow

interface UserRepository {
    suspend fun findById(id: Long): User?
}

class UserService(private val repo: UserRepository) : UserRepository {
    override suspend fun findById(id: Long): User? {
        return repo.findById(id)
    }
    fun listAll(): List<User> = emptyList()
}

data class User(val id: Long, val name: String)
"#;
    let nodes = parse_code(code, "kotlin").unwrap();
    let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"UserRepository"), "Kotlin interface, got: {:?}", names);
    assert!(names.contains(&"UserService"), "Kotlin class, got: {:?}", names);
    assert!(names.contains(&"findById"), "Kotlin fun, got: {:?}", names);
    assert!(names.contains(&"User"), "Kotlin data class, got: {:?}", names);

    let relations = extract_relations(code, "kotlin").unwrap();
    let imports: Vec<&str> = relations.iter()
        .filter(|r| r.relation == "imports")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(!imports.is_empty(), "Kotlin imports should be extracted, got: {:?}", imports);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test test_kotlin_parsing 2>&1 | tail -10`
Expected: FAIL

- [ ] **Step 3: Add dependency, detection, and language mapping**

`Cargo.toml`: `tree-sitter-kotlin = "0.3.8"`
`config.rs`: `"kt" | "kts" => Some("kotlin"),`
`languages.rs`: `"kotlin" => Some(tree_sitter_kotlin::LANGUAGE.into()),`

Note: Verify ABI compatibility — if `tree-sitter-kotlin = "0.3.8"` fails to compile with `tree-sitter = "0.24"`, try the `tree-sitter-kotlin2` crate or vendor the grammar.

- [ ] **Step 4: Add node extraction**

Kotlin tree-sitter grammar uses: `class_declaration`, `object_declaration`, `function_declaration` (not `method_declaration`), `interface_declaration`. Add missing patterns:

```rust
// Kotlin function declarations
"function_declaration" => {
    // Already handled for most languages — verify Kotlin uses same "name" field
    if let Some(mut parsed) = extract_function_node(&node, source, "function", parent_class) {
        parsed.is_test = node_is_test;
        results.push(parsed);
    }
}
```

Kotlin `data class`, `sealed class`, `object` → map to `class` type. `companion_object` → skip.

- [ ] **Step 5: Add relation extraction**

Kotlin imports: `import_header` → `identifier` chain. Add:

```rust
"import_header" => {
    if language == "kotlin" {
        // Kotlin: import foo.bar.Baz → extract "Baz" (last segment)
        let full_text = node_text(&node, source).trim().to_string();
        let name = full_text.split('.').last().unwrap_or("").trim_end_matches(';').to_string();
        if !name.is_empty() && name != "*" {
            results.push(ParsedRelation {
                source_name: "<module>".into(),
                target_name: name,
                relation: REL_IMPORTS.into(),
                metadata: None,
            });
        }
    }
}
```

Kotlin class inheritance (`: SuperClass, Interface`) uses `delegation_specifier` inside `class_declaration`. Pattern similar to C# `base_list`.

- [ ] **Step 6: Run tests and verify**

Run: `cargo test test_kotlin_parsing 2>&1 | tail -20`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml src/utils/config.rs src/parser/languages.rs src/parser/treesitter.rs src/parser/relations.rs tests/parser_deep_test.rs
git commit -m "feat(parser): add Kotlin language support — classes, interfaces, imports, inheritance"
```

---

## Task 7: Ruby Language Support

**Files:** Same 6 files as Task 5

- [ ] **Step 1: Write parser test for Ruby**

```rust
#[test]
fn test_ruby_parsing() {
    let code = r#"
require 'json'
require_relative 'helpers/auth'

class UserController < ApplicationController
  def index
    @users = User.all
    render json: @users
  end

  def show
    @user = User.find(params[:id])
  end

  private

  def authorize_user
    head :unauthorized unless current_user
  end
end

module Helpers
  def format_name(user)
    "#{user.first} #{user.last}"
  end
end
"#;
    let nodes = parse_code(code, "ruby").unwrap();
    let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"UserController"), "Ruby class, got: {:?}", names);
    assert!(names.contains(&"index"), "Ruby method, got: {:?}", names);
    assert!(names.contains(&"Helpers"), "Ruby module, got: {:?}", names);

    let relations = extract_relations(code, "ruby").unwrap();
    let inherits: Vec<&str> = relations.iter()
        .filter(|r| r.relation == "inherits")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(inherits.contains(&"ApplicationController"), "Ruby inheritance, got: {:?}", inherits);

    let imports: Vec<&str> = relations.iter()
        .filter(|r| r.relation == "imports")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(imports.contains(&"json"), "Ruby require, got: {:?}", imports);
}
```

- [ ] **Step 2: Run test to verify it fails**

- [ ] **Step 3: Add dependency, detection, mapping**

`Cargo.toml`: `tree-sitter-ruby = "0.23.1"`
`config.rs`: `"rb" => Some("ruby"),`
`languages.rs`: `"ruby" => Some(tree_sitter_ruby::LANGUAGE.into()),`

- [ ] **Step 4: Add node extraction**

Ruby tree-sitter: `class`, `module`, `method`, `singleton_method`. Add patterns:

```rust
// Ruby class — uses "name" field with "constant" kind
"class" if language == "ruby" => {
    if let Some(name) = get_child_by_field(&node, "name", source) {
        results.push(make_simple_node("class", name.clone(), &node, source, node_is_test));
        // Check superclass for inheritance
        extract_children(node, source, language, Some(&name), results, depth, node_is_test);
        return;
    }
}

// Ruby module — map to "class" type for consistency (modules act as namespaces/mixins)
"module" if language == "ruby" => {
    if let Some(name) = get_child_by_field(&node, "name", source) {
        results.push(make_simple_node("interface", name.clone(), &node, source, node_is_test));
        extract_children(node, source, language, Some(&name), results, depth, node_is_test);
        return;
    }
}

// Ruby method
"method" if language == "ruby" => {
    if let Some(mut parsed) = extract_function_node(&node, source, "method", parent_class) {
        parsed.is_test = node_is_test;
        results.push(parsed);
    }
}
```

Note: Ruby uses `"class"` and `"module"` as node kind strings which may conflict with existing match arms. The implementor must use guard clauses (`if language == "ruby"`) or restructure the match.

- [ ] **Step 5: Add relation extraction**

Ruby `require`/`require_relative` are method calls, not special syntax:

```rust
"call" if language == "ruby" => {
    let method_name = node.child_by_field_name("method")
        .map(|n| node_text(&n, source).to_string());
    if let Some(ref m) = method_name {
        if m == "require" || m == "require_relative" {
            if let Some(args) = node.child_by_field_name("arguments") {
                if let Some(arg) = args.named_child(0) {
                    let path = node_text(&arg, source)
                        .trim_matches(|c| c == '\'' || c == '"')
                        .to_string();
                    let name = path.split('/').last().unwrap_or(&path).to_string();
                    results.push(ParsedRelation {
                        source_name: "<module>".into(),
                        target_name: name,
                        relation: REL_IMPORTS.into(),
                        metadata: None,
                    });
                }
            }
        }
    }
}
```

Ruby inheritance (`class Dog < Animal`): tree-sitter uses `superclass` field on `class` node.

- [ ] **Step 6: Run tests, verify, commit**

```bash
git commit -m "feat(parser): add Ruby language support — classes, modules, methods, require imports"
```

---

## Task 8: PHP Language Support

**Files:** Same 6 files

- [ ] **Step 1: Write parser test for PHP**

```rust
#[test]
fn test_php_parsing() {
    let code = r#"<?php
use App\Models\User;
use Illuminate\Http\Request;

class UserController extends Controller {
    public function index(Request $request) {
        return User::all();
    }

    public function show(int $id) {
        return User::findOrFail($id);
    }
}

interface Authenticatable {
    public function getAuthIdentifier();
}
"#;
    let nodes = parse_code(code, "php").unwrap();
    let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"UserController"), "PHP class, got: {:?}", names);
    assert!(names.contains(&"index"), "PHP method, got: {:?}", names);
    assert!(names.contains(&"Authenticatable"), "PHP interface, got: {:?}", names);

    let relations = extract_relations(code, "php").unwrap();
    let inherits: Vec<&str> = relations.iter()
        .filter(|r| r.relation == "inherits")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(inherits.contains(&"Controller"), "PHP extends, got: {:?}", inherits);
}
```

- [ ] **Step 2: Run test to verify it fails**

- [ ] **Step 3: Add dependency, detection, mapping**

`Cargo.toml`: `tree-sitter-php = "0.24.2"` (note: version 0.24, not 0.23)
`config.rs`: `"php" => Some("php"),`
`languages.rs`: `"php" => Some(tree_sitter_php::LANGUAGE_PHP.into()),`

Note: `tree-sitter-php` exposes `LANGUAGE_PHP` (not `LANGUAGE`). Verify by checking the crate docs.

- [ ] **Step 4: Add node and relation extraction**

PHP tree-sitter: `class_declaration`, `interface_declaration`, `trait_declaration`, `method_declaration`, `function_definition`, `namespace_use_declaration`.

PHP `use` statements → `namespace_use_declaration` with `namespace_use_clause` children.
PHP `extends` → `base_clause` on class.
PHP `implements` → `class_interface_clause`.

- [ ] **Step 5: Run tests, verify, commit**

```bash
git commit -m "feat(parser): add PHP language support — classes, interfaces, use imports, extends/implements"
```

---

## Task 9: Swift Language Support

**Files:** Same 6 files

- [ ] **Step 1: Write parser test for Swift**

```rust
#[test]
fn test_swift_parsing() {
    let code = r#"
import Foundation
import UIKit

protocol UserRepository {
    func findById(_ id: Int) -> User?
}

class UserService: UserRepository {
    func findById(_ id: Int) -> User? {
        return database.find(id)
    }

    func listAll() -> [User] {
        return []
    }
}

struct User {
    let id: Int
    let name: String
}

enum UserRole {
    case admin
    case viewer
}
"#;
    let nodes = parse_code(code, "swift").unwrap();
    let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"UserRepository"), "Swift protocol, got: {:?}", names);
    assert!(names.contains(&"UserService"), "Swift class, got: {:?}", names);
    assert!(names.contains(&"User"), "Swift struct, got: {:?}", names);
    assert!(names.contains(&"UserRole"), "Swift enum, got: {:?}", names);

    let relations = extract_relations(code, "swift").unwrap();
    let imports: Vec<&str> = relations.iter()
        .filter(|r| r.relation == "imports")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(imports.contains(&"Foundation"), "Swift import, got: {:?}", imports);
}
```

- [ ] **Step 2-3: Add dependency, detection, mapping**

`Cargo.toml`: `tree-sitter-swift = "0.7.1"` (independent versioning — verify ABI)
`config.rs`: `"swift" => Some("swift"),`
`languages.rs`: `"swift" => Some(tree_sitter_swift::LANGUAGE.into()),`

If ABI incompatible, see spec fallback options.

- [ ] **Step 4: Add node and relation extraction**

Swift: `class_declaration`, `protocol_declaration` (→ interface), `struct_declaration`, `enum_declaration`, `function_declaration`, `import_declaration`.

Swift `protocol` → map to `interface` type.
Swift inheritance (`: Protocol, Class`) → `type_inheritance_clause`.
Swift `import Foundation` → `import_declaration`.

- [ ] **Step 5: Run tests, verify, commit**

```bash
git commit -m "feat(parser): add Swift language support — classes, protocols, structs, enums, imports"
```

---

## Task 10: Dart Language Support

**Files:** Same 6 files

- [ ] **Step 1: Write parser test for Dart**

```rust
#[test]
fn test_dart_parsing() {
    let code = r#"
import 'dart:async';
import 'package:flutter/material.dart';

abstract class UserRepository {
  Future<User?> findById(int id);
}

class UserService implements UserRepository {
  @override
  Future<User?> findById(int id) async {
    return await db.find(id);
  }

  List<User> listAll() => [];
}

class User {
  final int id;
  final String name;
  User(this.id, this.name);
}

enum UserRole { admin, viewer }
"#;
    let nodes = parse_code(code, "dart").unwrap();
    let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"UserRepository"), "Dart abstract class, got: {:?}", names);
    assert!(names.contains(&"UserService"), "Dart class, got: {:?}", names);
    assert!(names.contains(&"User"), "Dart class, got: {:?}", names);

    let relations = extract_relations(code, "dart").unwrap();
    let imports: Vec<&str> = relations.iter()
        .filter(|r| r.relation == "imports")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(!imports.is_empty(), "Dart imports should be extracted, got: {:?}", imports);
}
```

- [ ] **Step 2-3: Add dependency, detection, mapping**

`Cargo.toml`: `tree-sitter-dart = "0.1.0"` (early stage — verify ABI)
`config.rs`: `"dart" => Some("dart"),`
`languages.rs`: `"dart" => Some(tree_sitter_dart::LANGUAGE.into()),`

If ABI incompatible, see spec fallback options.

- [ ] **Step 4: Add node and relation extraction**

Dart: `class_declaration`, `enum_declaration`, `function_signature` + `function_body`, `method_signature`, `import_or_export`.

Dart `implements` → `interfaces` clause.
Dart `extends` → `superclass` clause.
Dart `import '...'` → `import_or_export` node.

- [ ] **Step 5: Run tests, verify, commit**

```bash
git commit -m "feat(parser): add Dart language support — classes, enums, imports, implements/extends"
```

---

## Task 11: INDEX_VERSION Bump & Full Validation

**Files:**
- Modify: `src/domain.rs:21`

- [ ] **Step 1: Bump INDEX_VERSION**

Change line 21 of `src/domain.rs`:
```rust
pub const INDEX_VERSION: i32 = 5;
```

- [ ] **Step 2: Run full test suite**

Run: `cargo test 2>&1 | tail -20`
Expected: All tests pass (including all 6 new language tests)

- [ ] **Step 3: Run build with no-default-features**

Run: `cargo check --no-default-features 2>&1 | tail -10`
Expected: No errors (tree-sitter grammars are not behind feature gate)

- [ ] **Step 4: Run full release build**

Run: `cargo build --release 2>&1 | tail -10`
Expected: Build succeeds

- [ ] **Step 5: Verify dead code tool works on this project**

Run: `cargo run --release -- serve` (in one terminal)
Then test via MCP call or:
Run: `echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"find_dead_code","arguments":{}}}' | cargo run --release`
Expected: List of orphan/exported-unused symbols in code-graph-mcp itself

- [ ] **Step 6: Commit**

```bash
git add src/domain.rs
git commit -m "chore: bump INDEX_VERSION 4→5 for new language parsers"
```

---

## Task 12: Version Bump & Release Preparation

**Files:**
- Modify: `Cargo.toml`, `package.json`, 5x `npm/*/package.json` (7 files total)

- [ ] **Step 1: Update version in all 9 files**

Change version from `0.5.48` to `0.6.0` in:
1. `Cargo.toml` — `version = "0.6.0"`
2. `package.json` — `"version": "0.6.0"` (also update 5 `optionalDependencies` version strings)
3. `npm/linux-x64/package.json` — `"version": "0.6.0"`
4. `npm/linux-arm64/package.json` — `"version": "0.6.0"`
5. `npm/darwin-x64/package.json` — `"version": "0.6.0"`
6. `npm/darwin-arm64/package.json` — `"version": "0.6.0"`
7. `npm/win32-x64/package.json` — `"version": "0.6.0"`
8. `.claude-plugin/marketplace.json` — `"version": "0.6.0"` (2 occurrences)
9. `claude-plugin/.claude-plugin/plugin.json` — `"version": "0.6.0"`

- [ ] **Step 2: Verify all versions match**

Run: `grep -rn "0.5.48" --include="*.json" --include="*.toml" . | grep -v node_modules | grep -v target`
Expected: No matches (all replaced with 0.6.0)

- [ ] **Step 3: Final test suite**

Run: `cargo test 2>&1 | tail -5`
Expected: All tests pass

- [ ] **Step 4: Commit and tag**

```bash
git add Cargo.toml package.json npm/*/package.json .claude-plugin/marketplace.json claude-plugin/.claude-plugin/plugin.json
git commit -m "chore: bump version to 0.6.0"
git tag v0.6.0
```

- [ ] **Step 5: Push**

```bash
git push && git push --tags
```
