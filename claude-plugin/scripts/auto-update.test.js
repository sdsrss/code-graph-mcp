'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const crypto = require('crypto');

// Git plumbing env vars git(1) honors over cwd. A partial `git commit` runs the
// pre-commit hook with these exported, so any `git` this suite shells out to for
// its tempdir fixtures would operate on the REAL repo index instead of the
// fixture. Strip them from THIS process (covers the raw `git clone` + the node
// `-e` sub-spawns, which inherit env) and per-call in git() below (hermetic even
// if a test sets one). Sibling of the v0.80.3 pre-commit.sh cargo-path fix (H4).
const GIT_ENV_VARS = [
  'GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_OBJECT_DIRECTORY',
  'GIT_COMMON_DIR', 'GIT_NAMESPACE', 'GIT_PREFIX',
];
for (const k of GIT_ENV_VARS) delete process.env[k];

// `CLAUDE_CONFIG_DIR` is dropped from THIS process before anything runs.
//
// The sandboxes below redirect HOME and spawn with `{...process.env}`, which
// passes the variable straight through — and `claudeHome()` is
// `CLAUDE_CONFIG_DIR || homedir/.claude`, so the env var WINS over the
// redirected HOME. For a developer who exports it (the documented multi-profile
// setup) these tests wrote into their LIVE config: measured, a fabricated
// `9.9.9` plugin version landed in the real plugins cache. Deleting it here
// covers every spawn site at once; the few tests that need the variable set it
// explicitly in their own child env, which still wins.
delete process.env.CLAUDE_CONFIG_DIR;
function cleanGitEnv() {
  const e = { ...process.env };
  for (const k of GIT_ENV_VARS) delete e[k];
  return e;
}

const {
  commandExists,
  fetchLatestRelease,
  isAutoUpdateDisabled,
  MAX_UPDATE_ATTEMPTS,
  getExtractedPluginVersion,
  parseLatestRelease,
  PLUGIN_ASSET_NAME,
  readBinaryVersion,
  promoteVerifiedBinary,
  cachedBinaryPath,
  cachedBinaryNeedsUpdate,
  cachedBinaryStaleVsState,
  getPlatformAssetName,
  downloadBinary,
  selfHealStaleBinary,
  selfHealGlobalPkgs,
  staleGlobalPkgs,
  globalPkgVersion,
  isInstallMissingMode,
  isSilentMode,
  shouldCheck,
  resolveProxy,
  requestJson,
  shouldHealGlobalsOnThrottle,
  inactiveNodeGlobalRelics,
} = require('./auto-update');

function mkDir(t, prefix) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
}

test('getExtractedPluginVersion reads extracted plugin manifest version', (t) => {
  const root = mkDir(t, 'code-graph-plugin-');
  const manifest = path.join(root, '.claude-plugin', 'plugin.json');
  fs.mkdirSync(path.dirname(manifest), { recursive: true });
  fs.writeFileSync(manifest, JSON.stringify({ version: '1.2.3' }, null, 2));
  assert.equal(getExtractedPluginVersion(root), '1.2.3');
});

// Promotion is fail-closed, so the mechanics tests below have to hand it the
// real digest of the fixture they just wrote.
function sha256Of(filePath) {
  return crypto.createHash('sha256').update(fs.readFileSync(filePath)).digest('hex');
}

function writeFakeBinary(filePath, version, mode = 0o755) {
  const script = [
    '#!/usr/bin/env bash',
    'if [ "$1" = "--version" ]; then',
    `  echo "code-graph-mcp ${version}"`,
    '  exit 0',
    'fi',
    'exit 0',
    `# ${'x'.repeat(1_100_000)}`,
    '',
  ].join('\n');
  fs.writeFileSync(filePath, script);
  fs.chmodSync(filePath, mode);
}

test('promoteVerifiedBinary accepts a runnable binary with the expected version', (t) => {
  const dir = mkDir(t, 'code-graph-bin-');
  const tmp = path.join(dir, 'code-graph-mcp.tmp');
  const dst = path.join(dir, 'code-graph-mcp');
  writeFakeBinary(tmp, '1.2.3');

  assert.equal(readBinaryVersion(tmp), '1.2.3');
  // Promotion is fail-closed, so hand it the real digest: this test pins the
  // chmod/rename mechanics, not the integrity policy (which has its own tests).
  assert.equal(promoteVerifiedBinary(tmp, dst, '1.2.3', sha256Of(tmp)), true);
  assert.equal(fs.existsSync(tmp), false);
  assert.equal(fs.existsSync(dst), true);
});

test('promoteVerifiedBinary rejects binaries with mismatched version', (t) => {
  const dir = mkDir(t, 'code-graph-bin-');
  const tmp = path.join(dir, 'code-graph-mcp.tmp');
  const dst = path.join(dir, 'code-graph-mcp');
  writeFakeBinary(tmp, '1.2.2');

  assert.equal(promoteVerifiedBinary(tmp, dst, '1.2.3'), false);
  assert.equal(fs.existsSync(tmp), false);
  assert.equal(fs.existsSync(dst), false);
});

test('promoteVerifiedBinary promotes a non-executable (0644) download — curl -o regression', (t) => {
  // `curl -o` writes the tmp file as 0644 (no exec bit). promoteVerifiedBinary
  // must chmod before reading the version (readBinaryVersion executes the
  // binary), otherwise the version read fails with EACCES → null → false and
  // every download path silently wedges. Regression for the binary-stuck-at-old
  // -version deadlock.
  if (process.platform === 'win32') return; // no exec bit on win32
  const dir = mkDir(t, 'code-graph-bin-');
  const tmp = path.join(dir, 'code-graph-mcp.tmp');
  const dst = path.join(dir, 'code-graph-mcp');
  writeFakeBinary(tmp, '1.2.3', 0o644);

  assert.equal(readBinaryVersion(tmp), null, 'precondition: 0644 binary is not executable');
  assert.equal(promoteVerifiedBinary(tmp, dst, '1.2.3', sha256Of(tmp)), true);
  assert.equal(fs.existsSync(dst), true);
  assert.equal(fs.statSync(dst).mode & 0o111, 0o111, 'promoted binary is executable');
  assert.equal(readBinaryVersion(dst), '1.2.3');
});

test('promoteVerifiedBinary accepts a binary matching the expected sha256', (t) => {
  const dir = mkDir(t, 'code-graph-bin-');
  const tmp = path.join(dir, 'code-graph-mcp.tmp');
  const dst = path.join(dir, 'code-graph-mcp');
  writeFakeBinary(tmp, '1.2.3');
  const sha = crypto.createHash('sha256').update(fs.readFileSync(tmp)).digest('hex');
  assert.equal(promoteVerifiedBinary(tmp, dst, '1.2.3', sha), true);
  assert.equal(fs.existsSync(dst), true);
});

test('promoteVerifiedBinary rejects a binary whose sha256 mismatches the sidecar', (t) => {
  // Tampered/corrupted download: the checksum gate runs BEFORE chmod+exec, so a
  // mismatched binary is refused and never made executable. Platform-independent
  // (no exec needed to reject).
  const dir = mkDir(t, 'code-graph-bin-');
  const tmp = path.join(dir, 'code-graph-mcp.tmp');
  const dst = path.join(dir, 'code-graph-mcp');
  writeFakeBinary(tmp, '1.2.3');
  const wrongSha = 'deadbeef'.repeat(8); // 64 hex chars, deliberately wrong
  assert.equal(promoteVerifiedBinary(tmp, dst, '1.2.3', wrongSha), false);
  assert.equal(fs.existsSync(dst), false, 'tampered binary must not be promoted');
  assert.equal(fs.existsSync(tmp), false, 'tmp cleaned up on rejection');
});

test('promoteVerifiedBinary refuses a binary with no expected sha256 (fail-closed)', (t) => {
  // Was the TOFU back-compat path: a null expected hash printed a warning and
  // installed anyway, making this the only fail-OPEN link among the four
  // download chains while src/snapshot/install.rs is fail-closed — and a warning
  // on stderr during a background auto-update is seen by nobody. Every release
  // back to v0.100.0 publishes a sidecar per binary and downloads always target
  // `releases/latest`, so there is no no-sidecar case left to serve.
  const dir = mkDir(t, 'code-graph-bin-');
  const tmp = path.join(dir, 'code-graph-mcp.tmp');
  const dst = path.join(dir, 'code-graph-mcp');
  writeFakeBinary(tmp, '1.2.3');
  assert.equal(promoteVerifiedBinary(tmp, dst, '1.2.3', null), false);
  assert.equal(fs.existsSync(dst), false, 'unverified binary must not be promoted');
  assert.equal(fs.existsSync(tmp), false, 'tmp cleaned up on refusal');
  // The gate runs BEFORE chmod, so nothing was ever made executable.
});

test('cachedBinaryNeedsUpdate is version-aware, not existence-only', (t) => {
  const dir = mkDir(t, 'code-graph-heal-');
  const binaryPath = path.join(dir, 'code-graph-mcp');
  const latest = { version: '0.45.0', binaryUrl: 'https://example.com/bin' };

  // missing binary → needs update
  assert.equal(cachedBinaryNeedsUpdate(latest, { binaryPath }), true);

  // present but stale (the actual deadlock: shell at 0.45.0, binary at 0.16.6)
  fs.writeFileSync(binaryPath, 'x');
  assert.equal(
    cachedBinaryNeedsUpdate(latest, { binaryPath, readVersion: () => '0.16.6' }),
    true,
  );

  // present and current → no update
  assert.equal(
    cachedBinaryNeedsUpdate(latest, { binaryPath, readVersion: () => '0.45.0' }),
    false,
  );

  // no binaryUrl / null latest → no-op (nothing to download)
  assert.equal(cachedBinaryNeedsUpdate({ version: '0.45.0', binaryUrl: null }, { binaryPath }), false);
  assert.equal(cachedBinaryNeedsUpdate(null, { binaryPath }), false);
});

test('cachedBinaryStaleVsState bypasses throttle only for a present-but-stale binary', (t) => {
  const dir = mkDir(t, 'code-graph-throttle-');
  const binaryPath = path.join(dir, 'code-graph-mcp');
  fs.writeFileSync(binaryPath, 'x'); // present

  // no prior latestVersion → don't bypass (first run fetches anyway)
  assert.equal(cachedBinaryStaleVsState({}, { binaryPath }), false);
  assert.equal(cachedBinaryStaleVsState(null, { binaryPath }), false);

  // present + stale vs last known latest → bypass throttle (the 6h-gap fix)
  assert.equal(
    cachedBinaryStaleVsState({ latestVersion: '0.45.1' }, { binaryPath, readVersion: () => '0.16.6' }),
    true,
  );

  // present + current → stay throttled
  assert.equal(
    cachedBinaryStaleVsState({ latestVersion: '0.45.1' }, { binaryPath, readVersion: () => '0.45.1' }),
    false,
  );

  // missing binary → false here (the separate binaryMissing bypass handles it)
  fs.rmSync(binaryPath);
  assert.equal(cachedBinaryStaleVsState({ latestVersion: '0.45.1' }, { binaryPath }), false);
});

test('shouldCheck re-verifies an up-to-date state on a short cadence (release-publish race)', () => {
  const minsAgo = (m) => new Date(Date.now() - m * 60 * 1000).toISOString();

  // never checked → always check
  assert.equal(shouldCheck({}), true);

  // Bug repro: the last check reported "up to date" (updateAvailable:false) and a
  // release published moments later. 45min on, the plain 6h throttle kept the
  // stale answer latched (every session reopen re-reported up-to-date); the short
  // up-to-date cadence must allow a re-check so the new release is discovered.
  assert.equal(shouldCheck({ lastCheck: minsAgo(45), updateAvailable: false }), true);

  // within the short window → still throttled (don't hammer the API every call)
  assert.equal(shouldCheck({ lastCheck: minsAgo(10), updateAvailable: false }), false);

  // a pending-but-unfinished update keeps the 6h steady-state interval
  assert.equal(shouldCheck({ lastCheck: minsAgo(45), updateAvailable: true }), false);

  // Rate-limit backoff wins even over the up-to-date short cadence. The window
  // is GitHub's own unauthenticated reset period (1h), not the 24h this used to
  // assert — that number was written while the flag was unreachable and became
  // load-bearing only when the state clobber was fixed.
  assert.equal(shouldCheck({ lastCheck: minsAgo(30), updateAvailable: false, rateLimited: true }), false);
  assert.equal(shouldCheck({ lastCheck: minsAgo(61), updateAvailable: false, rateLimited: true }), true,
    'past the reset window the backoff must clear, or a 403 stalls updates indefinitely');
});

