---
description: Deep dive into a module's architecture. Use when starting work in an unfamiliar area, asked to explain how code works, or before implementing changes in a module.
argument-hint: <file_or_dir_path>
---

## Module Overview
!`code-graph-mcp overview $ARGUMENTS 2>/dev/null || echo "No index or no symbols found. Run: code-graph-mcp incremental-index"`

## Call Graph (top symbols)
!`code-graph-mcp search "$ARGUMENTS" --limit 5 2>/dev/null`

Analyze the above and summarize: purpose, public API, key internal helpers, and hot paths.
