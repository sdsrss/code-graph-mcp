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
  const mapInjected = injectProjectMap();
  return { inactive: false, lifecycle, autoUpdateLaunched, mapInjected };
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
  injectProjectMap,
  runSessionInit,
};

if (require.main === module) {
  runSessionInit();
}
