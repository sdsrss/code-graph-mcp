#!/usr/bin/env node
'use strict';
const { execFileSync } = require('child_process');
const fs = require('fs');
const https = require('https');
const path = require('path');
const os = require('os');
const { CACHE_DIR, PLUGIN_ID, MARKETPLACE_NAME, readManifest, readJson, writeJsonAtomic } = require('./lifecycle');
const { clearCache: clearBinaryCache } = require('./find-binary');

// ── Environment Checks ────────────────────────────────────

/**
 * Check if a command-line tool is available on the system PATH.
 * @param {string} cmd - Command name (e.g., 'curl', 'tar')
 * @returns {boolean}
 */
function commandExists(cmd) {
  try {
    const whichCmd = process.platform === 'win32' ? 'where' : 'which';
    execFileSync(whichCmd, [cmd], { stdio: 'ignore' });
    return true;
  } catch {
    return false;
  }
}

// ── Configuration ──────────────────────────────────────────
const GITHUB_REPO = 'sdsrss/code-graph-mcp';
const STATE_FILE = path.join(CACHE_DIR, 'update-state.json');
const BINARY_CACHE_DIR = path.join(CACHE_DIR, 'bin');
const CHECK_INTERVAL_MS = 6 * 60 * 60 * 1000;        // 6h
const RATE_LIMIT_INTERVAL_MS = 24 * 60 * 60 * 1000;  // 24h if rate-limited
const FETCH_TIMEOUT_MS = 3000;
const VERSION_OUTPUT_RE = /^code-graph-mcp\s+(\d+\.\d+\.\d+)$/;

function isSilentMode(argv = process.argv.slice(2), env = process.env) {
  return argv.includes('--silent') || env.CODE_GRAPH_AUTO_UPDATE_SILENT === '1';
}

