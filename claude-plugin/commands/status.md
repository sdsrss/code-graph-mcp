---
description: Show code-graph index health and coverage. Use when search returns unexpected results, checking if index is current, or diagnosing code-graph issues.
---

!`code-graph-mcp health-check --format json 2>/dev/null || echo '{"error":"No index found"}'`
