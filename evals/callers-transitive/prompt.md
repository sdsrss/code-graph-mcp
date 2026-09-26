---
description: "Multi-hop callers up to the entry point; the index misses the two path-qualified calls in main.rs, so a correct answer needs them from somewhere else."
max_turns: 40
timeout_seconds: 600
allowed_tools: [Read, Glob, Grep, Bash, Agent]
tags: [structural]
---

This repository is a Rust project under src/. List every non-test function that can reach `ensure_code_graph_dir_ignored` through any chain of calls, all the way up to the program's entry points. Name each function.
