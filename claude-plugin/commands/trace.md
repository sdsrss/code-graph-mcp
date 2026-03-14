---
description: Trace a full HTTP request flow from route to data layer
argument-hint: <route_path>
---

# HTTP Request Flow Tracing

Trace the complete execution path of an HTTP request.

## Steps

1. Call `trace_http_chain(route, depth=5)` to get the full chain
2. For each key node in the chain, call `get_ast_node` (by node_id, with `context_lines`) to show the implementation
3. Map the flow: route → middleware → validation → business logic → data access → response
4. Highlight any error handling, authentication checks, or database operations
