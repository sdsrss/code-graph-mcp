---
description: Force code-graph index rebuild. Use when search results seem stale or wrong, after major codebase restructuring, or when index health check reports issues.
---

Run via Bash: `code-graph-mcp incremental-index`
This updates the index incrementally (only changed files).
For a full rebuild, delete `.code-graph/` first, then run the MCP server.
