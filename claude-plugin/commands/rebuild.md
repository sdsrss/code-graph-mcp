---
description: Force a full code-graph index rebuild
---

Run via Bash: `code-graph-mcp incremental-index`
This updates the index incrementally (only changed files).
For a full rebuild, delete `.code-graph/` first, then run the MCP server.
