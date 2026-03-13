#!/usr/bin/env node
'use strict';
const { execFileSync } = require('child_process');
const { findBinary } = require('./find-binary');
const { install, update, readManifest, getPluginVersion } = require('./lifecycle');
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

// --- 2. Lifecycle: install or update config (idempotent) ---
const manifest = readManifest();
const currentVersion = getPluginVersion();

if (!manifest.version) {
  install();
} else if (manifest.version !== currentVersion) {
  update();
}

// --- 3. Auto-update (throttled, non-blocking) ---
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
