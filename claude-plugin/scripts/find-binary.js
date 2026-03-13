#!/usr/bin/env node
'use strict';
const { execFileSync } = require('child_process');
const path = require('path');
const fs = require('fs');
const os = require('os');

const PLATFORM = os.platform();
const CACHE_FILE = path.join(os.homedir(), '.cache', 'code-graph', 'binary-path');

/**
 * Locate the code-graph-mcp binary using multiple strategies.
 * Results are cached to disk so repeated calls (e.g. per-hook) are fast.
 * Priority: cache > PATH > local dev build > cargo install > npm platform pkg > npx cache
 * Returns the absolute path or null if not found.
 */
function findBinary() {
  // Try disk cache first (avoids spawning `which` on hot paths)
  try {
    const cached = fs.readFileSync(CACHE_FILE, 'utf8').trim();
    if (cached && fs.existsSync(cached)) return cached;
  } catch { /* no cache or stale */ }

  const result = findBinaryUncached();

  // Write cache for subsequent calls
  if (result) {
    try {
      fs.mkdirSync(path.dirname(CACHE_FILE), { recursive: true });
      fs.writeFileSync(CACHE_FILE, result);
    } catch { /* ok */ }
  }

  return result;
}

function findBinaryUncached() {
  const name = PLATFORM === 'win32' ? 'code-graph-mcp.exe' : 'code-graph-mcp';

  // 1. PATH lookup (user has intentionally installed it)
  try {
    const which = PLATFORM === 'win32' ? 'where' : 'which';
    const found = execFileSync(which, [name], { stdio: ['pipe', 'pipe', 'pipe'] })
      .toString().trim().split('\n')[0];
    if (found && fs.existsSync(found)) return found;
  } catch { /* not in PATH */ }

  // 2. Local dev build (target/release in project directory)
  const projectRoot = process.env.CLAUDE_PROJECT_DIR || process.cwd();
  const devBin = path.join(projectRoot, 'target', 'release', name);
  if (fs.existsSync(devBin)) return devBin;

  // 3. Cargo install (~/.cargo/bin)
  const cargoBin = path.join(os.homedir(), '.cargo', 'bin', name);
  if (fs.existsSync(cargoBin)) return cargoBin;

  // 4. npm platform package (installed via @sdsrs/code-graph)
  const platformPkg = `@sdsrs/code-graph-${PLATFORM}-${os.arch()}`;
  try {
    const pkgPath = require.resolve(`${platformPkg}/package.json`);
    const bin = path.join(path.dirname(pkgPath), name);
    if (fs.existsSync(bin)) return bin;
  } catch { /* not installed via npm */ }

  // 5. npx cache (last resort — may be outdated)
  const npxDir = path.join(os.homedir(), '.npm', '_npx');
  try {
    for (const entry of fs.readdirSync(npxDir)) {
      const pkgJsonPath = path.join(npxDir, entry, 'node_modules', '@sdsrs', 'code-graph', 'package.json');
      if (!fs.existsSync(pkgJsonPath)) continue;
      const platDir = path.join(npxDir, entry, 'node_modules', '@sdsrs', `code-graph-${PLATFORM}-${os.arch()}`);
      const platBin = path.join(platDir, name);
      if (fs.existsSync(platBin)) return platBin;
    }
  } catch { /* no npx cache */ }

  return null;
}

module.exports = { findBinary };

// Allow direct invocation for testing
if (require.main === module) {
  const bin = findBinary();
  if (bin) {
    process.stdout.write(bin);
  } else {
    process.stderr.write('code-graph-mcp binary not found\n');
    process.exit(1);
  }
}
