#!/usr/bin/env node
'use strict';
const { execFileSync, spawn } = require('child_process');
const fs = require('fs');
const https = require('https');
const http = require('http');
const crypto = require('crypto');
const path = require('path');
const os = require('os');
const { CACHE_DIR, PLUGIN_ID, MARKETPLACE_NAME, readManifest, readJson, readJsonResult, backupCorruptFile, writeJsonAtomic, installedPluginsPath, pluginsCacheDir } = require('./lifecycle');
const { claudeHome } = require('./claude-config');
const { clearCache: clearBinaryCache, globalNodeModulesCandidates, nvmNodeModulesDirs, PLATFORM_PKG, detectLibc } = require('./find-binary');
const { readBinaryVersion, compareVersions, isDevMode } = require('./version-utils');
const { cgTmpDir } = require('./tmp-dir');
const { npmInvocation } = require('./npm-exec');
const { hidden } = require('./proc-opts');
const { acquireLock } = require('./install-lock');

// ── Environment Checks ────────────────────────────────────

/**
 * Check if a command-line tool is available on the system PATH.
 * @param {string} cmd - Command name (e.g., 'curl', 'tar')
 * @returns {boolean}
 */
function commandExists(cmd) {
  try {
    const whichCmd = process.platform === 'win32' ? 'where' : 'which';
    execFileSync(whichCmd, [cmd], hidden({ stdio: 'ignore' }));
    return true;
  } catch {
    return false;
  }
}

// ── Configuration ──────────────────────────────────────────
const GITHUB_REPO = 'sdsrss/code-graph-mcp';
const STATE_FILE = path.join(CACHE_DIR, 'update-state.json');
const BINARY_CACHE_DIR = path.join(CACHE_DIR, 'bin');
const CHECK_INTERVAL_MS = 6 * 60 * 60 * 1000;        // 6h — steady-state re-check
const UP_TO_DATE_RECHECK_MS = 30 * 60 * 1000;        // 30min — re-verify an "up to date" result (release-race guard)
const SESSION_START_MIN_GAP_MS = 2 * 60 * 1000;      // 2min — anti-hammer floor for forced (session-start) checks
// GitHub's unauthenticated REST quota is 60 req/hr and resets HOURLY, so one
// hour is the whole wait — 24h was written when this constant was unreachable
// (checkForUpdate erased `rateLimited` on the same call that set it) and became
// load-bearing the moment that was fixed, having never once been exercised. At
// 24h a single 403 on a shared/NAT'd IP froze every update check for a day,
// `--force` included, because the backoff arm sits above the force arm below.
const RATE_LIMIT_INTERVAL_MS = 60 * 60 * 1000;       // 1h — GitHub's reset window
const FETCH_TIMEOUT_MS = 3000;
// After this many consecutive FAILED install cycles for the SAME target version,
// stop re-running the download chain and go check-only until a new release moves
// the target. Some failures are permanent for a given machine (a GNU tar that
// cannot open `C:\...`, a locked plugin cache, a full disk) and every retry was
// a fresh ~MB download plus — before the windowsHide sweep — a burst of console
// windows, once per session, forever (issue #40). Kept equal to statusline.js's
// STUCK_UPDATE_ATTEMPTS (drift-guarded in statusline.test.js) so the moment the
// updater gives up is the moment the statusline stops promising "↻ updating".
const MAX_UPDATE_ATTEMPTS = 5;
// ...but suspension is not permanent. The cap alone assumed every repeated
// failure is permanent, and the causes are not distinguishable at the failure
// site: a briefly-missing `.sha256` sidecar, a captive portal, a temporarily
// full disk burn the budget just as fast as a broken tar — and SessionStart
// forces a check with only a 2-minute floor, so ~5 Claude Code restarts in ~10
// minutes exhaust it. Before this, recovery required a NEWER release (or
// hand-deleting update-state.json, which nothing tells the user to do), so a
// ten-minute outage could park the updater for days. One retry per day keeps
// the per-session treadmill dead while guaranteeing self-heal.
//
// Note the retry can NOT be keyed to `--force`: session-init passes --force on
// every session start, so re-arming there would restore the exact treadmill
// this cap exists to stop.
const SUSPENSION_RETRY_MS = 24 * 60 * 60 * 1000;

function isSilentMode(argv = process.argv.slice(2), env = process.env) {
  return argv.includes('--silent') || env.CODE_GRAPH_AUTO_UPDATE_SILENT === '1';
}

// Documented opt-out. Until now the only way to stop auto-update was the
// accidental one — CODE_GRAPH_DEV=1, which also changes binary resolution and
// several unrelated code paths (issue #40). This one does exactly what it says.
// It does NOT block --install-missing: that path exists to put a binary on disk
// for a server that has none, and disabling *updates* must not wedge a fresh
// install with no engine.
function isAutoUpdateDisabled(env = process.env) {
  return env.CODE_GRAPH_NO_AUTO_UPDATE === '1';
}

function isInstallMissingMode(argv = process.argv.slice(2)) {
  return argv.includes('--install-missing');
}

// High-intent trigger (session start / explicit reload) → bypass the soft
// throttle so an available update is picked up immediately, not on the next
// 6h/30min tick. Passed by session-init's launchBackgroundAutoUpdate.
function isForceMode(argv = process.argv.slice(2)) {
  return argv.includes('--force');
}

// ── Platform → GitHub release asset name mapping ──────────
function getPlatformAssetName({ platform = os.platform(), arch = os.arch(), libc = null } = {}) {
  // No musl asset is published: the glibc linux build downloads fine but cannot
  // exec on Alpine, so promoteVerifiedBinary always rejected it and — with the
  // binary still missing — every SessionStart bypassed the throttle and pulled
  // the same futile ~40MB again. Null stops the download path entirely; the
  // launcher surfaces unsupportedPlatformHint (cargo install / glibc image).
  if (platform === 'linux' && (libc || detectLibc()) === 'musl') return null;
  const key = `${platform}-${arch}`;
  const map = {
    'linux-x64': 'code-graph-mcp-linux-x64',
    'linux-arm64': 'code-graph-mcp-linux-arm64',
    'darwin-x64': 'code-graph-mcp-darwin-x64',
    'darwin-arm64': 'code-graph-mcp-darwin-arm64',
    'win32-x64': 'code-graph-mcp-win32-x64.exe',
  };
  return map[key] || null;
}

// ── State Persistence ──────────────────────────────────────

// `readJson(STATE_FILE) || {}` was the lossy-read shape the audit swept for on
// settings.json — still live here, on the one file that holds THREE independent
// give-up budgets: the update suspension (`updateAttempts` / `suspendedAt`), the
// binary self-heal budget (`binaryHealAttempts`) and the GitHub rate-limit
// backoff (`rateLimited` + `lastCheck`). Collapsing "could not read it" into
// "fresh install" re-armed all three at once, so one corrupt or unreadable cache
// file turned off every guard that exists to stop an unbounded retry loop —
// silently, and on every session thereafter (audit 2026-08-16 review Minor tail).
//
// Only a genuine ENOENT (or an empty file, which is what a crash mid-write
// leaves) may be read as a fresh start. Anything else returns the marker below;
// `checkForUpdate` skips the session and rewrites a clean file, so the next
// session starts from real state rather than looping here.
function readState() {
  const res = readJsonResult(STATE_FILE);
  if (res.value) return res.value;
  if (res.missing) return {};
  return { stateUnreadable: (res.error && res.error.code) || 'invalid-json' };
}

// One stderr line per process when the state file cannot be written. Not a
// throw: the caller's job (checking for an update) is unaffected, and a hook that
// dies over its own bookkeeping is worse than one that keeps going. But not
// silence either — every throttle in this file (update cooldown, GitHub
// rate-limit backoff, binary self-heal budget) is stored in that one file, so a
// read-only or full ~/.claude means the updater re-runs its whole check EVERY
// session, forever, with nothing anywhere saying why (2026-08-16 audit §四).
// The unlink/cleanup `catch {}`s elsewhere in this file stay silent on purpose:
// a failed cleanup costs a stale temp file, not a broken invariant.
let stateWriteWarned = false;
function saveState(state) {
  try {
    // The marker is an in-memory signal, never a persisted field: several call
    // sites do `saveState({ ...readState(), ... })`, and a persisted
    // `stateUnreadable` would park the updater permanently.
    const { stateUnreadable, ...clean } = state || {};
    void stateUnreadable;
    writeJsonAtomic(STATE_FILE, clean);
  } catch (e) {
    if (!stateWriteWarned) {
      stateWriteWarned = true;
      console.error(
        `[code-graph] Could not save update state to ${STATE_FILE} (${e && e.message ? e.message : e}). ` +
        'Update throttling and rate-limit backoff will not persist across sessions.',
      );
    }
  }
}

