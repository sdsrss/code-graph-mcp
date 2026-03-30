#!/usr/bin/env node
'use strict';
const { spawn, execSync, execFileSync } = require('child_process');
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
  // Self-heal: version matches but statusLine may have been lost or path corrupted
  // (e.g. plugin removed and reinstalled, or CLAUDE_PLUGIN_ROOT leaked from another plugin).
  // install() is idempotent — isOurComposite guard prevents duplicate work.
  const settings = readJson(path.join(os.homedir(), '.claude', 'settings.json')) || {};
  if (!settings.statusLine || !settings.statusLine.command ||
      !settings.statusLine.command.includes('statusline-composite')) {
    install();
    return 'self-healed';
  }
  // Also self-heal if composite path points to a non-existent script (path pollution)
  const scriptMatch = settings.statusLine.command.match(/node\s+"([^"]+)"/);
  if (scriptMatch && scriptMatch[1] && !fs.existsSync(scriptMatch[1])) {
    install();
    return 'self-healed-bad-path';
  }
  // Self-heal if any hook command points to a non-existent script (path pollution)
  if (settings.hooks) {
    for (const entries of Object.values(settings.hooks)) {
      if (!Array.isArray(entries)) continue;
      for (const entry of entries) {
        if (!entry.hooks) continue;
        for (const h of entry.hooks) {
          const m = h.command && h.command.match(/node\s+"([^"]+)"/);
          if (m && m[1] && m[1].includes('code-graph') && !fs.existsSync(m[1])) {
            install();
            return 'self-healed-bad-hook';
          }
        }
      }
    }
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

/**
 * Verify binary is available and executable.
 * On macOS, detect Gatekeeper quarantine (common after npm/GitHub download).
 * Returns { available, binary, issue? }.
 */
function verifyBinary() {
  const { findBinary } = require('./find-binary');
  const binary = findBinary();
  if (!binary) {
    process.stderr.write(
      '[code-graph] Binary not found — MCP server cannot start.\n' +
      'Install: npm install -g @sdsrs/code-graph\n'
    );
    return { available: false, binary: null };
  }

  // Check executable permission
  try {
    fs.accessSync(binary, fs.constants.X_OK);
  } catch {
    process.stderr.write(
      `[code-graph] Binary not executable: ${binary}\n` +
      `Fix: chmod +x "${binary}"\n`
    );
    if (process.platform === 'darwin') {
      process.stderr.write(`Also try: xattr -d com.apple.quarantine "${binary}"\n`);
    }
    return { available: false, binary, issue: 'not-executable' };
  }

  // On macOS, verify the binary can actually run (Gatekeeper may block it)
  if (process.platform === 'darwin') {
    try {
      execFileSync(binary, ['--version'], { timeout: 3000, stdio: 'pipe' });
    } catch (err) {
      const msg = (err.message || '') + (err.stderr ? err.stderr.toString() : '');
      if (msg.includes('quarantine') || msg.includes('not permitted') ||
          msg.includes('killed') || err.status === 137 || err.signal === 'SIGKILL') {
        process.stderr.write(
          `[code-graph] macOS Gatekeeper is blocking the binary: ${binary}\n` +
          `Fix: xattr -d com.apple.quarantine "${binary}"\n` +
          `Then restart Claude Code to reconnect the MCP server.\n`
        );
        return { available: false, binary, issue: 'quarantine' };
      }
      // Other errors (e.g., missing libs) — still report
      process.stderr.write(
        `[code-graph] Binary found but failed to run: ${binary}\n` +
        `Error: ${msg.slice(0, 200)}\n`
      );
      return { available: false, binary, issue: 'runtime-error' };
    }
  }

  return { available: true, binary };
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

  // Verify binary availability — catch issues early with actionable diagnostics
  const binaryCheck = verifyBinary();

  const autoUpdateLaunched = launchBackgroundAutoUpdate();
  const indexFreshness = binaryCheck.available ? ensureIndexFresh() : 'skipped';
  const mapInjected = binaryCheck.available ? injectProjectMap() : false;
  return { inactive: false, lifecycle, autoUpdateLaunched, indexFreshness, mapInjected, binaryCheck };
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
  verifyBinary,
  runSessionInit,
};

if (require.main === module) {
  runSessionInit();
}
