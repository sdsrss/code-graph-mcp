---
description: Trace call flow from a handler or route. Use when debugging API behavior, understanding request processing flow, or asked how an endpoint works.
argument-hint: <handler_or_route>
---

## Call Graph (callees)
!`code-graph-mcp callgraph $ARGUMENTS --direction callees --depth 4 2>/dev/null || echo "Symbol not found or no index."`

## Call Graph (callers)
!`code-graph-mcp callgraph $ARGUMENTS --direction callers --depth 2 2>/dev/null`

Map the flow and highlight error handling, auth checks, and data access points.
