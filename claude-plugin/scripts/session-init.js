#!/usr/bin/env node
'use strict';
const { spawn, execSync, execFileSync } = require('child_process');
const path = require('path');
const os = require('os');
const fs = require('fs');
const {
  install, update, readManifest, getPluginVersion, checkScopeConflict,
  cleanupDisabledStatusline, isPluginInactive, readJson, CACHE_DIR,
} = require('./lifecycle');
const { readBinaryVersion, isDevMode, getNewestMtime } = require('./version-utils');
const { maybeAutoAdopt, isAdopted } = require('./adopt');

// v0.9.0 — quietHooks 推导：显式 env override > adopted 状态。
// CODE_GRAPH_QUIET_HOOKS='0' 强制 noisy；'1' 强制 quiet；未设 → 跟随 adopted。
function computeQuietHooks({ adopted, env = {} } = {}) {
  const envQuiet = env.CODE_GRAPH_QUIET_HOOKS;
  if (envQuiet === '0') return false;
  if (envQuiet === '1') return true;
  return !!adopted;
}

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

/**
 * Lightweight consistency checks — called from runSessionInit().
 * Returns an array of issue objects: { id, msg, fix }.
 * Empty array = all consistent (silent).
 */
function consistencyCheck(binary) {
  const issues = [];

  // Check 1: Binary version vs plugin version
  try {
    const pluginVersion = getPluginVersion();
    const binaryVersion = readBinaryVersion(binary);
    if (binaryVersion && binaryVersion !== pluginVersion) {
      issues.push({
        id: 'version-mismatch',
        msg: `Binary v${binaryVersion}, plugin expects v${pluginVersion}`,
        fix: isDevMode() ? 'cargo build --release' : 'code-graph-mcp doctor',
      });
    }
  } catch { /* skip check on error */ }

  // Check 2: Source freshness (dev mode only)
  try {
    if (isDevMode()) {
      const srcDir = path.resolve(__dirname, '..', '..', 'src');
      const binaryMtime = fs.statSync(binary).mtimeMs;
      const latestSrcMtime = getNewestMtime(srcDir, '.rs');
      if (latestSrcMtime > binaryMtime) {
        const deltaMin = Math.round((latestSrcMtime - binaryMtime) / 60000);
        issues.push({
          id: 'binary-stale',
          msg: `src/ modified ${deltaMin}min after last build`,
          fix: 'cargo build --release',
        });
      }
    }
  } catch { /* skip check on error */ }

  // Check 3: Auto-update incomplete
  try {
    const statePath = path.join(CACHE_DIR, 'update-state.json');
    const state = readJson(statePath);
    if (state && state.updateAvailable && state.binaryUpdated === false) {
      issues.push({
        id: 'update-incomplete',
        msg: `Plugin updated to v${state.latestVersion}, binary not updated`,
        fix: 'code-graph-mcp doctor',
      });
    }
  } catch { /* skip check on error */ }

  // Output warnings to stderr
  if (issues.length > 0) {
    const lines = [`[code-graph] ${issues.length} consistency issue(s):`];
    issues.forEach((issue, i) => {
      lines.push(`  ${i + 1}. ${issue.msg}`);
      lines.push(`     → ${issue.fix}`);
    });
    process.stderr.write(lines.join('\n') + '\n');
  }

  return issues;
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

  // v0.9.0 C' 上下文感知默认：插件模式下首次 SessionStart 自动 adopt。
  // v0.11.0: 已 adopt 的项目如果 shipped template 漂移也会触发一次刷新。
  // 两种情况都发一次 stderr 提示，让用户知道发生了什么 + 如何回退。
  const autoAdopt = maybeAutoAdopt({ scriptPath: __dirname });
  if (autoAdopt.attempted && autoAdopt.result && autoAdopt.result.ok) {
    if (autoAdopt.reason === 'refreshed') {
      process.stderr.write(
        '[code-graph] Refreshed decision table to latest shipped version.\n' +
        '            Lock file:  CODE_GRAPH_NO_TEMPLATE_REFRESH=1 in ~/.claude/settings.json env\n'
      );
    } else {
      process.stderr.write(
        '[code-graph] Auto-adopted into project MEMORY.md (plugin install → knowing consent).\n' +
        '            Opt out:    CODE_GRAPH_NO_AUTO_ADOPT=1 in ~/.claude/settings.json env\n' +
        '            Reverse:    code-graph-mcp unadopt\n'
      );
    }
  }

  // quietHooks: adopted → quiet by default (rely on MEMORY.md pointer + on-demand
  // project_map tool); env '1'/'0' overrides for explicit control.
  const adopted = isAdopted();
  const quietHooks = computeQuietHooks({ adopted, env: process.env });

  const mapInjected = binaryCheck.available && !quietHooks ? injectProjectMap() : false;
  const consistencyIssues = binaryCheck.available
    ? consistencyCheck(binaryCheck.binary)
    : [];
  return {
    inactive: false, lifecycle,
    autoUpdateLaunched, indexFreshness, mapInjected, binaryCheck, consistencyIssues,
    quietHooks, adopted, autoAdopted: autoAdopt.attempted,
  };
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
  consistencyCheck,
  runSessionInit,
  computeQuietHooks,
};

if (require.main === module) {
  runSessionInit();
}
