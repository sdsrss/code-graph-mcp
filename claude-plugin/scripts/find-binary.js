#!/usr/bin/env node
'use strict';
const { execFileSync } = require('child_process');
const path = require('path');
const fs = require('fs');
const os = require('os');
const { readBinaryVersion, compareVersions } = require('./version-utils');
const { npmInvocation } = require('./npm-exec');
const { hidden } = require('./proc-opts');

const PLATFORM = os.platform();
const ARCH = os.arch();
const CACHE_FILE = path.join(os.homedir(), '.cache', 'code-graph', 'binary-path');
const BINARY_NAME = PLATFORM === 'win32' ? 'code-graph-mcp.exe' : 'code-graph-mcp';
// PATH lookup bound (NEW-07). 2 s matches the sibling `npm root -g` probe; the
// floor is what a budget-exhausted hook still gives it, because a `which` that
// cannot answer in 250 ms was not going to rescue this hook anyway.
const PATH_PROBE_TIMEOUT_MS = 2000;
const PATH_PROBE_MIN_MS = 250;

/**
 * How long the PATH probe may run: the hook budget when one is armed, the
 * default otherwise, never 0 and never fractional.
 *
 * Both edges matter. `timeout: 0` is read by node as NO TIMEOUT AT ALL — it
 * would restore the exact unbounded call this replaced — and a fractional value
 * makes `child_process` throw ERR_OUT_OF_RANGE, which the `catch` below would
 * silently turn into "not on PATH" (bugfix #7's shape, in a function that runs
 * before any hook has spent a millisecond).
 */
function pathProbeTimeoutMs(remaining = require('./hook-fail-open').remainingMs) {
  const left = remaining(PATH_PROBE_TIMEOUT_MS);
  const ms = left === null ? PATH_PROBE_MIN_MS : Math.floor(left);
  return Math.max(ms, PATH_PROBE_MIN_MS);
}
const PLATFORM_PKG = `@sdsrs/code-graph-${PLATFORM}-${ARCH}`;

/**
 * libc flavor of the current Linux runtime: 'glibc' or 'musl'. musl (Alpine) has
 * no `glibcVersionRuntime` in the Node process-report header. Best-effort; returns
 * 'glibc' off Linux or when detection is unavailable.
 */
function detectLibc() {
  if (PLATFORM !== 'linux') return 'glibc';
  try {
    const header = process.report.getReport().header;
    if (header && header.glibcVersionRuntime) return 'glibc';
    return 'musl';
  } catch {
    try { if (fs.existsSync('/etc/alpine-release')) return 'musl'; } catch { /* ignore */ }
    return 'glibc';
  }
}

/**
 * Actionable install hint for a platform that has NO published prebuilt binary
 * (Alpine/musl, or native Windows-on-ARM), or null when the platform is supported
 * (generic messaging applies). Pure in (platform, arch, libc) so it is unit-testable
 * without running on each OS. Prevents the misleading "npm install
 * @sdsrs/code-graph-<plat>-<arch>" suggestion for a package that does not exist.
 */
function unsupportedPlatformHint(platform = PLATFORM, arch = ARCH, libc = null) {
  const lc = libc || (platform === 'linux' ? detectLibc() : 'glibc');
  if (platform === 'linux' && lc === 'musl') {
    return 'Detected Alpine/musl libc, which has no prebuilt binary. Install from source:\n'
      + '  cargo install code-graph-mcp --features embed-model\n'
      + 'or use a glibc-based image (e.g. node:20-slim / debian, not node:20-alpine).';
  }
  if (platform === 'win32' && arch === 'arm64') {
    return 'Detected Windows on ARM (arm64), which has no native build. Either use an x64 '
      + 'build of Node.js (the published x64 binary runs under Windows ARM emulation), '
      + 'or install from source:\n  cargo install code-graph-mcp --features embed-model';
  }
  return null;
}

/**
 * Version that arms the gates below. Two shipped layouts resolve differently:
 * npm install has `<pkg>/package.json` two levels up; the marketplace/plugin-cache
 * copy ships ONLY the claude-plugin subtree, so `../.claude-plugin/plugin.json`
 * is the sole version source there. Without the fallback, every marketplace
 * install ran with a null version and each gate degraded to
 * first-candidate-wins (the pre-d578d99 relic-shadowing behavior).
 */
function getPackageVersion() {
  try { return require('../../package.json').version; } catch { /* not npm layout */ }
  try { return require('../.claude-plugin/plugin.json').version; }
  catch { return null; }
}

