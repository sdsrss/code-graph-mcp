#!/usr/bin/env node
'use strict';
const { execFileSync } = require('child_process');
const path = require('path');
const fs = require('fs');
const os = require('os');
const { readBinaryVersion } = require('./version-utils');

const PLATFORM = os.platform();
const ARCH = os.arch();
const CACHE_FILE = path.join(os.homedir(), '.cache', 'code-graph', 'binary-path');
const BINARY_NAME = PLATFORM === 'win32' ? 'code-graph-mcp.exe' : 'code-graph-mcp';
const PLATFORM_PKG = `@sdsrs/code-graph-${PLATFORM}-${ARCH}`;

/** Read the npm pkg version from this script's package.json (claude-plugin/../package.json). */
function getPackageVersion() {
  try { return require('../../package.json').version; }
  catch { return null; }
}

/** Compare semver-ish "M.m.p" strings; returns -1, 0, or 1. Non-numeric parts → 0. */
function compareVersions(a, b) {
  const pa = String(a).split('.').map(s => parseInt(s, 10));
  const pb = String(b).split('.').map(s => parseInt(s, 10));
  for (let i = 0; i < 3; i++) {
    const x = Number.isFinite(pa[i]) ? pa[i] : 0;
    const y = Number.isFinite(pb[i]) ? pb[i] : 0;
    if (x !== y) return x < y ? -1 : 1;
  }
  return 0;
}

/**
 * Candidate paths for npm global `node_modules`.
 *
 * `require.resolve` only searches `node_modules` walking up from the requiring
 * file — it does NOT search global installations (no NODE_PATH set in default
 * Node setups, including nvm). When a user runs `npm install -g`, the platform
 * package lands somewhere we have to discover ourselves.
 */
function globalNodeModulesCandidates() {
  const out = [];
  const nodeBinDir = path.dirname(process.execPath);

  // 1. Derive from process.execPath. Works for nvm + standard Unix prefixes
  //    (`<prefix>/bin/node` → globals at `<prefix>/lib/node_modules`); on
  //    Windows globals sit next to `node.exe`.
  if (PLATFORM === 'win32') {
    out.push(path.join(nodeBinDir, 'node_modules'));
  } else {
    out.push(path.resolve(nodeBinDir, '..', 'lib', 'node_modules'));
  }

  // 2. NPM_CONFIG_PREFIX env override (set by users using `~/.npm-global` etc.)
  const envPrefix = process.env.NPM_CONFIG_PREFIX || process.env.npm_config_prefix;
  if (envPrefix) {
    out.push(PLATFORM === 'win32'
      ? path.join(envPrefix, 'node_modules')
      : path.join(envPrefix, 'lib', 'node_modules'));
  }

  // 3. Common no-sudo user prefix
  out.push(path.join(os.homedir(), '.npm-global', 'lib', 'node_modules'));

  // 4. Last resort: ask npm directly. Slow (~50-200ms) but most accurate when
  //    user has a non-standard prefix. Cached at the disk-cache layer above.
  try {
    const root = execFileSync('npm', ['root', '-g'], {
      timeout: 2000,
      stdio: ['pipe', 'pipe', 'pipe'],
      encoding: 'utf8',
    }).trim();
    if (root) out.push(root);
  } catch { /* npm not on PATH or timed out */ }

  return [...new Set(out)];
}

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

/**
 * Locate the platform-specific binary in npm package layouts.
 *   - First via `require.resolve` (parent-walk node_modules / linked / npx).
 *   - Then by explicit probe of npm global `node_modules` candidates.
 *
 * `require.resolve` does NOT search global installs (no NODE_PATH on
 * nvm/standard setups), so a working `npm install -g @sdsrs/code-graph` can
 * still be invisible without the fallback.
 */
function findPlatformBinary() {
  // Fast path: standard module resolution.
  try {
    const pkgPath = require.resolve(`${PLATFORM_PKG}/package.json`);
    const bin = path.join(path.dirname(pkgPath), BINARY_NAME);
    if (isNativeBinary(bin)) return bin;
  } catch { /* not in node_modules walk-up */ }

  // Slow path: explicit global node_modules probe.
  for (const globalRoot of globalNodeModulesCandidates()) {
    const bin = path.join(globalRoot, '@sdsrs', `code-graph-${PLATFORM}-${ARCH}`, BINARY_NAME);
    if (isNativeBinary(bin)) return bin;
  }

  return null;
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
  // Cache wins when its version >= the npm pkg version. After `npm update`
  // refreshes the platform-pkg, an older auto-update cache binary must NOT
  // shadow the freshly-installed one; this version check prevents the
  // upgrade-race where users keep running stale binary until auto-update fires.
  const autoUpdateBin = path.join(os.homedir(), '.cache', 'code-graph', 'bin', BINARY_NAME);
  if (isNativeBinary(autoUpdateBin)) {
    const cacheVer = readBinaryVersion(autoUpdateBin);
    const pkgVer = getPackageVersion();
    if (!pkgVer || !cacheVer || compareVersions(cacheVer, pkgVer) >= 0) {
      return autoUpdateBin;
    }
    // Cache is older than npm pkg — fall through to platform-pkg.
  }

  // --- Platform-specific npm package (@sdsrs/code-graph-{os}-{arch}) ---
  const platformBin = findPlatformBinary();
  if (platformBin) return platformBin;

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

module.exports = {
  findBinary, findBinaryUncached, clearCache,
  globalNodeModulesCandidates, findPlatformBinary,
  getPackageVersion, compareVersions,
  CACHE_FILE, BINARY_NAME, PLATFORM_PKG,
};

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