test('shouldCheck lets a forced (session-start) check bypass the soft throttle', () => {
  const minsAgo = (m) => new Date(Date.now() - m * 60 * 1000).toISOString();

  // A new session / explicit reload is a strong "get me latest" signal: a forced
  // check runs even inside the 30min up-to-date window (contrast the non-forced
  // call on the same state, which stays throttled).
  assert.equal(shouldCheck({ lastCheck: minsAgo(10), updateAvailable: false }, { force: true }), true);
  assert.equal(shouldCheck({ lastCheck: minsAgo(10), updateAvailable: false }), false);

  // ...but a short anti-hammer floor still applies, so a crash/reopen loop can't
  // pound the GitHub API on every restart.
  assert.equal(shouldCheck({ lastCheck: minsAgo(0.5), updateAvailable: false }, { force: true }), false);

  // Rate-limit backoff wins even over force — never push more requests into a
  // 403. That ordering is only safe because the window is an hour: at 24h a
  // single 403 made `--force` a silent no-op for a full day.
  assert.equal(shouldCheck({ lastCheck: minsAgo(30), updateAvailable: false, rateLimited: true }, { force: true }), false);
  assert.equal(shouldCheck({ lastCheck: minsAgo(61), updateAvailable: false, rateLimited: true }, { force: true }), true,
    'force must work again once the reset window has passed');
});

test('selfHealStaleBinary wires the stale-binary check to a download (the v0.45.x glue)', async () => {
  const latest = { version: '0.45.2', binaryUrl: 'https://example/bin' };

  // Field failure mode: shell already at latest, binary pinned stale → MUST download.
  let downloaded = false;
  let stale = true;
  const healed = await selfHealStaleBinary(latest, {
    state: {},
    needsUpdate: () => stale,
    binaryPresent: () => true, // pin: the host's real binary cache must not steer this test
    download: async () => { downloaded = true; stale = false; return true; },
  });
  assert.equal(downloaded, true, 'stale binary must trigger a download');
  assert.equal(healed.healed, true);

  // Binary current → no download, no-op.
  let touched = false;
  const noop = await selfHealStaleBinary(latest, {
    state: {},
    needsUpdate: () => false,
    binaryPresent: () => true,
    download: async () => { touched = true; return true; },
  });
  assert.equal(touched, false, 'current binary must not download');
  assert.equal(noop.healed, false);

  // Download fails (no curl / network) → not healed, and the attempt is RECORDED
  // (see the bounded-retry test below; an unrecorded failure retried forever).
  const failed = await selfHealStaleBinary(latest, {
    state: {},
    needsUpdate: () => true,
    binaryPresent: () => true, // STALE (present) — a missing binary would deliberately not record
    download: async () => false,
  });
  assert.equal(failed.healed, false);
  assert.equal(failed.patch.binaryHealAttempts, 1);
  assert.equal(failed.patch.binaryHealVersion, '0.45.2');
});

// ── P1-14: the stale-binary self-heal must be BOUNDED ───────────────────────
//
// It had no counter at all, and the no-update branch that runs it also cleared
// `updateAttempts`/`suspendedAt` unconditionally — while `shouldCheck`'s
// `binaryStale` arm bypasses the throttle. A binary that cannot be promoted
// (the Windows "server holds the .exe" EACCES the code names at :526) therefore
// re-downloaded ~40MB on EVERY session, forever. Measured pre-fix with a stubbed
// downloader: 8 calls → 8 downloads.

test('selfHealStaleBinary stops after MAX_UPDATE_ATTEMPTS failures on the same version', async () => {
  const latest = { version: '1.0.0', binaryUrl: 'https://example/bin' };
  let downloads = 0;
  let state = {};
  for (let i = 0; i < 8; i++) {
    const r = await selfHealStaleBinary(latest, {
      state,
      needsUpdate: () => true,                 // promote keeps failing → still stale
      binaryPresent: () => true,               // STALE, not missing — the budget applies
      download: async () => { downloads++; return false; },
    });
    state = { ...state, ...r.patch };
  }
  assert.equal(downloads, MAX_UPDATE_ATTEMPTS,
    `8 sessions must cost at most ${MAX_UPDATE_ATTEMPTS} downloads, not 8`);
  assert.equal(state.binaryHealAttempts, MAX_UPDATE_ATTEMPTS);
});

test('a download that "succeeds" but leaves the binary stale still counts as a failure', async () => {
  // The Windows EACCES shape: curl+checksum fine, the rename onto the running
  // .exe fails. Counting the exit code instead of re-reading the disk is how a
  // capped retry budget never runs out (the same lesson as selfHealGlobalPkgs).
  const latest = { version: '1.0.0', binaryUrl: 'https://example/bin' };
  const r = await selfHealStaleBinary(latest, {
    state: {},
    needsUpdate: () => true,        // still stale AFTER the "successful" download
    binaryPresent: () => true,
    download: async () => true,
  });
  assert.equal(r.healed, false);
  assert.equal(r.patch.binaryHealAttempts, 1);
});

test('a successful heal clears the counter; a new release re-arms it', async () => {
  const v1 = { version: '1.0.0', binaryUrl: 'https://example/bin' };
  const v2 = { version: '1.1.0', binaryUrl: 'https://example/bin2' };
  const exhausted = { binaryHealVersion: '1.0.0', binaryHealAttempts: MAX_UPDATE_ATTEMPTS };

  // Same target, budget spent → no download at all.
  let downloads = 0;
  const parked = await selfHealStaleBinary(v1, {
    state: exhausted,
    needsUpdate: () => true,
    binaryPresent: () => true,
    download: async () => { downloads++; return true; },
  });
  assert.equal(downloads, 0, 'a parked heal must not spend bandwidth');
  assert.equal(parked.healed, false);

  // A NEWER release moves the target → full budget again.
  let stale = true;
  const rearmed = await selfHealStaleBinary(v2, {
    state: exhausted,
    needsUpdate: () => stale,
    binaryPresent: () => true,
    download: async () => { downloads++; stale = false; return true; },
  });
  assert.equal(downloads, 1, 'a new release re-arms the heal');
  assert.equal(rearmed.healed, true);
  assert.equal(rearmed.patch.binaryHealAttempts, 0, 'success resets the counter');
  assert.equal(rearmed.patch.binaryHealVersion, '1.1.0');

  // And a binary that is current clears a leftover counter.
  const clean = await selfHealStaleBinary(v1, {
    state: exhausted,
    needsUpdate: () => false,
    download: async () => { downloads++; return true; },
  });
  assert.equal(clean.patch.binaryHealAttempts, 0);
});

test('a MISSING binary is exempt from the heal budget — recovery stays unbounded', async () => {
  // Batch review of the P1-14 fix: `needsUpdate` is also true when the binary
  // does not exist at all. Letting the stale-heal counter absorb those
  // failures would park the ONLY recovery path after five offline session
  // starts (captive portal, air-gapped week), with no time-based re-arm —
  // the counter resets only when a NEWER release ships.
  const latest = { version: '1.0.0', binaryUrl: 'https://example/bin' };
  let downloads = 0;
  let state = { binaryHealVersion: '1.0.0', binaryHealAttempts: MAX_UPDATE_ATTEMPTS };
  for (let i = 0; i < 3; i++) {
    const r = await selfHealStaleBinary(latest, {
      state,
      needsUpdate: () => true,
      binaryPresent: () => false,           // no engine at all
      download: async () => { downloads++; return false; },
    });
    state = { ...state, ...r.patch };
  }
  assert.equal(downloads, 3, 'a missing binary must keep retrying even past the stale budget');
  assert.equal(state.binaryHealAttempts, MAX_UPDATE_ATTEMPTS,
    'missing-binary failures must not inflate the stale counter');

  // When the binary finally lands, the counter clears as usual.
  const healed = await selfHealStaleBinary(latest, {
    state,
    needsUpdate: (() => { let calls = 0; return () => { calls += 1; return calls === 1; }; })(),
    binaryPresent: () => false,
    download: async () => true,
  });
  assert.equal(healed.healed, true);
  assert.equal(healed.patch.binaryHealAttempts, 0);
});

test('a CORRUPT-but-present binary gets the same unbounded recovery as a missing one', async () => {
  // Pre-tag review of the P1-14 fix: `binaryPresent` keyed on existsSync alone,
  // so a truncated / non-executable / wrong-arch cached binary counted as
  // "present" → bounded by the 5-attempt stale budget → parked forever, since
  // isBinaryHealExhausted only re-arms when a NEW release ships (no time-based
  // retry, unlike the shell suspension). The engine is just as dead as a
  // missing one; every sibling predicate in this file already treats
  // unreadable as needing replacement.
  const latest = { version: '1.0.0', binaryUrl: 'https://example/bin' };
  let downloads = 0;
  let state = { binaryHealVersion: '1.0.0', binaryHealAttempts: MAX_UPDATE_ATTEMPTS };
  for (let i = 0; i < 3; i++) {
    const r = await selfHealStaleBinary(latest, {
      state,
      needsUpdate: () => true,
      // present on disk, but unusable — exactly what the fixed default computes
      binaryPresent: () => false,
      download: async () => { downloads++; return false; },
    });
    state = { ...state, ...r.patch };
  }
  assert.equal(downloads, 3,
    'a corrupt binary must keep retrying — it is as dead as a missing one');
  assert.equal(state.binaryHealAttempts, MAX_UPDATE_ATTEMPTS,
    'and those failures must not inflate the stale counter');
});

test('the DEFAULT binaryPresent treats an unreadable cached binary as absent', () => {
  // Guards the default itself (an injection point can hide a wrong default).
  // Runs in a child with a sandbox HOME so the real ~/.cache/code-graph is
  // never touched: cachedBinaryPath() resolves from HOME.
  const { execFileSync } = require('node:child_process');
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-corrupt-bin-'));
  const out = execFileSync(process.execPath, ['-e', `
    const fs = require('fs'); const path = require('path');
    const { selfHealStaleBinary, cachedBinaryPath, MAX_UPDATE_ATTEMPTS } = require(${JSON.stringify(path.join(__dirname, 'auto-update.js'))});
    const p = cachedBinaryPath();
    fs.mkdirSync(path.dirname(p), { recursive: true });
    fs.writeFileSync(p, 'not-a-binary');       // present on disk, cannot run
    fs.chmodSync(p, 0o755);
    (async () => {
      let downloads = 0;
      let state = { binaryHealVersion: '1.0.0', binaryHealAttempts: MAX_UPDATE_ATTEMPTS };
      for (let i = 0; i < 2; i++) {
        const r = await selfHealStaleBinary({ version: '1.0.0', binaryUrl: 'https://example/bin' }, {
          state,
          needsUpdate: () => true,
          download: async () => { downloads++; return false; },   // DEFAULT binaryPresent
        });
        state = { ...state, ...r.patch };
      }
      process.stdout.write(String(downloads));
    })();
  `], { env: { ...process.env, HOME: home }, stdio: ['pipe', 'pipe', 'pipe'] }).toString();
  fs.rmSync(home, { recursive: true, force: true });
  assert.equal(out, '2',
    'the default must classify an unreadable cached binary as absent (unbounded recovery), not park it as stale');
});

test('shouldCheck stops bypassing the throttle once the binary heal is exhausted', () => {
  const minsAgo = (m) => new Date(Date.now() - m * 60 * 1000).toISOString();
  const fresh = { lastCheck: minsAgo(10), latestVersion: '1.0.0', updateAvailable: false };
  assert.equal(shouldCheck(fresh, { binaryStale: true }), true,
    'precondition: a stale binary normally bypasses the throttle');

  const spent = { ...fresh, binaryHealVersion: '1.0.0', binaryHealAttempts: MAX_UPDATE_ATTEMPTS };
  assert.equal(shouldCheck(spent, { binaryStale: true }), false,
    'an exhausted heal must fall back to the ordinary interval — the bypass had nothing left to do');

  // Re-armed by a newer release: the bypass is available again.
  const newTarget = { ...spent, latestVersion: '1.1.0' };
  assert.equal(shouldCheck(newTarget, { binaryStale: true }), true);

  // A MISSING binary still outranks everything (no engine at all is worse).
  assert.equal(shouldCheck(spent, { binaryMissing: true }), true);
});

