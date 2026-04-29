#!/usr/bin/env node
'use strict';
/**
 * MCP server launcher — resolves binary via find-binary.js, auto-installs
 * if missing, then spawns with stdio forwarding for JSON-RPC.
 *
 * Used by .mcp.json so the plugin controls binary discovery instead of
 * relying on the binary being in PATH.
 */
const { spawn, spawnSync } = require('child_process');
const path = require('path');
const fs = require('fs');

// Set plugin root so find-binary.js can locate bundled/dev binaries
// Always derive from __dirname — CLAUDE_PLUGIN_ROOT can leak from other plugins
process.env._FIND_BINARY_ROOT = path.resolve(__dirname, '..');

const { findBinary, clearCache } = require('./find-binary');

let binary = findBinary();

// Auto-install binary if not found (first-time install)
if (!binary) {
  let version = 'latest';
  try {
    const pj = path.join(__dirname, '..', '.claude-plugin', 'plugin.json');
    version = JSON.parse(fs.readFileSync(pj, 'utf8')).version || 'latest';
  } catch { /* use latest */ }

  process.stderr.write(`[code-graph] Binary not found, installing @sdsrs/code-graph@${version}...\n`);
  const npmResult = spawnSync('npm', ['install', '-g', `@sdsrs/code-graph@${version}`], {
    timeout: 60000, stdio: ['ignore', 'pipe', 'pipe'], encoding: 'utf8',
  });
  if (npmResult.error || npmResult.status !== 0) {
    process.stderr.write('[code-graph] npm install failed.\n');
    if (npmResult.stderr) {
      process.stderr.write(npmResult.stderr.trim().split('\n').map(l => `[code-graph][npm] ${l}\n`).join(''));
    }
  } else {
    clearCache();
    binary = findBinary();
    if (binary) {
      process.stderr.write(`[code-graph] Installed at ${binary}\n`);
    }
  }
}

// Fallback: npm install may have succeeded but optionalDependencies for the
// platform binary can fail silently (npm tolerates OS-mismatch + flaky
// registry). Pull the platform binary directly from the GitHub release.
//
// --install-missing bypasses auto-update.js's isDevMode() short-circuit. The
// marketplace ships the full repo (including Cargo.toml at the workspace root),
// so dev-mode heuristics that look for Cargo.toml were misclassifying every
// marketplace install as dev mode and skipping this fallback (issue #12).
if (!binary) {
  process.stderr.write('[code-graph] Falling back to GitHub release download...\n');
  const result = spawnSync(
    process.execPath,
    [path.join(__dirname, 'auto-update.js'), '--silent', '--install-missing'],
    { timeout: 90000, stdio: ['ignore', 'pipe', 'pipe'], encoding: 'utf8' }
  );
  if (result.stderr && result.stderr.trim()) {
    process.stderr.write(result.stderr.trim().split('\n').map(l => `[code-graph][auto-update] ${l}\n`).join(''));
  }
  if (result.error) {
    process.stderr.write(`[code-graph] auto-update spawn failed: ${result.error.message}\n`);
  } else if (result.status !== 0) {
    process.stderr.write(`[code-graph] auto-update exited with status ${result.status}\n`);
  }
  clearCache();
  binary = findBinary();
  if (binary) {
    process.stderr.write(`[code-graph] Installed at ${binary}\n`);
  }
}

if (!binary) {
  const installedViaMarketplace = fs.existsSync(
    path.join(__dirname, '..', '.claude-plugin', 'plugin.json')
  );
  process.stderr.write('[code-graph] Binary not found. Install manually:\n');
  if (installedViaMarketplace) {
    process.stderr.write(
      '  # Re-install the plugin via Claude Code marketplace:\n' +
      '  /plugin uninstall code-graph-mcp && /plugin install code-graph-mcp@code-graph-mcp\n' +
      '  # Or install the binary directly via npm:\n'
    );
  }
  process.stderr.write(
    '  npm install -g @sdsrs/code-graph @sdsrs/code-graph-' + process.platform + '-' + process.arch + '\n' +
    '  # or, equivalent split form:\n' +
    '  npm install -g @sdsrs/code-graph\n' +
    '  npm install -g @sdsrs/code-graph-' + process.platform + '-' + process.arch + '\n'
  );
  process.exit(1);
}

// Pre-spawn: verify binary is executable (catches macOS quarantine, permission issues)
try {
  fs.accessSync(binary, fs.constants.X_OK);
} catch {
  process.stderr.write(`[code-graph] Binary not executable: ${binary}\n`);
  if (process.platform === 'darwin') {
    process.stderr.write(
      'macOS may be quarantining the downloaded binary. Fix with:\n' +
      `  xattr -d com.apple.quarantine "${binary}"\n` +
      `  chmod +x "${binary}"\n`
    );
  } else {
    process.stderr.write(`Fix: chmod +x "${binary}"\n`);
  }
  process.exit(1);
}

// Spawn binary with stdio inheritance for MCP JSON-RPC
const child = spawn(binary, ['serve'], {
  stdio: 'inherit',
  env: process.env,
});

child.on('error', (err) => {
  process.stderr.write(`[code-graph] Failed to start: ${err.message}\n`);
  if (process.platform === 'darwin' && (err.code === 'EACCES' || err.code === 'EPERM')) {
    process.stderr.write(
      'macOS may be blocking this binary. Try:\n' +
      `  xattr -d com.apple.quarantine "${binary}"\n`
    );
  }
  process.exit(1);
});

child.on('exit', (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
  } else {
    process.exit(code ?? 1);
  }
});
