#!/usr/bin/env node
'use strict';
const { execFileSync } = require('child_process');
const path = require('path');
const fs = require('fs');
const os = require('os');

const PLATFORM = os.platform();
const ARCH = os.arch();
const CACHE_FILE = path.join(os.homedir(), '.cache', 'code-graph', 'binary-path');
const BINARY_NAME = PLATFORM === 'win32' ? 'code-graph-mcp.exe' : 'code-graph-mcp';

function isNativeBinary(candidate) {
  if (!candidate) return false;
  try {
    if (!fs.existsSync(candidate)) return false;
    const realPath = fs.realpathSync(candidate);
    return path.basename(realPath) === BINARY_NAME;
  } catch {
    return false;
  }
}

/**
 * Locate the code-graph-mcp binary using multiple strategies.
 * Results are cached to disk so repeated calls (e.g. per-hook) are fast.
 *
 * Priority:
 *   cache (if valid) → dev-mode (target/release) → auto-update cache
 *   → platform npm pkg → bundled (bin/) → cargo install → PATH → npx cache
 *
 * Returns the absolute path or null if not found.
 */
function findBinary() {
  // Try disk cache first (avoids spawning `which` on hot paths)
  try {
    const cached = fs.readFileSync(CACHE_FILE, 'utf8').trim();
    if (isNativeBinary(cached)) return cached;
    if (cached) clearCache();
  } catch { /* no cache or stale */ }

  const result = findBinaryUncached();

  // Write cache for subsequent calls
  if (isNativeBinary(result)) {
    try {
      fs.mkdirSync(path.dirname(CACHE_FILE), { recursive: true });
      fs.writeFileSync(CACHE_FILE, result);
    } catch { /* ok */ }
  }

  return result;
}

/**
 * Detect if we're running from the source repo (e.g. npm link).
 * Checks relative to a given root directory for Cargo.toml.
 */
function isDevRepo(rootDir) {
  return fs.existsSync(path.join(rootDir, 'Cargo.toml'));
}

function findBinaryUncached() {
  // --- Dev mode: always prefer cargo build output when running from source repo ---
  // This covers: npm link, direct invocation from repo, CLAUDE_PROJECT_DIR set to repo
  const possibleRoots = new Set();

  // From plugin scripts context (claude-plugin/scripts/ → repo root is ../..)
  possibleRoots.add(path.resolve(__dirname, '..', '..'));
  // From bin/ context (cli.js sets FIND_BINARY_ROOT)
  if (process.env._FIND_BINARY_ROOT) {
    possibleRoots.add(path.resolve(process.env._FIND_BINARY_ROOT));
  }
  // From CLAUDE_PROJECT_DIR
  if (process.env.CLAUDE_PROJECT_DIR) {
    possibleRoots.add(path.resolve(process.env.CLAUDE_PROJECT_DIR));
  }

  for (const root of possibleRoots) {
    if (isDevRepo(root)) {
      const devBin = path.join(root, 'target', 'release', BINARY_NAME);
      if (isNativeBinary(devBin)) return devBin;
    }
  }

  // --- Auto-update cache (binary downloaded directly from GitHub release) ---
  const autoUpdateBin = path.join(os.homedir(), '.cache', 'code-graph', 'bin', BINARY_NAME);
  if (isNativeBinary(autoUpdateBin)) return autoUpdateBin;

  // --- Platform-specific npm package (@sdsrs/code-graph-{os}-{arch}) ---
  const platformPkg = `@sdsrs/code-graph-${PLATFORM}-${ARCH}`;
  try {
    const pkgPath = require.resolve(`${platformPkg}/package.json`);
    const bin = path.join(path.dirname(pkgPath), BINARY_NAME);
    if (isNativeBinary(bin)) return bin;
  } catch { /* not installed via npm */ }

  // --- Bundled binary (in same directory as cli.js or plugin scripts) ---
  // Check bin/ directory of the npm package
  const binDirs = new Set();
  if (process.env._FIND_BINARY_ROOT) {
    binDirs.add(path.join(process.env._FIND_BINARY_ROOT, 'bin'));
  }
  binDirs.add(path.resolve(__dirname, '..', '..', 'bin'));
  for (const dir of binDirs) {
    const bundled = path.join(dir, BINARY_NAME);
    if (isNativeBinary(bundled)) return bundled;
  }

  // --- Cargo install (~/.cargo/bin) ---
  const cargoBin = path.join(os.homedir(), '.cargo', 'bin', BINARY_NAME);
  if (isNativeBinary(cargoBin)) return cargoBin;

  // --- PATH lookup (last resort for intentionally installed binaries) ---
  try {
    const which = PLATFORM === 'win32' ? 'where' : 'which';
    const found = execFileSync(which, [BINARY_NAME], { stdio: ['pipe', 'pipe', 'pipe'] })
      .toString().trim().split('\n')[0];
    if (isNativeBinary(found)) return found;
  } catch { /* not in PATH */ }

  // --- npx cache (very last resort — may be outdated) ---
  const npxDir = path.join(os.homedir(), '.npm', '_npx');
  try {
    for (const entry of fs.readdirSync(npxDir)) {
      const platDir = path.join(npxDir, entry, 'node_modules', '@sdsrs', `code-graph-${PLATFORM}-${ARCH}`);
      const platBin = path.join(platDir, BINARY_NAME);
      if (isNativeBinary(platBin)) return platBin;
    }
  } catch { /* no npx cache */ }

  return null;
}

/**
 * Clear the disk cache. Call this after binary updates so the next
 * findBinary() picks up the new location.
 */
function clearCache() {
  try { fs.unlinkSync(CACHE_FILE); } catch { /* ok */ }
}

module.exports = { findBinary, findBinaryUncached, clearCache, CACHE_FILE, BINARY_NAME };

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
