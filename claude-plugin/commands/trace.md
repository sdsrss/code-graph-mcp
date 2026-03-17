---
description: Trace call flow from a handler or route
argument-hint: <handler_or_route>
---

## Call Graph (callees)
!`code-graph-mcp callgraph $ARGUMENTS --direction callees --depth 4 2>/dev/null || echo "Symbol not found or no index."`

## Call Graph (callers)
!`code-graph-mcp callgraph $ARGUMENTS --direction callers --depth 2 2>/dev/null`

Map the flow and highlight error handling, auth checks, and data access points.
