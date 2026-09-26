---
description: "Which production files break when a function signature changes (tests excluded)."
max_turns: 40
timeout_seconds: 600
allowed_tools: [Read, Glob, Grep, Bash, Agent]
tags: [structural]
---

This repository is a Rust project under src/. I'm going to change the parameters of `classify_impact`. Which production (non-test) source files will I have to update besides the file that defines it? Give paths relative to the repository root.
