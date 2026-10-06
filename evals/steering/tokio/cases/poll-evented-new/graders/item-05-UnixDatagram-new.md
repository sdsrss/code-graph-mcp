---
type: regex
pattern: '(?m)^(?=[^\n]*(?:\bUnixDatagram\b[^\n]*?(?:::|\.)new\b|(?<![\w:.])new\b))(?=[^\n]*datagram/socket\.rs)'
---

UnixDatagram.new @ tokio/src/net/unix/datagram/socket.rs:515 (code-graph: inferred)
