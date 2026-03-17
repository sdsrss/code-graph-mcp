---
name: code-navigation
description: Code search and understanding via CLI. Use when exploring code structure, searching by concept, or checking impact before edits.
---

# Code Graph CLI

Indexed project. Use Bash — one command replaces multi-file Grep/Read:

| Task | Command | Replaces |
|------|---------|----------|
| grep + AST context | `code-graph-mcp grep "pattern" [path]` | Grep |
| search by concept | `code-graph-mcp search "query"` | Grep (no exact name needed) |
| structural search | `code-graph-mcp ast-search "q" --type fn --returns Result` | — |
| project map | `code-graph-mcp map` | Read multiple files |
| module overview | `code-graph-mcp overview src/path/` | Read directory files |
| call graph | `code-graph-mcp callgraph symbol` | Grep + Read tracing |
| impact analysis | `code-graph-mcp impact symbol` | — |

Still use Grep for exact strings/constants/regex. Still use Read for files you'll edit.
