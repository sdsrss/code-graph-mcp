#!/usr/bin/env node
'use strict';
/**
 * MCP server launcher — resolves binary via find-binary.js, auto-installs
 * if missing, then spawns with stdio forwarding for JSON-RPC.
 *
 * Used by .mcp.json so the plugin controls binary discovery instead of
 * relying on the binary being in PATH.
 */
const { spawn, execFileSync } = require('child_process');
const path = require('path');
const fs = require('fs');

// Set plugin root so find-binary.js can locate bundled/dev binaries
process.env._FIND_BINARY_ROOT = process.env.CLAUDE_PLUGIN_ROOT || path.resolve(__dirname, '..');

const { findBinary, clearCache } = require('./find-binary');

let binary = findBinary();

// Auto-install binary if not found (first-time install)
if (!binary) {
  let version = 'latest';
  try {
    const pj = path.join(__dirname, '..', '.claude-plugin', 'plugin.json');
    version = JSON.parse(fs.readFileSync(pj, 'utf8')).version || 'latest';
  } catch { /* use latest */ }

  process.stderr.write(`[code-graph] Binary not found, installing @sdsrs/code-graph@${version}...\n`);
  try {
    execFileSync('npm', ['install', '-g', `@sdsrs/code-graph@${version}`], {
      timeout: 60000, stdio: 'pipe',
    });
    clearCache();
    binary = findBinary();
    if (binary) {
      process.stderr.write(`[code-graph] Installed at ${binary}\n`);
    }
  } catch {
    process.stderr.write('[code-graph] npm install failed. Trying direct download...\n');
  }

  // Fallback: direct binary download from GitHub release
  if (!binary) {
    try {
      const { downloadBinarySync } = require('./auto-update');
      if (typeof downloadBinarySync === 'function') {
        downloadBinarySync(version);
        clearCache();
        binary = findBinary();
      }
    } catch { /* not available */ }
  }
}

if (!binary) {
  process.stderr.write(
    '[code-graph] Binary not found. Install manually:\n' +
    '  npm install -g @sdsrs/code-graph\n'
  );
  process.exit(1);
}

// Spawn binary with stdio inheritance for MCP JSON-RPC
const child = spawn(binary, ['serve'], {
  stdio: 'inherit',
  env: process.env,
});

child.on('error', (err) => {
  process.stderr.write(`[code-graph] Failed to start: ${err.message}\n`);
  process.exit(1);
});

child.on('exit', (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
  } else {
    process.exit(code ?? 1);
  }
});