// compareVersions lives in version-utils.js (single canonical implementation,
// pre-release-aware); re-exported below for existing consumers.

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

  // 1. NPM_CONFIG_PREFIX env override (users with `~/.npm-global` etc.) FIRST:
  //    when set, `npm install -g` actually installs THERE, so it is the most
  //    authoritative location — matching npm's own prefix-resolution order.
  //    (It also used to rank below the execPath derivation, which let a stale
  //    relic in the nvm prefix shadow the user's real prefix.)
  const envPrefix = process.env.NPM_CONFIG_PREFIX || process.env.npm_config_prefix;
  if (envPrefix) {
    out.push(PLATFORM === 'win32'
      ? path.join(envPrefix, 'node_modules')
      : path.join(envPrefix, 'lib', 'node_modules'));
  }

  // 2. Derive from process.execPath. Works for nvm + standard Unix prefixes
  //    (`<prefix>/bin/node` → globals at `<prefix>/lib/node_modules`); on
  //    Windows globals sit next to `node.exe`.
  if (PLATFORM === 'win32') {
    out.push(path.join(nodeBinDir, 'node_modules'));
  } else {
    out.push(path.resolve(nodeBinDir, '..', 'lib', 'node_modules'));
  }

  // 3. Common no-sudo user prefix
  out.push(path.join(os.homedir(), '.npm-global', 'lib', 'node_modules'));

  // 4. Last resort: ask npm directly. Slow (~50-200ms) but most accurate when
  //    user has a non-standard prefix. Cached at the disk-cache layer above.
  try {
    const npm = npmInvocation(['root', '-g'], {
      timeout: 2000,
      stdio: ['pipe', 'pipe', 'pipe'],
      encoding: 'utf8',
    });
    const root = execFileSync(npm.file, npm.args, npm.opts).trim();
    if (root) out.push(root);
  } catch { /* npm not on PATH or timed out */ }

  return [...new Set(out)];
}

