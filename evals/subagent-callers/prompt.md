---
description: "Same question shape as callers-direct, but delegated to a subagent: the baseline for suggestion #3(a) (Explore skips CLAUDE.md)."
max_turns: 40
timeout_seconds: 600
allowed_tools: [Read, Glob, Grep, Bash, Agent]
tags: [structural]
---

This repository is a Rust project under src/. Use an Explore subagent to find which non-test functions call `split_identifier`, then report the callers with their files.
