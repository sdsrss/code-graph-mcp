---
name: index
description: |
  Diagnose and fix code-graph index issues. Use when: search returns unexpected/empty
  results, or after major codebase restructuring. Covers the health check and both
  rebuild paths from the CLI.
---

# Index Maintenance

## Check health
```bash
code-graph-mcp health-check
```

## Rebuild (incremental — only changed files)
```bash
code-graph-mcp incremental-index
```

## Full rebuild (when incremental isn't enough)
```bash
code-graph-mcp rebuild-index --confirm
```

This builds the new index in a temporary location and swaps it in with an atomic
rename, so a running MCP server (and its open WAL) never observes a half-built
index. Deleting the `.code-graph/` directory by hand skips that swap, which is
the exact situation the atomic path exists for.

`get_index_status` and `rebuild_index` do dispatch over JSON-RPC, but they are
deliberately kept out of `tools/list` to save tokens (`src/mcp/tools.rs`), so
they are not in your callable tool set — the CLI above is the surface you have.
It is also the only one that works with no server running.
