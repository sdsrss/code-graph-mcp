---
type: regex
pattern: '(?m)^(?=[^\n]*(?:\bTcpStream\b[^\n]*?(?:::|\.)poll_read_ready\b|(?<![\w:.])poll_read_ready\b))(?=[^\n]*tcp/stream\.rs)'
---

TcpStream.poll_read_ready @ tokio/src/net/tcp/stream.rs:546 (code-graph: missed)
