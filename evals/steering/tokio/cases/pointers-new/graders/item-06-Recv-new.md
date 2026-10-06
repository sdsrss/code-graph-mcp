---
type: regex
pattern: '(?m)^(?=[^\n]*(?:\bRecv\b[^\n]*?(?:::|\.)new\b|(?<![\w:.])new\b))(?=[^\n]*sync/broadcast\.rs)'
---

Recv.new @ tokio/src/sync/broadcast.rs:1367 (code-graph: inferred)
