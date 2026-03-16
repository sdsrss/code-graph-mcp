---
name: code-navigation
description: PROACTIVE code-graph tool selection. Triggers automatically when you need to explore, understand, trace, or modify code. Use BEFORE choosing Grep/Read for code understanding tasks — code-graph tools save 5-20x tokens. Especially critical before modifying functions (impact_analysis first).
---

# Code Navigation Rules

This project has a code-graph MCP server. The MCP instructions (injected at session start) are the authoritative decision table.

**Key principle: code-graph tools SUPERSEDE Grep/Agent for code understanding. Use compact=true for browsing, full when you need signatures or will edit.**

## Quick Reference

| Task | Tool | Savings |
|---|---|---|
| Architecture overview | `project_map(compact=true)` | 20x vs Read multiple |
| Who calls X / what X calls | `get_call_graph(symbol, compact=true)` | 13x vs Grep+Read |
| Understand module/directory | `module_overview(path, compact=true)` | 20x vs Read files |
| **Before modifying a function** | `impact_analysis(symbol)` FIRST | prevents breakage |
| Find code by concept | `semantic_code_search(query, compact=true)` | 10x vs Grep |
| Trace HTTP request flow | `trace_http_chain(route)` | 10x vs Read |
| Symbol signature+relations | `get_ast_node(node_id)` | 10x vs Read file |
| File dependencies | `dependency_graph(file)` | 5x vs Grep |

## Workflow Patterns

1. **Quick lookup**: `semantic_code_search(compact=true)` → `get_ast_node(node_id=N)`
2. **Before edit**: `impact_analysis(symbol)` → Edit
3. **Understand**: `project_map(compact=true)` → `module_overview(path, compact=true)` → `get_call_graph(symbol)`

## When to use native tools instead

- **Grep**: exact string match, constants, regex patterns, literal text search
- **Glob**: find files by name/path pattern
- **Read**: specific file you already know and need to edit
