# Phase 1 Optimization Design: Competitive Parity

> Date: 2026-03-21
> Status: Approved
> Version target: v0.6.0
> Execution order: B (low-cost high-impact) → A (language expansion)

## Context

Competitive analysis identified code-graph-mcp's unique position (Rust + SQLite + hybrid BM25/vector search + in-process Candle embeddings + zero dependencies) but revealed gaps in language coverage (10 vs 64 in codebase-memory-mcp), missing dead code detection, and insufficient marketing benchmarks. Phase 1 closes these gaps while preserving architectural advantages.

## Deliverables

### B-1: `find_dead_code` MCP Tool

**Purpose:** Scan entire project for unreferenced symbols with smart risk classification.

**Parameters:**

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `scope` | string | `"project"` | `"project"` full scan / `"module"` scoped |
| `path` | string? | null | Directory path when scope=module |
| `node_type` | string? | null | Filter: fn/class/struct/const/type/all |
| `include_tests` | bool | false | Include test code in results |
| `min_lines` | int | 3 | Minimum line count (skip trivial symbols) |
| `compact` | bool | true | Compact output mode |

**Output classification (two tiers):**

- **Orphan** (red): Zero incoming edges (calls/imports/inherits/implements) AND not exported AND not an entry point. Fully isolated dead code.
- **Exported-Unused** (yellow): Has export edge but zero usage within the project. Potentially dead public API.

**Exclusion rules (false positive reduction):**
- `main`/`lib` entry functions
- HTTP route handlers (have `routes_to` relation)
- `impl` trait methods (have `implements` relation)
- Test functions (excluded by default, toggleable)
- Symbols in `lib.rs`/`mod.rs` with `pub` visibility → classified as Exported-Unused, never Orphan

**Core query:**
```sql
SELECT n.id, n.name, n.type, n.start_line, n.end_line, f.path
FROM nodes n
JOIN files f ON n.file_id = f.id
WHERE n.type != 'module'
  AND n.name != '<module>'
  AND (n.end_line - n.start_line + 1) >= :min_lines
  AND (:node_type IS NULL OR n.type = :node_type)
  -- [dynamic: AND n.is_test = 0 when include_tests=false]
  AND (:path IS NULL OR f.path LIKE :path || '%')
  -- Exclude symbols with incoming usage edges
  AND NOT EXISTS (
    SELECT 1 FROM edges e
    WHERE e.target_id = n.id
    AND e.relation IN ('calls', 'imports', 'inherits', 'implements')
  )
  -- Exclude route handlers (routes_to is a self-edge: source_id = target_id)
  AND NOT EXISTS (
    SELECT 1 FROM edges e
    WHERE e.source_id = n.id
    AND e.relation = 'routes_to'
  )
ORDER BY (n.end_line - n.start_line) DESC
```

**`node_type` parameter mapping:**

| Input | SQL value | Notes |
|-------|-----------|-------|
| `fn` | `function` | Also matches `method` (both are callable) |
| `class` | `class` | Pass-through |
| `struct` | `struct` | Pass-through |
| `interface` | `interface` | Pass-through |
| `enum` | `enum` | Pass-through |
| `type` | `type` | Pass-through |
| `const` | `const` | Pass-through |
| `null`/`all` | omit filter | Returns all types except `module` |

**`include_tests` parameter:** When `false` (default), dynamically appends `AND n.is_test = 0` to the query. When `true`, omits this clause to return all symbols. Follows existing pattern in `fts5_search_impl` (queries.rs:1531).

**`scope=module` + `path`:** When `scope="module"` and `path` is set, adds `AND f.path LIKE :path || '%'` filter.

**Orphan vs Exported-Unused classification:** Done in Rust layer by checking for **incoming** `exports` edges (`e.target_id = n.id AND e.relation = 'exports'`). The `exports` edge direction is `source=<module>` → `target=exported_symbol`.

**Visibility heuristic (no schema change):** For languages without `exports` edges (Rust, Go, C, etc.), use code_content prefix heuristic:
- Rust: `code_content` starts with `pub `
- Go: name starts with uppercase letter
- Other languages: rely on `exports` edges only (JS/TS)
Symbols matching visibility heuristic but with zero callers → Exported-Unused. Others → Orphan.

