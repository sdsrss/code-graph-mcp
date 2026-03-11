# code-graph-mcp

A high-performance code knowledge graph server implementing the [Model Context Protocol (MCP)](https://modelcontextprotocol.io/). Indexes codebases into a structured AST knowledge graph with semantic search, call graph traversal, and HTTP route tracing — designed to give AI coding assistants deep, structured understanding of your code.

## Features

- **Multi-language parsing** — Tree-sitter based AST extraction for TypeScript, JavaScript, Go, Python, Rust, Java, C, C++, HTML, CSS
- **Semantic code search** — Hybrid BM25 full-text + vector semantic search with Reciprocal Rank Fusion (RRF), powered by sqlite-vec
- **Call graph traversal** — Recursive CTE queries to trace callers/callees with cycle detection
- **HTTP route tracing** — Map route paths to backend handler functions (Express, Flask/FastAPI, Go)
- **Incremental indexing** — Merkle tree change detection with file system watcher for real-time updates
- **Context compression** — Token-aware snippet extraction for LLM context windows
- **Embedding model** — Optional local embedding via Candle (feature-gated `embed-model`)
- **MCP protocol** — JSON-RPC 2.0 over stdio, plug-and-play with Claude Code, Cursor, and other MCP clients

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

### Option 1: Claude Code (Recommended)

One-command setup — registers as an MCP server directly in Claude Code:

```bash
claude mcp add code-graph-mcp npx @sdsrs/code-graph
```

### Option 2: npx (No Install)

Run directly without installing:

```bash
npx @sdsrs/code-graph
```

### Option 3: npm (Global Install)

Install globally, then run anywhere:

```bash
npm install -g @sdsrs/code-graph
code-graph-mcp
```

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

### Configure with Claude Code (Manual)

If you built from source, add to your MCP settings:

```json
{
  "mcpServers": {
    "code-graph": {
      "command": "/path/to/code-graph-mcp",
      "cwd": "/path/to/your/project"
    }
  }
}
```

## MCP Tools

| Tool | Description |
|------|-------------|
| `semantic_code_search` | Hybrid BM25 + vector + graph search for AST nodes |
| `get_call_graph` | Trace upstream/downstream call chains for a function |
| `find_http_route` | Map route path to backend handler function |
| `get_ast_node` | Extract a specific code symbol from a file |
| `read_snippet` | Read original code snippet by node ID with context |
| `start_watch` / `stop_watch` | Start/stop file system watcher for incremental indexing |
| `get_index_status` | Query index status and health |
| `rebuild_index` | Force full index rebuild |

## Supported Languages

TypeScript, JavaScript, Go, Python, Rust, Java, C, C++, HTML, CSS

## Storage

Uses SQLite with:
- FTS5 for full-text search
- sqlite-vec extension for vector similarity search
- Merkle tree hashes for incremental change detection

Data is stored in `.code-graph/index.db` under the project root (auto-created, gitignored).

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
