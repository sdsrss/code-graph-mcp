#!/usr/bin/env node
'use strict';
const { spawn, execSync } = require('child_process');
const path = require('path');
const os = require('os');
const fs = require('fs');
const {
  install, update, readManifest, getPluginVersion, checkScopeConflict,
  cleanupDisabledStatusline, isPluginInactive, readJson,
} = require('./lifecycle');

function launchBackgroundAutoUpdate(spawnFn = spawn, env = process.env) {
  try {
    const child = spawnFn(process.execPath, [path.join(__dirname, 'auto-update.js'), 'check', '--silent'], {
      detached: true,
      stdio: 'ignore',
      env: { ...env, CODE_GRAPH_AUTO_UPDATE_SILENT: '1' },
    });
    if (child && typeof child.unref === 'function') child.unref();
    return true;
  } catch {
    return false;
  }
}

function syncLifecycleConfig() {
  const manifest = readManifest();
  const currentVersion = getPluginVersion();

  if (!manifest.version) {
    install();
    return 'installed';
  }
  if (manifest.version !== currentVersion) {
    update();
    return 'updated';
  }
  // Self-heal: version matches but statusLine may have been lost
  // (e.g. plugin removed and reinstalled without lifecycle uninstall).
  // install() is idempotent — isOurComposite guard prevents duplicate work.
  const settings = readJson(path.join(os.homedir(), '.claude', 'settings.json')) || {};
  if (!settings.statusLine || !settings.statusLine.command ||
      !settings.statusLine.command.includes('statusline-composite')) {
    install();
    return 'self-healed';
  }
  return 'noop';
}

/**
 * Check if the index is stale by comparing git HEAD timestamp vs index.db mtime.
 * If stale, spawn background incremental-index to refresh.
 * Returns 'fresh' | 'refreshing' | 'skipped'.
 */
function ensureIndexFresh() {
  const { findBinary } = require('./find-binary');
  const bin = findBinary();
  if (!bin) return 'skipped';

  const cwd = process.cwd();
  const dbPath = path.join(cwd, '.code-graph', 'index.db');
  if (!fs.existsSync(dbPath)) return 'skipped';

  try {
    const dbMtime = fs.statSync(dbPath).mtimeMs;
    // Compare with git HEAD commit timestamp
    const gitTs = parseInt(
      execSync('git log -1 --format=%ct', { cwd, timeout: 2000, encoding: 'utf8', stdio: ['pipe', 'pipe', 'pipe'] }).trim()
    ) * 1000;
    if (gitTs <= dbMtime) return 'fresh';

    // Index is stale — run incremental-index in background
    const child = spawn(bin, ['incremental-index', '--quiet'], {
      cwd,
      detached: true,
      stdio: 'ignore',
    });
    if (child && typeof child.unref === 'function') child.unref();
    return 'refreshing';
  } catch {
    return 'skipped';
  }
}

function runSessionInit() {
  if (isPluginInactive()) {
    cleanupDisabledStatusline();
    return { inactive: true, lifecycle: 'noop', autoUpdateLaunched: false };
  }

  const conflict = checkScopeConflict();
  if (conflict) {
    process.stderr.write(
      `[code-graph] Warning: conflicting install detected — ${conflict.existingId} (${conflict.scope || 'unknown'} scope). ` +
      `Use /plugin to remove one to avoid config conflicts.\n`
    );
  }

  const lifecycle = syncLifecycleConfig();
  const autoUpdateLaunched = launchBackgroundAutoUpdate();
  const indexFreshness = ensureIndexFresh();
  const mapInjected = injectProjectMap();
  return { inactive: false, lifecycle, autoUpdateLaunched, indexFreshness, mapInjected };
}

/**
 * Inject project_map summary into session context if index exists.
 * Similar to aider's repo-map — gives Claude project structure upfront.
 */
function injectProjectMap() {
  try {
    const cwd = process.cwd();
    const dbPath = path.join(cwd, '.code-graph', 'index.db');
    if (!fs.existsSync(dbPath)) return false;

    const output = execSync('code-graph-mcp map --compact', {
      cwd,
      timeout: 5000,
      encoding: 'utf8',
      stdio: ['pipe', 'pipe', 'pipe'],
    });

    if (output && output.trim()) {
      process.stdout.write(
        '[code-graph] Project map (indexed):\n' + output.trim() + '\n'
      );
      return true;
    }
  } catch {
    // Index not ready or binary not found — skip silently
  }
  return false;
}

module.exports = {
  launchBackgroundAutoUpdate,
  syncLifecycleConfig,
  ensureIndexFresh,
  injectProjectMap,
  runSessionInit,
};

if (require.main === module) {
  runSessionInit();
}