// ── Throttle ───────────────────────────────────────────────

// The updater has given up on the current target release (MAX_UPDATE_ATTEMPTS
// consecutive failed installs of the SAME version, retried once a day).
// `suspendedAt` is stamped only on entry to that state and cleared on success
// and on a new target, so it alone identifies it; `updateAttempts` is required
// too so a hand-edited or half-written state file cannot park the updater.
function isUpdateSuspended(state) {
  return Boolean(state && state.suspendedAt) && (state.updateAttempts || 0) >= MAX_UPDATE_ATTEMPTS;
}

// Whether to hit GitHub now. Keyed to the previous check's outcome, with a force
// override for high-intent triggers (session start / explicit reload) and two
// binary-health overrides. EVERY bypass is decided here — the caller used to
// short-circuit `binaryMissing`/`binaryStale` outside this function, which put
// them above the rate-limit arm and made the "wins over everything" below false:
// a stale binary plus `rateLimited: true` hit the API on every session start
// (measured: 1 request per check, vs 0 for the same state with a current
// binary). Ordering:
//   1. rate-limit backoff (RATE_LIMIT_INTERVAL_MS, 1h = GitHub's own reset
//      window) wins over everything — force and both binary overrides included.
//      Never push more requests into a GitHub 403; a 403 cannot hand us a
//      download URL either, so the bypasses have nothing to gain by outranking
//      it. Safe to outrank force only because it is an hour; the 24h it said
//      before made one 403 a silent day-long no-op for `--force`.
//   2. binaryMissing → check now. This is the one repair still reachable while
//      the download chain is otherwise parked (the suspension branch in
//      checkForUpdate keeps that heal alive), so it outranks suspension.
//   3. suspension → neither `binaryStale` nor `force` applies. A stale binary
//      cannot be healed while the chain is parked, and since suspension makes
//      `cachedBinaryStaleVsState` permanently true, that bypass otherwise
//      fired on every single session forever and did nothing with the answer.
//      Both fall through to the ordinary interval, which still notices a newer
//      release (that un-suspends) and still lets the daily retry come due.
//   4. force → only the short SESSION_START_MIN_GAP_MS floor applies, so opening
//      a new session re-checks immediately while a crash/reopen loop still can't
//      hammer the API.
//   5. otherwise → an "up to date" result is re-verified on a short cadence
//      (UP_TO_DATE_RECHECK_MS). This is the release-publish race guard: a version
//      can go live seconds AFTER a check that said "up to date", and the plain 6h
//      interval left it invisible for the full 6h (observed live — v0.85.7
//      published 8s after a check pinned v0.85.6). A pending-but-unfinished update
//      keeps the 6h steady-state interval.
function shouldCheck(state, { force = false, binaryMissing = false, binaryStale = false } = {}) {
  if (!state.lastCheck) return true;
  const elapsed = Date.now() - new Date(state.lastCheck).getTime();
  if (state.rateLimited) return elapsed >= RATE_LIMIT_INTERVAL_MS;
  if (binaryMissing) return true;
  if (!isUpdateSuspended(state)) {
    // ...and only while that heal still has a retry budget. Once it is spent,
    // the bypass re-fetched the API and re-entered the ~40MB download on every
    // single session (P1-14) — the same reasoning that keeps `binaryStale` out
    // of the suspended branch.
    if (binaryStale && !isBinaryHealExhausted(state)) return true;
    if (force) return elapsed >= SESSION_START_MIN_GAP_MS;
  }
  const interval = state.updateAvailable === false ? UP_TO_DATE_RECHECK_MS : CHECK_INTERVAL_MS;
  return elapsed >= interval;
}

// ── Version Comparison ─────────────────────────────────────
// compareVersions is imported from version-utils.js (single canonical,
// pre-release-aware implementation) and re-exported below.

// ── GitHub API ─────────────────────────────────────────────

/**
 * Resolve the proxy URL to use for a target URL, honoring HTTPS_PROXY/HTTP_PROXY
 * (and lowercase variants) plus NO_PROXY. Returns null when no proxy applies, so
 * the direct path stays byte-identical for users without a proxy configured.
 * @param {string} targetUrl
 * @param {NodeJS.ProcessEnv} [env]
 * @returns {string|null}
 */
function resolveProxy(targetUrl, env = process.env) {
  let host;
  try { host = new URL(targetUrl).hostname.toLowerCase(); } catch { return null; }
  const noProxy = (env.NO_PROXY || env.no_proxy || '').trim();
  if (noProxy === '*') return null;
  for (const raw of noProxy.split(',').map(s => s.trim().toLowerCase()).filter(Boolean)) {
    const bare = raw.replace(/^\*?\./, ''); // ".github.com" / "*.github.com" → "github.com"
    if (host === bare || host.endsWith('.' + bare)) return null;
  }
  const proxy = env.HTTPS_PROXY || env.https_proxy || env.HTTP_PROXY || env.http_proxy;
  return proxy && proxy.trim() ? proxy.trim() : null;
}

// Overall deadline for one metadata GET, as a multiple of the per-socket
// inactivity budget. Deliberately looser than `timeoutMs`: that one is an
// inactivity timer and must stay tight, while this one only has to stop a
// request that can never finish, so a slow-but-progressing response must not
// trip it.
const FETCH_TOTAL_TIMEOUT_FACTOR = 4;

function requestJson(url, timeoutMs = FETCH_TIMEOUT_MS) {
  return new Promise((resolve, reject) => {
    // Every path below settles through these. `req.setTimeout` is an
    // INACTIVITY timer that lives on the socket, so a connection dropped
    // mid-body emitted no `end`, no request `error` and no timeout — with only
    // `data`/`end` wired on the response the promise stayed pending forever.
    // The per-session detached `check` then accumulated zombie node processes,
    // and a foreground run hung the terminal (audit 2026-08-22 P1-2). The
    // response listeners close that shape directly; the watchdog is the
    // backstop for any other never-settles shape. Both must be single-shot —
    // `error` after a partial body would otherwise re-settle a settled
    // promise — and both must clear the timer so it cannot hold the event
    // loop open past a normal response.
    let settled = false;
    let watchdog = null;
    // Settling the promise is only half of it. An undestroyed request is an
    // ACTIVE HANDLE: the caller's `await` returns while the socket stays open,
    // the event loop has a reason to live, and the detached per-session `check`
    // remains resident — the same zombie process, reached by the other half of
    // the problem. Every handle that can outlive the promise registers here.
    const handles = [];
    const teardown = () => {
      while (handles.length > 0) {
        const h = handles.pop();
        try { h.destroy(); } catch { /* already gone */ }
      }
    };
    const clearWatchdog = () => {
      if (watchdog !== null) {
        clearTimeout(watchdog);
        watchdog = null;
      }
    };
    const settleOk = (value) => {
      if (settled) return;
      settled = true;
      clearWatchdog();
      resolve(value);
    };
    const settleErr = (err) => {
      if (settled) return;
      settled = true;
      clearWatchdog();
      teardown();
      reject(err instanceof Error ? err : new Error(String(err)));
    };
    watchdog = setTimeout(
      () => settleErr(new Error('request watchdog timeout')),
      timeoutMs * FETCH_TOTAL_TIMEOUT_FACTOR,
    );
    // The timer is a handle too. If `https.request` throws synchronously (a
    // malformed URL), the executor's throw rejects the promise without ever
    // reaching `clearWatchdog`, and the timer alone holds the process open for
    // the whole `timeoutMs * FETCH_TOTAL_TIMEOUT_FACTOR` — 12s at the production
    // budget. Unref'd it still fires while a request is in flight (the socket
    // keeps the loop alive), but it can no longer be the ONLY reason to stay up.
    watchdog.unref();

    const headers = {
      'Accept': 'application/vnd.github+json',
      'User-Agent': 'code-graph-auto-update/1.0',
    };
    const onResponse = (res) => {
      let body = '';
      res.setEncoding('utf8');
      res.on('data', (chunk) => { body += chunk; });
      // A truncated body is not a usable answer — reject rather than hand
      // back half a JSON document. 'aborted' covers the Node versions that
      // do not surface the drop as a response 'error'.
      res.on('aborted', () => settleErr(new Error('response aborted before end')));
      res.on('error', settleErr);
      res.on('end', () => {
        if (!res.statusCode) {
          settleErr(new Error('missing status code'));
          return;
        }
        settleOk({ statusCode: res.statusCode, body });
      });
    };

    const proxy = resolveProxy(url);
    if (proxy) {
      // Node's https module ignores *_PROXY env vars. curl-based binary downloads
      // already honor the proxy; tunnel the release-metadata GET over an HTTP
      // CONNECT to reach parity for users behind a corporate proxy.
      let pu, target;
      try { pu = new URL(proxy); target = new URL(url); }
      catch { settleErr(new Error('invalid proxy or target URL')); return; }
      const connectHeaders = {};
      if (pu.username) {
        const cred = `${decodeURIComponent(pu.username)}:${decodeURIComponent(pu.password)}`;
        connectHeaders['Proxy-Authorization'] = 'Basic ' + Buffer.from(cred).toString('base64');
      }
      const connectReq = http.request({
        host: pu.hostname,
        port: pu.port || 80,
        method: 'CONNECT',
        path: `${target.hostname}:${target.port || 443}`,
        headers: connectHeaders,
      });
      connectReq.on('connect', (res, socket) => {
        if (res.statusCode !== 200) {
          socket.destroy();
          settleErr(new Error(`proxy CONNECT failed: ${res.statusCode}`));
          return;
        }
        // The tunnelled socket outlives connectReq, so it needs its own entry:
        // destroying the CONNECT request does not close the tunnel under it.
        handles.push(socket);
        const req = https.request(url, {
          method: 'GET', headers, socket, agent: false, servername: target.hostname,
        }, onResponse);
        handles.push(req);
        req.setTimeout(timeoutMs, () => req.destroy(new Error('request timeout')));
        req.on('error', settleErr);
        req.end();
      });
      handles.push(connectReq);
      connectReq.setTimeout(timeoutMs, () => connectReq.destroy(new Error('proxy connect timeout')));
      connectReq.on('error', settleErr);
      connectReq.end();
      return;
    }

    const req = https.request(url, { method: 'GET', headers }, onResponse);
    handles.push(req);
    req.setTimeout(timeoutMs, () => req.destroy(new Error('request timeout')));
    req.on('error', settleErr);
    req.end();
  });
}

