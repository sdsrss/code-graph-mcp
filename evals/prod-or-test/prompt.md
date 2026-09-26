---
description: "Is a function reached from production code or only from tests (two hops: impl <- wrapper <- try_install)."
max_turns: 40
timeout_seconds: 600
allowed_tools: [Read, Glob, Grep, Bash, Agent]
tags: [structural]
---

This repository is a Rust project under src/. Is `verify_checksum_impl` reachable from production code, or is it only exercised by tests? Show the call chain that justifies your answer.