test('parseLatestRelease selects the matching platform asset', () => {
  const latest = parseLatestRelease({
    tag_name: 'v1.2.3',
    tarball_url: 'https://example.com/tarball.tgz',
    assets: [
      { name: 'code-graph-mcp-linux-x64', browser_download_url: 'https://example.com/linux-x64' },
      { name: 'other', browser_download_url: 'https://example.com/other' },
    ],
  }, 'code-graph-mcp-linux-x64');
  // `pluginTarballUrl` is null here BY DESIGN: this fixture publishes no
  // claude-plugin.tar.gz, and that null is what makes downloadAndInstall refuse
  // to extract rather than fall back to the unchecksummed source tarball.

  assert.deepEqual(latest, {
    version: '1.2.3',
    tarballUrl: 'https://example.com/tarball.tgz',
    pluginTarballUrl: null,
    binaryUrl: 'https://example.com/linux-x64',
  });

  // And it IS picked up when the release publishes it.
  const withPlugin = parseLatestRelease({
    tag_name: 'v1.2.3',
    tarball_url: 'https://example.com/tarball.tgz',
    assets: [
      { name: 'code-graph-mcp-linux-x64', browser_download_url: 'https://example.com/linux-x64' },
      { name: PLUGIN_ASSET_NAME, browser_download_url: 'https://example.com/plugin.tgz' },
    ],
  }, 'code-graph-mcp-linux-x64');
  assert.equal(withPlugin.pluginTarballUrl, 'https://example.com/plugin.tgz');
});

// ── commandExists ──────────────────────────────────────────

test('commandExists returns true for a known command (node)', () => {
  assert.equal(commandExists('node'), true);
});

test('commandExists returns false for a non-existent command', () => {
  assert.equal(commandExists('__nonexistent_cmd_xyz_12345__'), false);
});

test('cachedBinaryPath returns expected platform binary path', () => {
  const p = cachedBinaryPath();
  const expectedName = process.platform === 'win32' ? 'code-graph-mcp.exe' : 'code-graph-mcp';
  assert.equal(path.basename(p), expectedName);
  assert.ok(p.includes('.cache') && p.includes('code-graph'),
    `expected cache path to live under ~/.cache/code-graph: ${p}`);
});

test('downloadBinary returns false for missing binaryUrl (no-op safety)', async () => {
  const result = await downloadBinary({ version: '1.0.0', binaryUrl: null });
  assert.equal(result, false);
});

test('downloadBinary returns false when latest is null', async () => {
  const result = await downloadBinary(null);
  assert.equal(result, false);
});

// ── Flag parsing ───────────────────────────────────────────

test('resolveProxy honors *_PROXY env vars, precedence, and NO_PROXY (L14)', () => {
  const U = 'https://api.github.com/repos/x/y/releases/latest';
  // No proxy configured → null (direct path unchanged for the common case).
  assert.equal(resolveProxy(U, {}), null);
  // HTTPS_PROXY selected; lowercase variant also honored.
  assert.equal(resolveProxy(U, { HTTPS_PROXY: 'http://p:8080' }), 'http://p:8080');
  assert.equal(resolveProxy(U, { https_proxy: 'http://p:3128' }), 'http://p:3128');
  // HTTP_PROXY is the fallback when no HTTPS_PROXY is present…
  assert.equal(resolveProxy(U, { HTTP_PROXY: 'http://p:1' }), 'http://p:1');
  // …but HTTPS_PROXY takes precedence over HTTP_PROXY.
  assert.equal(resolveProxy(U, { HTTPS_PROXY: 'http://s:1', HTTP_PROXY: 'http://h:2' }), 'http://s:1');
  // NO_PROXY: exact host, suffix (.github.com / *.github.com), and '*' all bypass.
  assert.equal(resolveProxy(U, { HTTPS_PROXY: 'http://p:1', NO_PROXY: 'api.github.com' }), null);
  assert.equal(resolveProxy(U, { HTTPS_PROXY: 'http://p:1', NO_PROXY: '.github.com' }), null);
  assert.equal(resolveProxy(U, { HTTPS_PROXY: 'http://p:1', NO_PROXY: '*.github.com' }), null);
  assert.equal(resolveProxy(U, { HTTPS_PROXY: 'http://p:1', no_proxy: '*' }), null);
  // NO_PROXY for an unrelated host does NOT bypass.
  assert.equal(resolveProxy(U, { HTTPS_PROXY: 'http://p:1', NO_PROXY: 'example.com' }), 'http://p:1');
  // Blank proxy value and unparseable target both yield null (no crash).
  assert.equal(resolveProxy(U, { HTTPS_PROXY: '   ' }), null);
  assert.equal(resolveProxy('not a url', { HTTPS_PROXY: 'http://p:1' }), null);
});

test('isInstallMissingMode detects --install-missing in argv', () => {
  assert.equal(isInstallMissingMode(['--install-missing']), true);
  assert.equal(isInstallMissingMode(['check', '--install-missing']), true);
  assert.equal(isInstallMissingMode(['check']), false);
  assert.equal(isInstallMissingMode([]), false);
});

test('isSilentMode honors --silent flag and CODE_GRAPH_AUTO_UPDATE_SILENT env', () => {
  assert.equal(isSilentMode(['--silent'], {}), true);
  assert.equal(isSilentMode([], { CODE_GRAPH_AUTO_UPDATE_SILENT: '1' }), true);
  assert.equal(isSilentMode([], {}), false);
});

test('fetchLatestRelease parses JSON without relying on global fetch', async () => {
  const latest = await fetchLatestRelease(async () => ({
    statusCode: 200,
    body: JSON.stringify({
      tag_name: 'v2.0.0',
      tarball_url: 'https://example.com/release.tgz',
      assets: [
        { name: 'code-graph-mcp-linux-x64', browser_download_url: 'https://example.com/bin' },
      ],
    }),
  }));

  assert.equal(latest.version, '2.0.0');
  assert.equal(latest.tarballUrl, 'https://example.com/release.tgz');
});
// ── refreshMarketplaceClone (v0.49.1 marketplace-staleness fix) ────────────

const { execFileSync: execGit } = require('child_process');
const { refreshMarketplaceClone, downloadAndInstall } = require('./auto-update');

function git(cwd, ...args) {
  return execGit('git', ['-C', cwd, '-c', 'user.email=t@t', '-c', 'user.name=t', ...args],
    { stdio: 'pipe', encoding: 'utf8', env: cleanGitEnv() });
}

test('git fixtures ignore inherited GIT_* env (H4 hermeticity)', (t) => {
  // A partial `git commit` runs the pre-commit hook with GIT_DIR / GIT_INDEX_FILE
  // exported into the environment. Every `git` this suite shells out to for its
  // tempdir fixtures would otherwise inherit them and mutate the REAL repo index
  // instead — v0.80.3 was this exact class, but that fix cleaned only the cargo
  // path in pre-commit.sh; this JS test section was the sibling hole (H4). The
  // git() helper must strip GIT_* so fixtures stay hermetic however the suite is
  // launched (hook, CI, or a direct `node --test`).
  const root = mkDir(t, 'code-graph-h4-');
  const bogus = path.join(root, 'bogus-gitdir');
  const saved = process.env.GIT_DIR;
  process.env.GIT_DIR = bogus;
  t.after(() => { if (saved === undefined) delete process.env.GIT_DIR; else process.env.GIT_DIR = saved; });

  const repo = path.join(root, 'repo');
  fs.mkdirSync(repo);
  git(repo, 'init', '-q', '-b', 'main');

  assert.ok(fs.existsSync(path.join(repo, '.git')),
    'git init must create repo/.git, not honor the inherited GIT_DIR');
  assert.ok(!fs.existsSync(bogus),
    'the inherited GIT_DIR must be ignored (no repo created there)');
});

test('refreshMarketplaceClone fast-forwards a stale clone', (t) => {
  const root = mkDir(t, 'code-graph-mp-');
  const remote = path.join(root, 'remote');
  const clone = path.join(root, 'clone');

  fs.mkdirSync(remote);
  git(remote, 'init', '-q', '-b', 'main');
  fs.writeFileSync(path.join(remote, 'marketplace.json'), '{"version":"0.48.0"}');
  git(remote, 'add', '.');
  git(remote, 'commit', '-q', '-m', 'v0.48.0');
  execGit('git', ['clone', '-q', remote, clone], { stdio: 'pipe' });

  // Remote advances (a release bumped marketplace.json) — clone is now stale.
  fs.writeFileSync(path.join(remote, 'marketplace.json'), '{"version":"0.49.0"}');
  git(remote, 'commit', '-q', '-am', 'v0.49.0');

  assert.equal(refreshMarketplaceClone({ dir: clone }), true);
  assert.match(fs.readFileSync(path.join(clone, 'marketplace.json'), 'utf8'), /0\.49\.0/);
});

test('refreshMarketplaceClone is a safe no-op on non-git dirs and pull failures', (t) => {
  const root = mkDir(t, 'code-graph-mp-');
  // Not a git repo → false, no throw.
  assert.equal(refreshMarketplaceClone({ dir: root }), false);
  // Missing dir → false, no throw.
  assert.equal(refreshMarketplaceClone({ dir: path.join(root, 'nope') }), false);
  // exec throws (diverged / dirty clone) → false, no throw.
  const fakeGitDir = path.join(root, 'repo');
  fs.mkdirSync(path.join(fakeGitDir, '.git'), { recursive: true });
  assert.equal(refreshMarketplaceClone({
    dir: fakeGitDir,
    exec: () => { throw new Error('not a fast-forward'); },
  }), false);
});

test('downloadAndInstall wires the marketplace refresh + binary download (orchestration glue)', async (t) => {
  // In-process with all side-effectful deps injected would still write the
  // manifest into the REAL ~/.cache (CACHE_DIR is bound at module load), so
  // run in a subprocess with a sandboxed HOME — same pattern as install-e2e.
  const sandboxHome = mkDir(t, 'code-graph-dai-');
  const script = `
    const fs = require('fs');
    const path = require('path');
    const { downloadAndInstall } = require(${JSON.stringify(path.join(__dirname, 'auto-update.js'))});
    const crypto = require('crypto');
    const latest = {
      version: '9.9.9',
      tarballUrl: 'https://example/tar',
      pluginTarballUrl: 'https://example/claude-plugin.tar.gz',
      binaryUrl: null,
    };
    const calls = [];
    let tarCall = null;
    // The stub has to satisfy the integrity gate now: write the archive, then
    // write ITS OWN digest as the sidecar. A stub that skipped the sidecar would
    // be exercising the refusal path, not the install path.
    const exec = (cmd, args, opts) => {
      calls.push(cmd);
      if (cmd === 'curl') {
        const out = args[args.indexOf('-o') + 1];
        if (out.endsWith('.sha256')) {
          const archive = out.slice(0, -'.sha256'.length);
          const sha = crypto.createHash('sha256').update(fs.readFileSync(archive)).digest('hex');
          fs.writeFileSync(out, sha + '  ' + path.basename(archive));
        } else {
          fs.writeFileSync(out, 'not-a-real-gzip-but-hashable');
        }
      }
      if (cmd === 'tar') {
        tarCall = { args, opts };
        // Simulate extraction: produce claude-plugin/ with a matching version.
        // Extraction target comes from opts.cwd — the archive is named
        // RELATIVELY (see the assertions below).
        const tmpDir = opts.cwd;
        const mDir = path.join(tmpDir, 'claude-plugin', '.claude-plugin');
        fs.mkdirSync(mDir, { recursive: true });
        fs.writeFileSync(path.join(mDir, 'plugin.json'), JSON.stringify({ version: '9.9.9' }));
      }
    };
    (async () => {
      let refreshed = 0;
      let binDownloads = 0;
      const result = await downloadAndInstall(latest, {
        exec,
        cmdExists: () => true, // don't depend on host curl/tar
        refreshMarketplace: () => { refreshed++; return true; },
        downloadBin: async () => { binDownloads++; return true; },
      });
      console.log(JSON.stringify({ result, refreshed, binDownloads, calls, tarCall }));
    })();
  `;
  const out = execGit(process.execPath, ['-e', script], {
    env: { ...process.env, HOME: sandboxHome },
    encoding: 'utf8',
  });
  const { result, refreshed, binDownloads, tarCall } = JSON.parse(out.trim().split('\n').pop());
  assert.equal(result.pluginUpdated, true, 'plugin files must install from the extracted tarball');
  // Issue #40 / #34-#35 family: GNU tar (git-for-Windows, MSYS) reads the drive
  // letter in `C:\...\claude-plugin.tar.gz` as a REMOTE HOST and refuses to
  // open it, which is what made plugin updates permanently unachievable there.
  // The portable spelling is a relative archive name resolved via `cwd` — so
  // assert on the SHAPE (no path separators, no colon, no -C), not just that
  // extraction happened.
  assert.ok(tarCall, 'tar must be invoked to extract the plugin asset');
  assert.equal(tarCall.args.includes('-C'), false, 'tar must not take -C with an absolute dir');
  assert.equal(typeof tarCall.opts.cwd, 'string', 'tar must extract via opts.cwd');
  for (const a of tarCall.args) {
    assert.equal(/[\\/:]/.test(a), false, `tar arg must stay relative/flag-only, got: ${a}`);
  }
  assert.equal(refreshed, 1, 'marketplace refresh must run exactly once after a plugin update');
  assert.equal(result.marketplaceRefreshed, true);
  assert.equal(binDownloads, 1, 'binary download must run');
  assert.equal(result.binaryUpdated, true);
  // Plugin landed in the sandboxed cache, not the real one.
  const dst = path.join(sandboxHome, '.claude', 'plugins', 'cache',
    'code-graph-mcp', 'code-graph-mcp', '9.9.9', '.claude-plugin', 'plugin.json');
  assert.equal(fs.existsSync(dst), true, 'plugin copied into sandbox plugins cache');
});