// Published by release.yml alongside the five platform binaries, each with a
// `.sha256` sidecar. Distinct from `tarball_url`, which is GitHub's
// auto-generated source archive and has no checksum published anywhere.
const PLUGIN_ASSET_NAME = 'claude-plugin.tar.gz';

function parseLatestRelease(data, assetName = getPlatformAssetName()) {
  if (!data || typeof data.tag_name !== 'string' || typeof data.tarball_url !== 'string') {
    return null;
  }

  const assetUrl = (name) => {
    if (!name || !Array.isArray(data.assets)) return null;
    const asset = data.assets.find((entry) => entry && entry.name === name);
    return asset && typeof asset.browser_download_url === 'string'
      ? asset.browser_download_url
      : null;
  };

  return {
    version: data.tag_name.replace(/^v/, ''),
    tarballUrl: data.tarball_url,
    pluginTarballUrl: assetUrl(PLUGIN_ASSET_NAME),
    binaryUrl: assetUrl(assetName),
  };
}

async function fetchLatestRelease(requestJsonFn = requestJson) {
  const url = `https://api.github.com/repos/${GITHUB_REPO}/releases/latest`;
  try {
    const res = await requestJsonFn(url, FETCH_TIMEOUT_MS);

    if (res.statusCode === 403) {
      const state = readState();
      saveState({ ...state, rateLimited: true });
      return null;
    }
    if (res.statusCode < 200 || res.statusCode >= 300) return null;

    const data = JSON.parse(res.body);
    return parseLatestRelease(data);
  } catch { return null; }
}

// ── Helpers ────────────────────────────────────────────────

function copyDirSync(src, dst) {
  fs.mkdirSync(dst, { recursive: true });
  for (const entry of fs.readdirSync(src, { withFileTypes: true })) {
    const srcPath = path.join(src, entry.name);
    const dstPath = path.join(dst, entry.name);
    if (entry.isDirectory()) {
      copyDirSync(srcPath, dstPath);
    } else {
      fs.copyFileSync(srcPath, dstPath);
    }
  }
}

function getExtractedPluginVersion(pluginSrc) {
  const manifest = readJson(path.join(pluginSrc, '.claude-plugin', 'plugin.json'));
  return manifest && typeof manifest.version === 'string' ? manifest.version : null;
}

function cachedBinaryPath() {
  const name = os.platform() === 'win32' ? 'code-graph-mcp.exe' : 'code-graph-mcp';
  return path.join(BINARY_CACHE_DIR, name);
}

/**
 * Decide whether the cached native binary must be (re)downloaded: true when it
 * is missing OR its actual version differs from the latest release. Version-aware
 * rather than existence-only — a stale-but-present binary must still self-heal
 * even when the plugin shell version already matches latest. manifest.version
 * tracks the plugin shell (the marketplace bumps it independently of the native
 * binary), so an existence-only check leaves the engine permanently pinned to an
 * old binary while the updater reports "up to date".
 */
function cachedBinaryNeedsUpdate(latest, { binaryPath = cachedBinaryPath(), readVersion = readBinaryVersion } = {}) {
  if (!latest || !latest.binaryUrl) return false;
  if (!fs.existsSync(binaryPath)) return true;
  const current = readVersion(binaryPath);
  if (!current) return true; // unreadable/broken binary — let the heal replace it
  // Ordered compare, not string inequality: a binary NEWER than releases/latest
  // (dev build, or the API momentarily lagging a publish) must not be downgraded.
  return compareVersions(current, latest.version) < 0;
}

/**
 * Throttle-bypass predicate: is a *present* cached binary stale relative to the
 * last known latest release (`state.latestVersion`, set on the previous fetch —
 * no network here)? Used so a present-but-stale binary skips the time-based
 * throttle instead of staying pinned for up to a full check interval. Returns
 * false when there is no prior latestVersion (first run fetches anyway) or the
 * binary is missing (handled by the separate `binaryMissing` bypass).
 */
function cachedBinaryStaleVsState(state, { binaryPath = cachedBinaryPath(), readVersion = readBinaryVersion } = {}) {
  if (!state || !state.latestVersion) return false;
  if (!fs.existsSync(binaryPath)) return false;
  const current = readVersion(binaryPath);
  if (!current) return true; // unreadable/broken — bypass throttle so the heal runs
  // Ordered compare (see cachedBinaryNeedsUpdate): newer-than-state is not stale.
  return compareVersions(current, state.latestVersion) < 0;
}

/**
 * Download just the platform binary from a GitHub release into the cache.
 * Used in two paths:
 *   1. As part of `downloadAndInstall` after a plugin tarball update.
 *   2. As a standalone self-heal when the cached binary is missing but the
 *      installed plugin version already matches `latest` (e.g. previous
 *      download failed silently, cache was wiped, optionalDependency
 *      install dropped the platform package).
 *
 * Returns true on successful promote, false otherwise. Never throws.
 */
async function downloadBinary(latest) {
  if (!latest || !latest.binaryUrl) return false;
  if (!commandExists('curl')) {
    console.error('[code-graph] Binary download skipped: curl not on PATH.');
    return false;
  }

  const binaryDst = cachedBinaryPath();
  const binaryTmp = binaryDst + '.tmp.' + process.pid;

  try {
    fs.mkdirSync(BINARY_CACHE_DIR, { recursive: true });
    // `-f` (fail on HTTP >= 400), same as the sidecar fetch below. Without it
    // curl writes GitHub's 404/503 HTML body to binaryTmp and exits 0, so the
    // error page travelled on as a candidate binary and was only caught two
    // gates later — by the silent size check, which reported nothing about why.
    execFileSync('curl', [
      '-sfL', '-o', binaryTmp,
      latest.binaryUrl,
    ], hidden({ timeout: 60000, stdio: 'pipe' }));

    // Integrity sidecar (<asset>.sha256), fail-CLOSED. `curl -f` turns a 404 into
    // a throw. One retry, because the alternative to a transient network blip is
    // no update this cycle — the installed binary keeps working and the next
    // check tries again, which is a strictly safer failure than exec'ing bytes
    // nothing vouched for.
    //
    // This used to fall through to a TOFU path on a missing sidecar, which made
    // it the one download chain in the repo that was fail-OPEN while
    // `src/snapshot/install.rs` (whose comment reads "this used to warn and fail
    // OPEN") is fail-closed. release.yml publishes a sidecar for every binary of
    // every release — verified back to v0.100.0 — and downloads always target
    // `releases/latest`, so there is no reachable no-sidecar case left to serve.
    // Same-origin, so this defends transit/CDN corruption and truncation, not a
    // release-asset swap; the version-exec check is the backstop there.
    let expectedSha = null;
    const shaTmp = binaryTmp + '.sha256';
    for (let attempt = 0; attempt < 2 && !expectedSha; attempt++) {
      try {
        execFileSync('curl', ['-sfL', '-o', shaTmp, latest.binaryUrl + '.sha256'],
          hidden({ timeout: 30000, stdio: 'pipe' }));
        expectedSha = (fs.readFileSync(shaTmp, 'utf8').trim().split(/\s+/)[0]) || null;
      } catch { /* retry once, then refuse below */ } finally {
        try { if (fs.existsSync(shaTmp)) fs.unlinkSync(shaTmp); } catch { /* ok */ }
      }
    }
    if (!expectedSha) {
      console.error(`[code-graph] Refusing to install: no sha256 sidecar for ${latest.binaryUrl} (fetched twice). The current binary is unchanged; the next update check will retry.`);
      try { fs.unlinkSync(binaryTmp); } catch { /* ok */ }
      return false;
    }

    return promoteVerifiedBinary(binaryTmp, binaryDst, latest.version, expectedSha);
  } catch (e) {
    console.error(`[code-graph] Binary download failed: ${e.message}`);
    return false;
  }
}

