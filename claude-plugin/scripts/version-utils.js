'use strict';
const { execFileSync } = require('child_process');
const fs = require('fs');
const path = require('path');

const VERSION_OUTPUT_RE = /^code-graph-mcp\s+(\d+\.\d+\.\d+)$/;

function readBinaryVersion(binaryPath) {
  try {
    const out = execFileSync(binaryPath, ['--version'], {
      timeout: 2000,
      stdio: ['pipe', 'pipe', 'pipe'],
    }).toString().trim();
    const match = out.match(VERSION_OUTPUT_RE);
    return match ? match[1] : null;
  } catch {
    return null;
  }
}

function isDevMode() {
  // Always derive from __dirname — CLAUDE_PLUGIN_ROOT can leak from other plugins
  const pluginRoot = path.resolve(__dirname, '..');
  // Dev mode: running from source repo (has Cargo.toml nearby)
  if (fs.existsSync(path.join(pluginRoot, '..', 'Cargo.toml'))) return true;
  // Dev mode: plugin root is a symlink
  try { if (fs.lstatSync(pluginRoot).isSymbolicLink()) return true; } catch { /* ok */ }
  return false;
}

function getNewestMtime(dir, ext = '.rs') {
  let newest = 0;
  try {
    const entries = fs.readdirSync(dir, { withFileTypes: true });
    for (const entry of entries) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        const sub = getNewestMtime(full, ext);
        if (sub > newest) newest = sub;
      } else if (entry.name.endsWith(ext)) {
        const mt = fs.statSync(full).mtimeMs;
        if (mt > newest) newest = mt;
      }
    }
  } catch { /* dir doesn't exist or not readable */ }
  return newest;
}

module.exports = { readBinaryVersion, isDevMode, getNewestMtime, VERSION_OUTPUT_RE };
