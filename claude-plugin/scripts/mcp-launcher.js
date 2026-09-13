#!/usr/bin/env node
'use strict';
/**
 * MCP server launcher — resolves binary via find-binary.js, auto-installs
 * if missing, then spawns with stdio forwarding for JSON-RPC.
 *
 * Used by .mcp.json so the plugin controls binary discovery instead of
 * relying on the binary being in PATH.
 */
const { spawn } = require('child_process');
const path = require('path');
const fs = require('fs');
const { isNonProjectCwd } = require('./project-detect');
const { serveEmptyMcpStub } = require('./mcp-stub');
const { hidden } = require('./proc-opts');

// Set plugin root so find-binary.js can locate bundled/dev binaries
// Always derive from __dirname — CLAUDE_PLUGIN_ROOT can leak from other plugins
process.env._FIND_BINARY_ROOT = path.resolve(__dirname, '..');

// --- Tool-catalog dedup gate -----------------------------------------------
// If the user's project has its own .mcp.json registering a code-graph server
// (the recommended pattern for dev work on this repo — points at a local
// `target/release/code-graph-mcp` so usage telemetry lands in the project's
// `.code-graph/usage.jsonl`), the plugin's own MCP server adds a SECOND copy
// of the same 7 tools to the catalog, costing context budget and splitting
// the agent's choice between two equivalent namespaces.
//
// Detect that case and serve a minimal "0-tools" MCP stub so this plugin
// stops contributing to the catalog. Hooks, skills, agents stay registered
// (they live outside the MCP server). Env override
// `CODE_GRAPH_FORCE_PLUGIN_MCP=1` bypasses the gate.
function projectHasLocalCodeGraphMcp(cwd) {
  try {
    const p = path.join(cwd, '.mcp.json');
    if (!fs.existsSync(p)) return false;
    const cfg = JSON.parse(fs.readFileSync(p, 'utf8'));
    const servers = (cfg && cfg.mcpServers) || {};
    return Object.keys(servers).some(n => /code[-_]?graph/i.test(n));
  } catch { return false; }
}

// serveEmptyMcpStub lives in ./mcp-stub.js — a permanent 0-tool stub by default,
// or an in-place upgrading stub when passed { upgrade } (see the non-project gate).

if (process.env.CODE_GRAPH_FORCE_PLUGIN_MCP !== '1' && projectHasLocalCodeGraphMcp(process.cwd())) {
  process.stderr.write(
    '[code-graph] project .mcp.json registers a code-graph server; ' +
    'plugin MCP serving 0 tools to avoid duplicate catalog entries. ' +
    'Set CODE_GRAPH_FORCE_PLUGIN_MCP=1 to override.\n'
  );
  serveEmptyMcpStub();
  return; // top-level function scope of mcp-launcher.js
}

// --- Non-project cwd gate ---------------------------------------------------
// In a non-project working directory (no .git/manifest — e.g. /tmp, where
// claude-mem-lite spawns ~2035 headless `claude -p` JSON-extraction calls that
// never use code-graph), don't spawn the binary at all: serve the same 0-tool
// stub. Eliminates the MCP-server spin-up + the ~780B `instructions` block +
// an empty .code-graph/index.db being created in throwaway dirs. Same
// CODE_GRAPH_FORCE_PLUGIN_MCP=1 override as the dedup gate above.
if (process.env.CODE_GRAPH_FORCE_PLUGIN_MCP !== '1' && isNonProjectCwd(process.cwd())) {
  process.stderr.write(
    '[code-graph] non-project cwd (no .git/manifest); plugin MCP serving 0 tools ' +
    '(auto-upgrades to real tools if this dir becomes a project — no restart). ' +
    'Set CODE_GRAPH_FORCE_PLUGIN_MCP=1 to override.\n'
  );
  // Upgradeable stub: the non-project verdict is re-checked on a poll. If the
  // cwd becomes a real project (git init / scaffold) with no local code-graph
  // server, spawn the real binary and hand the live MCP connection over to it —
  // fixes the "stub latched at launch" gap without a Claude Code restart.
  serveEmptyMcpStub({
    upgrade: {
      shouldUpgrade: () =>
        !isNonProjectCwd(process.cwd()) && !projectHasLocalCodeGraphMcp(process.cwd()),
      spawnReal: () => {
        const { findBinary } = require('./find-binary');
        const bin = findBinary();
        if (!bin) return null;
        process.stderr.write(`[code-graph] cwd became a project — upgrading plugin MCP to real tools via ${bin} (restart Claude Code for full tool steering)\n`);
        return spawn(bin, ['serve'], hidden({ stdio: ['pipe', 'pipe', 'inherit'], env: process.env }));
      },
    },
  });
  return;
}

const { findBinary, clearCache, unsupportedPlatformHint } = require('./find-binary');
const { installBinaryInBackground } = require('./launcher-install');

const binary = findBinary();

// Manual-install guidance, printed when the background install chain exhausts
// both steps without producing a binary. Unlike the old sync path this does NOT
// exit: the upgradeable stub stays connected (0 tools), so a manual
// `npm install -g` mid-session still upgrades the live connection.
function printManualInstallHints() {
  const installedViaMarketplace = fs.existsSync(
    path.join(__dirname, '..', '.claude-plugin', 'plugin.json')
  );
  const platformHint = unsupportedPlatformHint();
  if (platformHint) {
    // Unsupported platform (Alpine/musl or native Windows-on-ARM): the per-platform
    // npm package does not exist, so the generic "npm install @sdsrs/code-graph-<plat>-<arch>"
    // suggestion below would point at a nonexistent package. Show the source/emulation hint.
    process.stderr.write('[code-graph] Binary not found.\n' + platformHint + '\n');
    return;
  }
  process.stderr.write('[code-graph] Binary install failed. Install manually:\n');
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
}

