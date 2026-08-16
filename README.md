# code-graph-mcp

A high-performance code knowledge graph server implementing the [Model Context Protocol (MCP)](https://modelcontextprotocol.io/). Indexes codebases into a structured AST knowledge graph with semantic search, call graph traversal, and HTTP route tracing — designed to give AI coding assistants deep, structured understanding of your code.

## Features

- **Multi-language parsing** — Tree-sitter AST extraction across tiers of depth:
  - **Full** (calls + imports + inheritance): TypeScript/TSX, JavaScript, Go, Python, Rust, Java. HTTP route extraction additionally covers TypeScript/TSX + JavaScript (Express/Connect), Go (`net/http`), Python (Flask/FastAPI), and Rust axum (`.route()` chains with inline `.nest()` prefixes; named handlers only) — actix/rocket and Java Spring are not yet route-extracted
  - **Smoke-tested** (calls + imports + inheritance): C#, Kotlin, Ruby, PHP, Swift, Dart
  - **Limited** (functions + calls + `#include` imports + gtest test markers + C++ base-class inheritance + `Class::method` scope qualification): C, C++
  - **Scripting**: Bash (functions + commands + `source`/`.` imports), Markdown (headings)
  - **File-FTS only** (no AST symbol extraction): HTML, CSS, JSON
  - **Test markers** — detected from the AST for Rust (`#[test]` / `#[cfg(test)]`), JavaScript/TypeScript/TSX (`describe`/`it`/`test` blocks and their callbacks) and C/C++ (gtest `TEST` macros). Go, Python, Java and the rest fall back to path/name heuristics (`tests/`, `src/test/java/`, `*_test.*`, `*.test.js`, `test_*`, `*Test`)
  - **Value/type references** — a `references` relation for symbols that are used without being called, imported or inherited (path-qualified constants, type-position uses, functions passed as values): Rust, TypeScript/TSX, JavaScript, Python, Go, Java, C, C++
- **Semantic code search** — Hybrid BM25 full-text + vector semantic search with Reciprocal Rank Fusion (RRF), powered by sqlite-vec
- **Call graph traversal** — Recursive CTE queries to trace callers/callees with cycle detection
- **HTTP route tracing** — Map route paths to backend handler functions (Express, Flask/FastAPI, Go `net/http`, Rust axum)
- **Dead code detection** — Find unreferenced symbols with smart Orphan/Exported-Unused classification
- **Impact analysis** — Determine the blast radius of code changes by tracing all dependents
- **Incremental indexing** — Merkle tree change detection with file system watcher for real-time updates. Smart event filtering skips metadata-only changes (chmod, xattr)
- **Context compression** — Token-aware snippet extraction for LLM context windows (L0→full code, L1→summaries, L2→file groups, L3→directory overview). Compact JSON output saves 15-20% tokens
- **Embedding model** — Optional local embedding via Candle (feature-gated `embed-model`). Context reordered to prioritize structural relations over code for better embedding quality
- **Self-healing** — Automatic SQLite corruption recovery with rebuild. Startup repair for incomplete indexing (Phase 3 failures)
- **MCP protocol** — JSON-RPC 2.0 over stdio, plug-and-play with Claude Code, Cursor, Windsurf, and other MCP clients
- **Claude Code Plugin** — First-class plugin with skills (`explore`, `index`), a `code-explorer` agent, auto-indexing hooks, StatusLine integration, and self-updating

## Why code-graph-mcp?

Unlike naive full-text search or simple AST dumps, code-graph-mcp builds a **structured knowledge graph** that understands the relationships between symbols across your entire codebase.

### Incremental by Design

BLAKE3 Merkle tree tracks every file's content hash. On re-index, only changed files are re-parsed — unchanged directory subtrees are skipped entirely via mtime cache. When a function signature changes, **dirty propagation** automatically regenerates context for all downstream callers across files.

### Hybrid Search, Not Just Grep

Combines BM25 full-text ranking (FTS5) with vector semantic similarity (sqlite-vec) via **Reciprocal Rank Fusion (RRF)** with raw score blending — so searching "handle user login" finds the right function even if it's named `authenticate_session`. Results are auto-compressed to fit LLM context windows.

### Scope-Aware Relation Extraction

The parser doesn't just find function calls — it tracks them within their proper scope context. Extracts calls, imports, inheritance, interface implementations, exports (ESM `export` **and** CommonJS `module.exports = { … }` / `exports.name = …`), value/type references, and HTTP route bindings. Same-file targets are preferred over cross-file matches to minimize false-positive edges.

### HTTP Request Flow Tracing

Unique to code-graph-mcp: trace from `GET /api/users` → route handler → service layer → database call in a single query. Supports Express/Connect, Flask/FastAPI, Go `net/http`, and Rust axum.

### Zero External Dependencies at Runtime

Single binary, embedded SQLite, bundled sqlite-vec extension, optional local embedding model via Candle — no database server, no cloud API, no Docker required. Runs entirely on your machine.

### Built for AI Assistants

Every design decision — from token-aware compression to node_id-based snippet expansion — is optimized for LLM context windows. Works out of the box with Claude Code, Cursor, Windsurf, and any MCP-compatible client.

## Performance

| Metric | Value |
|--------|-------|
| Indexing speed | **300+ files/second** (single-threaded, release build) |
| Incremental re-index | **<250ms** no-change detection via BLAKE3 Merkle tree |
| FTS search P50 / P99 | **<300us / <1ms** |
| Database overhead | **~3.5MB** per 800 nodes |
| Token savings | **5-20x fewer tokens** per code understanding task vs grep+read |

Run `code-graph-mcp benchmark` on your own project to measure.

## Efficiency: code-graph vs Traditional Tools

Real-world benchmarks comparing code-graph-mcp tools against traditional approaches (Grep + Read + Glob) on a 33-file Rust project (~537 AST nodes).

### Tool Call Reduction

| Scenario | Traditional | code-graph | Savings |
|----------|:-----------:|:----------:|:-------:|
| Project architecture overview | 5-8 calls | 1 call (`project_map`) | **~85%** |
| Find function by concept | 3-5 calls | 1 call (`semantic_code_search`) | **~75%** |
| Trace 2-level call chain | 8-15 calls | 1 call (`get_call_graph`) | **~90%** |
| Pre-change impact analysis | 10-20+ calls | 1 call (`get_ast_node` + `include_impact`) | **~95%** |
| Module structure & exports | 5+ calls | 1 call (`module_overview`) | **~80%** |
| File dependency mapping | 3-5 calls | 1 call (`module_overview` + `include_deps`) | **~75%** |
| Similar code detection | N/A | 1 call (`get_ast_node` + `include_similar`) | **unique** |

### Overall Session Efficiency

| Metric | Without code-graph | With code-graph | Improvement |
|--------|:------------------:|:---------------:|:-----------:|
| Tool calls per navigation task | ~6 | ~1.2 | **~80% fewer** |
| Source lines read into context | ~8,000 lines | ~400 lines (structured) | **~95% less** |
| Navigation token cost | ~36K tokens | ~7K tokens | **~80% saved** |
| Full session token savings | — | — | **40-60%** |

### What code-graph Uniquely Enables

- **Impact analysis** — "Changing `conn` affects 33 functions across 4 files, 78 tests at HIGH risk" — impossible to derive manually with Grep
- **Transitive call tracing** — Follow `main` → `run_serve` → `handle_message` → `handle_tools_call` → `conn` in one query
- **Semantic search** — Find `authenticate_session` when searching "handle user login"
- **Dependency strength** — Not just "file A imports file B", but "file A uses 38 symbols from file B"

### When Traditional Tools Are Still Better

| Use Case | Best Tool |
|----------|-----------|
| Exact string / constant search | Grep |
| Reading a file to edit it | Read |
| Finding files by name pattern | Glob |

## Architecture

```
src/
├── domain.rs     # Shared constants, relation types, env-var config
├── resolve.rs    # Shared symbol resolution + ambiguity verdicts (CLI and MCP)
├── outcome.rs    # Retrieval-adoption metrics from session transcripts
├── cli/          # Every `code-graph-mcp <cmd>` subcommand (one file per command)
├── mcp/          # MCP protocol layer (JSON-RPC, tool registry, server)
│   └── server/   # McpServer with IndexingState + CacheState sub-structs
├── parser/       # Tree-sitter parsing, relation extraction, LanguageConfig dispatch
├── indexer/      # 3-phase pipeline, Merkle tree, file watcher
├── storage/      # SQLite schema (v10), CRUD, FTS5, migrations
├── graph/        # Recursive CTE call graph queries
├── search/       # RRF fusion search combining BM25 + vector
├── embedding/    # Candle embedding model (optional, masked mean pooling)
├── snapshot/     # Portable graph snapshots (create / verify / install)
├── sandbox/      # Context compressor with token estimation
└── utils/        # Language detection, config
```

## Installation

### Option 1: Claude Code Plugin (Recommended)

Install as a Claude Code plugin for the best experience — includes skills, the `code-explorer` agent, auto-indexing hooks, StatusLine health display, and automatic updates:

```bash
# Step 1: Add the marketplace
/plugin marketplace add sdsrss/code-graph-mcp

# Step 2: Install the plugin
/plugin install code-graph-mcp
```

What you get:
- **MCP Server** — All code-graph tools available to Claude
- **Skills** — `explore` (structure-first navigation before reading files) and `index` (health-check / re-index / full rebuild); see [Plugin Skills](#plugin-skills)
- **Code Explorer Agent** — Deep code understanding expert via `code-explorer`
- **Auto-indexing Hook** — Incremental index on every file edit (PostToolUse)
- **StatusLine** — Real-time health display (nodes, files, watch status) — compatible with other plugins' StatusLine via composite multiplexer
- **Auto-update** — Checks for a new version at session start (throttled to at most one check every 2 minutes). Between forced checks the re-check interval is 30 minutes after an "up to date" answer and 6 hours while an update is already pending. Updates install silently.

#### Manual Update

```bash
npm update -g @sdsrs/code-graph
```

Then reconnect the MCP server in Claude Code with `/mcp`.

> **Note:** Auto-update is disabled in the source repo directory (dev mode). Use manual update when developing the plugin itself.

#### Turning auto-update off

| Environment variable | Effect |
|----------------------|--------|
| `CODE_GRAPH_NO_AUTO_UPDATE=1` | Skips the update check entirely — no version check, no download, and no updater process is started. A **missing** binary is still installed on first run, so the MCP server can't be left with no engine. Update manually with the command above. |

Set it in the `env` block of `~/.claude/settings.json` (the environment of the
process hosting the MCP server), then reconnect with `/mcp`.

An update that keeps failing also stops hammering: after 5 failed attempts at
the *same* release the updater drops to one retry per day (and retries
immediately when a newer release is published), instead of re-downloading on
every session. While it is in that state the statusline shows `⚠ update stuck`
and `code-graph-mcp doctor` prints the manual update command.

#### Invited-memory mode (quieter prompts)

By default, every user prompt the plugin deems code-related gets a small context injection from `code-graph` CLI output. If you'd rather rely on MEMORY.md + explicit tool calls, opt into invited-memory mode:

1. Adopt the plugin contract into your project (idempotent, self-heals):
   ```bash
   code-graph-mcp adopt
   ```
   This writes a sentinel-wrapped managed block into `<cwd>/CLAUDE.md` (auto-loaded each session) plus `<cwd>/.claude/plugin_code_graph_mcp.md` (the full decision table, opened on demand). Run `code-graph-mcp unadopt` to remove both; content outside the managed block is kept.
2. Set the activation env var in `~/.claude/settings.json`:
   ```json
   {
     "env": { "CODE_GRAPH_QUIET_HOOKS": "1" }
   }
   ```
3. Restart Claude Code. Session startup skips the project-map injection, UserPromptSubmit stops auto-injecting context, and the MCP `instructions` become a short pointer to the MEMORY.md file.

### Option 2: Claude Code MCP Server Only

Register as an MCP server without the plugin features:

```bash
claude mcp add code-graph-mcp -- npx -y @sdsrs/code-graph
```

### Option 3: Cursor / Windsurf / Other MCP Clients

Add to your MCP settings file (e.g. `~/.cursor/mcp.json`):

```json
{
  "mcpServers": {
    "code-graph": {
      "command": "npx",
      "args": ["-y", "@sdsrs/code-graph"]
    }
  }
}
```

### Option 4: npx (No Install)

Run directly without installing:

```bash
npx -y @sdsrs/code-graph
```

### Option 5: npm (Global Install)

Install globally, then run anywhere:

```bash
npm install -g @sdsrs/code-graph
code-graph-mcp
```

## Uninstallation

### Claude Code Plugin

```bash
# Uninstall the plugin
/plugin uninstall code-graph-mcp

# (Optional) Remove the marketplace
/plugin marketplace remove code-graph-mcp

# (Optional) Clean up all config and cache data
node ~/.claude/plugins/cache/code-graph-mcp/code-graph-mcp/*/scripts/lifecycle.js uninstall
```

### Claude Code MCP Server

```bash
claude mcp remove code-graph-mcp
```

### Cursor / Windsurf / Other MCP Clients

Remove the `code-graph` entry from your MCP settings file (e.g. `~/.cursor/mcp.json`).

### npm (Global)

Run the teardown **first**, while the CLI still exists — npm 7 removed the
`preuninstall`/`postuninstall` lifecycle scripts, so nothing runs on your behalf
during `npm uninstall`. Doing it in the other order leaves the hook entries in
`~/.claude/settings.json` and the ~40 MB binary cache behind, with no command
left on disk to remove them.

```bash
code-graph-mcp uninstall     # restore statusline, strip hooks, drop the cache
npm uninstall -g @sdsrs/code-graph
```

## MCP Tools

`tools/list` advertises exactly these seven. Several older niche tools were folded into flags on them, so one call now covers what used to take a separate tool:

| Tool | Description |
|------|-------------|
| `project_map` | Full project architecture: modules, dependencies, entry points, hot functions. `include_centrality` adds architectural chokepoints |
| `semantic_code_search` | Hybrid BM25 + vector search (RRF) for AST nodes. Supports `compact` mode |
| `get_call_graph` | Trace upstream/downstream call chains for a function. Pass `route_path='GET /api/x'` to trace an HTTP route → handler → downstream instead |
| `get_ast_node` | One symbol with signature, body and relations. `include_impact` adds the blast radius, `include_similar` embedding-similar nodes, `include_references` callers/callees. Supports `compact` mode |
| `module_overview` | Symbols in a directory or file, grouped by type and caller count. `include_dead` lists unreferenced symbols under the path; `include_deps` adds the file dependency graph (single-file paths only) |
| `ast_search` | Search AST nodes by text and/or structural filters (type, return type, params) |
| `find_references` | Find all references to a symbol (callers, importers, inheritors, implementors, value/type references). Supports `compact` mode |

**Hidden aliases.** These names are not in `tools/list` but still dispatch via `tools/call`, so existing clients keep working: `trace_http_chain` / `find_http_route` (→ `get_call_graph` with `route_path`), `read_snippet` (→ `get_ast_node`), `dependency_graph`, `find_similar_code`, `find_dead_code`, plus the management tools `start_watch`, `stop_watch`, `get_index_status` and `rebuild_index`. `impact_analysis` is **removed** — calling it returns `Unknown tool`; use `get_ast_node` with `include_impact=true`, or the CLI's `impact --json` for the full report.

## CLI Commands

All tools are also available as CLI subcommands for shell scripts, hooks, and terminal workflows:

| Command | MCP Equivalent | Description |
|---------|---------------|-------------|
| `search <query>` | `semantic_code_search` | FTS5 search by concept |
| `ast-search [query]` | `ast_search` | Structural search with `--type`/`--returns`/`--params` filters |
| `callgraph <symbol>` | `get_call_graph` | Show call graph (callers/callees) |
| `impact <symbol>` | `get_ast_node` (`include_impact=true`) | Impact analysis (callers, routes, risk level) |
| `show <symbol>` | `get_ast_node` | Show symbol details (code, type, signature) |
| `map` | `project_map` | Project architecture map |
| `overview <path>` | `module_overview` | Module symbols grouped by file and type |
| `deps <file>` | `module_overview` (`include_deps=true`) | File-level dependency graph |
| `trace <route>` | `get_call_graph` (`route_path=…`) | Trace HTTP route → handler → downstream calls |
| `similar <symbol>` | `get_ast_node` (`include_similar=true`) | Find semantically similar code (requires embeddings) |
| `refs <symbol>` | `find_references` | Find all references to a symbol |
| `dead-code [path]` | `module_overview` (`include_dead=true`) | Find unused code (orphans and exported-unused) |
| `grep <pattern>` | — | AST-context grep (ripgrep + containing function/class) |
| `incremental-index` | — | Run incremental index update (auto-creates DB if needed) |
| `health-check` | `get_index_status` | Query index status and freshness |
| `benchmark` | — | Benchmark index speed, query latency, token savings |
| `affected [files…]` | — | Changed files → the test files to re-run (`--stdin`, `--depth`) |
| `tour [path]` | — | Dependency-ordered reading order for a repo or subtree |
| `centrality` | `project_map` (`include_centrality=true`) | Rank architectural chokepoints (betweenness over the call graph) |
| `cycles` | — | Detect circular import dependencies (file-level) |
| `surprising` | — | Surface unexpected cross-module couplings (uncertain edges) |
| `report` | — | Consolidated code-health report (summary + all analyses) |
| `stats` | — | Aggregate session metrics from `.code-graph/usage.jsonl` |
| `outcome` | — | Retrieval adoption from session transcripts (field-MRR; read-only) |
| `rebuild-index` | `rebuild_index` | Drop and rebuild the index from scratch (requires `--confirm`) |
| `reindex` | — | Incremental refresh; `--from-snapshot` refetches the published snapshot |
| `snapshot create\|inspect` | — | Build or inspect a portable graph snapshot |
| `doctor` | — | Diagnose and repair environment issues |
| `adopt` | — | Install the steering block into the project `CLAUDE.md` + detail doc |
| `unadopt` | — | Remove the steering block + detail doc |
| `serve` | — | Start the MCP JSON-RPC server on stdio (the default with no subcommand) |

Common options: `--json` (JSON output), `--compact` (compact output), `--limit N`, `--depth N`, `--file <path>`.

As of **v0.37.0** the CLI is [clap](https://docs.rs/clap)-based: **every subcommand has `--help`** for its full flag list (`code-graph-mcp <command> --help`), value flags accept both `--flag value` and `--flag=value`, and unknown flags or malformed arguments fail fast with a clear error and a non-zero exit code (`2`) instead of being silently ignored. For example, `trace` hides downstream middleware with `--no-middleware` (shown by default), and `snapshot` is a `create`/`inspect` subcommand pair.

## Plugin Skills

Installing the plugin ships two skills that Claude loads on its own when the
situation matches — there are no slash commands to remember:

| Skill | Loaded when | What it does |
|-------|-------------|--------------|
| `explore` | Starting work in unfamiliar code, or before editing a module | Routes the question to `overview` / `map` / `callgraph` / `search` / `impact` instead of reading files one at a time |
| `index` | Search returns empty or stale results, or after a large restructuring | Walks `health-check`, incremental re-index, and full rebuild |

Both are thin routers over the CLI subcommands documented above, so anything a
skill does is also runnable by hand.

## Supported Languages (19)

| Language | Extensions | Relations Extracted |
|----------|-----------|-------------------|
| TypeScript | .ts, .tsx | calls, imports, exports, inherits, implements, routes_to, references |
| JavaScript | .js, .jsx, .mjs, .cjs | calls, imports, exports (ESM + CommonJS), inherits, routes_to, references |
| Go | .go | calls, imports, inherits, routes_to, references |
| Python | .py, .pyi | calls, imports, inherits, routes_to, references |
| Rust | .rs | calls, imports, implements, routes_to (axum), references |
| Java | .java | calls, imports, inherits, implements, references |
| C# | .cs | calls, imports, inherits, implements |
| Kotlin | .kt, .kts | calls, imports, inherits |
| Ruby | .rb | calls, imports, inherits |
| PHP | .php | calls, imports, inherits, implements |
| Swift | .swift | calls, imports, inherits |
| Dart | .dart | calls, imports, inherits, implements |
| C | .c, .h | calls, imports, references |
| C++ | .cpp, .cc, .cxx, .hpp, .hh, .hxx | calls, imports, inherits, references |
| Bash | .sh, .bash | functions, commands, `source`/`.` imports |
| Markdown | .md, .mdx, .markdown | headings |
| HTML | .html, .htm | file-FTS only (no AST symbols) |
| CSS | .css | file-FTS only (no AST symbols) |
| JSON | .json | file-FTS only (no AST symbols) |

**Known limitations:**
- **Rust has no `inherits` edges** — the language has no class inheritance, so `impl Trait for Type` is recorded as `implements` and an `inherits`-filtered query returns empty for Rust.
- **Kotlin/Swift interface conformance** is recorded as `inherits` (both use a single `: Type` grammar for base classes and protocols/interfaces), so `implements`-filtered queries return empty for these two languages.
- **Cross-file dead-code detection** may false-positive a type whose only cross-file reference sits beyond the 4096-byte stored-content cap per node (documented accepted limitation, v0.97.1).

## Team-shared graph snapshot

Skip the full local index for team members and CI runners by publishing a
~3-5MB graph snapshot with each GitHub release.

**Setup (one-time):**
1. Copy `node_modules/@sdsrs/code-graph/claude-plugin/templates/code-graph-snapshot.yml`
   into your repo's `.github/workflows/`.
2. Push a release tag. The workflow uploads
   `code-graph-snapshot-<sha>.db.zst` as a release asset.

**Verify:**

```bash
npx -y -p @sdsrs/code-graph code-graph-mcp snapshot inspect ./code-graph-snapshot-<sha>.db.zst
```

After setup, the auto-fetch is **opt-in per consumer**: an untrusted repo could
otherwise seed a misleading graph, so an unconfigured clone prints a hint and
skips the install. Enable it with one of the trust signals below. These live in
the *environment* (never in `.code-graph.toml`) so a committed/PR-injected config
file cannot set them.

| Environment variable | Effect |
|----------------------|--------|
| `CODE_GRAPH_SNAPSHOT_TRUST_ORIGIN=1` | Trust the auto-detected GitHub-release snapshot for this repo's `origin` remote, allowing auto-install. |
| `CODE_GRAPH_SNAPSHOT_PIN=<blake3 hex>` | Pin the expected artifact digest (64-char blake3 hex). When set it is the **sole** integrity authority — the download must match it, no network sidecar is consulted — and it also implicitly trusts the origin path. |
| `CODE_GRAPH_SNAPSHOT_TRUST_URL=1` | Honor a `.code-graph.toml [snapshot] url` override (an arbitrary, non-origin URL). Off by default because a committed URL could redirect the graph to an attacker-chosen database. |

Integrity **fail-closes**: with no pin set and no fetchable `<url>.blake3` sidecar,
install is refused rather than accepting unverified content. Set a pin, or have
the publisher serve the `.blake3` sidecar alongside the snapshot. (`file://`
sources, used in tests, are exempt.)

## Offline / air-gapped usage

The optional embedding model (needed for vector semantic search) is downloaded
lazily on first use. To control that in a restricted environment:

| Environment variable | Effect |
|----------------------|--------|
| `CODE_GRAPH_MODEL_DIR=<dir>` | Load `model.safetensors` from `<dir>` instead of downloading (highest-priority lookup). If the file isn't there, it warns and falls back to the normal search paths. |
| `CODE_GRAPH_DISABLE_MODEL_DOWNLOAD=1` | Disable the automatic background model download. An already-cached model is still loaded and used; the server otherwise stays in FTS5-only mode. |

### Installing the model by hand

`CODE_GRAPH_MODEL_DIR` is the supported manual route. Hand-populating the
*default* cache dir (`<platform cache>/code-graph/models`) does **not** work:
that dir is only trusted once `extract_and_promote` has written a `.model-id`
marker, so a manually filled copy is treated as not-current and re-downloaded.

```bash
# Download + verify against the published checksum, then extract flat
# (the archive has no wrapping folder: model.safetensors, tokenizer.json, config.json).
curl -sL -o models.tar.gz \
  https://github.com/sdsrss/code-graph-mcp/releases/download/v<version>/models.tar.gz
curl -sL https://github.com/sdsrss/code-graph-mcp/releases/download/v<version>/models.tar.gz.sha256
sha256sum -c models.tar.gz.sha256
mkdir -p ~/.cache/code-graph/models-manual
tar -xzf models.tar.gz -C ~/.cache/code-graph/models-manual
export CODE_GRAPH_MODEL_DIR=~/.cache/code-graph/models-manual
```

On Windows use `%LOCALAPPDATA%\code-graph\models-manual`, and give `tar` a POSIX
path for `-C` (`/c/Users/<you>/...`) — `tar -C C:\...` fails with
"Cannot connect to C: resolve failed". The model is pinned by content hash, so
a manual install must be redone on each version bump.

### Diagnosing a model that never downloads

`code-graph-mcp doctor` reports the model state, distinguishing "model files
present but not loaded" (weights on disk — e.g. installed by the npm plugin —
just no MCP server session yet) from "no download has been attempted" and from
"download FAILED after N attempt(s): …" with the underlying error. The raw record is
`<platform cache>/code-graph/model-download.json`. On a network that performs
TLS inspection, the download retries automatically against the OS certificate
store after the bundled roots are rejected.

## Storage

Uses SQLite with:
- FTS5 for full-text search
- sqlite-vec extension for vector similarity search
- Merkle tree hashes for incremental change detection

Data is stored in `.code-graph/index.db` under the project root (auto-created, gitignored).

## Build from Source

### Prerequisites

- Rust 1.95.0 (2021 edition) — the toolchain CI and the release build pin
  (`dtolnay/rust-toolchain@1.95.0` in `.github/workflows/`). No older toolchain
  is tested: `Cargo.lock` is lockfile **version 4**, which Cargo 1.75 cannot
  read at all, and no `rust-version` floor is declared in `Cargo.toml`.
- A C compiler (for bundled SQLite / sqlite-vec)

### Build

```bash
# Default build — FTS5-only (~10 MB binary, no embedding model)
cargo build --release

# With local embedding model (~150 MB binary; downloads ~120 MB model lazily on first semantic search)
cargo build --release --features embed-model
```

> Direct `cargo install` users get the FTS5-only build by default. npm/npx/plugin
> users get the full hybrid (FTS5 + vector) build automatically — release CI
> compiles with `--features embed-model` for shipped binaries.

### Configure (from source)

Add the compiled binary to your MCP settings:

```json
{
  "mcpServers": {
    "code-graph": {
      "command": "/path/to/target/release/code-graph-mcp"
    }
  }
}
```

## Development

```bash
# Run tests
cargo test

# Run tests without embedding model
cargo test --no-default-features

# Check compilation
cargo check

# Run performance benchmarks (indexing, search, call graph)
cargo bench --no-default-features
```

## License

See [LICENSE](LICENSE) for details.
