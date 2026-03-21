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
SELECT n.id, n.name, n.node_type, n.start_line, n.end_line, f.path
FROM nodes n
JOIN files f ON n.file_id = f.id
WHERE n.is_test = 0
  AND (n.end_line - n.start_line + 1) >= :min_lines
  AND NOT EXISTS (
    SELECT 1 FROM edges e
    WHERE e.target_id = n.id
    AND e.relation IN ('calls', 'imports', 'inherits', 'implements')
  )
ORDER BY (n.end_line - n.start_line) DESC
```

Orphan vs Exported-Unused classification done in Rust layer by checking for outgoing `exports` edges.

**Files modified:**
- `src/mcp/tools.rs` — Register tool, bump TOOL_COUNT 11→12
- `src/mcp/server/tools.rs` — Tool implementation (~100 lines)
- `src/storage/queries.rs` — `find_dead_code()` query function (~40 lines)

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
- `src/main.rs` or CLI module — Add `benchmark` subcommand
- New module `src/benchmark.rs` (~200 lines)

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

**Tree-sitter dependencies:**
```toml
tree-sitter-c-sharp = "0.23"
tree-sitter-kotlin = "0.23"
tree-sitter-ruby = "0.23"
tree-sitter-php = "0.23"
tree-sitter-swift = "0.23"
tree-sitter-dart = "0.23"   # Use latest stable if 0.23 unavailable
```

**INDEX_VERSION:** Bump 4 → 5 (triggers automatic re-index on existing projects).

**Not in scope:**
- Markup languages (YAML, JSON, Markdown)
- Language-specific advanced analysis (C# LINQ, Kotlin coroutines)
- Full framework coverage (only top 1-2 per language for routes)

---

### Release: Version Bump to v0.6.0

Minor version bump (new tool + new languages = feature addition).

**8 files requiring version sync** (per project convention):
- Cargo.toml
- package.json
- npm/code-graph/package.json
- npm/code-graph-{platform}/package.json (5 platform packages)

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
