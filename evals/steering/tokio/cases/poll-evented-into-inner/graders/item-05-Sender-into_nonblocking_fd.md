---
type: regex
pattern: '(?m)^(?=[^\n]*(?:\bSender\b[^\n]*?(?:::|\.)into_nonblocking_fd\b|(?<![\w:.])into_nonblocking_fd\b))(?=[^\n]*unix/pipe\.rs)'
---

Sender.into_nonblocking_fd @ tokio/src/net/unix/pipe.rs:740 (code-graph: ambiguous)
