---
description: Deep dive into a module or file's architecture and relationships
argument-hint: <file_or_dir_path>
---

# Module Deep Dive

Understand what a module does, its public API, and how it connects to the rest of the codebase.

## Steps

1. Call `module_overview(path)` to get exports, hot paths, and file structure in one call
2. Call `dependency_graph(file_path)` for the main file to map imports and dependents
3. For key functions that need deeper understanding, call `get_call_graph` to trace call chains
4. Summarize:
   - **Purpose**: what this module does
   - **Public API**: exported functions/classes with signatures
   - **Internal structure**: key internal helpers
   - **Dependencies**: what it imports
   - **Dependents**: who imports it
   - **Hot paths**: most-called functions (by caller count)
