---
description: "Set answer that needs a per-function check: the 9 functions in src/storage/queries/ that no production code calls. One grader per function, plus one that fails a reply listing map_incoming_ref (passed as a function value to query_map, so it IS used)."
max_turns: 60
timeout_seconds: 900
allowed_tools: [Read, Glob, Grep, Bash, Agent]
tags: [structural, hard]
---

This repository is a Rust project under src/. Which functions defined in `src/storage/queries/` are never used by non-test code anywhere under src/ (called only from tests, or not at all)? Give one function name per line under the heading NEVER CALLED:, and put any notes after that list, separated by a blank line.
