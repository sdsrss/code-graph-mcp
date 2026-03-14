#!/usr/bin/env node
'use strict';
const { execFileSync } = require('child_process');
const fs = require('fs');
const path = require('path');
const { findBinary } = require('./find-binary');

// Only show status in projects that have a code-graph index.
// The statusLine config is global, so we must exit silently for
// directories that aren't code-graph projects.
const cwd = process.cwd();
if (!fs.existsSync(path.join(cwd, '.code-graph', 'index.db'))) {
  process.exit(0);
}

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
