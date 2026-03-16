#!/usr/bin/env node
'use strict';
const { execFileSync } = require('child_process');
const fs = require('fs');
const path = require('path');
const { findBinary } = require('./find-binary');
const lifecycle = require('./lifecycle');
const cleanupDisabledStatusline = lifecycle.cleanupDisabledStatusline || (() => ({ cleaned: false }));

const disabledCleanup = cleanupDisabledStatusline();
if (disabledCleanup.cleaned) process.exit(0);

// Only show status in projects that have a code-graph directory.
// The statusLine config is global, so we must exit silently for
// directories that aren't code-graph projects.
const cwd = process.cwd();
const codeGraphDir = path.join(cwd, '.code-graph');
if (!fs.existsSync(codeGraphDir)) {
  process.exit(0);
}

// Check for background indexing progress file first
const progressFile = path.join(codeGraphDir, 'indexing-status.json');
try {
  const raw = fs.readFileSync(progressFile, 'utf8');
  const p = JSON.parse(raw);
  if (p.s === 'indexing' && p.t > 0) {
    const pct = Math.round((p.d / p.t) * 100);
    process.stdout.write(`code-graph: \u21BB indexing ${p.d}/${p.t} (${pct}%)`);
    process.exit(0);
  }
} catch { /* no progress file or parse error — continue to health check */ }

// No indexing in progress — show normal health status
if (!fs.existsSync(path.join(codeGraphDir, 'index.db'))) {
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