**Test plan:**
- Function with zero incoming edges, no exports → Orphan
- Function with incoming `exports` edge but no callers → Exported-Unused
- `main` function → excluded (entry point)
- Function with `routes_to` self-edge → excluded (route handler)
- Trait impl method (has `implements` edge) → excluded
- Test function with `is_test=1` → excluded by default, included when `include_tests=true`
- `module` node type → always excluded

**Files modified:**
- `src/mcp/tools.rs` — Register tool, bump TOOL_COUNT 11→12, update `test_tool_registry_has_all_tools`
- `src/mcp/server/tools.rs` — Tool implementation (~120 lines)
- `src/storage/queries.rs` — `find_dead_code()` query function (~50 lines)

---

### B-2: `benchmark` CLI Subcommand

**Usage:**
```bash
code-graph-mcp benchmark [project_path]
code-graph-mcp benchmark --format json
```

**Measurements:**

| Metric | Method | Example Output |
|--------|--------|----------------|
| Index speed | Full index timing | `1,247 files in 2.3s (542 files/s)` |
| Incremental index | Single-file re-index timing | `Incremental: 45ms` |
| Query latency | Representative queries P50/P99 | `semantic_search: P50=12ms P99=45ms` |
| Index size | .code-graph/ directory size | `Database: 4.2MB` |
| Token savings | Estimated vs grep+read baseline | `8-20x reduction` |

**Token savings estimation methodology:**
- 5 representative tasks (project architecture, concept search, call chain trace, impact analysis, module structure)
- grep+read baseline: avg 2000 tokens/Grep + 4000 tokens/Read, multiply by typical call count
- code-graph: actual output character count * 0.33 tokens/char
- Report per-task and aggregate reduction factor

**Output formats:**
- Default: human-readable table
- `--format json`: structured JSON for CI/automation

**Files modified:**
- `src/cli.rs` — Add `cmd_benchmark()` function following existing `cmd_*` pattern (~200 lines)
- `src/main.rs` — Wire `"benchmark"` subcommand to `cmd_benchmark()`

---

### B-3: README Benchmark Update

Add performance headline after existing efficiency table:

```markdown
## Performance

- **8-20x fewer tokens** per code understanding task vs grep+read
- **95% fewer source lines** read per session
- **500+ files/second** indexing speed (Rust, single-threaded)
- **<50ms P50** query latency
- **Single binary**, zero external dependencies
```

Principles:
- Use conservative estimates from actual benchmark runs
- No direct competitor comparisons (avoid controversy)
- Data backed by `code-graph-mcp benchmark` output

**Files modified:** `README.md`

---

### A-1 through A-6: Language Expansion (10 → 16)

**New languages:** C#, Kotlin, Ruby, PHP, Swift, Dart

**Language family grouping (shared extraction logic):**

| Family | Languages | Reuse Source | Shared % |
|--------|-----------|-------------|:--------:|
| JVM | C#, Kotlin | Java | ~80% |
| Script | Ruby, PHP | Python | ~70% |
| Mobile | Swift, Dart | Rust/Java hybrid | ~60% |

**Per-language file changes (5 files each):**

| File | Change per language |
|------|-------------------|
| `Cargo.toml` | +1 dependency line |
| `src/utils/config.rs` | +2-4 lines (extension mapping) |
| `src/parser/languages.rs` | +1-2 lines (Language enum) |
| `src/parser/treesitter.rs` | +20-60 lines (node extraction) |
| `src/parser/relations.rs` | +20-60 lines (relation extraction) |

**Relation extraction depth guarantee:**

| Relation | All 6 langs | Notes |
|----------|:-----------:|-------|
| `calls` | Guaranteed | Function/method calls |
| `imports` | Guaranteed | Module/package imports |
| `inherits` | Guaranteed | Class inheritance |
| `implements` | C#/Kotlin/Dart/Swift | Interface/protocol implementation |
| `exports` | PHP/Ruby | Module exports |
| `routes_to` | Best-effort | Only top 1-2 frameworks per language |

