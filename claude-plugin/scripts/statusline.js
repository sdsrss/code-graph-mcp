#!/usr/bin/env node
const { execSync } = require('child_process');
try {
  const out = execSync('code-graph-mcp health-check --format json', {
    timeout: 3000,
    stdio: ['pipe', 'pipe', 'pipe']
  }).toString().trim();
  const s = JSON.parse(out);
  const icon = s.healthy ? '\u2713' : '\u2717';
  process.stdout.write(
    `code-graph: ${icon} ${s.nodes} nodes | ${s.files} files` +
    (s.watching ? ' | watching' : '')
  );
} catch {
  process.stdout.write('code-graph: offline');
}