test('downloadAndInstall does NOT repoint install state when the plugin copy is skipped (version drift)', async (t) => {
  // Guard for a silent-breakage bug: when the extracted tarball's plugin.json version
  // doesn't match latest.version, the copy is skipped and pluginDst is never created.
  // installed_plugins.json must NOT be advanced to that nonexistent dir, or Claude Code
  // ends up pointed at a missing install while state reads "up to date".
  const sandboxHome = mkDir(t, 'code-graph-dai-skip-');
  const installedDir = path.join(sandboxHome, '.claude', 'plugins');
  fs.mkdirSync(installedDir, { recursive: true });
  const installedPath = path.join(installedDir, 'installed_plugins.json');
  fs.writeFileSync(installedPath, JSON.stringify({
    plugins: { 'code-graph-mcp@code-graph-mcp': [
      { installPath: '/old/install/path', version: '0.0.1', lastUpdated: 'seed' },
    ] },
  }));

  const script = `
    const fs = require('fs');
    const path = require('path');
    const { downloadAndInstall } = require(${JSON.stringify(path.join(__dirname, 'auto-update.js'))});
    const latest = { version: '9.9.9', tarballUrl: 'https://example/tar', binaryUrl: null };
    const exec = (cmd, args) => {
      if (cmd === 'tar') {
        // Extract a claude-plugin/ whose version DRIFTS from latest → copy is skipped.
        const tmpDir = args[args.indexOf('-C') + 1];
        const mDir = path.join(tmpDir, 'claude-plugin', '.claude-plugin');
        fs.mkdirSync(mDir, { recursive: true });
        fs.writeFileSync(path.join(mDir, 'plugin.json'), JSON.stringify({ version: '0.0.0' }));
      }
    };
    (async () => {
      const result = await downloadAndInstall(latest, {
        exec,
        cmdExists: () => true, // don't depend on host curl/tar — exercise the guard deterministically
        refreshMarketplace: () => true,
        downloadBin: async () => true,
      });
      console.log(JSON.stringify({ result }));
    })();
  `;
  const out = execGit(process.execPath, ['-e', script], {
    env: { ...process.env, HOME: sandboxHome },
    encoding: 'utf8',
  });
  const { result } = JSON.parse(out.trim().split('\n').pop());
  assert.equal(result.pluginUpdated, false, 'version drift must skip the plugin copy');

  // The pre-seeded record must be UNTOUCHED — not repointed to the 9.9.9 dir.
  const rec = JSON.parse(fs.readFileSync(installedPath, 'utf8'))
    .plugins['code-graph-mcp@code-graph-mcp'][0];
  assert.equal(rec.installPath, '/old/install/path',
    'installPath must not be repointed when the copy was skipped');
  assert.equal(rec.version, '0.0.1',
    'version must not be advanced when the copy was skipped');
});

// ── selfHealGlobalPkgs: keep global npm installs (CLI shim, platform relic) in step ──
// The drift it pins: the `code-graph-mcp` CLI on PATH is the GLOBAL
// @sdsrs/code-graph package, untouched by /plugin update or the binary
// self-heal — observed at 0.46.0 while the plugin ran 0.101.0; and an
// explicitly-installed top-level platform pkg relic (0.16.6) was the landmine
// behind the MCP connect-timeout incident.

test('selfHealGlobalPkgs refreshes stale globals and resets the attempt counter', async () => {
  const latest = { version: '0.101.0' };
  let installedSpecs = null;
  // `readStale` reflects the world: stale before the install, clean after. The
  // counter resets on the SECOND reading, not on npm's exit code.
  let healed = false;
  const patch = await selfHealGlobalPkgs(latest, {}, {
    readStale: () => (healed ? [] : [{ name: '@sdsrs/code-graph', version: '0.46.0' }]),
    install: async (specs) => { installedSpecs = specs; healed = true; return true; },
  });
  assert.deepEqual(installedSpecs, ['@sdsrs/code-graph@0.101.0']);
  assert.deepEqual(patch, { globalPkgHealVersion: '0.101.0', globalPkgHealAttempts: 0 });
});

test('selfHealGlobalPkgs counts an install that exits 0 but heals nothing as a failure (P2-22)', async () => {
  // `npm i -g` installs into the prefix the CURRENT node resolves. Under nvm
  // with several node versions — or an `npm --prefix` in the user's npmrc — that
  // is not where the stale copy lives, so npm exits 0 and the stale package is
  // exactly where it was. Trusting the exit code reset the counter every run,
  // and the retry budget could never be spent: one npm install per throttle
  // window, forever, with nothing to show for it.
  const latest = { version: '0.101.0' };
  const stillStale = () => [{ name: '@sdsrs/code-graph', version: '0.46.0' }];
  let runs = 0;
  const patch = await selfHealGlobalPkgs(latest, {}, {
    readStale: stillStale,
    install: async () => { runs += 1; return true; },
  });
  assert.equal(runs, 1, 'the heal is still attempted once');
  assert.deepEqual(patch, { globalPkgHealVersion: '0.101.0', globalPkgHealAttempts: 1 },
    'an unverified "success" must consume an attempt, or the cap never bites');

  // And the cap does bite, so the loop terminates.
  let touched = false;
  const capped = await selfHealGlobalPkgs(
    latest,
    { globalPkgHealVersion: '0.101.0', globalPkgHealAttempts: 3 },
    { readStale: stillStale, install: async () => { touched = true; return true; } },
  );
  assert.equal(touched, false, 'a repeatedly-ineffective heal must stop being attempted');
  assert.deepEqual(capped, {});
});

test('selfHealGlobalPkgs never installs when nothing of ours is globally installed', async () => {
  let touched = false;
  const patch = await selfHealGlobalPkgs({ version: '0.101.0' }, {}, {
    readStale: () => [],
    install: async () => { touched = true; return true; },
  });
  assert.equal(touched, false, 'no global install → no npm run (never introduce one)');
  assert.deepEqual(patch, {});
});

test('selfHealGlobalPkgs clears a leftover counter once globals are healthy', async () => {
  const patch = await selfHealGlobalPkgs(
    { version: '0.101.0' },
    { globalPkgHealVersion: '0.101.0', globalPkgHealAttempts: 2 },
    { readStale: () => [], install: async () => true },
  );
  assert.deepEqual(patch, { globalPkgHealAttempts: 0, globalPkgHealVersion: null });
});

test('selfHealGlobalPkgs counts failures per target version and stops at the cap', async () => {
  const latest = { version: '0.101.0' };
  const failInstall = async () => false;
  const stale = () => [{ name: '@sdsrs/code-graph', version: '0.46.0' }];

  // Failure increments the counter for THIS target version.
  const p1 = await selfHealGlobalPkgs(latest, {}, { readStale: stale, install: failInstall });
  assert.deepEqual(p1, { globalPkgHealVersion: '0.101.0', globalPkgHealAttempts: 1 });

  // At the cap, install is no longer attempted for the same target.
  let touched = false;
  const p2 = await selfHealGlobalPkgs(
    latest,
    { globalPkgHealVersion: '0.101.0', globalPkgHealAttempts: 3 },
    { readStale: stale, install: async () => { touched = true; return true; } },
  );
  assert.equal(touched, false, 'capped target must not retry');
  assert.deepEqual(p2, {});

  // A NEW release re-arms the counter.
  let specs = null;
  let healed = false;
  const p3 = await selfHealGlobalPkgs(
    { version: '0.102.0' },
    { globalPkgHealVersion: '0.101.0', globalPkgHealAttempts: 3 },
    { readStale: () => (healed ? [] : [{ name: '@sdsrs/code-graph', version: '0.46.0' }]),
      install: async (s) => { specs = s; healed = true; return true; } },
  );
  assert.deepEqual(specs, ['@sdsrs/code-graph@0.102.0']);
  assert.deepEqual(p3, { globalPkgHealVersion: '0.102.0', globalPkgHealAttempts: 0 });
});

test('staleGlobalPkgs / globalPkgVersion read top-level global installs from disk', (t) => {
  const root = mkDir(t, 'global-pkgs-');
  const shellDir = path.join(root, '@sdsrs', 'code-graph');
  fs.mkdirSync(shellDir, { recursive: true });
  fs.writeFileSync(path.join(shellDir, 'package.json'), JSON.stringify({ version: '0.46.0' }));

  assert.equal(globalPkgVersion('@sdsrs/code-graph', [root]), '0.46.0');
  assert.equal(globalPkgVersion('@sdsrs/does-not-exist', [root]), null);

  const stale = staleGlobalPkgs('0.101.0', [root]);
  assert.deepEqual(stale, [{ name: '@sdsrs/code-graph', version: '0.46.0' }]);
  assert.deepEqual(staleGlobalPkgs('0.46.0', [root]), [],
    'a global install matching latest is not stale');
});

// ── P2.1: throttle-path global heal reach (RCA 2026-07-24) ──────────────────
// The post-fetch heal never runs on the throttle early-return; the only context
// that can SEE the user's nvm/global prefix is a throttled CLI run — so a stale
// global shim (0.101.0 while the binary reached 0.103.0) never healed. This
// predicate is what lets the throttle path still attempt it.

test('shouldHealGlobalsOnThrottle: true when a target version is known and a global is stale', () => {
  const yes = shouldHealGlobalsOnThrottle(
    { latestVersion: '0.103.0' },
    { readStale: () => [{ name: '@sdsrs/code-graph', version: '0.101.0' }] });
  assert.equal(yes, true);
});

test('shouldHealGlobalsOnThrottle: false when nothing of ours is globally stale', () => {
  const no = shouldHealGlobalsOnThrottle(
    { latestVersion: '0.103.0' },
    { readStale: () => [] });
  assert.equal(no, false, 'no stale global → no npm path on the hot throttle branch');
});

test('shouldHealGlobalsOnThrottle: false before a latest version is ever known', () => {
  assert.equal(shouldHealGlobalsOnThrottle({}, { readStale: () => [{ name: 'x', version: '0' }] }), false);
  assert.equal(shouldHealGlobalsOnThrottle(null, { readStale: () => [] }), false);
});

test('shouldHealGlobalsOnThrottle: false when the parent launcher already holds the install lock', () => {
  const prev = process.env.CODE_GRAPH_INSTALL_LOCK_HELD;
  process.env.CODE_GRAPH_INSTALL_LOCK_HELD = '1';
  try {
    assert.equal(shouldHealGlobalsOnThrottle(
      { latestVersion: '0.103.0' },
      { readStale: () => [{ name: '@sdsrs/code-graph', version: '0.101.0' }] }), false,
      'must not contend for the lock the launcher parent already holds (deadlock/double-npm)');
  } finally {
    if (prev === undefined) delete process.env.CODE_GRAPH_INSTALL_LOCK_HELD;
    else process.env.CODE_GRAPH_INSTALL_LOCK_HELD = prev;
  }
});

// ── inactiveNodeGlobalRelics: our globals stranded under a non-active node ──

test('inactiveNodeGlobalRelics: reports our global under a non-active node prefix, skips the active one', (t) => {
  const root = mkDir(t, 'nvm-relics-');
  const activeDir = path.join(root, 'v24.18.0', 'lib', 'node_modules');
  const relicDir = path.join(root, 'v24.11.1', 'lib', 'node_modules');
  for (const [dir, version] of [[activeDir, '0.103.0'], [relicDir, '0.46.0']]) {
    const pkg = path.join(dir, '@sdsrs', 'code-graph');
    fs.mkdirSync(pkg, { recursive: true });
    fs.writeFileSync(path.join(pkg, 'package.json'), JSON.stringify({ version }));
  }
  const relics = inactiveNodeGlobalRelics({ dirs: [activeDir, relicDir], activeDir });
  assert.deepEqual(relics, [{ name: '@sdsrs/code-graph', version: '0.46.0', nodeModulesDir: relicDir }],
    'the active node prefix is not a relic; only the stranded one is reported');
});

