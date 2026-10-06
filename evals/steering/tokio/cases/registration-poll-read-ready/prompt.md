---
description: "Direct callers of Registration.poll_read_ready in tokio/src: 10 in 8 files (rust-analyzer). `poll_read_ready` has 10 definitions in the index; code-graph's default floor finds 0 of the 10. One grader per caller, so the score is recall."
max_turns: 50
timeout_seconds: 900
allowed_tools: [Read, Glob, Grep, Bash, Agent]
tags: [tokio]
workspace: tokio
---

This repository is tokio 1.41.1, a Rust workspace. Which functions in the library code under `tokio/src/` call `Registration::poll_read_ready`, the method defined at `tokio/src/runtime/io/registration.rs:109`? Count only direct calls to that definition. Leave out test code: `#[cfg(test)]` modules and anything under a `tests/` directory. End your reply with the complete list, one caller per line, formatted as `Type::method @ path/to/file.rs` (for a free function, `name @ path/to/file.rs`).