/**
 * Hex sha256 of a file's contents (lowercase).
 * @param {string} filePath
 * @returns {string}
 */
function sha256File(filePath) {
  return crypto.createHash('sha256').update(fs.readFileSync(filePath)).digest('hex');
}

function promoteVerifiedBinary(binaryTmp, binaryDst, expectedVersion, expectedSha256) {
  try {
    // Size floor: every published binary is tens of MB, so anything under 1 MB
    // is a truncated transfer or an error page. It used to return false without
    // a word — and it sits ABOVE the two gates that DO explain themselves, so
    // the most common download failures were also the only silent ones, each
    // burning one of MAX_UPDATE_ATTEMPTS with nothing on stderr to explain it.
    const stat = fs.statSync(binaryTmp);
    if (stat.size <= 1_000_000) {
      console.error(
        `[code-graph] Refusing to install: downloaded binary is ${stat.size} bytes — far below the ~1 MB floor, ` +
        'so the transfer was truncated or the server returned an error page. ' +
        'The current binary is unchanged; the next update check retries.'
      );
      return false;
    }

    // Integrity gate BEFORE the file is made executable or run, so a corrupted
    // or tampered download is never exec'd. The published <asset>.sha256 sidecar
    // is same-origin, so this defends transit/CDN corruption + truncation, not a
    // full release compromise (an attacker swapping the binary swaps the sidecar
    // too — the version-exec check below is the backstop there).
    //
    // Fail-CLOSED: no expected sha, no install. The previous "warn and proceed"
    // arm made this the only fail-open link in the four download chains, against
    // a fail-closed `src/snapshot/install.rs` — and a warning printed to stderr
    // during a background auto-update is seen by nobody.
    if (!expectedSha256) {
      console.error('[code-graph] No expected sha256 supplied — refusing to install an unverified binary.');
      try { fs.unlinkSync(binaryTmp); } catch { /* ok */ }
      return false;
    }
    const actualSha = sha256File(binaryTmp);
    if (actualSha.toLowerCase() !== String(expectedSha256).toLowerCase()) {
      console.error(`[code-graph] Binary checksum mismatch (sha256): expected ${expectedSha256}, got ${actualSha} — refusing to install.`);
      return false;
    }

    // chmod BEFORE reading the version. readBinaryVersion executes the binary
    // (`--version`), which requires the exec bit; `curl -o` writes the tmp file
    // as 0644 (no exec bit), so reading the version first fails with EACCES →
    // null → false, which silently wedged every download path. rename preserves
    // the mode, so the promoted dst ends up 0755.
    if (os.platform() !== 'win32') {
      fs.chmodSync(binaryTmp, 0o755);
    }

    const actualVersion = readBinaryVersion(binaryTmp);
    if (!actualVersion || (expectedVersion && actualVersion !== expectedVersion)) {
      // Sibling of the size floor above: silent for the same reason and with the
      // same cost. `--version` failing to run at all (wrong arch, missing libc)
      // reads identically to a version mismatch without this.
      console.error(
        `[code-graph] Refusing to install: downloaded binary reports ${actualVersion ? `v${actualVersion}` : 'no runnable --version'}` +
        `${expectedVersion ? `, expected v${expectedVersion}` : ''} — not installing it.`
      );
      return false;
    }

    fs.renameSync(binaryTmp, binaryDst);
    clearBinaryCache();
    return true;
  } catch (e) {
    // `e.code` is the whole diagnosis for this arm: ENOSPC (full disk), EACCES /
    // EPERM (locked cache dir, or Windows refusing to replace the .exe the MCP
    // server is running), EBUSY, EXDEV. A bare `catch { return false }` made all
    // of them one indistinguishable failure that the caller counted as an
    // attempt and printed nothing about.
    console.error(`[code-graph] Binary promote failed${e && e.code ? ` (${e.code})` : ''}: ${e && e.message}`);
    return false;
  } finally {
    try {
      if (fs.existsSync(binaryTmp)) fs.unlinkSync(binaryTmp);
    } catch { /* ok */ }
  }
}

// ── Marketplace clone refresh ──────────────────────────────

function marketplaceCloneDir() {
  return path.join(claudeHome(), 'plugins', 'marketplaces', MARKETPLACE_NAME);
}

/**
 * Fast-forward the Claude Code marketplace clone after a plugin update.
 *
 * Auto-update writes the plugin cache + installed_plugins.json directly and
 * never touched the marketplace clone, so its marketplace.json stayed pinned
 * at the version present when the user last ran a /plugin command (observed
 * live: clone at 0.48.0 four days after 0.49.0 shipped). A stale clone makes
 * the /plugin UI report the old version and lets Claude Code re-install the
 * old plugin files from it. --ff-only + silent failure: a dirty or diverged
 * clone is Claude Code's property — never force anything there.
 */
function refreshMarketplaceClone({ dir = marketplaceCloneDir(), exec = execFileSync, timeoutMs = 15000 } = {}) {
  try {
    if (!fs.existsSync(path.join(dir, '.git'))) return false;
    if (!commandExists('git')) return false;
    exec('git', ['-C', dir, 'pull', '--ff-only', '--quiet'], hidden({ timeout: timeoutMs, stdio: 'pipe' }));
    return true;
  } catch {
    return false;
  }
}

// ── Download & Install ─────────────────────────────────────

