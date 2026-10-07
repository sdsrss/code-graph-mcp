---
description: "Control: rust-analyzer finds no caller of TcpStream.set_ttl anywhere in the workspace. The only grader is the closing NONE line."
max_turns: 50
timeout_seconds: 900
allowed_tools: [Read, Glob, Grep, Bash, Agent]
tags: [tokio, control]
workspace: tokio
---

This repository is tokio 1.41.1, a Rust workspace. Which functions in the library code under `tokio/src/` call `TcpStream::set_ttl`, the method defined at `tokio/src/net/tcp/stream.rs:1246`? Count only direct calls to that definition. Leave out test code: `#[cfg(test)]` modules and anything under a `tests/` directory. End your reply with the complete list, one caller per line, formatted as `Type::method @ path/to/file.rs` (for a free function, `name @ path/to/file.rs`). If nothing calls it, end your reply with the single line `NONE` instead.
