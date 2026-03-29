---
name: explore
description: |
  Understand code structure efficiently using the AST index. Use BEFORE reading
  files one by one — when starting work in unfamiliar code, exploring a module
  before changes, or finding the right file to edit. One overview call replaces
  5+ Read calls and saves significant context.
---

# Explore Code (indexed project)

Use these BEFORE reading individual files:

| Need | Command | Replaces |
|------|---------|----------|
| Module structure | `code-graph-mcp overview <dir>` | 5+ Read calls |
| Project architecture | `code-graph-mcp map --compact` | ls + README |
| Who calls / what calls | `code-graph-mcp callgraph <symbol>` | Grep + manual trace |
| Find by concept | `code-graph-mcp search "concept"` | 3+ Grep attempts |
| Impact before edit | `code-graph-mcp impact <symbol>` | Grep for callers |

**Workflow**: overview first → Read only the file you will edit.
