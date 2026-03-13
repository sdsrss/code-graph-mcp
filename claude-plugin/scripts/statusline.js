#!/usr/bin/env node
'use strict';
const { execFileSync } = require('child_process');
const { findBinary } = require('./find-binary');

const bin = findBinary();
if (!bin) {
  process.stdout.write('code-graph: offline');
  process.exit(0);
}

try {
  const out = execFileSync(bin, ['health-check', '--format', 'json'], {
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
