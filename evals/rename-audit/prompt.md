---
description: "Every file that references a struct, for a rename (17 files; grep -lw alone gets it)."
max_turns: 40
timeout_seconds: 600
allowed_tools: [Read, Glob, Grep, Bash, Agent]
tags: [structural]
---

This repository is a Rust project under src/. I want to rename the struct `FileRecord`. List every file that references it, as a complete list of paths relative to the repository root.