// ── Platform → GitHub release asset name mapping ──────────
function getPlatformAssetName() {
  const platform = os.platform();
  const arch = os.arch();
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

function readState() {
  return readJson(STATE_FILE) || {};
}

function saveState(state) {
  try {
    writeJsonAtomic(STATE_FILE, state);
  } catch { /* ok */ }
}

// ── Dev Mode Detection ─────────────────────────────────────

function isDevMode() {
  // Always derive from __dirname — CLAUDE_PLUGIN_ROOT can leak from other plugins
  const pluginRoot = path.resolve(__dirname, '..');
  // Dev mode: running from source repo (has Cargo.toml nearby)
  if (fs.existsSync(path.join(pluginRoot, '..', 'Cargo.toml'))) return true;
  // Dev mode: plugin root is a symlink
  try { if (fs.lstatSync(pluginRoot).isSymbolicLink()) return true; } catch { /* ok */ }
  return false;
}

// ── Throttle ───────────────────────────────────────────────

function shouldCheck(state) {
  if (!state.lastCheck) return true;
  const elapsed = Date.now() - new Date(state.lastCheck).getTime();
  const interval = state.rateLimited ? RATE_LIMIT_INTERVAL_MS : CHECK_INTERVAL_MS;
  return elapsed >= interval;
}

// ── Version Comparison (semver) ────────────────────────────

function compareVersions(a, b) {
  const pa = a.split('.').map(Number);
  const pb = b.split('.').map(Number);
  for (let i = 0; i < 3; i++) {
    if ((pa[i] || 0) > (pb[i] || 0)) return 1;
    if ((pa[i] || 0) < (pb[i] || 0)) return -1;
  }
  return 0;
}

// ── GitHub API ─────────────────────────────────────────────

function requestJson(url, timeoutMs = FETCH_TIMEOUT_MS) {
  return new Promise((resolve, reject) => {
    const req = https.request(url, {
      method: 'GET',
      headers: {
        'Accept': 'application/vnd.github+json',
        'User-Agent': 'code-graph-auto-update/1.0',
      },
    }, (res) => {
      let body = '';
      res.setEncoding('utf8');
      res.on('data', (chunk) => { body += chunk; });
      res.on('end', () => {
        if (!res.statusCode) {
          reject(new Error('missing status code'));
          return;
        }
        resolve({ statusCode: res.statusCode, body });
      });
    });

    req.setTimeout(timeoutMs, () => req.destroy(new Error('request timeout')));
    req.on('error', reject);
    req.end();
  });
}

function parseLatestRelease(data, assetName = getPlatformAssetName()) {
  if (!data || typeof data.tag_name !== 'string' || typeof data.tarball_url !== 'string') {
    return null;
  }

  let binaryUrl = null;
  if (assetName && Array.isArray(data.assets)) {
    const asset = data.assets.find((entry) => entry && entry.name === assetName);
    if (asset && typeof asset.browser_download_url === 'string') {
      binaryUrl = asset.browser_download_url;
    }
  }

  return {
    version: data.tag_name.replace(/^v/, ''),
    tarballUrl: data.tarball_url,
    binaryUrl,
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

function promoteVerifiedBinary(binaryTmp, binaryDst, expectedVersion) {
  try {
    const stat = fs.statSync(binaryTmp);
    if (stat.size <= 1_000_000) return false;

    const actualVersion = readBinaryVersion(binaryTmp);
    if (!actualVersion || (expectedVersion && actualVersion !== expectedVersion)) {
      return false;
    }

    fs.renameSync(binaryTmp, binaryDst);
    if (os.platform() !== 'win32') {
      fs.chmodSync(binaryDst, 0o755);
    }
    clearBinaryCache();
    return true;
  } catch {
    return false;
  } finally {
    try {
      if (fs.existsSync(binaryTmp)) fs.unlinkSync(binaryTmp);
    } catch { /* ok */ }
  }
}

// ── Download & Install ─────────────────────────────────────

async function downloadAndInstall(latest) {
  // Pre-flight: check required CLI tools before attempting any download
  const missingTools = ['curl', 'tar'].filter(cmd => !commandExists(cmd));
  if (missingTools.length > 0) {
    console.error(`[code-graph] Auto-update skipped: missing required tools: ${missingTools.join(', ')}. Install them to enable auto-updates.`);
    return { pluginUpdated: false, binaryUpdated: false };
  }

  const tmpDir = path.join(os.tmpdir(), `code-graph-update-${Date.now()}`);
  let pluginUpdated = false;
  let binaryUpdated = false;

  try {
    fs.mkdirSync(tmpDir, { recursive: true });

    // ── Step 1: Download and install plugin files from tarball ──
    const tarballPath = path.join(tmpDir, 'release.tar.gz');
    execFileSync('curl', [
      '-sL', '-o', tarballPath,
      '-H', 'Accept: application/vnd.github+json',
      latest.tarballUrl,
    ], { timeout: 30000, stdio: 'pipe' });

    execFileSync('tar', [
      'xzf', tarballPath, '-C', tmpDir, '--strip-components=1',
    ], { timeout: 15000, stdio: 'pipe' });

    const pluginSrc = path.join(tmpDir, 'claude-plugin');
    const pluginDst = path.join(
      os.homedir(), '.claude', 'plugins', 'cache', MARKETPLACE_NAME, 'code-graph-mcp', latest.version
    );

    if (fs.existsSync(pluginSrc) && getExtractedPluginVersion(pluginSrc) === latest.version) {
      fs.mkdirSync(pluginDst, { recursive: true });
      copyDirSync(pluginSrc, pluginDst);
      pluginUpdated = true;
    }

    // Update installed_plugins.json to point to new version
    const installedPath = path.join(os.homedir(), '.claude', 'plugins', 'installed_plugins.json');
    try {
      const installed = readJson(installedPath);
      if (installed && installed.plugins && installed.plugins[PLUGIN_ID]) {
        installed.plugins[PLUGIN_ID][0].installPath = pluginDst;
        installed.plugins[PLUGIN_ID][0].version = latest.version;
        installed.plugins[PLUGIN_ID][0].lastUpdated = new Date().toISOString();
        writeJsonAtomic(installedPath, installed);
      }
    } catch { /* not fatal */ }

    // Update install manifest
    try {
      const manifest = readManifest();
      manifest.version = latest.version;
      manifest.updatedAt = new Date().toISOString();
      writeJsonAtomic(path.join(CACHE_DIR, 'install-manifest.json'), manifest);
    } catch { /* not fatal */ }

    // Run the NEW lifecycle.js to update settings.json hooks with new paths.
    // Without this, settings.json hooks still point to the old version directory
    // until the next session's self-heal corrects them.
    if (pluginUpdated) {
      try {
        const newLifecycle = path.join(pluginDst, 'scripts', 'lifecycle.js');
        if (fs.existsSync(newLifecycle)) {
          execFileSync(process.execPath, [newLifecycle, 'update'], {
            timeout: 5000, stdio: 'pipe',
          });
        }
      } catch { /* not fatal — syncLifecycleConfig will self-heal on next session */ }
    }

    // ── Step 2: Download platform binary directly from GitHub release ──
    if (latest.binaryUrl) {
      try {
        const binaryName = os.platform() === 'win32' ? 'code-graph-mcp.exe' : 'code-graph-mcp';
        const binaryDst = path.join(BINARY_CACHE_DIR, binaryName);
        const binaryTmp = binaryDst + '.tmp.' + process.pid;

        fs.mkdirSync(BINARY_CACHE_DIR, { recursive: true });
        execFileSync('curl', [
          '-sL', '-o', binaryTmp,
          latest.binaryUrl,
        ], { timeout: 60000, stdio: 'pipe' });

        if (promoteVerifiedBinary(binaryTmp, binaryDst, latest.version)) {
          binaryUpdated = true;
        }
      } catch (e) {
        // Binary download failed — plugin update still counts as success
        console.error(`[code-graph] Binary download failed: ${e.message}`);
      }
    }

    return { pluginUpdated, binaryUpdated };
  } catch (e) {
    console.error(`[code-graph] Plugin download/extract failed: ${e.message}`);
    return { pluginUpdated: false, binaryUpdated: false };
  } finally {
    try { fs.rmSync(tmpDir, { recursive: true, force: true }); } catch { /* ok */ }
  }
}

// ── Main Entry ─────────────────────────────────────────────

async function checkForUpdate() {
  try {
    // Skip in dev mode
    if (isDevMode()) return null;

    const state = readState();

    // Time-based throttle
    if (!shouldCheck(state)) {
      if (state.updateAvailable && state.latestVersion) {
        return { updateAvailable: true, from: state.installedVersion, to: state.latestVersion };
      }
      return null;
    }

    // Check GitHub for latest release
    const latest = await fetchLatestRelease();
    if (!latest) {
      saveState({ ...state, lastCheck: new Date().toISOString() });
      return null;
    }

    // Compare versions
    const manifest = readManifest();
    const currentVersion = manifest.version || '0.0.0';
    const hasUpdate = compareVersions(latest.version, currentVersion) > 0;

    if (hasUpdate) {
      const result = await downloadAndInstall(latest);
      const success = result.pluginUpdated;
      const newState = {
        lastCheck: new Date().toISOString(),
        installedVersion: success ? latest.version : currentVersion,
        latestVersion: latest.version,
        updateAvailable: !success,
        lastUpdate: success ? new Date().toISOString() : state.lastUpdate,
        rateLimited: false,
        binaryUpdated: result.binaryUpdated,
      };
      saveState(newState);

      return {
        updateAvailable: !success,
        updated: success,
        binaryUpdated: result.binaryUpdated,
        from: currentVersion,
        to: latest.version,
      };
    }

    // No update needed
    saveState({
      ...state,
      lastCheck: new Date().toISOString(),
      latestVersion: latest.version,
      updateAvailable: false,
      rateLimited: false,
    });
    return null;
  } catch {
    // Silent failure — never block session
    return null;
  }
}

module.exports = {
  checkForUpdate, commandExists, isDevMode, readState, compareVersions,
  getExtractedPluginVersion, readBinaryVersion, promoteVerifiedBinary, isSilentMode,
  requestJson, parseLatestRelease, fetchLatestRelease,
};

// CLI: node auto-update.js [check|status]
if (require.main === module) {
  (async () => {
    const argv = process.argv.slice(2);
    const cmd = argv.find(arg => !arg.startsWith('--')) || 'check';
    const silent = isSilentMode(argv);
    if (cmd === 'status') {
      const state = readState();
      console.log(JSON.stringify(state, null, 2));
    } else {
      if (!silent) console.log('Checking for updates...');
      const result = await checkForUpdate();
      if (silent) return;
      if (result && result.updated) {
        console.log(`Updated: v${result.from} → v${result.to} (binary: ${result.binaryUpdated ? 'yes' : 'no'})`);
      } else if (result && result.updateAvailable) {
        console.log(`Update available: v${result.to} (auto-install failed)`);
      } else if (isDevMode()) {
        console.log('Dev mode — auto-update skipped');
      } else {
        const manifest = readManifest();
        console.log(`Up to date (v${manifest.version || 'unknown'})`);
      }
    }
  })();
}