// Every nvm-managed node version's global node_modules dir (`~/.nvm/versions/
// node/*/lib/node_modules`). nvm keeps a SEPARATE global prefix per node
// version; switching the default node strands the previous version's globals —
// a global `@sdsrs/code-graph` there is invisible to globalNodeModulesCandidates
// (execPath-derived → only the ACTIVE node) yet still shadows PATH shims / seeds
// stale settings.json hooks (the v24.11.1@0.46.0 relic — RCA 2026-07-24). Used
// for detection/reporting only; `npm install -g` cannot target another node's
// prefix. `base` is injectable for hermetic tests (never the real ~/.nvm).
function nvmNodeModulesDirs(base = path.join(os.homedir(), '.nvm', 'versions', 'node')) {
  let entries;
  try { entries = fs.readdirSync(base); } catch { return []; }
  return entries
    .map((v) => path.join(base, v, 'lib', 'node_modules'))
    .filter((d) => { try { return fs.statSync(d).isDirectory(); } catch { return false; } });
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
 * Decide whether a cached binary path is fresh enough to skip the full
 * discovery walk. Matches the auto-update cache logic at ~:185-188 —
 * cache wins only when its binary's version >= pkg version. Without this
 * check, a stale cache entry (e.g. dev checkout's `bin/code-graph-mcp`
 * recorded once, then never refreshed) shadows newer auto-update or
 * platform-pkg binaries forever (see mem #8454).
 *
 * Permissive on unknown values: missing pkg version or unreadable binary
 * version → trust cache (don't refuse the only path we know about).
 */
function isCachedBinaryFresh(cachedPath, pkgVersion, knownVersion) {
  if (!isNativeBinary(cachedPath)) return false;
  if (!pkgVersion) return true;
  // `knownVersion` lets the caller skip the `--version` spawn when the cache
  // entry already recorded it AND the file has not changed since — see
  // `readCacheEntry`. Absent, behaviour is exactly as before.
  const cacheVer = knownVersion || readBinaryVersion(cachedPath);
  if (!cacheVer) return true;
  return compareVersions(cacheVer, pkgVersion) >= 0;
}

/// Identity stamp for a binary file: if this is unchanged, the version we
/// recorded for it is still its version. mtime alone would be fooled by a
/// same-second replacement, so size rides along.
function binaryStamp(binPath) {
  try {
    const st = fs.statSync(binPath);
    return `${st.mtimeMs}:${st.size}`;
  } catch {
    return null;
  }
}

/// Read the on-disk cache entry.
///
/// The file used to hold a bare path, so every cache HIT still spawned
/// `code-graph-mcp --version` to check freshness — four `findBinary()` calls in
/// one SessionStart process meant four spawns, plus one more from the
/// consistency check. Cheap warm (2-4ms), but a cold spawn behind Windows AV
/// has been measured above 2s in this codebase's own notes, and they run
/// serially inside a hook's time budget (audit 2026-08-22 P2-17).
///
/// The entry now carries the version and a file stamp. A pre-existing bare-path
/// file still reads (no version → verified once, then rewritten in the new
/// shape), and an older plugin reading the new file sees a non-path string,
/// fails `isNativeBinary`, and re-discovers — one extra walk, never a wrong
/// answer.
function readCacheEntry() {
  let raw;
  try {
    raw = fs.readFileSync(CACHE_FILE, 'utf8').trim();
  } catch {
    return null;
  }
  if (!raw) return null;
  if (raw[0] === '{') {
    try {
      const parsed = JSON.parse(raw);
      return parsed && typeof parsed.path === 'string' ? parsed : null;
    } catch {
      return null;
    }
  }
  return { path: raw };
}

function writeCacheEntry(binPath) {
  try {
    fs.mkdirSync(path.dirname(CACHE_FILE), { recursive: true });
    fs.writeFileSync(
      CACHE_FILE,
      JSON.stringify({
        path: binPath,
        version: readBinaryVersion(binPath) || null,
        stamp: binaryStamp(binPath),
      })
    );
  } catch { /* ok */ }
}

/// Per-process memo. `findBinary()` is called four times in a single
/// session-init run; only the first should touch the disk. Only a FOUND path is
/// memoized — a miss must stay retryable, because `launcher-install` installs
/// the binary and looks again in the same process.
let memoizedBinary = null;

/**
 * Locate the code-graph-mcp binary using multiple strategies.
 * Results are cached to disk so repeated calls (e.g. per-hook) are fast.
 *
 * Priority:
 *   cache (if valid + version >= pkg) → dev-mode (target/release) →
 *   auto-update cache → platform npm pkg → bundled (bin/) →
 *   cargo install → PATH → npx cache
 *
 * Every tier after dev-mode is version-gated (createVersionGate): the first
 * candidate at/above the pkg version wins; when NONE is current, the newest
 * stale candidate is returned rather than null.
 *
 * Returns the absolute path or null if not found.
 */
function findBinary() {
  if (memoizedBinary) return memoizedBinary;

  // Try disk cache first (avoids spawning `which` on hot paths)
  const entry = readCacheEntry();
  if (entry) {
    // Trust the recorded version only while the FILE is the one it was recorded
    // for; a replaced binary re-reads it.
    const stamped = entry.stamp && entry.stamp === binaryStamp(entry.path);
    const known = stamped ? entry.version : null;
    if (isCachedBinaryFresh(entry.path, getPackageVersion(), known)) {
      if (!known) writeCacheEntry(entry.path); // upgrade a bare-path / stale-stamp record
      memoizedBinary = entry.path;
      return memoizedBinary;
    }
    clearCache();
  }

  const result = findBinaryUncached();

  // Write cache for subsequent calls
  if (isNativeBinary(result)) {
    writeCacheEntry(result);
    memoizedBinary = result;
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
// Truncation gate for the npm platform-package tier ONLY: an interrupted npm
// install can leave a partial binary with the right name, and unlike the
// GitHub-download path (size + sha256 sidecar + version-exec before promote)
// nothing else checks this tier. Real release binaries are ~40MB; 1MB matches
// promoteVerifiedBinary's floor. Deliberately NOT inside isNativeBinary —
// dev builds, cargo installs, and test fixtures go through other tiers.
function isPlausibleReleaseBinary(candidate) {
  try { return fs.statSync(candidate).size > 1_000_000; } catch { return false; }
}

/**
 * Both places npm may put the platform optionalDependency inside a
 * `node_modules` root, most-likely first:
 *
 *   <root>/@sdsrs/code-graph-<plat>-<arch>/                        (hoisted)
 *   <root>/@sdsrs/code-graph/node_modules/@sdsrs/code-graph-…/     (nested)
 *
 * Hoisting is an npm implementation detail, not a contract: npm 12 leaves the
 * optionalDependency nested under the shell package on a plain
 * `npm install -g @sdsrs/code-graph`, while older npm hoists it (both layouts
 * were observed on one machine, 2026-08-17). Probing only the hoisted spelling
 * made a SUCCESSFUL npm install read as "did not yield a binary": the launcher
 * then re-downloaded ~41MB from GitHub, and — the part that outlives the
 * session — never called `recordGlobalInstall()`, so `uninstall` had no marker
 * proving the plugin owns those global packages and left them on PATH forever.
 *
 * Discovery only. The global-package INVENTORY scanners (staleGlobalPkgs,
 * installedGlobalPkgs, doctor) stay top-level-on-purpose: a nested
 * optionalDependency is the shell package's private dependency, so
 * `npm install -g`/`npm uninstall -g` on its name would introduce or orphan a
 * top-level global the user never asked for.
 */
function platformPkgBinaries(nodeModulesRoot) {
  const pkg = `code-graph-${PLATFORM}-${ARCH}`;
  return [
    path.join(nodeModulesRoot, '@sdsrs', pkg, BINARY_NAME),
    path.join(nodeModulesRoot, '@sdsrs', 'code-graph', 'node_modules', '@sdsrs', pkg, BINARY_NAME),
  ];
}

function platformBinaryCandidates() {
  const out = [];
  // Fast path: standard module resolution.
  try {
    const pkgPath = require.resolve(`${PLATFORM_PKG}/package.json`);
    const bin = path.join(path.dirname(pkgPath), BINARY_NAME);
    if (isNativeBinary(bin) && isPlausibleReleaseBinary(bin)) out.push(bin);
  } catch { /* not in node_modules walk-up */ }

  // Slow path: explicit global node_modules probe, both npm layouts.
  for (const globalRoot of globalNodeModulesCandidates()) {
    for (const bin of platformPkgBinaries(globalRoot)) {
      if (isNativeBinary(bin) && isPlausibleReleaseBinary(bin)) out.push(bin);
    }
  }

  return out;
}

function findPlatformBinary() {
  return platformBinaryCandidates()[0] || null;
}

/**
 * Version gate for discovery candidates. `consider(bin)` accepts a candidate
 * outright when it is current (version >= pkgVersion) or unverifiable (no pkg
 * version / binary won't report one — don't refuse the only path we may have);
 * a candidate OLDER than the package is recorded as a stale fallback instead of
 * being returned, and `best()` yields the NEWEST of those.
 *
 * Why: candidates below the auto-update cache had no version check at all, so
 * a years-old relic (a 0.16.6 `npm install -g` leftover in the nvm global
 * node_modules) was returned VERBATIM during every post-release window in
 * which the cache binary was one version behind — an ancient server on a
 * modern schema, presenting as the MCP 30s connect-timeout. Newest-stale
 * beats null because every consumer (statusline, hooks, CLI) degrades more
 * gracefully on a slightly-old binary than on "offline", and the stale-binary
 * self-heal in auto-update.js re-downloads shortly anyway.
 */
function createVersionGate(pkgVersion, { readVersion = readBinaryVersion } = {}) {
  let bestStale = null;
  return {
    consider(bin) {
      if (!isNativeBinary(bin)) return null;
      if (!pkgVersion) return bin;
      const ver = readVersion(bin);
      if (!ver || compareVersions(ver, pkgVersion) >= 0) return bin;
      if (!bestStale || compareVersions(ver, bestStale.ver) > 0) bestStale = { bin, ver };
      return null;
    },
    best() { return bestStale ? bestStale.bin : null; },
  };
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
  // From CLAUDE_PROJECT_DIR — behind an explicit opt-in, and ONLY that.
  //
  // This root is the arbitrary directory the developer opened; the two above are
  // derived from the plugin's own install. Accepting it made "clone an untrusted
  // repo and open it" arbitrary code execution: this tier sits above the version
  // gate, `isDevRepo` asks only whether a `Cargo.toml` exists, and resolution
  // itself runs the file (`writeCacheEntry` → `readBinaryVersion`) — all before
  // any file is read or any tool approved. A tracked, mode-755
  // `target/release/code-graph-mcp` survives `.gitignore` into a fresh clone,
  // so supplying one costs the attacker nothing.
  //
  // No property of the opened directory can re-establish trust here (an attacker
  // supplies all of them), so the gate is the developer's own opt-in, spelled
  // the same way `version-utils.js:isDevMode` spells it.
  if (process.env.CLAUDE_PROJECT_DIR && process.env.CODE_GRAPH_DEV === '1') {
    possibleRoots.add(path.resolve(process.env.CLAUDE_PROJECT_DIR));
  }

  for (const root of possibleRoots) {
    if (isDevRepo(root)) {
      const devBin = path.join(root, 'target', 'release', BINARY_NAME);
      if (isNativeBinary(devBin)) return devBin;
    }
  }

  // Every tier below runs through the version gate: a candidate at or above
  // the npm pkg version is returned on the spot (tier order = priority, same
  // as before); an OLDER candidate is only remembered as a fallback. Without
  // the gate, tiers below the auto-update cache accepted any binary verbatim —
  // so when the cache was one release behind (every post-release window), an
  // ancient global-npm relic could shadow it (the 0.16.6-serves-a-modern-DB
  // incident behind the MCP 30s connect-timeout).
  const gate = createVersionGate(getPackageVersion());

  // --- Auto-update cache (binary downloaded directly from GitHub release) ---
  // Cache wins when its version >= the npm pkg version. After `npm update`
  // refreshes the platform-pkg, an older auto-update cache binary must NOT
  // shadow the freshly-installed one; this version check prevents the
  // upgrade-race where users keep running stale binary until auto-update fires.
  const autoUpdateBin = path.join(os.homedir(), '.cache', 'code-graph', 'bin', BINARY_NAME);
  {
    const hit = gate.consider(autoUpdateBin);
    if (hit) return hit;
  }

  // --- Platform-specific npm package (@sdsrs/code-graph-{os}-{arch}) ---
  for (const platformBin of platformBinaryCandidates()) {
    const hit = gate.consider(platformBin);
    if (hit) return hit;
  }

  // --- Bundled binary (in same directory as cli.js or plugin scripts) ---
  // Check bin/ directory of the npm package
  const binDirs = new Set();
  if (process.env._FIND_BINARY_ROOT) {
    binDirs.add(path.join(process.env._FIND_BINARY_ROOT, 'bin'));
  }
  binDirs.add(path.resolve(__dirname, '..', '..', 'bin'));
  for (const dir of binDirs) {
    const hit = gate.consider(path.join(dir, BINARY_NAME));
    if (hit) return hit;
  }

  // --- Cargo install (~/.cargo/bin) ---
  {
    const hit = gate.consider(path.join(os.homedir(), '.cargo', 'bin', BINARY_NAME));
    if (hit) return hit;
  }

  // --- PATH lookup (last resort for intentionally installed binaries) ---
  //
  // BOUNDED. This was the one child process in the whole hook path with no
  // timeout at all, and it runs BEFORE any hook has called `remainingMs` even
  // once — `findBinary()` is the first thing every hook does (audit 2026-09-05
  // NEW-07). A `which` against a wedged PATH entry (a dead NFS mount, an
  // automounter) hangs until Claude Code kills the hook, which surfaces to the
  // user as an error on THEIR tool call.
  //
  // Spends the hook budget when one is armed, floored at 250 ms so the probe is
  // always actually attempted: skipping it outright would make `findBinary`
  // answer "no binary" for one that is on PATH, and a fabricated absence is
  // worse than a probe that ran out of time. Outside a hook `remainingMs`
  // returns the default, so nothing changes for doctor / statusline / the
  // launcher.
  try {
    const which = PLATFORM === 'win32' ? 'where' : 'which';
    const found = execFileSync(which, [BINARY_NAME], hidden({
      timeout: pathProbeTimeoutMs(),
      stdio: ['pipe', 'pipe', 'pipe'],
    }))
      .toString().trim().split('\n')[0];
    const hit = gate.consider(found);
    if (hit) return hit;
  } catch { /* not in PATH */ }

  // --- npx cache (very last resort — may be outdated) ---
  const npxDir = path.join(os.homedir(), '.npm', '_npx');
  try {
    for (const entry of fs.readdirSync(npxDir)) {
      // Same hoisted-or-nested question as the global prefix — an npx tree is
      // just another node_modules root, and npm decides the layout there too.
      for (const bin of platformPkgBinaries(path.join(npxDir, entry, 'node_modules'))) {
        const hit = gate.consider(bin);
        if (hit) return hit;
      }
    }
  } catch { /* no npx cache */ }

  // Nothing current anywhere: the newest stale candidate (if any) beats null —
  // consumers degrade better on a slightly-old binary than on "offline", and
  // auto-update's stale-binary self-heal replaces it shortly.
  return gate.best();
}

/**
 * Clear the disk cache. Call this after binary updates so the next
 * findBinary() picks up the new location.
 */
function clearCache() {
  memoizedBinary = null; // the memo is a cache too — an update must invalidate both
  try { fs.unlinkSync(CACHE_FILE); } catch { /* ok */ }
}

module.exports = {
  findBinary, findBinaryUncached, clearCache,
  globalNodeModulesCandidates, nvmNodeModulesDirs, findPlatformBinary, platformBinaryCandidates, createVersionGate,
  getPackageVersion, compareVersions, isCachedBinaryFresh,
  detectLibc, unsupportedPlatformHint,
  CACHE_FILE, BINARY_NAME, PLATFORM_PKG,
  pathProbeTimeoutMs, PATH_PROBE_TIMEOUT_MS, PATH_PROBE_MIN_MS,
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