test('inactiveNodeGlobalRelics: empty when our global lives only under the active node', (t) => {
  const root = mkDir(t, 'nvm-norelic-');
  const activeDir = path.join(root, 'v24.18.0', 'lib', 'node_modules');
  const pkg = path.join(activeDir, '@sdsrs', 'code-graph');
  fs.mkdirSync(pkg, { recursive: true });
  fs.writeFileSync(path.join(pkg, 'package.json'), JSON.stringify({ version: '0.103.0' }));
  assert.deepEqual(inactiveNodeGlobalRelics({ dirs: [activeDir], activeDir }), []);
});

// ── getPlatformAssetName: libc gating ───────────────────────────────────────

test('getPlatformAssetName returns null on musl (no published asset → no futile download)', () => {
  // Alpine: the glibc build downloads fine but cannot exec, so promote always
  // rejected it and every SessionStart re-pulled ~40MB forever.
  assert.equal(getPlatformAssetName({ platform: 'linux', arch: 'x64', libc: 'musl' }), null);
  assert.equal(getPlatformAssetName({ platform: 'linux', arch: 'x64', libc: 'glibc' }),
    'code-graph-mcp-linux-x64');
  assert.equal(getPlatformAssetName({ platform: 'win32', arch: 'x64', libc: 'glibc' }),
    'code-graph-mcp-win32-x64.exe');
});

// ── cachedBinaryNeedsUpdate / cachedBinaryStaleVsState: ordered compare ─────

test('cached binary NEWER than latest is not downgraded; unreadable is healed', (t) => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-newer-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const binaryPath = path.join(dir, 'code-graph-mcp');
  fs.writeFileSync(binaryPath, 'x');
  const latest = { version: '1.0.0', binaryUrl: 'https://example.com/bin' };

  // Newer than releases/latest (dev build / API lagging a publish) → keep it.
  assert.equal(
    cachedBinaryNeedsUpdate(latest, { binaryPath, readVersion: () => '9.9.9' }),
    false, 'a newer binary must not be replaced by an older release');
  // Unreadable --version → broken → let the heal replace it.
  assert.equal(
    cachedBinaryNeedsUpdate(latest, { binaryPath, readVersion: () => null }),
    true);

  const state = { latestVersion: '1.0.0' };
  assert.equal(
    cachedBinaryStaleVsState(state, { binaryPath, readVersion: () => '9.9.9' }),
    false, 'newer-than-state must not bypass the throttle');
  assert.equal(
    cachedBinaryStaleVsState(state, { binaryPath, readVersion: () => null }),
    true, 'unreadable binary bypasses the throttle so the heal can run');
});

// ── 403 rate-limit backoff survives the check that triggered it ─────────────
//
// `fetchLatestRelease` writes `rateLimited: true` on a GitHub 403, and
// `shouldCheck` reads it to hold off for RATE_LIMIT_INTERVAL_MS (1h). Between
// those two, `checkForUpdate` took its state snapshot BEFORE the fetch and then
// wrote `{ ...state, lastCheck: now }` on the null return — spreading the stale
// snapshot straight over the flag the fetch had just set. The backoff was dead
// code from the day it was written: every 403 refreshed `lastCheck` and cleared
// `rateLimited`, so the next tick hit GitHub on the ordinary interval while
// already rate-limited.
//
// Driven through a child process because CACHE_DIR (and therefore the state
// file) is `os.homedir()/.cache/code-graph`, resolved at module load: the parent
// has already resolved it against the REAL home, and this test must not write
// there. `requestJsonFn` is the injected 403 — no network.
test('a GitHub 403 leaves the rate-limit backoff armed after checkForUpdate returns', (t) => {
  const { spawnSync } = require('child_process');
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-au-403-home-'));
  t.after(() => fs.rmSync(home, { recursive: true, force: true }));

  const autoUpdate = path.join(__dirname, 'auto-update.js');
  const script = `
    const au = require(${JSON.stringify(autoUpdate)});
    (async () => {
      await au.checkForUpdate({
        installMissing: true,
        force: true,
        requestJsonFn: async () => ({ statusCode: 403, body: '' }),
      });
      process.stdout.write(JSON.stringify(au.readState()));
    })().catch(e => { process.stderr.write(String(e)); process.exit(1); });
  `;
  const r = spawnSync(process.execPath, ['-e', script], {
    env: { ...cleanGitEnv(), HOME: home, CLAUDE_CONFIG_DIR: path.join(home, '.claude') },
    encoding: 'utf8',
    timeout: 30000,
  });
  assert.equal(r.status, 0, `child failed: ${r.stderr}`);

  const state = JSON.parse(r.stdout);
  assert.equal(state.rateLimited, true,
    'the 403 flag must survive the saveState on checkForUpdate\'s null-return path');
  // The flag only matters through shouldCheck — assert the behaviour, not just
  // the field, or a rename leaves this passing while the backoff stays dead.
  assert.equal(shouldCheck(state), false,
    'with rateLimited set and lastCheck just now, the next check must back off');
  assert.equal(shouldCheck(state, { force: true }), false,
    'the backoff outranks force — a session start must not hammer a 403');

  // And it RECOVERS. The backoff arm sits above the force arm, so if the window
  // were wrong the stall would be silent and total: `--force` no-ops for its
  // whole duration. It is one hour because that is GitHub's unauthenticated
  // reset window; 24h was a constant that had never run (the flag was erased on
  // the same call that set it) and became load-bearing only when that was fixed.
  const hourAgo = new Date(Date.now() - 61 * 60 * 1000).toISOString();
  assert.equal(shouldCheck({ ...state, lastCheck: hourAgo }), true,
    'an hour after the 403 the backoff must clear on its own');
  const halfHourAgo = new Date(Date.now() - 30 * 60 * 1000).toISOString();
  assert.equal(shouldCheck({ ...state, lastCheck: halfHourAgo }), false,
    'half an hour is still inside the window');
});

// `readState()` was `readJson(STATE_FILE) || {}` — the lossy-read shape the
// audit swept for elsewhere, still live on the one file that holds THREE
// independent give-up budgets. Collapsing "unreadable" into "fresh install"
// re-arms all of them at once: the update suspension (updateAttempts), the
// binary self-heal budget (binaryHealAttempts) and the GitHub rate-limit
// backoff (rateLimited + lastCheck). A single corrupt/unreadable cache file
// therefore turned every one of those guards off, silently and permanently —
// each of which exists to stop an unbounded retry loop.
//
// Child process + sandboxed HOME: CACHE_DIR is resolved at module load.
test('an UNREADABLE update state does not silently re-arm the give-up budgets', (t) => {
  const { spawnSync } = require('child_process');
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-au-badstate-'));
  t.after(() => fs.rmSync(home, { recursive: true, force: true }));

  const autoUpdate = path.join(__dirname, 'auto-update.js');
  const script = `
    const fs = require('fs'); const path = require('path');
    const au = require(${JSON.stringify(autoUpdate)});
    const { CACHE_DIR } = require(${JSON.stringify(path.join(__dirname, 'lifecycle.js'))});
    const stateFile = path.join(CACHE_DIR, 'update-state.json');
    fs.mkdirSync(CACHE_DIR, { recursive: true });
    fs.writeFileSync(stateFile, '{ this is not json');
    const state = au.readState();
    let fetched = 0;
    (async () => {
      await au.checkForUpdate({
        installMissing: true, force: true,
        requestJsonFn: async () => { fetched++; return { statusCode: 200, body: '{}' }; },
      });
      process.stdout.write(JSON.stringify({
        state,
        fetched,
        afterRaw: fs.readFileSync(stateFile, 'utf8'),
      }));
    })().catch(e => { process.stderr.write(String(e)); process.exit(1); });
  `;
  const r = spawnSync(process.execPath, ['-e', script], {
    env: { ...cleanGitEnv(), HOME: home, CLAUDE_CONFIG_DIR: path.join(home, '.claude') },
    encoding: 'utf8',
    timeout: 30000,
  });
  assert.equal(r.status, 0, `child failed: ${r.stderr}`);
  const out = JSON.parse(r.stdout);

  // 1. The read must not pretend the file was absent.
  assert.ok(out.state.stateUnreadable,
    `an unparseable state must be flagged, got ${JSON.stringify(out.state)}`);

  // 2. Behaviour, not just the field: an unreadable state must not send us to
  //    GitHub (and from there into the download path) on this session.
  assert.equal(out.fetched, 0,
    'a state we could not read must not authorise a fetch — that is how the ' +
    'rate-limit backoff and the suspension get bypassed');

  // 3. It self-heals: the cache file is rewritten as valid JSON so the NEXT
  //    session starts from a real state instead of looping here forever.
  const after = JSON.parse(out.afterRaw);
  assert.ok(after.lastCheck, `state must be rewritten with a lastCheck: ${out.afterRaw}`);
  assert.equal(after.stateUnreadable, undefined,
    'the in-memory marker must never be persisted');
});

// ── Plugin tarball integrity is fail-closed ────────────────────────────────
//
// This chain extracts an archive and copies its JAVASCRIPT into the plugin
// cache, where Claude Code runs it as hooks on every tool call. It used to pull
// GitHub's auto-generated source `tarball_url`, for which no checksum is
// published anywhere — the only one of the four download chains with zero
// integrity verification, and the only one whose payload becomes executed code
// (audit 2026-07-27 P2-23). release.yml now publishes claude-plugin.tar.gz with
// a .sha256 sidecar and this refuses to extract without a match.
//
// The assertion that matters is `tar` never running: a refusal that still
// extracted would have written the untrusted JS to disk before deciding.
for (const [label, mutate] of [
  ['no sidecar published', (o) => { if (o.endsWith('.sha256')) throw new Error('404'); }],
  ['sidecar does not match', (o, fsMod) => {
    if (o.endsWith('.sha256')) fsMod.writeFileSync(o, 'de'.repeat(32));
  }],
  ['release publishes no plugin asset', null],
]) {
  test(`downloadAndInstall refuses to extract the plugin tarball when the ${label}`, (t) => {
    const sandboxHome = mkDir(t, 'code-graph-int-');
    const noAsset = mutate === null;
    const script = `
      const fs = require('fs');
      const path = require('path');
      const crypto = require('crypto');
      const { downloadAndInstall } = require(${JSON.stringify(path.join(__dirname, 'auto-update.js'))});
      const mutate = ${mutate ? mutate.toString() : 'null'};
      const latest = {
        version: '9.9.9',
        tarballUrl: 'https://example/tar',
        pluginTarballUrl: ${noAsset ? 'null' : "'https://example/claude-plugin.tar.gz'"},
        binaryUrl: null,
      };
      const calls = [];
      const exec = (cmd, args) => {
        calls.push(cmd);
        if (cmd === 'curl') {
          const out = args[args.indexOf('-o') + 1];
          if (mutate) { mutate(out, fs); }
          if (!out.endsWith('.sha256')) fs.writeFileSync(out, 'payload');
          else if (!fs.existsSync(out)) {
            const archive = out.slice(0, -'.sha256'.length);
            fs.writeFileSync(out, crypto.createHash('sha256').update(fs.readFileSync(archive)).digest('hex'));
          }
        }
        if (cmd === 'tar') {
          const tmpDir = args[args.indexOf('-C') + 1];
          const mDir = path.join(tmpDir, 'claude-plugin', '.claude-plugin');
          fs.mkdirSync(mDir, { recursive: true });
          fs.writeFileSync(path.join(mDir, 'plugin.json'), JSON.stringify({ version: '9.9.9' }));
        }
      };
      (async () => {
        const result = await downloadAndInstall(latest, {
          exec,
          cmdExists: () => true,
          refreshMarketplace: () => true,
          downloadBin: async () => true,
        });
        console.log(JSON.stringify({ result, calls }));
      })();
    `;
    const out = execGit(process.execPath, ['-e', script], {
      env: { ...process.env, HOME: sandboxHome },
      encoding: 'utf8',
    });
    const { result, calls } = JSON.parse(out.trim().split('\n').pop());
    assert.equal(result.pluginUpdated, false, `${label}: plugin must not be installed`);
    assert.equal(calls.includes('tar'), false,
      `${label}: refused BEFORE extraction — untrusted JS must never reach disk`);
    // The binary chain has its own integrity gate and still runs, so a bad
    // plugin asset does not strand the user on an old binary.
    assert.equal(result.binaryUpdated, true, `${label}: binary update still proceeds`);
    const dst = path.join(sandboxHome, '.claude', 'plugins', 'cache',
      'code-graph-mcp', 'code-graph-mcp', '9.9.9');
    assert.equal(fs.existsSync(dst), false, `${label}: nothing copied into the plugin cache`);
  });
}

