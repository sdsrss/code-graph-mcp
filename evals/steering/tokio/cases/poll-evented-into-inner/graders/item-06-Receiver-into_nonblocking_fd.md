---
type: regex
pattern: '(?m)^(?=[^\n]*(?:\bReceiver\b[^\n]*?(?:::|\.)into_nonblocking_fd\b|(?<![\w:.])into_nonblocking_fd\b))(?=[^\n]*unix/pipe\.rs)'
---

Receiver.into_nonblocking_fd @ tokio/src/net/unix/pipe.rs:1333 (code-graph: ambiguous)
