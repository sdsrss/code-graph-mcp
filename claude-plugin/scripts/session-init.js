#!/usr/bin/env node
'use strict';
const { spawn } = require('child_process');
const path = require('path');
const {
  install, update, readManifest, getPluginVersion, checkScopeConflict,
  cleanupDisabledStatusline, isPluginInactive,
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
  return { inactive: false, lifecycle, autoUpdateLaunched };
}

module.exports = {
  launchBackgroundAutoUpdate,
  syncLifecycleConfig,
  runSessionInit,
};

if (require.main === module) {
  runSessionInit();
}
