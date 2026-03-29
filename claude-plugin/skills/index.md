---
name: index
description: |
  Diagnose and fix code-graph index issues. Use when: search returns unexpected/empty
  results, or after major codebase restructuring. These management commands are NOT
  exposed via MCP tools — this skill is the only way to access them.
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
rm -rf .code-graph/ && code-graph-mcp incremental-index
```
