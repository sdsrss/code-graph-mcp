---
description: Analyze the impact scope of changing a symbol before modifying it
argument-hint: <symbol_name>
---

# Impact Analysis

Analyze what will be affected if you change the given symbol.

## Steps

1. Call `get_call_graph(symbol, "callers", depth=3)` to find all upstream callers
2. Call `get_call_graph(symbol, "callees", depth=2)` to find downstream dependencies
3. Check if any callers are HTTP route handlers (look for route-related nodes)
4. Summarize findings:
   - **Affected files**: list all unique files containing callers/callees
   - **Affected routes**: list any HTTP routes that flow through this symbol
   - **Risk level**: LOW (≤3 callers, no routes), MEDIUM (4-10 callers OR 1-2 routes), HIGH (>10 callers OR ≥3 routes)
5. Present the analysis before making any modifications