// ── Failed-update backoff (issue #40) ───────────────────────────────────────
//
// `updateAttempts` was counted from the first release that shipped it and never
// read by anything but the statusline. So an update that CANNOT succeed on a
// given machine — a GNU tar that refuses `C:\...`, a locked plugin cache, a
// full disk — re-ran the whole download chain on every single session, forever:
// the field report saw `updateAttempts: 8` and climbing, with a burst of
// console windows each time. The counter is now per-target-version and the
// download chain stops once it hits MAX_UPDATE_ATTEMPTS.
//
// Child process + sandboxed HOME because CACHE_DIR is resolved from
// os.homedir() at module load. `PATH: ''` makes the run hermetic: curl/tar/npm
// resolve to nothing, so downloadAndInstall takes its missing-tools failure arm
// and the global-npm heal short-circuits — no network, no global installs, and
// the failure path under test is reached deterministically.
function runCheckWithState(t, seedState, { installedVersion = '1.0.0', tag = 'v9.9.9' } = {}) {
  const { spawnSync } = require('child_process');
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-au-backoff-'));
  t.after(() => fs.rmSync(home, { recursive: true, force: true }));
  const cacheDir = path.join(home, '.cache', 'code-graph');
  fs.mkdirSync(cacheDir, { recursive: true });
  fs.writeFileSync(path.join(cacheDir, 'update-state.json'), JSON.stringify(seedState));
  fs.writeFileSync(path.join(cacheDir, 'install-manifest.json'),
    JSON.stringify({ version: installedVersion, config: {} }));

  const script = `
    const au = require(${JSON.stringify(path.join(__dirname, 'auto-update.js'))});
    (async () => {
      const result = await au.checkForUpdate({
        installMissing: true, // bypass the dev-mode gate; this repo IS a dev tree
        force: true,
        requestJsonFn: async () => ({
          statusCode: 200,
          body: JSON.stringify({ tag_name: ${JSON.stringify(tag)}, tarball_url: 'https://127.0.0.1:1/t', assets: [] }),
        }),
      });
      process.stdout.write(JSON.stringify({ result, state: au.readState() }));
    })().catch(e => { process.stderr.write(String(e && e.stack || e)); process.exit(1); });
  `;
  const r = spawnSync(process.execPath, ['-e', script], {
    env: { ...cleanGitEnv(), HOME: home, CLAUDE_CONFIG_DIR: path.join(home, '.claude'), PATH: '' },
    encoding: 'utf8',
    timeout: 60000,
  });
  assert.equal(r.status, 0, `child failed: ${r.stderr}`);
  return JSON.parse(r.stdout.trim().split('\n').pop());
}

test('a repeatedly failing update keeps retrying below the attempt cap', (t) => {
  const { result, state } = runCheckWithState(t,
    { latestVersion: '9.9.9', updateAvailable: true, updateAttempts: MAX_UPDATE_ATTEMPTS - 1 });
  assert.notEqual(result.suspended, true, 'one attempt short of the cap must still try');
  assert.equal(state.updateAttempts, MAX_UPDATE_ATTEMPTS, 'a failed attempt increments the counter');
  assert.equal(state.updateAvailable, true);
});

test('a failing update is SUSPENDED once it hits the attempt cap', (t) => {
  const { result, state } = runCheckWithState(t,
    { latestVersion: '9.9.9', updateAvailable: true, updateAttempts: MAX_UPDATE_ATTEMPTS });
  assert.equal(result.suspended, true, 'at the cap the download chain must stop running');
  assert.equal(result.updateAvailable, true, 'the update is still reported as available');
  assert.equal(state.updateAttempts, MAX_UPDATE_ATTEMPTS,
    'a suspended cycle must not keep inflating the counter');
  assert.ok(state.lastCheck, 'the check itself still happened (cheap, network-only)');
});

test('a NEWER release re-arms the attempt budget (counter is per target version)', (t) => {
  // Seeded above the cap, but for the PREVIOUS version — a fresh release must
  // never inherit the old one's exhausted budget.
  const { result, state } = runCheckWithState(t,
    { latestVersion: '9.9.8', updateAvailable: true, updateAttempts: MAX_UPDATE_ATTEMPTS + 4 });
  assert.notEqual(result.suspended, true, 'a new target version must be attempted');
  assert.equal(state.updateAttempts, 1, 'the counter restarts at 1 for the new version');
  assert.equal(state.latestVersion, '9.9.9');
});

test('statusline STUCK_UPDATE_ATTEMPTS matches auto-update MAX_UPDATE_ATTEMPTS', () => {
  // Two files, one number: the statusline stops promising "↻ updating" at
  // STUCK_UPDATE_ATTEMPTS and the updater stops trying at MAX_UPDATE_ATTEMPTS.
  // If they drift apart, one of the two states is a lie — either the statusline
  // claims an in-flight self-heal that has been abandoned, or it goes quiet
  // while retries are still running.
  const src = fs.readFileSync(path.join(__dirname, 'statusline.js'), 'utf8');
  const m = /STUCK_UPDATE_ATTEMPTS\s*=\s*(\d+)/.exec(src);
  assert.ok(m, 'statusline.js must still define STUCK_UPDATE_ATTEMPTS');
  assert.equal(Number(m[1]), MAX_UPDATE_ATTEMPTS);
});

// ── Documented opt-out: CODE_GRAPH_NO_AUTO_UPDATE=1 (issue #40) ─────────────
//
// Until this existed the only working opt-out was the accidental one —
// CODE_GRAPH_DEV=1, which also rewires binary resolution. Both arms run from a
// COPY of the plugin tree in a tmpdir: in the real repo isDevMode() is true
// (Cargo.toml + target/ at the parent), which would make the opt-out arm pass
// for the wrong reason.
test('CODE_GRAPH_NO_AUTO_UPDATE=1 skips the update check entirely', (t) => {
  const { spawnSync } = require('child_process');
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-au-optout-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  // Copy regular files and directories only. `.gitignore`'s blanket `.*` lets a
  // dot-directory appear next to the sources, grow, and never show up in
  // `git status`. One did: a `.claude/` whose `hooks` entry is a CHARACTER
  // DEVICE, which `cpSync` cannot copy — EINVAL, and a test failing for a reason
  // that has nothing to do with what it asserts (2026-08-16 audit §四). Filtering
  // by TYPE, not by leading dot, so a legitimate dot-entry is never dropped.
  // CI never saw this; only a working tree does.
  fs.cpSync(__dirname, path.join(root, 'plugin', 'scripts'), {
    recursive: true,
    filter: (src) => {
      try {
        const st = fs.lstatSync(src);
        return st.isFile() || st.isDirectory();
      } catch { return false; }
    },
  });

  const run = (extraEnv) => {
    const home = fs.mkdtempSync(path.join(root, 'home-'));
    const script = `
      const au = require(${JSON.stringify('/PLUGIN/scripts/auto-update.js')}.replace('/PLUGIN', process.env.CG_PLUGIN_ROOT));
      (async () => {
        const result = await au.checkForUpdate({
          requestJsonFn: async () => ({ statusCode: 500, body: '' }),
        });
        process.stdout.write(JSON.stringify({ result, devMode: au.isDevMode() }));
      })().catch(e => { process.stderr.write(String(e && e.stack || e)); process.exit(1); });
    `;
    const r = spawnSync(process.execPath, ['-e', script], {
      env: {
        ...cleanGitEnv(), HOME: home, CLAUDE_CONFIG_DIR: path.join(home, '.claude'),
        CG_PLUGIN_ROOT: path.join(root, 'plugin'), PATH: '', ...extraEnv,
      },
      encoding: 'utf8',
      timeout: 60000,
    });
    assert.equal(r.status, 0, `child failed: ${r.stderr}`);
    return {
      ...JSON.parse(r.stdout.trim().split('\n').pop()),
      stateWritten: fs.existsSync(path.join(home, '.cache', 'code-graph', 'update-state.json')),
    };
  };

  const off = run({});
  assert.equal(off.devMode, false, 'control: the tmpdir copy must NOT be classified as a dev tree');
  assert.equal(off.stateWritten, true, 'control: without the opt-out the check runs and records state');

  const on = run({ CODE_GRAPH_NO_AUTO_UPDATE: '1' });
  assert.equal(on.result, null, 'opted out → no update work');
  assert.equal(on.stateWritten, false, 'opted out → the updater does not even touch its state file');
});

test('the opt-out does NOT block --install-missing (a server with no binary must still get one)', () => {
  assert.equal(isAutoUpdateDisabled({ CODE_GRAPH_NO_AUTO_UPDATE: '1' }), true);
  assert.equal(isAutoUpdateDisabled({ CODE_GRAPH_NO_AUTO_UPDATE: '0' }), false);
  assert.equal(isAutoUpdateDisabled({}), false);
  // The gate in checkForUpdate reads `!installMissing && (isDevMode() || isAutoUpdateDisabled())`
  // — asserted here as source, because the behavioural arm needs a binary-less
  // sandbox that would download ~40MB to prove it.
  const src = fs.readFileSync(path.join(__dirname, 'auto-update.js'), 'utf8');
  assert.match(src, /if \(!installMissing && \(isDevMode\(\) \|\| isAutoUpdateDisabled\(\)\)\) return null;/);
});

// ── Suspension is throttled, not permanent (post-v0.111.0 review, M1) ───────
//
// The cap alone treated every repeated failure as permanent, and the causes are
// not distinguishable at the failure site: a briefly-missing .sha256 sidecar, a
// captive portal, a temporarily full disk burn the budget as fast as a broken
// tar — and SessionStart forces a check with only a 2-minute floor, so ~5
// restarts in ~10 minutes exhaust it. Recovery then required a NEWER release,
// so a ten-minute outage could park the updater for days.

test('a suspended release is retried once the daily timer expires', (t) => {
  const dayAgo = new Date(Date.now() - 25 * 60 * 60 * 1000).toISOString();
  const { result, state } = runCheckWithState(t, {
    latestVersion: '9.9.9', updateAvailable: true,
    updateAttempts: MAX_UPDATE_ATTEMPTS, suspendedAt: dayAgo,
  });
  assert.notEqual(result.suspended, true, 'past the daily timer the download must be attempted again');
  assert.equal(state.updateAttempts, MAX_UPDATE_ATTEMPTS + 1, 'the spent retry counts');
  // ...and the clock RESTARTS, or `retryDue` would stay true and the retry
  // would fire every session — the treadmill this cap exists to stop.
  assert.notEqual(state.suspendedAt, dayAgo, 'a failed retry must re-stamp the suspension clock');
  assert.ok(Date.now() - Date.parse(state.suspendedAt) < 60_000, 'clock restarts at now');
});

test('a suspended release is NOT retried before the daily timer expires', (t) => {
  const hourAgo = new Date(Date.now() - 60 * 60 * 1000).toISOString();
  const { result, state } = runCheckWithState(t, {
    latestVersion: '9.9.9', updateAvailable: true,
    updateAttempts: MAX_UPDATE_ATTEMPTS, suspendedAt: hourAgo,
  });
  assert.equal(result.suspended, true, 'an hour in, the download chain must stay parked');
  assert.equal(state.updateAttempts, MAX_UPDATE_ATTEMPTS, 'a parked cycle does not inflate the counter');
  assert.equal(state.suspendedAt, hourAgo, 'the clock must NOT be reset by an ordinary check');
});

test('entering suspension stamps the retry clock', (t) => {
  const { result, state } = runCheckWithState(t,
    { latestVersion: '9.9.9', updateAvailable: true, updateAttempts: MAX_UPDATE_ATTEMPTS });
  assert.equal(result.suspended, true);
  assert.ok(state.suspendedAt, 'without a stamp the daily retry can never become due');
});

test('a new target version clears a stale suspension clock', (t) => {
  // Seeded suspended on the PREVIOUS version. The new one must neither inherit
  // the exhausted budget nor look instantly retry-due from the old stamp.
  const dayAgo = new Date(Date.now() - 25 * 60 * 60 * 1000).toISOString();
  const { state } = runCheckWithState(t, {
    latestVersion: '9.9.8', updateAvailable: true,
    updateAttempts: MAX_UPDATE_ATTEMPTS + 3, suspendedAt: dayAgo,
  });
  assert.equal(state.updateAttempts, 1, 'fresh budget for the new release');
  assert.equal(state.suspendedAt, null, 'stale stamp from the old release must be dropped');
});

