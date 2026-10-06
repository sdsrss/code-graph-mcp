---
type: regex
pattern: '(?m)^(?=[^\n]*(?:\bTcpStream\b[^\n]*?(?:::|\.)poll_peek\b|(?<![\w:.])poll_peek\b))(?=[^\n]*tcp/stream\.rs)'
---

TcpStream.poll_peek @ tokio/src/net/tcp/stream.rs:358 (code-graph: missed)
