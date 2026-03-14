#!/usr/bin/env node
'use strict';
const { execFileSync } = require('child_process');
const fs = require('fs');
const path = require('path');
const os = require('os');
const { CACHE_DIR, PLUGIN_ID, MARKETPLACE_NAME, readManifest, readJson, writeJsonAtomic } = require('./lifecycle');

// ── Configuration ──────────────────────────────────────────
const GITHUB_REPO = 'sdsrss/code-graph-mcp';
const NPM_PACKAGE = '@sdsrs/code-graph';
const STATE_FILE = path.join(CACHE_DIR, 'update-state.json');
const CHECK_INTERVAL_MS = 24 * 60 * 60 * 1000;       // 24h
const RATE_LIMIT_INTERVAL_MS = 6 * 60 * 60 * 1000;   // 6h if rate-limited
const FETCH_TIMEOUT_MS = 3000;

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
  const pluginRoot = process.env.CLAUDE_PLUGIN_ROOT || path.resolve(__dirname, '..');
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

async function fetchLatestRelease() {
  const url = `https://api.github.com/repos/${GITHUB_REPO}/releases/latest`;
  try {
    const res = await fetch(url, {
      signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
      headers: {
        'Accept': 'application/vnd.github+json',
        'User-Agent': 'code-graph-auto-update/1.0',
      },
    });

    if (res.status === 403) {
      // Rate limited
      const state = readState();
      saveState({ ...state, rateLimited: true });
      return null;
    }
    if (!res.ok) return null;

    const data = await res.json();
    return {
      version: data.tag_name.replace(/^v/, ''),
      tarballUrl: data.tarball_url,
    };
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

// ── Download & Install ─────────────────────────────────────

async function downloadAndInstall(latest) {
  const tmpDir = path.join(os.tmpdir(), `code-graph-update-${Date.now()}`);
  try {
    fs.mkdirSync(tmpDir, { recursive: true });

    // 1. Download tarball (safe: no shell interpolation)
    const tarballPath = path.join(tmpDir, 'release.tar.gz');
    execFileSync('curl', [
      '-sL', '-o', tarballPath,
      '-H', 'Accept: application/vnd.github+json',
      latest.tarballUrl,
    ], { timeout: 30000, stdio: 'pipe' });

    // 2. Extract tarball
    execFileSync('tar', [
      'xzf', tarballPath, '-C', tmpDir, '--strip-components=1',
    ], { timeout: 15000, stdio: 'pipe' });

    // 3. Copy plugin files to cache (cross-platform)
    const pluginSrc = path.join(tmpDir, 'claude-plugin');
    const pluginDst = path.join(
      os.homedir(), '.claude', 'plugins', 'cache', MARKETPLACE_NAME, 'code-graph', latest.version
    );

    if (fs.existsSync(pluginSrc)) {
      fs.mkdirSync(pluginDst, { recursive: true });
      copyDirSync(pluginSrc, pluginDst);
    }

    // 4. Update installed_plugins.json to point to new version
    const installedPath = path.join(os.homedir(), '.claude', 'plugins', 'installed_plugins.json');
    try {
      const installed = readJson(installedPath);
      if (installed && installed.plugins && installed.plugins[PLUGIN_ID]) {
        installed.plugins[PLUGIN_ID][0].installPath = pluginDst;
        installed.plugins[PLUGIN_ID][0].version = latest.version;
        installed.plugins[PLUGIN_ID][0].lastUpdated = new Date().toISOString();
        writeJsonAtomic(installedPath, installed);
      }
    } catch { /* installed_plugins update failed — not fatal */ }

    // 5. Update install manifest with tag version
    try {
      const manifest = readManifest();
      manifest.version = latest.version;
      manifest.updatedAt = new Date().toISOString();
      writeJsonAtomic(path.join(CACHE_DIR, 'install-manifest.json'), manifest);
    } catch { /* manifest update failed — not fatal */ }

    // 6. Update npm binary (non-blocking, best-effort)
    try {
      execFileSync('npm', ['install', '-g', `${NPM_PACKAGE}@${latest.version}`], {
        timeout: 60000,
        stdio: 'pipe',
      });
    } catch {
      // npm install failed — plugin files still updated
      // User can manually update binary later
    }

    return true;
  } catch { return false; }
  finally {
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
      // Report pending update from previous check
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
      // Auto-update
      const success = await downloadAndInstall(latest);
      const newState = {
        lastCheck: new Date().toISOString(),
        installedVersion: success ? latest.version : currentVersion,
        latestVersion: latest.version,
        updateAvailable: !success,
        lastUpdate: success ? new Date().toISOString() : state.lastUpdate,
        rateLimited: false,
      };
      saveState(newState);

      return {
        updateAvailable: !success,
        updated: success,
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

module.exports = { checkForUpdate, isDevMode, readState };

// CLI: node auto-update.js [check|status]
if (require.main === module) {
  (async () => {
    const cmd = process.argv[2] || 'check';
    if (cmd === 'status') {
      const state = readState();
      console.log(JSON.stringify(state, null, 2));
    } else {
      console.log('Checking for updates...');
      const result = await checkForUpdate();
      if (result && result.updated) {
        console.log(`Updated: v${result.from} → v${result.to}`);
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