// ── checkForUpdate state carry-forward (audit 2026-08-01) ──────────────────
//
// Every saveState() in checkForUpdate spreads `...state` except one: the
// hasUpdate branch rebuilt the object from scratch. Any key it did not name was
// therefore erased on every check that found a new release — and the keys it
// does not name are exactly selfHealGlobalPkgs', which that function returns as
// `{}` once its retry cap is hit. So the capped global-npm heal got a fresh
// budget every release: the treadmill the cap exists to stop.

/**
 * Drive the real checkForUpdate() against a sandboxed HOME with a stubbed
 * GitHub response. The release names no downloadable assets, so the download
 * chain fails immediately (no network, no marketplace clone) — which is the
 * failure path these assertions are about.
 */
function updateStateSandbox(t, { manifestVersion, state, globalPkg }) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-upd-state-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const home = path.join(root, 'home');
  const cache = path.join(home, '.cache', 'code-graph');
  fs.mkdirSync(cache, { recursive: true });
  fs.writeFileSync(path.join(cache, 'install-manifest.json'),
    JSON.stringify({ version: manifestVersion, config: {} }));
  fs.writeFileSync(path.join(cache, 'update-state.json'), JSON.stringify(state));

  // NPM_CONFIG_PREFIX is globalNodeModulesCandidates' FIRST candidate, so this
  // is how a "stale global package" is staged without touching the real one.
  const prefix = path.join(root, 'npm-prefix');
  if (globalPkg) {
    const pkgDir = path.join(prefix, 'lib', 'node_modules', '@sdsrs', 'code-graph');
    fs.mkdirSync(pkgDir, { recursive: true });
    fs.writeFileSync(path.join(pkgDir, 'package.json'),
      JSON.stringify({ name: '@sdsrs/code-graph', version: globalPkg }));
  } else {
    fs.mkdirSync(prefix, { recursive: true });
  }
  return { root, home, prefix, statePath: path.join(cache, 'update-state.json') };
}

function runCheckForUpdate(sb, latestVersion) {
  const script = `
    const release = {
      tag_name: 'v${latestVersion}',
      tarball_url: 'http://127.0.0.1:1/src.tar.gz',
      assets: [],
    };
    require(${JSON.stringify(path.join(__dirname, 'auto-update.js'))})
      .checkForUpdate({ installMissing: true, requestJsonFn: async () => ({ statusCode: 200, body: JSON.stringify(release) }) })
      .then(() => process.exit(0), () => process.exit(0));
  `;
  const { execFileSync } = require('node:child_process');
  execFileSync(process.execPath, ['-e', script], {
    cwd: path.dirname(__dirname),
    env: {
      ...cleanGitEnv(),
      HOME: sb.home, USERPROFILE: sb.home,
      TMPDIR: sb.root, TMP: sb.root, TEMP: sb.root,
      NPM_CONFIG_PREFIX: sb.prefix,
      CODE_GRAPH_AUTO_UPDATE_SILENT: '1',
    },
    stdio: ['pipe', 'pipe', 'pipe'],
    timeout: 60000,
  });
  return JSON.parse(fs.readFileSync(sb.statePath, 'utf8'));
}

test('a failed update keeps the global-npm heal counters instead of resetting them', (t) => {
  // globalPkgHealAttempts is AT the cap and the staged global package is stale,
  // so selfHealGlobalPkgs returns {} (it has given up) and contributes nothing
  // to the spread — the branch's own object is the only thing that can carry
  // the counters, and it did not.
  const sb = updateStateSandbox(t, {
    manifestVersion: '0.0.1',
    globalPkg: '0.0.1',
    state: {
      latestVersion: '9.9.9',
      updateAttempts: 1,
      globalPkgHealVersion: '9.9.9',
      globalPkgHealAttempts: 3,   // == GLOBAL_PKG_HEAL_MAX_ATTEMPTS
    },
  });

  const after = runCheckForUpdate(sb, '9.9.9');

  assert.equal(after.globalPkgHealAttempts, 3,
    'a capped heal must stay capped — resetting it hands the failing npm install a fresh budget every release');
  assert.equal(after.globalPkgHealVersion, '9.9.9',
    'and the version the cap is scoped to must survive, or the cap loses its anchor');
  assert.equal(after.updateAttempts, 2, 'the failed download still counts (sanity: the branch under test ran)');
});

test('an out-of-band manual update clears the failure record', (t) => {
  // The suspension notice tells the user to update manually. When they do, the
  // next check finds nothing to install — and used to leave updateAttempts and
  // suspendedAt untouched, so `doctor` kept warning "v9.9.9 failed to install
  // 5× — auto-retry throttled" about a version already installed.
  const sb = updateStateSandbox(t, {
    manifestVersion: '0.0.1',            // now equal to latest → nothing to do
    state: {
      latestVersion: '0.0.1',
      updateAvailable: true,
      updateAttempts: 5,
      suspendedAt: '2026-01-01T00:00:00.000Z',
    },
  });

  const after = runCheckForUpdate(sb, '0.0.1');

  assert.equal(after.updateAvailable, false, 'sanity: the no-update branch ran');
  assert.equal(after.updateAttempts, 0, 'the failure counter describes an update that is no longer pending');
  assert.equal(after.suspendedAt, null, 'and the suspension clock must not outlive the suspension');
});

// ── Binary bypasses are INSIDE the throttle, not around it (audit BIN-1) ────
//
// `checkForUpdate` used to read
//   `if (!binaryMissing && !binaryStale && !shouldCheck(state, { force }))`
// which put both binary bypasses ABOVE shouldCheck's rate-limit arm — directly
// contradicting the "wins over everything, force included" comment on it. Two
// states are affected, and both are self-sustaining:
//   * rateLimited: a 403 cannot hand back a download URL, so the bypass spent a
//     request per check to learn nothing (measured: 1 request with a stale
//     binary vs 0 with a current one, same 403 state).
//   * suspended: the download chain is parked, so the cached binary stays
//     behind forever and `cachedBinaryStaleVsState` is therefore permanently
//     true — one GitHub request per session start, in perpetuity, discarded.
// A MISSING binary is the exception in both directions: it is the one repair
// the suspension branch deliberately keeps alive, so it still bypasses.

const { isUpdateSuspended } = require('./auto-update');

function runThrottleProbe(t, { state, cachedBinary = false, force = true }) {
  const { spawnSync } = require('child_process');
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-au-throttle-'));
  t.after(() => fs.rmSync(home, { recursive: true, force: true }));
  const cacheDir = path.join(home, '.cache', 'code-graph');
  fs.mkdirSync(path.join(cacheDir, 'bin'), { recursive: true });
  fs.writeFileSync(path.join(cacheDir, 'update-state.json'), JSON.stringify(state));
  fs.writeFileSync(path.join(cacheDir, 'install-manifest.json'),
    JSON.stringify({ version: '1.0.0', config: {} }));
  if (cachedBinary) {
    // Present but with an unreadable `--version` → `cachedBinaryStaleVsState`
    // true, which is exactly the shape a suspended machine is stuck in.
    fs.writeFileSync(
      path.join(cacheDir, 'bin', os.platform() === 'win32' ? 'code-graph-mcp.exe' : 'code-graph-mcp'),
      'not a real binary');
  }

  // The ONLY observable that separates "backed off" from "checked and did
  // nothing" is whether the request function ran, so count it.
  const script = `
    const au = require(${JSON.stringify(path.join(__dirname, 'auto-update.js'))});
    let fetches = 0;
    (async () => {
      await au.checkForUpdate({
        installMissing: true, // bypass the dev-mode gate; this repo IS a dev tree
        force: ${force ? 'true' : 'false'},
        requestJsonFn: async () => {
          fetches++;
          return { statusCode: 200, body: JSON.stringify({
            tag_name: 'v9.9.9', tarball_url: 'https://127.0.0.1:1/t', assets: [] }) };
        },
      });
      process.stdout.write(JSON.stringify({ fetches }));
    })().catch(e => { process.stderr.write(String(e && e.stack || e)); process.exit(1); });
  `;
  const r = spawnSync(process.execPath, ['-e', script], {
    env: {
      ...cleanGitEnv(), HOME: home, CLAUDE_CONFIG_DIR: path.join(home, '.claude'),
      NPM_CONFIG_PREFIX: path.join(home, 'npm-prefix'), PATH: '',
    },
    encoding: 'utf8',
    timeout: 60000,
  });
  assert.equal(r.status, 0, `child failed: ${r.stderr}`);
  return JSON.parse(r.stdout.trim().split('\n').pop()).fetches;
}

const JUST_NOW = () => new Date().toISOString();

test('a stale cached binary does NOT punch through the rate-limit backoff (BIN-1)', (t) => {
  const base = {
    latestVersion: '9.9.9', updateAvailable: true, lastCheck: JUST_NOW(),
  };
  assert.equal(
    runThrottleProbe(t, { state: { ...base, rateLimited: true }, cachedBinary: true }),
    0, 'rateLimited must outrank the stale-binary bypass — a 403 has no URL to give');

  // Control, and it has to be here: without it a child that crashed early, or a
  // dev-mode short-circuit, would also read as 0 requests. Identical state minus
  // the 403 flag → the bypass fires and the request happens.
  assert.equal(
    runThrottleProbe(t, { state: base, cachedBinary: true }),
    1, 'control: with no backoff armed, the stale binary DOES bypass the throttle');
});

test('a suspended update stops re-checking GitHub once per session (BIN-1)', (t) => {
  const base = {
    latestVersion: '9.9.9', updateAvailable: true, lastCheck: JUST_NOW(),
    suspendedAt: new Date(Date.now() - 60 * 60 * 1000).toISOString(),
  };
  assert.equal(
    runThrottleProbe(t, { state: { ...base, updateAttempts: MAX_UPDATE_ATTEMPTS }, cachedBinary: true }),
    0, 'while the chain is parked a stale binary cannot be healed — the request buys nothing');

  // Control: one attempt below the cap is not suspended, so the same stale
  // binary and the same forced check DO reach GitHub. Without this the test
  // would pass against a build that simply never checks.
  assert.equal(
    runThrottleProbe(t, { state: { ...base, updateAttempts: MAX_UPDATE_ATTEMPTS - 1 }, cachedBinary: true }),
    1, 'control: below the cap the stale-binary bypass still fires');
});

test('a suspended update still checks when the cached binary is MISSING (BIN-1 over-suppression guard)', (t) => {
  // The suspension branch downloads a missing binary on purpose (without it the
  // MCP server has no engine at all), so suppressing this fetch would break the
  // one repair suspension is meant to leave reachable.
  assert.equal(
    runThrottleProbe(t, {
      state: {
        latestVersion: '9.9.9', updateAvailable: true, lastCheck: JUST_NOW(),
        updateAttempts: MAX_UPDATE_ATTEMPTS,
        suspendedAt: new Date(Date.now() - 60 * 60 * 1000).toISOString(),
      },
      cachedBinary: false,
    }),
    1, 'a missing binary must still reach the release metadata it needs');
});

test('shouldCheck orders the binary bypasses below the backoff and the suspension (BIN-1)', () => {
  const minsAgo = (m) => new Date(Date.now() - m * 60 * 1000).toISOString();
  const suspended = {
    lastCheck: minsAgo(1), updateAvailable: true,
    updateAttempts: MAX_UPDATE_ATTEMPTS, suspendedAt: minsAgo(120),
  };

  // 1. rate limit outranks both bypasses AND force.
  const limited = { lastCheck: minsAgo(1), rateLimited: true, updateAvailable: true };
  assert.equal(shouldCheck(limited, { binaryStale: true }), false);
  assert.equal(shouldCheck(limited, { binaryMissing: true }), false);
  assert.equal(shouldCheck(limited, { binaryMissing: true, binaryStale: true, force: true }), false);
  assert.equal(shouldCheck({ ...limited, lastCheck: minsAgo(61) }, { binaryStale: true }), true,
    'and it still releases after its hour');

  // 2. suspension suppresses the stale bypass and force, but not missing.
  assert.equal(shouldCheck(suspended, { binaryStale: true, force: true }), false);
  assert.equal(shouldCheck(suspended, { binaryMissing: true }), true);
  assert.equal(shouldCheck({ ...suspended, lastCheck: minsAgo(7 * 60) }, {}), true,
    'the ordinary 6h cadence keeps running, so a newer release still un-suspends it');

  // 3. not suspended → the bypasses behave as before (no over-suppression).
  const plain = { lastCheck: minsAgo(1), updateAvailable: true, updateAttempts: 1 };
  assert.equal(shouldCheck(plain, { binaryStale: true }), true);
  assert.equal(shouldCheck(plain, { binaryMissing: true }), true);
  assert.equal(shouldCheck(plain, {}), false);

  // 4. the suspension predicate needs BOTH markers, so a stale stamp alone
  //    cannot park the updater.
  assert.equal(isUpdateSuspended({ suspendedAt: minsAgo(10), updateAttempts: 1 }), false);
  assert.equal(isUpdateSuspended({ updateAttempts: MAX_UPDATE_ATTEMPTS }), false);
  assert.equal(isUpdateSuspended({ suspendedAt: minsAgo(10), updateAttempts: MAX_UPDATE_ATTEMPTS }), true);
});

