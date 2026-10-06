---
type: regex
pattern: '(?m)^(?=[^\n]*(?:\bUnixDatagram\b[^\n]*?(?:::|\.)poll_recv_ready\b|(?<![\w:.])poll_recv_ready\b))(?=[^\n]*datagram/socket\.rs)'
---

UnixDatagram.poll_recv_ready @ tokio/src/net/unix/datagram/socket.rs:366 (code-graph: missed)
