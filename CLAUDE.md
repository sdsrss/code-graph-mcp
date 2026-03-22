# code-graph-mcp

## Project Overview

Rust MCP server that indexes codebases into an AST knowledge graph with semantic search. Communicates via JSON-RPC 2.0 over stdio.

## Tech Stack

- **Language**: Rust 2021 edition
- **Parser**: Tree-sitter (16 languages: TS, JS, Go, Python, Rust, Java, C, C++, C#, Kotlin, Ruby, PHP, Swift, Dart, HTML, CSS)
- **Storage**: SQLite (rusqlite with bundled-full) + FTS5 + sqlite-vec (bundled C extension via build.rs)
- **Embedding**: Candle (optional, feature-gated `embed-model`)
- **File watching**: notify crate
- **Hashing**: blake3 for Merkle tree change detection

## Module Layout

```
src/
├── domain.rs     # Shared constants (relation types, limits, dimensions) — canonical source
├── mcp/          # JSON-RPC protocol, tool registry, server (stdio entry point)
├── parser/       # Tree-sitter AST parsing, relation extraction, language dispatch
├── indexer/      # 3-phase pipeline (parse → extract → embed), Merkle tree, file watcher
├── storage/      # SQLite schema init, CRUD operations, parameterized queries
├── graph/        # Recursive CTE call graph queries (callers/callees)
├── search/       # RRF fusion (BM25 + vector similarity)
├── embedding/    # EmbeddingModel struct, context builder
├── sandbox/      # Context compressor with token estimation
└── utils/        # Language detection from file extension, config
```

## Key Commands

```bash
cargo check                        # Type check
cargo build --release              # Full build with embedding
cargo build --no-default-features  # Build without embedding model
cargo test                         # Run all tests
cargo test --no-default-features   # Tests without embedding
```

## Important Patterns

- **Feature gating**: `embed-model` feature controls Candle dependencies; code using embeddings must be behind `#[cfg(feature = "embed-model")]`
- **Database**: SQLite with sqlite-vec compiled from `vendor/sqlite-vec/sqlite-vec.c` via `build.rs`
- **Relation constants**: Defined in `src/domain.rs` (re-exported from `storage/schema.rs`) — use constants (e.g., `REL_CALLS`) instead of hardcoded strings
- **Schema**: Defined in `src/storage/schema.rs` — parameterized queries in `src/storage/queries.rs`
- **MCP tools**: Registered in `src/mcp/tools.rs`, handled in `src/mcp/server.rs`
- **Data directory**: `.code-graph/` under project root, auto-created and gitignored

## Conventions

- Commit format: `<type>(<scope>): <subject>` (e.g., `feat(parser): add relation extraction`)
- Error handling: `anyhow::Result` throughout, tracing for logging to stderr
- Tests: Unit tests in modules, integration tests in `tests/integration.rs`

## Code Graph Integration

Code graph tools are available via MCP. The MCP server injects `instructions` at session start to guide tool selection. Use the `code-navigation` skill for the full decision tree.


<claude-mem-context>
### Last Session
Request: 按建议修复优先级开始修复
Completed: Error: cli_e2e.rs: Compiling code-graph-mcp v0.7.4 (/mnt/data_ssd/de…; Modified cli.rs, domain.rs

### Key Context
- [bugfix] Error: cli_e2e.rs: Compiling code-graph-mcp v0.7.4 (/mnt/data_ssd/de… (#4912)
- [bugfix] Error: cli.rs: Compiling code-graph-mcp v0.7.4 (/mnt/data_ssd/de… (#4905)
- [bugfix] Error: cli.rs: Compiling code-graph-mcp v0.7.4 (/mnt/data_ssd/de… (#4872)
- [bugfix] Error: session-init.test.js, session-init.test.js: TAP version 13 # Subtest: sy… (#4846)
- [bugfix] Error: mod.rs: diff --git a/src/indexer/pipeline.rs b/src/indexe… (#4839)

</claude-mem-context>
