---
type: regex
pattern: '(?m)^(?=[^\n]*(?:\bWaiter\b[^\n]*?(?:::|\.)new\b|(?<![\w:.])new\b))(?=[^\n]*sync/notify\.rs)'
---

Waiter.new @ tokio/src/sync/notify.rs:239 (code-graph: inferred)
