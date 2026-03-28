---
description: Analyze blast radius before modifying a symbol. Use when about to edit/rename/remove a function, or asked about change risk and affected callers.
argument-hint: <symbol_name>
---

## Impact Analysis
!`code-graph-mcp impact $ARGUMENTS 2>/dev/null || echo "Symbol not found or no index. Run: code-graph-mcp incremental-index"`

Present the risk assessment and recommend whether it's safe to proceed.
