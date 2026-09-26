---
description: "Direct callers of a helper used across six files (grep-friendly: the name is unique)."
max_turns: 40
timeout_seconds: 600
allowed_tools: [Read, Glob, Grep, Bash, Agent]
tags: [structural]
---

This repository is a Rust project under src/. Which non-test functions call `ensure_owned_dir`? List each caller's function name and file.
