# code-graph-mcp

A high-performance code knowledge graph server implementing the [Model Context Protocol (MCP)](https://modelcontextprotocol.io/). Indexes codebases into a structured AST knowledge graph with semantic search, call graph traversal, and HTTP route tracing — designed to give AI coding assistants deep, structured understanding of your code.

## Features

- **Multi-language parsing** — Tree-sitter AST extraction for 16 languages: TypeScript, JavaScript, Go, Python, Rust, Java, C, C++, C#, Kotlin, Ruby, PHP, Swift, Dart, HTML, CSS
- **Semantic code search** — Hybrid BM25 full-text + vector semantic search with Reciprocal Rank Fusion (RRF), powered by sqlite-vec
- **Call graph traversal** — Recursive CTE queries to trace callers/callees with cycle detection
- **HTTP route tracing** — Map route paths to backend handler functions (Express, Flask/FastAPI, Go, ASP.NET, Rails, Laravel, Vapor)
- **Dead code detection** — Find unreferenced symbols with smart Orphan/Exported-Unused classification
- **Impact analysis** — Determine the blast radius of code changes by tracing all dependents
- **Incremental indexing** — Merkle tree change detection with file system watcher for real-time updates. Smart event filtering skips metadata-only changes (chmod, xattr)
- **Context compression** — Token-aware snippet extraction for LLM context windows (L0→full code, L1→summaries, L2→file groups, L3→directory overview). Compact JSON output saves 15-20% tokens
- **Embedding model** — Optional local embedding via Candle (feature-gated `embed-model`). Context reordered to prioritize structural relations over code for better embedding quality
- **Self-healing** — Automatic SQLite corruption recovery with rebuild. Startup repair for incomplete indexing (Phase 3 failures)
- **MCP protocol** — JSON-RPC 2.0 over stdio, plug-and-play with Claude Code, Cursor, Windsurf, and other MCP clients
- **Claude Code Plugin** — First-class plugin with slash commands (`/understand`, `/trace`, `/impact`), agents, skills, auto-indexing hooks, StatusLine integration, and self-updating

## Why code-graph-mcp?

Unlike naive full-text search or simple AST dumps, code-graph-mcp builds a **structured knowledge graph** that understands the relationships between symbols across your entire codebase.

### Incremental by Design

BLAKE3 Merkle tree tracks every file's content hash. On re-index, only changed files are re-parsed — unchanged directory subtrees are skipped entirely via mtime cache. When a function signature changes, **dirty propagation** automatically regenerates context for all downstream callers across files.

### Hybrid Search, Not Just Grep

Combines BM25 full-text ranking (FTS5) with vector semantic similarity (sqlite-vec) via **Reciprocal Rank Fusion (RRF)** with raw score blending — so searching "handle user login" finds the right function even if it's named `authenticate_session`. Results are auto-compressed to fit LLM context windows.

### Scope-Aware Relation Extraction

The parser doesn't just find function calls — it tracks them within their proper scope context. Extracts calls, imports, inheritance, interface implementations, exports, and HTTP route bindings. Same-file targets are preferred over cross-file matches to minimize false-positive edges.

### HTTP Request Flow Tracing

Unique to code-graph-mcp: trace from `GET /api/users` → route handler → service layer → database call in a single query. Supports Express, Flask/FastAPI, and Go HTTP frameworks.

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
| Pre-change impact analysis | 10-20+ calls | 1 call (`impact_analysis`) | **~95%** |
| Module structure & exports | 5+ calls | 1 call (`module_overview`) | **~80%** |
| File dependency mapping | 3-5 calls | 1 call (`dependency_graph`) | **~75%** |
| Similar code detection | N/A | 1 call (`find_similar_code`) | **unique** |

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
├── mcp/          # MCP protocol layer (JSON-RPC, tool registry, server)
│   └── server/   # McpServer with IndexingState + CacheState sub-structs
├── parser/       # Tree-sitter parsing, relation extraction, LanguageConfig dispatch
├── indexer/      # 3-phase pipeline, Merkle tree, file watcher
├── storage/      # SQLite schema (v6), CRUD, FTS5, migrations
├── graph/        # Recursive CTE call graph queries
├── search/       # RRF fusion search combining BM25 + vector
├── embedding/    # Candle embedding model (optional, masked mean pooling)
├── sandbox/      # Context compressor with token estimation
└── utils/        # Language detection, config
```

## Installation

### Option 1: Claude Code Plugin (Recommended)

Install as a Claude Code plugin for the best experience — includes slash commands, agents, skills, auto-indexing hooks, StatusLine health display, and automatic updates:

```bash
# Step 1: Add the marketplace
/plugin marketplace add sdsrss/code-graph-mcp

# Step 2: Install the plugin
/plugin install code-graph-mcp
```

What you get:
- **MCP Server** — All code-graph tools available to Claude
- **Slash Commands** — `/understand <module>`, `/trace <route>`, `/impact <symbol>`
- **Code Explorer Agent** — Deep code understanding expert via `code-explorer`
- **Auto-indexing Hook** — Incremental index on every file edit (PostToolUse)
- **StatusLine** — Real-time health display (nodes, files, watch status) — compatible with other plugins' StatusLine via composite multiplexer
- **Auto-update** — Checks for new versions every 6h, updates silently

#### Manual Update

```bash
npm update -g @sdsrs/code-graph
```

Then reconnect the MCP server in Claude Code with `/mcp`.

> **Note:** Auto-update is disabled in the source repo directory (dev mode). Use manual update when developing the plugin itself.

#### Invited-memory mode (quieter prompts)

By default, every user prompt the plugin deems code-related gets a small context injection from `code-graph` CLI output. If you'd rather rely on MEMORY.md + explicit tool calls, opt into invited-memory mode:

1. Adopt the plugin contract into your project's memory index (idempotent, self-heals):
   ```bash
   code-graph-mcp adopt
   ```
   This writes `plugin_code_graph_mcp.md` (decision rules) into `~/.claude/projects/<slug>/memory/` and links it from `MEMORY.md` inside a sentinel block. Run `code-graph-mcp unadopt` to remove.
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

```bash
npm uninstall -g @sdsrs/code-graph
```

## MCP Tools

| Tool | Description |
|------|-------------|
| `project_map` | Full project architecture: modules, dependencies, entry points, hot functions |
| `semantic_code_search` | Hybrid BM25 + vector + graph search for AST nodes |
| `get_call_graph` | Trace upstream/downstream call chains for a function |
| `trace_http_chain` | Full request flow: route → handler → downstream call chain |
| `impact_analysis` | Analyze the blast radius of changing a symbol |
| `module_overview` | High-level overview of a module's structure and exports |
| `dependency_graph` | Visualize dependency relationships between modules. Supports `compact` mode |
| `find_similar_code` | Find semantically similar code via embeddings. Requires `symbol_name` or `node_id` |
| `get_ast_node` | Extract a specific code symbol with signature, body, and relations. Supports `compact` mode |
| `ast_search` | Search AST nodes by text and/or structural filters (type, return type, params) |
| `find_references` | Find all references to a symbol (callers, importers, inheritors). Supports `compact` mode |
| `find_dead_code` | Find unused code — orphan symbols and exported-but-unused public APIs |

## CLI Commands

All tools are also available as CLI subcommands for shell scripts, hooks, and terminal workflows:

| Command | MCP Equivalent | Description |
|---------|---------------|-------------|
| `search <query>` | `semantic_code_search` | FTS5 search by concept |
| `ast-search [query]` | `ast_search` | Structural search with `--type`/`--returns`/`--params` filters |
| `callgraph <symbol>` | `get_call_graph` | Show call graph (callers/callees) |
| `impact <symbol>` | `impact_analysis` | Impact analysis (callers, routes, risk level) |
| `show <symbol>` | `get_ast_node` | Show symbol details (code, type, signature) |
| `map` | `project_map` | Project architecture map |
| `overview <path>` | `module_overview` | Module symbols grouped by file and type |
| `deps <file>` | `dependency_graph` | File-level dependency graph |
| `trace <route>` | `trace_http_chain` | Trace HTTP route → handler → downstream calls |
| `similar <symbol>` | `find_similar_code` | Find semantically similar code (requires embeddings) |
| `refs <symbol>` | `find_references` | Find all references to a symbol |
| `dead-code [path]` | `find_dead_code` | Find unused code (orphans and exported-unused) |
| `grep <pattern>` | — | AST-context grep (ripgrep + containing function/class) |
| `incremental-index` | — | Run incremental index update (auto-creates DB if needed) |
| `health-check` | `get_index_status` | Query index status and freshness |
| `benchmark` | — | Benchmark index speed, query latency, token savings |

Common options: `--json` (JSON output), `--compact` (compact output), `--limit N`, `--depth N`, `--file <path>`.

## Plugin Slash Commands

Available when installed as a Claude Code plugin:

| Command | Description |
|---------|-------------|
| `/understand <module>` | Deep dive into a module or file's architecture and relationships |
| `/trace <route>` | Trace a full HTTP request flow from route to data layer |
| `/impact <symbol>` | Analyze the impact scope of changing a symbol before modifying it |
| `/status` | Show code-graph index status and embedding progress |
| `/rebuild` | Force a full code-graph index rebuild |

## Supported Languages (16)

| Language | Extensions | Relations Extracted |
|----------|-----------|-------------------|
| TypeScript | .ts, .tsx | calls, imports, exports, inherits, implements, routes_to |
| JavaScript | .js, .jsx, .mjs, .cjs | calls, imports, exports, inherits, routes_to |
| Go | .go | calls, imports, routes_to |
| Python | .py, .pyi | calls, imports, inherits, routes_to |
| Rust | .rs | calls, imports, inherits, implements |
| Java | .java | calls, imports, inherits, implements |
| C# | .cs | calls, imports, inherits, implements |
| Kotlin | .kt, .kts | calls, imports, inherits |
| Ruby | .rb | calls, imports, inherits |
| PHP | .php | calls, imports, inherits, implements |
| Swift | .swift | calls, imports, inherits |
| Dart | .dart | calls, imports, implements |
| C | .c, .h | calls, imports |
| C++ | .cpp, .cc, .cxx, .hpp | calls, imports, inherits |
| HTML | .html, .htm | structural parsing |
| CSS | .css | structural parsing |

## Storage

Uses SQLite with:
- FTS5 for full-text search
- sqlite-vec extension for vector similarity search
- Merkle tree hashes for incremental change detection

Data is stored in `.code-graph/index.db` under the project root (auto-created, gitignored).

## Build from Source

### Prerequisites

- Rust 1.75+ (2021 edition)
- A C compiler (for bundled SQLite / sqlite-vec)

### Build

```bash
# Default build (with local embedding model)
cargo build --release

# Without embedding model (lighter build)
cargo build --release --no-default-features
```

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
