---
description: Deep dive into a module or file's architecture and relationships
argument-hint: <file_or_dir_path>
---

# Module Deep Dive

Understand what a module does, its public API, and how it connects to the rest of the codebase.

## Steps

1. Call `get_index_status` to verify the code graph index is current
2. Call `semantic_code_search` with the module name to find its key symbols
3. For each important export, call `get_call_graph` to map caller/callee relationships
4. Summarize:
   - **Purpose**: what this module does
   - **Public API**: exported functions/classes with signatures
   - **Internal structure**: key internal helpers
   - **Dependencies**: what it imports
   - **Dependents**: who imports it
   - **Hot paths**: most-called functions (by caller count)