**Route extraction frameworks (best-effort):**
- C#: ASP.NET `[HttpGet("/path")]` attributes
- Kotlin: Spring Boot `@GetMapping` / Ktor routing
- Ruby: Rails `get '/', to: 'controller#action'`
- PHP: Laravel `Route::get('/path', ...)`
- Swift: Vapor `app.get("path") { ... }`
- Dart: Shelf/Dart Frog (low priority)

**Tree-sitter dependencies (actual crates.io versions):**
```toml
tree-sitter-c-sharp = "0.23.1"   # Follows tree-sitter versioning
tree-sitter-ruby = "0.23.1"      # Follows tree-sitter versioning
tree-sitter-php = "0.24.2"       # Latest stable, independent versioning
tree-sitter-kotlin = "0.3.8"     # Independent versioning scheme
tree-sitter-swift = "0.7.1"      # Independent versioning scheme
tree-sitter-dart = "0.1.0"       # Independent versioning, early stage
```

**ABI compatibility note:** Kotlin, Swift, and Dart crates use independent versioning that does not follow the tree-sitter core scheme. These must be validated for ABI compatibility with `tree-sitter = "0.24"` before integration. If incompatible, options:
1. Use alternative crate (e.g., community fork)
2. Vendor the grammar C source and build via `build.rs` (like sqlite-vec)
3. Drop the language from Phase 1 and revisit in Phase 2

**INDEX_VERSION:** Bump 4 → 5 in `src/domain.rs` (triggers automatic re-index on existing projects). Note: `INDEX_VERSION` (domain.rs) and `SCHEMA_VERSION` (schema.rs) are separate constants — only `INDEX_VERSION` needs bumping here since no schema DDL changes.

**Not in scope:**
- Markup languages (YAML, JSON, Markdown)
- Language-specific advanced analysis (C# LINQ, Kotlin coroutines)
- Full framework coverage (only top 1-2 per language for routes)

---

### Release: Version Bump to v0.6.0

Minor version bump (new tool + new languages = feature addition).

**7 files requiring version sync:**
- `Cargo.toml`
- `package.json` (root)
- `npm/linux-x64/package.json`
- `npm/linux-arm64/package.json`
- `npm/darwin-x64/package.json`
- `npm/darwin-arm64/package.json`
- `npm/win32-x64/package.json`

---

### External: MCP Directory Registration

Submit to these platforms after release:

| Platform | Method |
|----------|--------|
| [punkpeye/awesome-mcp-servers](https://github.com/punkpeye/awesome-mcp-servers) | PR |
| [mcpservers.org](https://mcpservers.org) | Web form |
| [pulsemcp.com](https://pulsemcp.com) | Web form |
| npm keywords | Update package.json |

**Submission description template:**
> code-graph-mcp — Rust MCP server that indexes codebases into an AST knowledge graph with hybrid semantic search (BM25 + vector). 16 languages, 12 tools, single binary, zero external dependencies. 8-20x token reduction vs grep+read.

---

## Execution Order

```
Phase B (low-cost, high-impact):
  B-1: find_dead_code tool         (~1 day)
  B-2: benchmark CLI subcommand    (~1 day)
  B-3: README benchmark update     (~0.5 day)

Phase A (language expansion):
  A-1: C# support                  (~0.5 day)
  A-2: Kotlin support              (~0.5 day)
  A-3: Ruby support                (~0.5 day)
  A-4: PHP support                 (~0.5 day)
  A-5: Swift support               (~0.5 day)
  A-6: Dart support + INDEX_VERSION bump (~0.5 day)

Release:
  Version bump 0.5.x → 0.6.0
  MCP directory registration (external)
```

## Architecture Decisions

1. **Keep SQLite** — External DB kills adoption (validated by market data)
2. **Smart classification** over binary dead/alive — Orphan vs Exported-Unused tiers reduce false positives
3. **Language families** — Batch similar languages to maximize code reuse
4. **Conservative benchmarks** — Under-promise, use real measurements
5. **INDEX_VERSION bump once** — Single bump at end of language expansion, not per-language
