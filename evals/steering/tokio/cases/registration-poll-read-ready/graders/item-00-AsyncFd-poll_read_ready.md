---
type: regex
pattern: '(?m)^(?=[^\n]*(?:\bAsyncFd\b[^\n]*?(?:::|\.)poll_read_ready\b|(?<![\w:.])poll_read_ready\b))(?=[^\n]*io/async_fd\.rs)'
---

AsyncFd.poll_read_ready @ tokio/src/io/async_fd.rs:375 (code-graph: missed)
