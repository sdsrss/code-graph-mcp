---
type: regex
pattern: '(?m)^(?=[^\n]*(?:\bTcpStream\b[^\n]*?(?:::|\.)new\b|(?<![\w:.])new\b))(?=[^\n]*tcp/stream\.rs)'
---

TcpStream.new @ tokio/src/net/tcp/stream.rs:159 (code-graph: inferred)
