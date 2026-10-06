---
type: regex
pattern: '(?m)^(?=[^\n]*(?:\bRecv\b[^\n]*?(?:::|\.)drop\b|(?<![\w:.])drop\b))(?=[^\n]*sync/broadcast\.rs)'
---

Recv.drop @ tokio/src/sync/broadcast.rs:1415 (code-graph: missed)