async function downloadAndInstall(latest, {
  exec = execFileSync,
  downloadBin = downloadBinary,
  refreshMarketplace = refreshMarketplaceClone,
  cmdExists = commandExists,
} = {}) {
  // Pre-flight: check required CLI tools before attempting any download
  const missingTools = ['curl', 'tar'].filter(cmd => !cmdExists(cmd));
  if (missingTools.length > 0) {
    console.error(`[code-graph] Auto-update skipped: missing required tools: ${missingTools.join(', ')}. Install them to enable auto-updates.`);
    return { pluginUpdated: false, binaryUpdated: false };
  }

  const tmpDir = path.join(cgTmpDir(), `update-${Date.now()}`);
  let pluginUpdated = false;
  let binaryUpdated = false;
  let marketplaceRefreshed = false;

  try {
    fs.mkdirSync(tmpDir, { recursive: true });

    // ── Step 1: Download and install plugin files from the release asset ──
    //
    // Fail-CLOSED on integrity, like the binary chain. This step extracts an
    // archive and then COPIES ITS JAVASCRIPT into the plugin cache, where Claude
    // Code runs it as hooks on every tool call — so of the four download chains
    // it is the one where unverified bytes become executed code, and it was the
    // only one with no checksum at all (`tarball_url` is GitHub's generated
    // source archive; nothing publishes a digest for it).
    // `claude-plugin.tar.gz` + `.sha256` are published by release.yml for every
    // release from the one carrying this change onward, and updates always
    // target `releases/latest` — so a missing asset means something is wrong
    // with the release, not that we are talking to an older one. Refusing leaves
    // the user on their current, working plugin version; the binary update below
    // still runs.
    if (!latest.pluginTarballUrl) {
      console.error(`[code-graph] Plugin update skipped: release ${latest.version} publishes no ${PLUGIN_ASSET_NAME} — refusing to install plugin code from an unverifiable source archive.`);
      return { pluginUpdated: false, binaryUpdated: await downloadBin(latest), marketplaceRefreshed: false };
    }
    const tarballPath = path.join(tmpDir, PLUGIN_ASSET_NAME);
    // `-f` like every sibling fetch in this file (the binary at :435 and both
    // sha256 sidecars). This was the one download without it, so a 404/503 wrote
    // GitHub's HTML body here and exited 0; the checksum below still failed
    // closed, but as "sha mismatch" — a wrong diagnosis of a fetch that never
    // succeeded (2026-08-16 audit §四).
    exec('curl', [
      '-sfL', '-o', tarballPath,
      '-H', 'Accept: application/octet-stream',
      latest.pluginTarballUrl,
    ], hidden({ timeout: 30000, stdio: 'pipe' }));

    // One retry, matching the binary sidecar at :355 — same failure mode, same
    // argument: a transient blip should cost an update cycle, not force a
    // refusal. The first version of this gave the binary two attempts and the
    // plugin one, for no reason anyone could state.
    const shaPath = tarballPath + '.sha256';
    let expectedSha = null;
    for (let attempt = 0; attempt < 2 && !expectedSha; attempt++) {
      try {
        exec('curl', ['-sfL', '-o', shaPath, latest.pluginTarballUrl + '.sha256'],
          hidden({ timeout: 30000, stdio: 'pipe' }));
        expectedSha = (fs.readFileSync(shaPath, 'utf8').trim().split(/\s+/)[0]) || null;
      } catch { /* retried once, then refused just below */ }
    }
    const actualSha = fs.existsSync(tarballPath) ? sha256File(tarballPath) : null;
    if (!expectedSha || !actualSha || expectedSha.toLowerCase() !== actualSha.toLowerCase()) {
      console.error(`[code-graph] Plugin tarball integrity check failed (expected ${expectedSha || '<no sidecar>'}, got ${actualSha || '<no download>'}) — refusing to extract.`);
      return { pluginUpdated: false, binaryUpdated: await downloadBin(latest), marketplaceRefreshed: false };
    }

    // No --strip-components: the asset archives `claude-plugin/` itself, while
    // GitHub's source tarball wraps everything in `<owner>-<repo>-<sha>/`.
    //
    // Relative archive name + `cwd`, never an absolute path with `-C`: GNU tar
    // (git-for-Windows / MSYS, first on PATH for many Windows users) reads the
    // drive letter in `C:\Users\...\claude-plugin.tar.gz` as a REMOTE HOST and
    // fails with "Cannot connect to C: resolve failed" — the same colon-parsing
    // family as issues #34/#35. Windows' built-in bsdtar accepts both spellings,
    // so this form is the portable one. On a failing GNU tar this was the step
    // that made plugin updates unachievable, which is what put the whole
    // download chain on a per-session repeat loop (issue #40).
    exec('tar', [
      'xzf', PLUGIN_ASSET_NAME,
    ], hidden({ cwd: tmpDir, timeout: 15000, stdio: 'pipe' }));

    const pluginSrc = path.join(tmpDir, 'claude-plugin');
    const pluginDst = path.join(
      pluginsCacheDir(), MARKETPLACE_NAME, 'code-graph-mcp', latest.version
    );

    if (fs.existsSync(pluginSrc) && getExtractedPluginVersion(pluginSrc) === latest.version) {
      fs.mkdirSync(pluginDst, { recursive: true });
      copyDirSync(pluginSrc, pluginDst);
      pluginUpdated = true;
    }

    // Repoint state at the new version ONLY if the plugin copy actually landed.
    // Guarding on pluginUpdated: when the copy above was skipped (pluginSrc absent, or
    // its plugin.json version drifted from the tag — the project's version sync is known
    // fragile), pluginDst was never created. Advancing installPath/manifest to it anyway
    // pointed Claude Code at a nonexistent install dir while state read "up to date".
    if (pluginUpdated) {
      // Update installed_plugins.json to point to new version.
      //
      // Through the same three-way read the lifecycle.js site uses. The lenient
      // `readJson` returns null for ENOENT, EACCES and unparseable alike, and
      // the `if (installed && …)` guard below then skipped the repoint in
      // SILENCE — while the plugin copy had landed and the manifest below is
      // about to be advanced. Claude Code keeps launching the old install dir
      // with state reading "up to date": the split-brain shape the binary-pin
      // incident was made of, and one this file cannot fix by guessing at bytes
      // it could not read. So it says so instead, which keeps `/plugin update`
      // reachable as the manual way out.
      const installedPath = installedPluginsPath();
      const installedRead = readJsonResult(installedPath);
      // Whether the registry entry is STILL pointing at the old version when this
      // block ends. It gates the manifest advance below, and that gate is the
      // whole difference between a report and a fix: `checkForUpdate` reads
      // `readManifest().version` as the authoritative installed version, so
      // advancing it past a repoint that did not happen makes the next session
      // compute "up to date" — the message below prints ONCE, into a SessionStart
      // hook's stderr, and the split-brain then has nothing behind it. Left
      // behind, the ordinary check interval retries the whole install and
      // re-reports, and the repoint lands by itself the moment the file is
      // repaired. `missing` is not blocked: no registry means nothing to repoint.
      let repointBlocked = false;
      if (installedRead.corrupt) {
        // Value unusable — the bytes are not ours to guess at.
        const why = installedRead.error
          ? (installedRead.error.code || installedRead.error.message)
          : 'it does not contain a JSON object';
        console.error(
          `[code-graph] plugin ${latest.version} is installed, but ${installedPath} ` +
          `could not be read (${why}) — its entry for this plugin still points at the ` +
          'previous version. Run `/plugin update` or repair that file by hand.'
        );
        repointBlocked = true;
      } else {
        let installed = installedRead.value;
        // `lossy` is NOT `corrupt`: the value parsed and is usable, it is the
        // BYTES that will not survive our rewrite (a cp1252 byte inside a path,
        // see readJsonResult). lifecycle.js's readSettingsForWrite route applies
        // here for the same reason — preserve the true bytes, then proceed, since
        // refusing outright strands the install over a byte we can work around.
        // Collapsing this into the corrupt arm also misreported it: a lossy result
        // carries no `error`, so the message called a parseable file unparseable.
        if (installed && installedRead.lossy) {
          const backup = backupCorruptFile(installedPath, installedRead.raw);
          if (backup) {
            console.error(
              `[code-graph] ${installedPath} contains bytes that are not valid UTF-8; ` +
              `repointing it at ${latest.version} will replace them. Saved the original ` +
              `to ${backup} first.`
            );
          } else {
            console.error(
              `[code-graph] plugin ${latest.version} is installed, but ${installedPath} ` +
              'contains bytes that are not valid UTF-8 and no backup copy could be made — ' +
              'its entry for this plugin still points at the previous version. Rewriting it ' +
              'would replace those bytes permanently. Run `/plugin update` after repairing it.'
            );
            installed = null;
            repointBlocked = true;
          }
        }
        if (installed && installed.plugins && installed.plugins[PLUGIN_ID]) {
          installed.plugins[PLUGIN_ID][0].installPath = pluginDst;
          installed.plugins[PLUGIN_ID][0].version = latest.version;
          installed.plugins[PLUGIN_ID][0].lastUpdated = new Date().toISOString();
          try {
            writeJsonAtomic(installedPath, installed);
          } catch (err) {
            console.error(
              `[code-graph] plugin ${latest.version} is installed, but ${installedPath} ` +
              `could not be written (${err.code || err.name}) — its entry for this plugin ` +
              'still points at the previous version. Run `/plugin update`.'
            );
            repointBlocked = true;
          }
        }
      }

      // Update install manifest — only when nothing is left pointing at the old
      // version. See `repointBlocked` above: this value IS the update gate.
      if (!repointBlocked) {
        try {
          const manifest = readManifest();
          manifest.version = latest.version;
          manifest.updatedAt = new Date().toISOString();
          writeJsonAtomic(path.join(CACHE_DIR, 'install-manifest.json'), manifest);
        } catch { /* not fatal */ }
      }

      // Run the NEW lifecycle.js to update settings.json hooks with new paths.
      // Without this, settings.json hooks still point to the old version directory
      // until the next session's self-heal corrects them.
      try {
        const newLifecycle = path.join(pluginDst, 'scripts', 'lifecycle.js');
        if (fs.existsSync(newLifecycle)) {
          exec(process.execPath, [newLifecycle, 'update'], hidden({
            timeout: 5000, stdio: 'pipe',
          }));
        }
      } catch { /* not fatal — syncLifecycleConfig will self-heal on next session */ }
    }

    // ── Step 1.5: Fast-forward the marketplace clone so /plugin UI and any
    //    Claude-Code-side reinstall see the version we just installed.
    if (pluginUpdated) {
      marketplaceRefreshed = refreshMarketplace();
    }

    // ── Step 2: Download platform binary directly from GitHub release ──
    if (await downloadBin(latest)) {
      binaryUpdated = true;
    }

    return { pluginUpdated, binaryUpdated, marketplaceRefreshed };
  } catch (e) {
    console.error(`[code-graph] Plugin download/extract failed: ${e.message}`);
    return { pluginUpdated: false, binaryUpdated: false, marketplaceRefreshed };
  } finally {
    try { fs.rmSync(tmpDir, { recursive: true, force: true }); } catch { /* ok */ }
  }
}

// ── Main Entry ─────────────────────────────────────────────

/**
 * Self-heal the cached native binary when the plugin shell is already at latest
 * but the binary lags (missing OR a different version). This is the orchestration
 * glue that broke twice in the field (v0.45.1, v0.45.2): the decision predicate
 * was correct, but nothing guaranteed checkForUpdate actually invoked the download
 * on the shell-matches-latest path. Extracted + injectable so the wiring itself is
 * regression-tested, not just the predicate. Returns true iff a download promoted.
 */
