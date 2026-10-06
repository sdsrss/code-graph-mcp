---
type: regex
pattern: '(?m)^(?=[^\n]*(?:\bTcpListener\b[^\n]*?(?:::|\.)poll_accept\b|(?<![\w:.])poll_accept\b))(?=[^\n]*tcp/listener\.rs)'
---

TcpListener.poll_accept @ tokio/src/net/tcp/listener.rs:177 (code-graph: missed)
