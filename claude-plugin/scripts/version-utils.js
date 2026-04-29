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

function isDevMode(pluginRoot = path.resolve(__dirname, '..')) {
  // Explicit opt-in always wins (also lets users force dev mode in any layout)
  if (process.env.CODE_GRAPH_DEV === '1') return true;
  // Plugin root is a symlink (e.g. `npm link`)
  try { if (fs.lstatSync(pluginRoot).isSymbolicLink()) return true; } catch { /* ok */ }
  // Source repo: Cargo.toml AND target/ at parent. Marketplace installs ship
  // Cargo.toml (git-tracked) but NOT target/ (gitignored), so target/ is the
  // discriminator — without it, a marketplace clone was being misclassified as
  // dev mode and the launcher's GitHub-release fallback was unreachable
  // (see GitHub issue #12). If a dev hasn't built yet, they fall through to
  // the user-mode auto-install path, which still produces a working binary.
  const parent = path.dirname(pluginRoot);
  if (fs.existsSync(path.join(parent, 'Cargo.toml')) &&
      fs.existsSync(path.join(parent, 'target'))) {
    return true;
  }
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