/**
 * Replace a missing/stale cached binary — BOUNDED, per target version.
 *
 * This had no counter, so a promote that could not land (the Windows case named
 * at promoteVerifiedBinary: the running MCP server holds the .exe, rename →
 * EACCES) re-downloaded ~40MB on every session forever: the caller cleared
 * `updateAttempts`/`suspendedAt` unconditionally right after calling this, and
 * `shouldCheck`'s `binaryStale` arm bypasses the throttle (audit 2026-08-16
 * P1-14; measured 8 calls → 8 downloads).
 *
 * Counted the same way as selfHealGlobalPkgs, including its hard-won rule:
 * success is "the binary is no longer stale", NOT "download() returned true".
 * A download whose promote silently failed used to reset the budget on every
 * run, which is a cap that can never be reached.
 *
 * The counter is deliberately SEPARATE from `updateAttempts`: that one tracks
 * the plugin-shell update, and the branch this runs in resets it because the
 * shell IS current. Sharing it would have made each reset re-arm the other.
 *
 * @returns {{healed: boolean, patch: object}} patch is spread into the state save
 */
async function selfHealStaleBinary(latest, {
  state = {}, needsUpdate = cachedBinaryNeedsUpdate, download = downloadBinary,
  // "Present" must mean USABLE, not merely on disk. A truncated, non-executable
  // or wrong-arch cached binary leaves the MCP server exactly as dead as a
  // missing one, and every sibling predicate here already treats unreadable as
  // needing replacement (cachedBinaryNeedsUpdate, cachedBinaryStaleVsState).
  // Keying on existsSync alone put a corrupt binary under the stale budget,
  // which isBinaryHealExhausted only re-arms when a NEW release ships — so five
  // quick failures parked the only recovery path permanently (pre-tag review of
  // the P1-14 fix).
  binaryPresent = () => {
    const p = cachedBinaryPath();
    return fs.existsSync(p) && readBinaryVersion(p) !== null;
  },
} = {}) {
  if (!latest || !needsUpdate(latest)) {
    // Healthy → clear any leftover counter so the next real staleness starts fresh.
    return {
      healed: false,
      patch: state.binaryHealAttempts ? { binaryHealAttempts: 0, binaryHealVersion: null } : {},
    };
  }
  // A MISSING binary is exempt from the attempt budget: with no engine at all
  // the MCP server is dead, and `needsUpdate` returns true for "absent" too —
  // letting the stale-heal counter absorb those failures would permanently
  // park the only recovery path after five offline session starts (batch
  // review of the P1-14 fix). The budget exists to stop re-downloading over a
  // binary that RUNS but cannot be replaced (Windows EACCES-on-rename); a
  // missing binary keeps the pre-P1-14 unbounded retry on purpose.
  const missing = !binaryPresent();
  const attempts = state.binaryHealVersion === latest.version ? (state.binaryHealAttempts || 0) : 0;
  if (!missing && attempts >= MAX_UPDATE_ATTEMPTS) return { healed: false, patch: {} };
  await download(latest);
  // Re-read the disk, not the return value (see above).
  const stillStale = needsUpdate(latest);
  return {
    healed: !stillStale,
    patch: {
      binaryHealVersion: latest.version,
      binaryHealAttempts: !stillStale ? 0 : missing ? attempts : attempts + 1,
    },
  };
}

/**
 * The stale-binary heal has spent its budget on the release we are tracking.
 * Read by shouldCheck: with the heal parked, the `binaryStale` throttle bypass
 * can accomplish nothing and would just re-fetch the API (and, worse, re-enter
 * the download path) every session. Re-arms itself when `latestVersion` moves.
 */
function isBinaryHealExhausted(state) {
  return Boolean(state)
    && Boolean(state.binaryHealVersion)
    && state.binaryHealVersion === state.latestVersion
    && (state.binaryHealAttempts || 0) >= MAX_UPDATE_ATTEMPTS;
}

// ── Global npm package self-heal ───────────────────────────
// The `code-graph-mcp` CLI on the user's PATH is the GLOBAL npm shell package
// (@sdsrs/code-graph) — a delivery surface entirely outside the marketplace
// plugin, so /plugin update and the binary self-heal above never touch it. In
// the field it drifts for months (a 0.46.0 wrapper delegating to a 0.101.0
// binary) and users were expected to run `npm update -g` by hand — which also
// breaks on unrelated npm-config quirks (EALLOWGIT). Same story for a platform
// package installed EXPLICITLY at the global top level (the old launcher's
// manual-install hint suggested exactly that): that relic was the 0.16.6
// landmine behind the MCP connect-timeout incident.
//
// Heal contract: refresh ONLY what the user already installed globally (never
// introduce a global install they didn't ask for), one bounded npm run per
// release target, silent failure (an unhealable npm env must not block or spam).

const SHELL_PKG = '@sdsrs/code-graph';
const GLOBAL_PKG_HEAL_MAX_ATTEMPTS = 3;
const GLOBAL_PKG_HEAL_TIMEOUT_MS = 180000; // npm resolves + downloads the platform optionalDependency (~40MB)

/** Installed version of a top-level GLOBAL npm package, or null when absent. */
function globalPkgVersion(name, roots = null) {
  for (const root of (roots || globalNodeModulesCandidates())) {
    try {
      const pkg = readJson(path.join(root, name, 'package.json'));
      if (pkg && pkg.version) return pkg.version;
    } catch { /* not installed under this root */ }
  }
  return null;
}

/** Globally-installed packages of ours whose version lags `latestVersion`. */
function staleGlobalPkgs(latestVersion, roots = null) {
  const out = [];
  for (const name of [SHELL_PKG, PLATFORM_PKG]) {
    const ver = globalPkgVersion(name, roots);
    if (ver && compareVersions(ver, latestVersion) < 0) out.push({ name, version: ver });
  }
  return out;
}

/**
 * Global installs of ours stranded under a NON-active node version. nvm keeps a
 * separate global prefix per node; switching the default node leaves the old
 * prefix's `@sdsrs/code-graph` behind — invisible to selfHealGlobalPkgs (which
 * only sees, and can only `npm install -g` into, the ACTIVE node's prefix) yet
 * still able to seed stale settings.json hooks / shadow PATH shims (the
 * v24.11.1@0.46.0 relic firing beside the active install — RCA 2026-07-24).
 * Detection-only: returns each relic's package + version + node prefix so doctor
 * can surface it with manual remediation. `dirs`/`activeDir` injectable for tests.
 */
function inactiveNodeGlobalRelics({ dirs = null, activeDir = null } = {}) {
  const active = path.resolve(activeDir
    || path.join(path.dirname(process.execPath), '..', 'lib', 'node_modules'));
  const roots = dirs || nvmNodeModulesDirs();
  const out = [];
  for (const dir of roots) {
    if (path.resolve(dir) === active) continue; // active prefix → not a relic
    for (const name of [SHELL_PKG, PLATFORM_PKG]) {
      const version = globalPkgVersion(name, [dir]);
      if (version) out.push({ name, version, nodeModulesDir: dir });
    }
  }
  return out;
}

/** One targeted `npm install -g` for the given specs. Resolves true on exit 0. */
function npmInstallGlobal(specs) {
  return new Promise((resolve) => {
    if (!commandExists('npm')) { resolve(false); return; }
    const npm = npmInvocation(['install', '-g', ...specs], {
      timeout: GLOBAL_PKG_HEAL_TIMEOUT_MS,
      stdio: ['ignore', 'ignore', 'pipe'],
    });
    const child = spawn(npm.file, npm.args, npm.opts);
    let stderr = '';
    child.stderr.on('data', (d) => { stderr += d.toString(); });
    child.on('error', () => resolve(false));
    child.on('exit', (code) => {
      if (code === 0) {
        console.error(`[code-graph] global npm package(s) refreshed: ${specs.join(' ')}`);
        resolve(true);
      } else {
        const tail = stderr.trim().split('\n').slice(-2).join(' | ');
        console.error(`[code-graph] global npm refresh failed (exit ${code}): ${tail}`);
        resolve(false);
      }
    });
  });
}

/**
 * Self-heal globally-installed shell/platform packages to `latest.version`.
 * Returns a state patch (spread into the update-state save): attempts are
 * counted PER target version so a persistently-failing npm env stops being
 * retried after GLOBAL_PKG_HEAL_MAX_ATTEMPTS, and the counter re-arms when the
 * next release moves the target.
 */
