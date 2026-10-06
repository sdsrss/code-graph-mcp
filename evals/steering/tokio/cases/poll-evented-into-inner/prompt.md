---
description: "Direct callers of PollEvented.into_inner in tokio/src: 9 in 8 files (rust-analyzer). `into_inner` has 48 definitions in the index; code-graph's default floor finds 0 of the 9. One grader per caller, so the score is recall."
max_turns: 50
timeout_seconds: 900
allowed_tools: [Read, Glob, Grep, Bash, Agent]
tags: [tokio]
workspace: tokio
---

This repository is tokio 1.41.1, a Rust workspace. Which functions in the library code under `tokio/src/` call `PollEvented::into_inner`, the method defined at `tokio/src/io/poll_evented.rs:135`? Count only direct calls to that definition. Leave out test code: `#[cfg(test)]` modules and anything under a `tests/` directory. End your reply with the complete list, one caller per line, formatted as `Type::method @ path/to/file.rs` (for a free function, `name @ path/to/file.rs`).
