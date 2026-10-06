---
description: "Direct callers of PollEvented.new in tokio/src: 12 in 7 files (rust-analyzer). `new` has 284 definitions in the index; code-graph's default floor finds 12 of the 12. One grader per caller, so the score is recall."
max_turns: 50
timeout_seconds: 900
allowed_tools: [Read, Glob, Grep, Bash, Agent]
tags: [tokio]
workspace: tokio
---

This repository is tokio 1.41.1, a Rust workspace. Which functions in the library code under `tokio/src/` call `PollEvented::new`, the method defined at `tokio/src/io/poll_evented.rs:89`? Count only direct calls to that definition. Leave out test code: `#[cfg(test)]` modules and anything under a `tests/` directory. End your reply with the complete list, one caller per line, formatted as `Type::method @ path/to/file.rs` (for a free function, `name @ path/to/file.rs`).
