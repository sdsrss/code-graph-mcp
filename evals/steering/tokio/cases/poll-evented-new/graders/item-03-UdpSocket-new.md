---
type: regex
pattern: '(?m)^(?=[^\n]*(?:\bUdpSocket\b[^\n]*?(?:::|\.)new\b|(?<![\w:.])new\b))(?=[^\n]*net/udp\.rs)'
---

UdpSocket.new @ tokio/src/net/udp.rs:173 (code-graph: inferred)
