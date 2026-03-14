#!/usr/bin/env node
'use strict';
const { execFileSync } = require('child_process');
const { findBinary } = require('./find-binary');
const { install, update, readManifest, getPluginVersion, checkScopeConflict } = require('./lifecycle');
const { checkForUpdate } = require('./auto-update');

const BIN = findBinary();

// --- 1. Health check (always runs) ---
if (BIN) {
  try {
    const out = execFileSync(BIN, ['health-check', '--format', 'oneline'], {
      timeout: 2000,
      stdio: ['pipe', 'pipe', 'pipe']
    }).toString().trim();
    if (out) process.stdout.write(out);
  } catch { /* timeout — silent */ }
}

// --- 1b. Suggest project_map as first action ---
if (BIN) {
  process.stdout.write(
    '\n[code-graph] TIP: Call project_map first to get a full architecture overview ' +
    '(modules, dependencies, hot functions, entry points) in one call.\n'
  );
}

// --- 2. Scope conflict warning ---
const conflict = checkScopeConflict();
if (conflict) {
  process.stderr.write(
    `[code-graph] Warning: conflicting install detected — ${conflict.existingId} (${conflict.scope || 'unknown'} scope). ` +
    `Use /plugin to remove one to avoid config conflicts.\n`
  );
}

// --- 3. Lifecycle: install or update config (idempotent) ---
const manifest = readManifest();
const currentVersion = getPluginVersion();

if (!manifest.version) {
  install();
} else if (manifest.version !== currentVersion) {
  update();
}

// --- 4. Auto-update (throttled, non-blocking) ---
(async () => {
  const result = await checkForUpdate();
  if (result && result.updated) {
    process.stderr.write(`[code-graph] Updated: v${result.from} \u2192 v${result.to}\n`);
  } else if (result && result.updateAvailable) {
    process.stderr.write(
      `[code-graph] Update available: v${result.from} \u2192 v${result.to}. ` +
      `Run: npx @sdsrs/code-graph@latest\n`
    );
  }
})();
