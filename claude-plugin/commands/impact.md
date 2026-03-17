---
description: Analyze impact scope before modifying a symbol
argument-hint: <symbol_name>
---

## Impact Analysis
!`code-graph-mcp impact $ARGUMENTS 2>/dev/null || echo "Symbol not found or no index. Run: code-graph-mcp incremental-index"`

Present the risk assessment and recommend whether it's safe to proceed.