// ── Download failures explain themselves (audit BIN-4) ──────────────────────
//
// The two most common ways a download dies — a truncated transfer and an HTTP
// error page written to the output file — were also the only two that printed
// nothing. Each one burns one of MAX_UPDATE_ATTEMPTS, so five silent failures
// suspend the updater with no record anywhere of what went wrong.

function captureStderr(t) {
  const lines = [];
  const orig = console.error;
  console.error = (...args) => lines.push(args.join(' '));
  t.after(() => { console.error = orig; });
  return lines;
}

test('promoteVerifiedBinary says WHY it discards a sub-1MB download (BIN-4)', (t) => {
  const dir = mkDir(t, 'cg-au-sizefloor-');
  const tmp = path.join(dir, 'download.tmp');
  fs.writeFileSync(tmp, '<html>404: Not Found</html>');
  const dst = path.join(dir, 'code-graph-mcp');
  const errs = captureStderr(t);

  const size = fs.statSync(tmp).size;   // read now; promote unlinks the tmp file

  assert.equal(promoteVerifiedBinary(tmp, dst, '1.2.3', sha256Of(tmp)), false);
  const said = errs.join('\n');
  assert.match(said, new RegExp(`${size} bytes`),
    'the observed size is the whole diagnosis — print it');
  assert.match(said, /Refusing to install/);
  assert.equal(fs.existsSync(dst), false, 'and nothing is promoted');
});

test('promoteVerifiedBinary names the version it got instead of failing mute (BIN-4)', (t) => {
  const dir = mkDir(t, 'cg-au-versionarm-');
  const tmp = path.join(dir, 'download.tmp');
  writeFakeBinary(tmp, '0.0.1');
  const errs = captureStderr(t);

  assert.equal(promoteVerifiedBinary(tmp, path.join(dir, 'code-graph-mcp'), '9.9.9', sha256Of(tmp)), false);
  const said = errs.join('\n');
  assert.match(said, /0\.0\.1/, 'what arrived');
  assert.match(said, /9\.9\.9/, 'what was expected');
});

test('promoteVerifiedBinary reports the errno when the promote itself throws (BIN-4)', (t) => {
  const dir = mkDir(t, 'cg-au-promoteerr-');
  const errs = captureStderr(t);
  // A tmp path that never materialised: statSync throws ENOENT. ENOSPC, EACCES
  // and the Windows EPERM-on-a-running-.exe all land in the same bare catch, and
  // without the code they were one indistinguishable "false".
  assert.equal(
    promoteVerifiedBinary(path.join(dir, 'never-downloaded'), path.join(dir, 'dst'), '1.2.3', 'de'.repeat(32)),
    false);
  assert.match(errs.join('\n'), /ENOENT/, 'the errno is the diagnosis');
});

test('downloadBinary fetches the binary with curl -f, like the sidecar (BIN-4)', { skip: os.platform() === 'win32' ? 'POSIX shell fixture' : false }, (t) => {
  // Without `-f`, curl writes a 404/503 body to the output file and exits 0, so
  // the error page travels on as a candidate binary. The sidecar fetch has used
  // `-sfL` since it was written; the binary fetch used `-sL`.
  const root = mkDir(t, 'cg-au-curlf-');
  const fakeBin = path.join(root, 'fakebin');
  fs.mkdirSync(fakeBin);
  const curlLog = path.join(root, 'curl.log');
  fs.writeFileSync(path.join(fakeBin, 'curl'), [
    '#!/bin/sh',
    'printf "%s\\n" "$*" >> "$CURL_LOG"',
    'out=""; prev=""',
    'for a in "$@"; do if [ "$prev" = "-o" ]; then out="$a"; fi; prev="$a"; done',
    'if [ -n "$out" ]; then printf x > "$out"; fi',
    'exit 0',
    '',
  ].join('\n'));
  fs.chmodSync(path.join(fakeBin, 'curl'), 0o755);
  const home = path.join(root, 'home');
  fs.mkdirSync(home);

  const { spawnSync } = require('child_process');
  const script = `
    const { downloadBinary } = require(${JSON.stringify(path.join(__dirname, 'auto-update.js'))});
    downloadBinary({ version: '9.9.9', binaryUrl: 'https://example/code-graph-mcp-linux-x64' })
      .then(r => process.stdout.write(String(r)));
  `;
  const r = spawnSync(process.execPath, ['-e', script], {
    env: {
      ...cleanGitEnv(), HOME: home, CLAUDE_CONFIG_DIR: path.join(home, '.claude'),
      CURL_LOG: curlLog, PATH: `${fakeBin}:/usr/bin:/bin`,
    },
    encoding: 'utf8',
    timeout: 30000,
  });
  assert.equal(r.status, 0, `child failed: ${r.stderr}`);
  assert.equal(r.stdout.trim().split('\n').pop(), 'false', 'the 1-byte result must not be installed');

  const calls = fs.readFileSync(curlLog, 'utf8').trim().split('\n');
  const binaryCall = calls.find(c => !c.includes('.sha256'));
  assert.ok(binaryCall, `the binary fetch must have run; log was:\n${calls.join('\n')}`);
  assert.match(binaryCall, /(^|\s)-sfL(\s|$)/,
    'the binary fetch must fail-fast on HTTP >= 400, exactly as the sidecar fetch does');
  const sidecarCall = calls.find(c => c.includes('.sha256'));
  assert.match(sidecarCall, /(^|\s)-sfL(\s|$)/, 'control: the sidecar fetch already did');
  // Same run proves the size floor now speaks (the fake curl writes 1 byte).
  assert.match(r.stderr, /1 bytes/, 'and the discard is explained on stderr');
});

test('an unreachable GitHub reports UNKNOWN, not "up to date"', (t) => {
  // The CLI's final else prints `Up to date (v<manifest.version>)`, and both a
  // failed fetch and a lock held by a concurrent session used to return the
  // same bare `null` into it — so a user running this precisely BECAUSE they
  // are stuck on an old version was told the old version is current. Observed
  // 2026-08-17 in a sandboxed HOME: `check --force` printed
  // "Up to date (v0.118.0)" while v0.119.0 was the published release.
  const { spawnSync } = require('child_process');
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-au-unreach-'));
  t.after(() => fs.rmSync(home, { recursive: true, force: true }));
  const cacheDir = path.join(home, '.cache', 'code-graph');
  fs.mkdirSync(cacheDir, { recursive: true });
  fs.writeFileSync(path.join(cacheDir, 'install-manifest.json'),
    JSON.stringify({ version: '1.0.0', config: {} }));

  const script = `
    const au = require(${JSON.stringify(path.join(__dirname, 'auto-update.js'))});
    (async () => {
      const result = await au.checkForUpdate({
        installMissing: true, force: true,
        requestJsonFn: async () => { throw new Error('ENETUNREACH'); },
      });
      process.stdout.write(JSON.stringify(result));
    })().catch(e => { process.stderr.write(String(e && e.stack || e)); process.exit(1); });
  `;
  const r = spawnSync(process.execPath, ['-e', script], {
    env: { ...cleanGitEnv(), HOME: home, CLAUDE_CONFIG_DIR: path.join(home, '.claude'), PATH: '' },
    encoding: 'utf8', timeout: 60000,
  });
  assert.equal(r.status, 0, `child failed: ${r.stderr}`);
  const result = JSON.parse(r.stdout.trim().split('\n').pop());
  assert.equal(result && result.noop, true, 'a failed fetch is a NOOP with a reason, not a bare null');
  assert.equal(result.reason, 'fetch-failed');
  assert.equal(result.from, '1.0.0');
  // The CLI branches keyed off the reasons must exist — without them the value
  // falls through to the `Up to date (v…)` else, which is the bug. Matched as
  // independent tokens rather than one span, so reformatting the branch bodies
  // cannot silently disarm this.
  const cli = fs.readFileSync(path.join(__dirname, 'auto-update.js'), 'utf8');
  assert.match(cli, /result\.reason === 'fetch-failed'/);
  assert.match(cli, /update status UNKNOWN/);
  assert.match(cli, /result\.reason === 'install-lock-held'/);
});

// --- requestJson never-settles (audit 2026-08-22 P1-2) --------------------
//
// `req.setTimeout` is an INACTIVITY timer that lives on the socket: once the
// socket is destroyed mid-body there is no `end`, no `error` on the request,
// and the timer is gone with it. With only `data`/`end` wired on the response,
// the promise never settled — the detached per-session `check` turned into a
// zombie `node` process, and a foreground run hung the terminal.
//
// The stub replaces `https.request` so the shapes below are reachable without
// a network or a TLS fixture; every assertion runs the REAL `requestJson`.
function stubHttpsRequest(t, driveResponse) {
  const https = require('https');
  const { EventEmitter } = require('events');
  const original = https.request;
  // resolveProxy() reads the ambient env — a developer behind a proxy would
  // otherwise take the CONNECT branch and never reach the stub.
  const priorNoProxy = process.env.NO_PROXY;
  process.env.NO_PROXY = '*';
  t.after(() => {
    https.request = original;
    if (priorNoProxy === undefined) delete process.env.NO_PROXY;
    else process.env.NO_PROXY = priorNoProxy;
  });
  https.request = (_url, _opts, onResponse) => {
    const req = new EventEmitter();
    req.setTimeout = () => {}; // dead with the socket — the whole point
    req.destroy = () => {};
    req.end = () => {
      const res = new EventEmitter();
      res.setEncoding = () => {};
      res.statusCode = 200;
      onResponse(res);
      setImmediate(() => driveResponse(res));
    };
    return req;
  };
}

// 'hung' rather than a real hang, so the pre-fix state is a readable failure
// instead of a suite that never finishes.
async function settleOrHang(promise, ms = 2000) {
  let timer;
  const outcome = await Promise.race([
    promise.then(() => 'resolved', () => 'rejected'),
    new Promise((r) => { timer = setTimeout(() => r('hung'), ms); }),
  ]);
  clearTimeout(timer);
  return outcome;
}

test('requestJson rejects when the response errors mid-body', async (t) => {
  stubHttpsRequest(t, (res) => {
    res.emit('data', '{"tag_na');
    res.emit('error', Object.assign(new Error('socket hang up'), { code: 'ECONNRESET' }));
  });
  assert.equal(
    await settleOrHang(requestJson('https://api.github.com/x', 50)),
    'rejected',
    'a connection dropped mid-body must reject, not leave the promise pending forever',
  );
});

test('requestJson rejects when the response aborts mid-body', async (t) => {
  stubHttpsRequest(t, (res) => {
    res.emit('data', '{"tag_na');
    res.emit('aborted');
  });
  assert.equal(
    await settleOrHang(requestJson('https://api.github.com/x', 50)),
    'rejected',
    "'aborted' (no 'error' on older Node) must reject too",
  );
});

test('requestJson watchdog settles a response that goes silent', async (t) => {
  // Nothing at all after the headers: no data, no end, no error, and the
  // inactivity timer cannot fire. Only an overall deadline can settle this.
  stubHttpsRequest(t, () => {});
  assert.equal(
    await settleOrHang(requestJson('https://api.github.com/x', 50)),
    'rejected',
    'a silent response must hit the overall watchdog instead of hanging the process',
  );
});

test('requestJson still resolves a normal response (watchdog does not fire early)', async (t) => {
  stubHttpsRequest(t, (res) => {
    res.emit('data', '{"tag_name":"v1.0.0"}');
    res.emit('end');
  });
  const out = await requestJson('https://api.github.com/x', 50);
  assert.equal(out.statusCode, 200);
  assert.equal(out.body, '{"tag_name":"v1.0.0"}');
});
