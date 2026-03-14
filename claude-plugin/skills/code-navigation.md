---
name: code-navigation
description: PROACTIVE code-graph tool selection. Triggers automatically when you need to explore, understand, trace, or modify code. Use BEFORE choosing Grep/Read for code understanding tasks — code-graph tools save 10-40x tokens. Especially critical before modifying functions (impact_analysis first).
---

# Code Navigation Rules

This project has a code-graph MCP server. These tools return structured, token-efficient results.
**Use them as your PRIMARY navigation method. Fall back to Grep/Read only for exact-match or file editing.**

## Decision Rules

| Task | MUST use | NOT this | Token savings |
|---|---|---|---|
| Who calls X / what X calls | `get_call_graph` | Grep + Read ×5 | 13x |
| Understand module/directory | `module_overview` | Read multiple files | 20x |
| **Before modifying a function** | `impact_analysis` FIRST | Just Edit | prevents breakage |
| Find code by concept/meaning | `semantic_code_search` | Grep multiple patterns | 10x |
| Trace HTTP request flow | `trace_http_chain` | Read router→handler→service | 10x |
| One symbol's signature+relations | `get_ast_node` | Read entire file | 10x |
| File import/export dependencies | `dependency_graph` | Grep import statements | 5x |
| Find similar/duplicate code | `find_similar_code` | Grep partial names | 5x |
| Read code of a search result | `read_snippet` (by node_id) | Read entire file | 3x |

## When to use native tools instead

- **Grep**: exact string match, constants, regex patterns, literal text search
- **Glob**: find files by name/path pattern
- **Read**: specific file you already know and need to edit
- **Write/Edit**: creating or modifying files