// --- Missing binary: answer the handshake NOW, install in the background ----
// The old chain ran `npm install -g` (60s timeout) and the GitHub-release
// fallback (90s) SYNCHRONOUSLY before answering any MCP JSON-RPC. Claude
// Code's connect timeout is 30s, so a cold install always presented as
// "MCP server connection timed out after 30000ms" and the tools only appeared
// on a later reconnect. Serve the upgradeable 0-tool stub first (initialize is
// answered instantly), run the same install chain in the background, and hand
// the live connection to the real binary via the same upgrade mechanism the
// non-project gate uses — no reconnect, no restart.
//
// --install-missing bypasses auto-update.js's isDevMode() short-circuit. The
// marketplace ships the full repo (including Cargo.toml at the workspace root),
// so dev-mode heuristics that look for Cargo.toml were misclassifying every
// marketplace install as dev mode and skipping this fallback (issue #12).
if (!binary) {
  let version = 'latest';
  try {
    const pj = path.join(__dirname, '..', '.claude-plugin', 'plugin.json');
    version = JSON.parse(fs.readFileSync(pj, 'utf8')).version || 'latest';
  } catch { /* use latest */ }

  process.stderr.write(
    `[code-graph] Binary not found — serving 0-tool stub while installing ` +
    `@sdsrs/code-graph@${version} in the background (tools appear when it lands)...\n`
  );

  const stub = serveEmptyMcpStub({
    upgrade: {
      // What a tool call gets told while this gate holds. Without it the stub
      // answered with the OTHER gate's reason ("upgrades when cwd becomes a
      // project"), which is both wrong here and unactionable — the cwd is fine,
      // the binary is what is missing.
      hint: `binary not installed yet — installing @sdsrs/code-graph@${version} in the background; if tools never appear, run \`code-graph-mcp doctor\``,
      // Each probe is a full discovery walk (incl. `npm root -g`, up to 2s);
      // offline the binary never appears, so back the poll off toward 60s.
      // The install chain's onInstalled nudge below still upgrades instantly.
      backoff: true,
      shouldUpgrade: () => !!findBinary(),
      spawnReal: () => {
        const bin = findBinary();
        if (!bin) return null;
        process.stderr.write(`[code-graph] binary ready at ${bin} — upgrading plugin MCP to real tools (restart Claude Code for full tool steering)\n`);
        return spawn(bin, ['serve'], hidden({ stdio: ['pipe', 'pipe', 'inherit'], env: process.env }));
      },
    },
  });

  const { GLOBAL_INSTALL_MARKER, INSTALL_LOCK_FILE } = require('./lifecycle');
  installBinaryInBackground({
    version,
    findBinary,
    clearCache,
    // Nudge the handover immediately instead of waiting for the stub's next poll.
    onInstalled: () => stub.attemptUpgrade(),
    onFailed: () => printManualInstallHints(),
    // Marker: this npm install was OURS, so lifecycle.js uninstall knows it
    // owns removing the global packages (never yanks a user's own install).
    recordGlobalInstall: () => {
      fs.mkdirSync(path.dirname(GLOBAL_INSTALL_MARKER), { recursive: true });
      fs.writeFileSync(GLOBAL_INSTALL_MARKER, JSON.stringify({
        installedBy: 'code-graph-mcp launcher', version, at: new Date().toISOString(),
      }, null, 2) + '\n');
    },
    // Serialize against other cold sessions + auto-update (parallel global npm
    // installs corrupt the shared prefix).
    lockPath: INSTALL_LOCK_FILE,
  });
  return; // top-level function scope of mcp-launcher.js
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

// Both ways a spawn failure can reach us, reported through one function so
// neither path can drift into being the one with the good message.
//
// node hands exactly FIVE errnos to the async 'error' event — EACCES, EAGAIN,
// EMFILE, ENFILE, ENOENT ("Run-time errors should emit an error, not throw an
// exception", internal/child_process.js) — and throws every other one
// synchronously out of `spawn`. So the EACCES branch below always did reach its
// handler; what had no path at all was the synchronous side, where an ETXTBSY
// from a binary an auto-update replaced moments ago printed a raw stack instead
// of any of this. Unlike the stub's, this process exits 1 either way: here the
// catch buys the message, not the process.
function reportSpawnFailure(err) {
  process.stderr.write(`[code-graph] Failed to start: ${err.message}\n`);
  if (process.platform === 'darwin' && (err.code === 'EACCES' || err.code === 'EPERM')) {
    process.stderr.write(
      'macOS may be blocking this binary. Try:\n' +
      `  xattr -d com.apple.quarantine "${binary}"\n`
    );
  }
  // ETXTBSY: something still holds a writer fd on the binary — an auto-update
  // that replaced it moments ago. Retryable, unlike the rest of this function.
  if (err.code === 'ETXTBSY') {
    process.stderr.write(
      'The binary was still being written when we tried to run it.\n' +
      '  Retry: restart Claude Code, or run `code-graph-mcp doctor`\n'
    );
  }
  // A glibc binary installed on musl (older npm ignores the `libc` field) is present
  // but execs into a loader error — surface the actionable platform hint.
  const platformHint = unsupportedPlatformHint();
  if (platformHint) process.stderr.write(platformHint + '\n');
  process.exit(1);
}

// Spawn binary with stdio inheritance for MCP JSON-RPC
let child;
try {
  child = spawn(binary, ['serve'], hidden({
    stdio: 'inherit',
    env: process.env,
  }));
} catch (err) {
  reportSpawnFailure(err);   // exits; the return keeps `child` from being read as undefined
  return;
}

child.on('error', reportSpawnFailure);

child.on('exit', (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
  } else {
    process.exit(code ?? 1);
  }
});
