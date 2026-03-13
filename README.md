# code-graph-mcp

A high-performance code knowledge graph server implementing the [Model Context Protocol (MCP)](https://modelcontextprotocol.io/). Indexes codebases into a structured AST knowledge graph with semantic search, call graph traversal, and HTTP route tracing — designed to give AI coding assistants deep, structured understanding of your code.

## Features

- **Multi-language parsing** — Tree-sitter AST extraction for TypeScript, JavaScript, Go, Python, Rust, Java, C, C++, HTML, CSS
- **Semantic code search** — Hybrid BM25 full-text + vector semantic search with Reciprocal Rank Fusion (RRF), powered by sqlite-vec
- **Call graph traversal** — Recursive CTE queries to trace callers/callees with cycle detection
- **HTTP route tracing** — Map route paths to backend handler functions (Express, Flask/FastAPI, Go)
- **Impact analysis** — Determine the blast radius of code changes by tracing all dependents
- **Incremental indexing** — Merkle tree change detection with file system watcher for real-time updates
- **Context compression** — Token-aware snippet extraction for LLM context windows (L0→full code, L1→summaries, L2→file groups, L3→directory overview)
- **Embedding model** — Optional local embedding via Candle (feature-gated `embed-model`)
- **MCP protocol** — JSON-RPC 2.0 over stdio, plug-and-play with Claude Code, Cursor, Windsurf, and other MCP clients
- **Claude Code Plugin** — First-class plugin with slash commands (`/understand`, `/trace`, `/impact`), agents, skills, auto-indexing hooks, StatusLine integration, and self-updating

## Why code-graph-mcp?

Unlike naive full-text search or simple AST dumps, code-graph-mcp builds a **structured knowledge graph** that understands the relationships between symbols across your entire codebase.

### Incremental by Design

BLAKE3 Merkle tree tracks every file's content hash. On re-index, only changed files are re-parsed — unchanged directory subtrees are skipped entirely via mtime cache. When a function signature changes, **dirty propagation** automatically regenerates context for all downstream callers across files.

### Hybrid Search, Not Just Grep

Combines BM25 full-text ranking (FTS5) with vector semantic similarity (sqlite-vec) via **Reciprocal Rank Fusion (RRF)** — so searching "handle user login" finds the right function even if it's named `authenticate_session`. Results are auto-compressed to fit LLM context windows.

### Scope-Aware Relation Extraction

The parser doesn't just find function calls — it tracks them within their proper scope context. Extracts calls, imports, inheritance, interface implementations, exports, and HTTP route bindings. Same-file targets are preferred over cross-file matches to minimize false-positive edges.

### HTTP Request Flow Tracing

Unique to code-graph-mcp: trace from `GET /api/users` → route handler → service layer → database call in a single query. Supports Express, Flask/FastAPI, and Go HTTP frameworks.

### Zero External Dependencies at Runtime

Single binary, embedded SQLite, bundled sqlite-vec extension, optional local embedding model via Candle — no database server, no cloud API, no Docker required. Runs entirely on your machine.

### Built for AI Assistants

Every design decision — from token-aware compression to node_id-based snippet expansion — is optimized for LLM context windows. Works out of the box with Claude Code, Cursor, Windsurf, and any MCP-compatible client.

## Architecture

```
src/
├── mcp/          # MCP protocol layer (JSON-RPC, tool registry, server)
├── parser/       # Tree-sitter parsing, relation extraction, language support
├── indexer/      # 3-phase pipeline, Merkle tree, file watcher
├── storage/      # SQLite schema, CRUD, FTS5 full-text search
├── graph/        # Recursive CTE call graph queries
├── search/       # RRF fusion search combining BM25 + vector
├── embedding/    # Candle embedding model (optional)
├── sandbox/      # Context compressor with token estimation
└── utils/        # Language detection, config
```

## Installation

### Option 1: Claude Code Plugin (Recommended)

Install as a Claude Code plugin for the best experience — includes slash commands, agents, skills, auto-indexing hooks, StatusLine health display, and automatic updates:

```bash
claude plugin install --url https://raw.githubusercontent.com/sdsrss/code-graph-mcp/main/claude-plugin/marketplace.json
```

What you get:
- **MCP Server** — All code-graph tools available to Claude
- **Slash Commands** — `/understand <module>`, `/trace <route>`, `/impact <symbol>`
- **Code Explorer Agent** — Deep code understanding expert via `code-explorer`
- **Auto-indexing Hook** — Incremental index on every file edit (PostToolUse)
- **StatusLine** — Real-time health display (nodes, files, watch status) — compatible with other plugins' StatusLine via composite multiplexer
- **Auto-update** — Checks for new versions every 24h, updates silently

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
claude plugin uninstall code-graph
```

To also clean up all config and cache data:

```bash
node ~/.claude/plugins/cache/sdsrss/code-graph/*/scripts/lifecycle.js uninstall
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
| `semantic_code_search` | Hybrid BM25 + vector + graph search for AST nodes |
| `get_call_graph` | Trace upstream/downstream call chains for a function |
| `find_http_route` | Map route path to backend handler function |
| `trace_http_chain` | Full request flow: route → handler → downstream call chain |
| `impact_analysis` | Analyze the blast radius of changing a symbol |
| `module_overview` | High-level overview of a module's structure and exports |
| `dependency_graph` | Visualize dependency relationships between modules |
| `find_similar_code` | Find code snippets similar to a given pattern |
| `get_ast_node` | Extract a specific code symbol from a file |
| `read_snippet` | Read original code snippet by node ID with context |
| `start_watch` / `stop_watch` | Start/stop file system watcher for incremental indexing |
| `get_index_status` | Query index status and health |
| `rebuild_index` | Force full index rebuild |

## Plugin Slash Commands

Available when installed as a Claude Code plugin:

| Command | Description |
|---------|-------------|
| `/understand <module>` | Deep dive into a module or file's architecture and relationships |
| `/trace <route>` | Trace a full HTTP request flow from route to data layer |
| `/impact <symbol>` | Analyze the impact scope of changing a symbol before modifying it |

## Supported Languages

TypeScript, JavaScript, Go, Python, Rust, Java, C, C++, HTML, CSS

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
```

## License

See [LICENSE](LICENSE) for details.
