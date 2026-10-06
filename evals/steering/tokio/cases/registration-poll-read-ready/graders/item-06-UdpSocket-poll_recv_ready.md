---
type: regex
pattern: '(?m)^(?=[^\n]*(?:\bUdpSocket\b[^\n]*?(?:::|\.)poll_recv_ready\b|(?<![\w:.])poll_recv_ready\b))(?=[^\n]*net/udp\.rs)'
---

UdpSocket.poll_recv_ready @ tokio/src/net/udp.rs:735 (code-graph: missed)
