#!/usr/bin/env node
'use strict';
const { execFileSync } = require('child_process');
const fs = require('fs');
const path = require('path');
const os = require('os');
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

// --- 1c. Binary version sync (plugin may update before npm binary) ---
if (BIN) {
  try {
    const binOut = execFileSync(BIN, ['--version'], { timeout: 2000, stdio: 'pipe' }).toString().trim();
    const binVersion = binOut.replace(/^code-graph-mcp\s+/, '');
    const pluginVersion = getPluginVersion();
    if (binVersion && pluginVersion && /^\d+\.\d+\.\d+$/.test(binVersion)) {
      const bv = binVersion.split('.').map(Number);
      const pv = pluginVersion.split('.').map(Number);
      const pluginNewer = (pv[0] > bv[0]) ||
        (pv[0] === bv[0] && pv[1] > bv[1]) ||
        (pv[0] === bv[0] && pv[1] === bv[1] && pv[2] > bv[2]);
      if (pluginNewer) {
        process.stderr.write(`[code-graph] Binary v${binVersion} < plugin v${pluginVersion}, updating...\n`);
        try {
          execFileSync('npm', ['install', '-g', `@sdsrs/code-graph@${pluginVersion}`], {
            timeout: 30000, stdio: 'pipe'
          });
          // Clear cached binary path so next lookup finds the new binary
          try { fs.unlinkSync(path.join(os.homedir(), '.cache', 'code-graph', 'binary-path')); } catch {}
          process.stderr.write(`[code-graph] Binary updated to v${pluginVersion}\n`);
        } catch {
          process.stderr.write(
            `[code-graph] Auto-update failed. Run: npm install -g @sdsrs/code-graph@${pluginVersion}\n`
          );
        }
      }
    }
  } catch { /* version check failed — not critical */ }
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