async function selfHealGlobalPkgs(latest, state, {
  readStale = staleGlobalPkgs,
  install = npmInstallGlobal,
} = {}) {
  if (!latest || !latest.version) return {};
  const stale = readStale(latest.version);
  if (stale.length === 0) {
    // Healthy (or nothing installed globally) — clear any leftover counter.
    return state.globalPkgHealAttempts ? { globalPkgHealAttempts: 0, globalPkgHealVersion: null } : {};
  }
  const attempts = state.globalPkgHealVersion === latest.version ? (state.globalPkgHealAttempts || 0) : 0;
  if (attempts >= GLOBAL_PKG_HEAL_MAX_ATTEMPTS) return {};
  const ok = await install(stale.map((s) => `${s.name}@${latest.version}`));
  // Success is "the stale copies are gone", not "npm exited 0". `npm i -g`
  // installs into the prefix the CURRENT node resolves, which is not
  // necessarily where the stale copy lives (nvm with several node versions,
  // an `npm --prefix` in the user's npmrc, a sudo-owned /usr/local). In that
  // shape npm reported success on every run while `staleGlobalPkgs` kept
  // returning the same entry — the counter reset each time, so the retry
  // budget never ran out and the install re-ran forever, once per throttle
  // window. Re-read instead of trusting the exit code.
  const remaining = ok ? readStale(latest.version) : stale;
  return {
    globalPkgHealVersion: latest.version,
    globalPkgHealAttempts: remaining.length === 0 ? 0 : attempts + 1,
  };
}

// Whether a THROTTLED checkForUpdate should still attempt the global-npm
// self-heal. The post-fetch heal below only runs on the non-throttle path, but
// the ONLY context that can SEE a user's nvm/global prefix is a CLI run under
// that node (globalNodeModulesCandidates is execPath-derived) — and such a run,
// once binary+shell are current, short-circuits at the throttle early-return and
// never reaches the heal. That gap stranded a global `code-graph-mcp` shim at
// 0.101.0 while the binary reached 0.103.0 (RCA 2026-07-24). Cheap local
// package.json read (readStale) gates the slow, lock-guarded npm path. Split out
// so the decision is unit-testable without the full checkForUpdate harness.
function shouldHealGlobalsOnThrottle(state, { readStale = staleGlobalPkgs } = {}) {
  if (!state || !state.latestVersion) return false;
  if (process.env.CODE_GRAPH_INSTALL_LOCK_HELD === '1') return false; // parent launcher holds it
  return readStale(state.latestVersion).length > 0;
}

// `requestJsonFn` is a test seam, forwarded to fetchLatestRelease — the same
// injection point that function already exposes. It exists so the 403 path can
// be driven without a network: that path is where the rate-limit backoff either
// engages or is silently erased, and no other observable distinguishes the two.
async function checkForUpdate({ installMissing = false, force = false, requestJsonFn } = {}) {
  let installLock = null;
  try {
    // Skip in dev mode / when the user opted out — unless the launcher
    // explicitly requested a missing-binary install, in which case we MUST
    // proceed regardless of mode (the alternative is wedging the MCP server
    // with no binary on disk).
    if (!installMissing && (isDevMode() || isAutoUpdateDisabled())) return null;

    const state = readState();
    // manifest.version is authoritative — /plugin update writes it directly and
    // bypasses auto-update.js, so re-sync state.installedVersion every call.
    const installedVersion = readManifest().version || '0.0.0';

    // A state we could not read authorises nothing: every throttle, budget and
    // suspension below is derived from it, so acting on a blank stand-in would
    // bypass all of them at once. Skip this session and rewrite a clean file —
    // `lastCheck` stamped now means the ordinary interval applies from here, so
    // the recovery is bounded rather than an immediate retry.
    if (state.stateUnreadable) {
      saveState({ installedVersion, lastCheck: new Date().toISOString() });
      return null;
    }

    // Time-based throttle. Two conditions override it: a missing cache binary
    // (launcher cannot start) and a present-but-stale binary (otherwise it stays
    // pinned to the old version for up to a full check interval — the binary
    // self-heal would never run inside the throttle window). Both bypass to the
    // fetch + self-heal path below — but they are ARGUMENTS to shouldCheck, not
    // `||`-ed around it: as short-circuits out here they sat above the
    // rate-limit backoff and the suspension state, the two conditions under
    // which a fetch cannot accomplish anything at all.
    const binaryMissing = !fs.existsSync(cachedBinaryPath());
    const binaryStale = cachedBinaryStaleVsState(state);
    if (!shouldCheck(state, { force, binaryMissing, binaryStale })) {
      if (state.installedVersion !== installedVersion) {
        saveState({ ...state, installedVersion });
      }
      // Global-npm shell/platform self-heal reaches the throttle window too (see
      // shouldHealGlobalsOnThrottle). Cheap local check first; only the actually-
      // stale case takes the slow, lock-guarded npm path.
      if (shouldHealGlobalsOnThrottle(state)) {
        installLock = acquireLock(path.join(CACHE_DIR, 'install.lock'));
        if (installLock) {
          const globalHeal = await selfHealGlobalPkgs({ version: state.latestVersion }, state);
          saveState({ ...readState(), ...globalHeal });
        }
      }
      if (state.updateAvailable && state.latestVersion
          && compareVersions(state.latestVersion, installedVersion) > 0) {
        return { updateAvailable: true, from: installedVersion, to: state.latestVersion };
      }
      return null;
    }

    // Check GitHub for latest release
    const latest = await fetchLatestRelease(requestJsonFn || requestJson);
    if (!latest) {
      // Re-read, do NOT spread the pre-fetch `state`. On a 403 fetchLatestRelease
      // writes `rateLimited: true` to the state file, and this is the branch it
      // returns null through — spreading the stale snapshot wrote that flag
      // straight back to whatever it was before (normally absent). The
      // RATE_LIMIT_INTERVAL_MS backoff in shouldCheck() therefore never engaged:
      // it read a state where rateLimited had just been erased by the very call
      // that set it, and kept polling GitHub on the ordinary interval while
      // already rate-limited. Dead code since the backoff was written.
      saveState({ ...readState(), installedVersion, lastCheck: new Date().toISOString() });
      // NOT null: null is the CLI's "nothing to do" and it prints "Up to date
      // (vX)". A failed fetch means the opposite — the update status is
      // UNKNOWN — and a user running this because they are stuck on an old
      // version was being told the old version is current (offline, captive
      // portal, proxy, GitHub 5xx all land here).
      return { noop: true, reason: 'fetch-failed', from: installedVersion };
    }

    // Compare versions
    const hasUpdate = compareVersions(latest.version, installedVersion) > 0;

    // Inter-process gate for every mutating path below (plugin-cache copy,
    // binary download, global npm heals): concurrent sessions racing here ran
    // parallel `npm install -g` against one global prefix and clobbered each
    // other's state-file counters (rateLimited, heal attempts). Skip-if-held:
    // the holder does the work and its state outcome wins. The launcher's
    // install chain already holds this lock across its spawn of this script —
    // it marks that with CODE_GRAPH_INSTALL_LOCK_HELD so we don't deadlock
    // against our own parent.
    if (process.env.CODE_GRAPH_INSTALL_LOCK_HELD !== '1') {
      installLock = acquireLock(path.join(CACHE_DIR, 'install.lock'));
      // Reason, not null — same misreport as the fetch failure above: the
      // holder is mid-update, so "Up to date (v<old>)" is exactly wrong.
      if (!installLock) return { noop: true, reason: 'install-lock-held', from: installedVersion };
    }

    if (hasUpdate) {
      // Attempts are counted PER target version, so a newly published release
      // always starts with a full budget. The counter used to be unscoped, which
      // only mattered because nothing ever read it: the download chain re-ran on
      // every single session no matter how many times it had already failed.
      const sameTarget = state.latestVersion === latest.version;
      const attempts = sameTarget ? (state.updateAttempts || 0) : 0;
      // A suspended release gets one retry per day (SUSPENSION_RETRY_MS), so a
      // transient cause — sidecar blip, captive portal, briefly-full disk —
      // heals itself instead of parking the updater until the next release.
      // Spend the retry by entering the attempt path with the budget one short:
      // if it fails again it re-suspends immediately (and re-stamps the clock),
      // costing at most one download per day rather than one per session.
      const suspendedAt = sameTarget && state.suspendedAt ? Date.parse(state.suspendedAt) : NaN;
      const retryDue = Number.isFinite(suspendedAt) && (Date.now() - suspendedAt) >= SUSPENSION_RETRY_MS;
      if (attempts >= MAX_UPDATE_ATTEMPTS && !retryDue) {
        // Suspended — check-only from here until a newer release moves the
        // target. The one thing still worth attempting is a MISSING cached
        // binary: without it the MCP server has no engine at all, so that
        // self-heal stays reachable while the (separately failing) plugin
        // tarball chain and the global-npm heal stay parked.
        const healedMissing = !fs.existsSync(cachedBinaryPath()) && await downloadBinary(latest);
        saveState({
          ...state,
          installedVersion,
          lastCheck: new Date().toISOString(),
          latestVersion: latest.version,
          updateAvailable: true,
          updateAttempts: attempts,
          // Stamp on ENTRY to suspension, then leave it alone: the retry clock
          // must measure time since we gave up, not time since the last check
          // (which every session would reset, making the retry unreachable).
          suspendedAt: (sameTarget && state.suspendedAt) || new Date().toISOString(),
          rateLimited: false,
          binaryUpdated: healedMissing || state.binaryUpdated,
        });
        console.error(
          `[code-graph] Auto-update to v${latest.version} suspended after ${attempts} failed attempts on this machine. ` +
          'Update manually (`/plugin update code-graph-mcp`, or `npm install -g @sdsrs/code-graph`) or run `code-graph-mcp doctor`. ' +
          'Retried automatically once a day, and immediately when a newer release is published.'
        );
        return { updateAvailable: true, suspended: true, from: installedVersion, to: latest.version };
      }
      const result = await downloadAndInstall(latest);
      const success = result.pluginUpdated;
      // Suspension clock. It restarts when the daily retry is spent and fails,
      // which is what keeps `retryDue` from staying true and turning the retry
      // back into a per-session treadmill; it clears on success and on a new
      // target version, so a stale stamp from the previous release cannot make
      // the next one look instantly retry-due.
      let nextSuspendedAt;
      if (success) nextSuspendedAt = null;
      else if (retryDue) nextSuspendedAt = new Date().toISOString();
      else if (!sameTarget) nextSuspendedAt = null;
      else nextSuspendedAt = state.suspendedAt || null;
      const newState = {
        // Carry the prior state forward. Every OTHER saveState in this function
        // spreads `...state`; this one rebuilt from scratch, so any key it does
        // not name was dropped on every update check that found a new release.
        // The keys that matter are selfHealGlobalPkgs' — globalPkgHealAttempts /
        // globalPkgHealVersion — because that function returns `{}` once the
        // attempt cap is hit, meaning the spread below contributes nothing and
        // the counter reset to zero. A capped-out global heal therefore got a
        // fresh budget on every release, which is precisely the retry treadmill
        // the cap exists to stop. Keys this object sets are overwritten below;
        // nothing stale leaks through.
        ...state,
        lastCheck: new Date().toISOString(),
        installedVersion: success ? latest.version : installedVersion,
        latestVersion: latest.version,
        updateAvailable: !success,
        // Consecutive failed-download counter. The statusline shows "↻ updating"
        // while updateAvailable is set; without a bound, a persistently-failing
        // update (missing tar/curl, full disk, blocked network) pins "updating"
        // forever, asserting a self-heal that never happens. The statusline stops
        // trusting it past STUCK_UPDATE_ATTEMPTS; success resets to 0.
        updateAttempts: success ? 0 : attempts + 1,
        suspendedAt: nextSuspendedAt,
        lastUpdate: success ? new Date().toISOString() : state.lastUpdate,
        rateLimited: false,
        binaryUpdated: result.binaryUpdated,
        marketplaceRefreshed: result.marketplaceRefreshed,
      };
      // Keep any globally-installed shell/platform npm packages in step with
      // the release the plugin just moved to (see selfHealGlobalPkgs).
      const globalHeal = await selfHealGlobalPkgs(latest, state);
      saveState({ ...newState, ...globalHeal });

      return {
        updateAvailable: !success,
        updated: success,
        binaryUpdated: result.binaryUpdated,
        from: installedVersion,
        to: latest.version,
      };
    }

    // No plugin-shell update — but self-heal the native binary if it is missing
    // OR stale (see selfHealStaleBinary). The shell version (manifest.version)
    // can match latest while the cached binary lags — this is exactly the wild
    // failure observed in the field (shell at v0.45, binary pinned at v0.16.6).
    const binaryHeal = await selfHealStaleBinary(latest, { state });
    const selfHealedBinary = binaryHeal.healed;

    // Same for the GLOBAL npm delivery surface (the `code-graph-mcp` CLI on
    // PATH + any explicitly-installed platform package): nothing else ever
    // updates it, and stale copies drift for months (0.46.0 wrapper) or years
    // (the 0.16.6 platform relic).
    const globalHeal = await selfHealGlobalPkgs(latest, state);

    saveState({
      ...state,
      installedVersion,
      lastCheck: new Date().toISOString(),
      latestVersion: latest.version,
      updateAvailable: false,
      // Reaching here means the installed shell IS the latest release, so any
      // failure record describes an update that is no longer pending. Leaving it
      // set kept `doctor` warning "vX failed to install 5× — auto-retry
      // throttled" about a version already installed, and left a suspension
      // stamp that the next release then had to age out. The common way to get
      // here from a suspended state is the manual route the suspension notice
      // itself recommends (`npm install -g` / `/plugin update`) — the updater
      // has to notice that its advice was taken.
      updateAttempts: 0,
      suspendedAt: null,
      rateLimited: false,
      binaryUpdated: selfHealedBinary || state.binaryUpdated,
      // The shell-update counters above reset because the shell IS current.
      // The BINARY heal keeps its own, un-reset budget — clearing it here is
      // what made the stale-binary re-download unbounded (P1-14).
      ...binaryHeal.patch,
      ...globalHeal,
    });
    return selfHealedBinary
      ? { updated: false, binaryUpdated: true, from: installedVersion, to: installedVersion }
      : null;
  } catch {
    // Silent failure — never block session
    return null;
  } finally {
    if (installLock) installLock.release();
  }
}

