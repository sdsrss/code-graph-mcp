---
name: code-navigation
description: Code-graph tool selection guide. Use BEFORE any code exploration, bug fixing, feature implementation, or refactoring task to choose the most token-efficient tool (code-graph vs Grep/Read). Especially important before modifying functions — check impact_analysis first.
---

# Code Navigation with Code Graph

This project has a code-graph MCP server providing AST-level code intelligence.
These tools return structured, token-efficient results. Use them as your
PRIMARY navigation method for code understanding tasks.

| You want to... | Use this | NOT this |
|---|---|---|
| Find code by concept/meaning | `semantic_code_search` | Grep multiple patterns |
| Understand who calls what | `get_call_graph` | Grep function name + Read files |
| Trace HTTP request flow | `trace_http_chain` | Read router -> handler -> service |
| Find route handler | `find_http_route` | Grep route string |
| Get symbol signature + relations | `get_ast_node` | Read entire file |
| Read specific code after search | `read_snippet` (by node_id) | Read entire file |
| Analyze change impact | `impact_analysis` | Manual call graph tracing |
| Understand a module | `module_overview` | Read all files in directory |
| Map dependencies | `dependency_graph` | Grep import statements |
| Find similar code | `find_similar_code` | Grep partial function names |

## Still use native tools for
- Exact string match → Grep
- File path lookup → Glob
- Reading a specific known file → Read
- Creating/editing files → Write/Edit
