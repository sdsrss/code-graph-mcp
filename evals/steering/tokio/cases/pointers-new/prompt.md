---
description: "Direct callers of Pointers.new in tokio/src: 9 in 7 files (rust-analyzer). `new` has 284 definitions in the index; code-graph's default floor finds 9 of the 9. One grader per caller, so the score is recall."
max_turns: 50
timeout_seconds: 900
allowed_tools: [Read, Glob, Grep, Bash, Agent]
tags: [tokio]
workspace: tokio
---

This repository is tokio 1.41.1, a Rust workspace. Which functions in the library code under `tokio/src/` call `Pointers::new`, the method defined at `tokio/src/util/linked_list.rs:423`? Count only direct calls to that definition. Leave out test code: `#[cfg(test)]` modules and anything under a `tests/` directory. End your reply with the complete list, one caller per line, formatted as `Type::method @ path/to/file.rs` (for a free function, `name @ path/to/file.rs`).
