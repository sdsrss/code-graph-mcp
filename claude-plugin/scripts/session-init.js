#!/usr/bin/env node
'use strict';
const { execFileSync } = require('child_process');
const fs = require('fs');
const path = require('path');
const os = require('os');
const { findBinary } = require('./find-binary');
const { install, update, readManifest, getPluginVersion, checkScopeConflict } = require('./lifecycle');
const { checkForUpdate, readState: readUpdateState } = require('./auto-update');

let BIN = findBinary();

// --- 0b. Retry pending binary update from previous failed auto-update ---
{
  const updateState = readUpdateState();
  if (updateState.pendingBinaryUpdate) {
    const pendingVer = updateState.pendingBinaryUpdate;
    try {
      execFileSync('npm', ['install', '-g', `@sdsrs/code-graph@${pendingVer}`], {
        timeout: 30000, stdio: 'pipe'
      });
      try { fs.unlinkSync(path.join(os.homedir(), '.cache', 'code-graph', 'binary-path')); } catch {}
      // Clear pending flag
      const { writeJsonAtomic, CACHE_DIR } = require('./lifecycle');
      const s = readUpdateState();
      delete s.pendingBinaryUpdate;
      writeJsonAtomic(path.join(CACHE_DIR, 'update-state.json'), s);
      process.stderr.write(`[code-graph] Binary retry succeeded: v${pendingVer}\n`);
      BIN = findBinary(); // refresh
    } catch { /* npm still not available — will retry next session */ }
  }
}

// --- 0. Auto-install binary if missing ---
if (!BIN) {
  const version = getPluginVersion();
  process.stderr.write(`[code-graph] Binary not found, installing @sdsrs/code-graph@${version}...\n`);
  try {
    execFileSync('npm', ['install', '-g', `@sdsrs/code-graph@${version}`], {
      timeout: 60000, stdio: 'pipe'
    });
    // Clear cached path so findBinary picks up the new install
    try { fs.unlinkSync(path.join(os.homedir(), '.cache', 'code-graph', 'binary-path')); } catch {}
    BIN = findBinary();
    if (BIN) {
      process.stderr.write(`[code-graph] Installed v${version} at ${BIN}\n`);
    } else {
      process.stderr.write('[code-graph] Install succeeded but binary not found in PATH. Try: npx @sdsrs/code-graph@latest\n');
    }
  } catch {
    process.stderr.write(
      `[code-graph] Auto-install failed. Run manually: npm install -g @sdsrs/code-graph@${version}\n`
    );
  }
}

// --- 1. Health check (always runs) ---
let healthNodes = -1;
if (BIN) {
  try {
    const out = execFileSync(BIN, ['health-check', '--format', 'oneline'], {
      timeout: 2000,
      stdio: ['pipe', 'pipe', 'pipe']
    }).toString().trim();
    if (out) process.stdout.write(out);
    // Parse node count for empty-index detection
    const m = out.match(/(\d+)\s*nodes/);
    if (m) healthNodes = parseInt(m[1], 10);
  } catch { /* timeout — silent */ }
}

// --- 1a. Auto-index empty databases (fallback if MCP hasn't triggered indexing) ---
if (BIN && healthNodes === 0) {
  const dbExists = fs.existsSync(path.join(process.cwd(), '.code-graph', 'index.db'));
  if (dbExists) {
    // DB exists but empty — MCP server likely hasn't received notifications/initialized yet.
    // Trigger CLI indexing as fallback so the index is ready before first tool call.
    try {
      process.stderr.write('[code-graph] Empty index detected, running initial indexing...\n');
      const result = execFileSync(BIN, ['incremental-index', '--quiet'], {
        timeout: 15000, // 15s max (SessionStart hook has 20s budget)
        stdio: ['pipe', 'pipe', 'pipe'],
      }).toString().trim();
      if (result) process.stderr.write(`[code-graph] ${result}\n`);
      // Re-run health check to update statusline with new counts
      try {
        const out2 = execFileSync(BIN, ['health-check', '--format', 'oneline'], {
          timeout: 2000, stdio: ['pipe', 'pipe', 'pipe']
        }).toString().trim();
        if (out2) process.stdout.write(`\n${out2}`);
      } catch { /* ok */ }
    } catch (e) {
      process.stderr.write(`[code-graph] Auto-index failed: ${e.message || e}\n`);
    }
  }
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
        let binarySynced = false;
        try {
          execFileSync('npm', ['install', '-g', `@sdsrs/code-graph@${pluginVersion}`], {
            timeout: 30000, stdio: 'pipe'
          });
          // Clear cached binary path so next lookup finds the new binary
          try { fs.unlinkSync(path.join(os.homedir(), '.cache', 'code-graph', 'binary-path')); } catch {}
          process.stderr.write(`[code-graph] Binary updated to v${pluginVersion}\n`);
          binarySynced = true;
        } catch {
          process.stderr.write(
            `[code-graph] Auto-update failed. Run: npm install -g @sdsrs/code-graph@${pluginVersion}\n`
          );
        }
        if (binarySynced) {
          // MCP server is still running old binary — prompt user to reconnect
          process.stdout.write(
            `\n\u26A0\uFE0F [code-graph] Binary updated v${binVersion} \u2192 v${pluginVersion}. ` +
            `Run /mcp to reconnect MCP server with new version.\n`
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
    process.stdout.write(
      `\n\uD83D\uDD04 [code-graph] Auto-updated v${result.from} \u2192 v${result.to}. ` +
      `Run /mcp to use the new version.\n`
    );
  } else if (result && result.updateAvailable) {
    process.stderr.write(
      `[code-graph] Update available: v${result.from} \u2192 v${result.to}. ` +
      `Run: npx @sdsrs/code-graph@latest\n`
    );
  }
})();
