---
description: "Direct callers of spawn_blocking in tokio/src: 6 in 3 files (rust-analyzer). `spawn_blocking` has 11 definitions in the index; code-graph's default floor finds 3 of the 6. One grader per caller, so the score is recall."
max_turns: 50
timeout_seconds: 900
allowed_tools: [Read, Glob, Grep, Bash, Agent]
tags: [tokio]
workspace: tokio
---

This repository is tokio 1.41.1, a Rust workspace. Which functions in the library code under `tokio/src/` call `spawn_blocking`, the function defined at `tokio/src/runtime/blocking/pool.rs:179`? Count only direct calls to that definition. Leave out test code: `#[cfg(test)]` modules and anything under a `tests/` directory. End your reply with the complete list, one caller per line, formatted as `Type::method @ path/to/file.rs` (for a free function, `name @ path/to/file.rs`).
