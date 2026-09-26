---
description: "Wide multi-hop closure: 19 production functions up to 7 hops above the target (18 graded — `main` is too common a word to grade), two of them only through path-qualified calls in main.rs. One grader per function, so the score is recall."
max_turns: 60
timeout_seconds: 900
allowed_tools: [Read, Glob, Grep, Bash, Agent]
tags: [structural, hard]
---

This repository is a Rust project under src/. I am about to change `count_suppressed_seed_edges`. List every non-test function that can reach it through any chain of calls, all the way up to the program's entry points. Give one function name per line under the heading CALLERS:.