module.exports = {
  checkForUpdate, commandExists, isDevMode, readState, compareVersions, shouldCheck,
  isUpdateSuspended,
  getExtractedPluginVersion, readBinaryVersion, promoteVerifiedBinary,
  isSilentMode, isInstallMissingMode, isForceMode, isAutoUpdateDisabled,
  MAX_UPDATE_ATTEMPTS,
  requestJson, resolveProxy, parseLatestRelease, fetchLatestRelease,
  PLUGIN_ASSET_NAME,
  downloadBinary, cachedBinaryPath, cachedBinaryNeedsUpdate, cachedBinaryStaleVsState,
  getPlatformAssetName,
  selfHealStaleBinary, isBinaryHealExhausted,
  selfHealGlobalPkgs, staleGlobalPkgs, globalPkgVersion, npmInstallGlobal,
  shouldHealGlobalsOnThrottle, inactiveNodeGlobalRelics,
  downloadAndInstall, refreshMarketplaceClone, marketplaceCloneDir,
};

// CLI: node auto-update.js [check|status] [--silent] [--install-missing]
if (require.main === module) {
  (async () => {
    const argv = process.argv.slice(2);
    const cmd = argv.find(arg => !arg.startsWith('--')) || 'check';
    const silent = isSilentMode(argv);
    const installMissing = isInstallMissingMode(argv);
    const force = isForceMode(argv);
    if (cmd === 'status') {
      const state = readState();
      console.log(JSON.stringify(state, null, 2));
    } else {
      if (!silent) console.log('Checking for updates...');
      const result = await checkForUpdate({ installMissing, force });
      if (silent) return;
      if (result && result.updated) {
        console.log(`Updated: v${result.from} → v${result.to} (binary: ${result.binaryUpdated ? 'yes' : 'no'})`);
      } else if (result && result.suspended) {
        console.log(`Update available: v${result.to} — auto-install SUSPENDED after ${MAX_UPDATE_ATTEMPTS} failed attempts. Update manually; retries resume on the next release.`);
      } else if (result && result.updateAvailable) {
        console.log(`Update available: v${result.to} (auto-install failed)`);
      } else if (result && result.binaryUpdated) {
        console.log(`Repaired binary cache (v${result.to})`);
      } else if (result && result.noop && result.reason === 'fetch-failed') {
        // Message only — the exit code stays 0 on purpose: an unreachable
        // GitHub is not a failure of this command, and the launcher's install
        // chain plus `doctor` both spawn it.
        console.log(`Could not reach GitHub — update status UNKNOWN (still on v${result.from}). ` +
          'Retried on the next session; run `code-graph-mcp doctor` if it persists.');
      } else if (result && result.noop && result.reason === 'install-lock-held') {
        console.log(`Another session is installing/updating right now — check skipped (on v${result.from}).`);
      } else if (!installMissing && isAutoUpdateDisabled()) {
        console.log('CODE_GRAPH_NO_AUTO_UPDATE=1 — auto-update skipped');
      } else if (!installMissing && isDevMode()) {
        console.log('Dev mode — auto-update skipped');
      } else {
        const manifest = readManifest();
        console.log(`Up to date (v${manifest.version || 'unknown'})`);
      }
    }
  })();
}
