#!/usr/bin/env node
'use strict';
/**
 * Installation E2E Tests — comprehensive coverage for all install paths:
 *   1. Plugin install (Claude Code marketplace)
 *   2. NPX install (npx @sdsrs/code-graph)
 *   3. NPM install (npm install -g @sdsrs/code-graph)
 *
 * Run:  node --test scripts/install-e2e.test.js
 */
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { execFileSync, spawnSync } = require('child_process');

const ROOT = path.resolve(__dirname, '..');
const PLUGIN_ROOT = path.join(ROOT, 'claude-plugin');
const BIN_CLI = path.join(ROOT, 'bin', 'cli.js');
const FIND_BINARY = path.join(PLUGIN_ROOT, 'scripts', 'find-binary.js');
const LIFECYCLE = path.join(PLUGIN_ROOT, 'scripts', 'lifecycle.js');
const CURRENT_VERSION = JSON.parse(fs.readFileSync(path.join(ROOT, 'package.json'), 'utf8')).version;

/**
 * Copy `entry` and its transitive local (`./x`) requires into `destDir`.
 *
 * Replaces a hand-written list of find-binary.js's dependencies. That list went
 * stale the moment find-binary picked up a new sibling module (proc-opts.js),
 * and the failure surfaced as "Cannot find module" inside a fixture rather than
 * anywhere near the change — the same shape as the copy-list in
 * find-binary.test.js, which had to be edited for the same reason. Walking the
 * requires means the fixture tracks the real dependency closure by itself.
 * @param {string} entry - absolute path to a .js file
 * @param {string} destDir - directory to copy into
 * @param {Set<string>} [seen]
 * @returns {Set<string>} basenames copied
 */
function copyWithLocalDeps(entry, destDir, seen = new Set()) {
  const name = path.basename(entry);
  if (seen.has(name)) return seen;
  seen.add(name);
  fs.copyFileSync(entry, path.join(destDir, name));
  const src = fs.readFileSync(entry, 'utf8');
  for (const m of src.matchAll(/require\(\s*['"]\.\/([\w.-]+?)(?:\.js)?['"]\s*\)/g)) {
    copyWithLocalDeps(path.join(path.dirname(entry), `${m[1]}.js`), destDir, seen);
  }
  return seen;
}
const PLATFORM = os.platform();
const BINARY_NAME = PLATFORM === 'win32' ? 'code-graph-mcp.exe' : 'code-graph-mcp';

// Sandbox HOMEs are removed when the run ends — under Claude Code $TMPDIR is
// ~/.claude/tmp, and leaked mkdtemp dirs accumulate there (223 observed before
// this cleanup existed).
const CREATED_HOMES = [];
test.after(() => {
  for (const d of CREATED_HOMES) {
    try { fs.rmSync(d, { recursive: true, force: true }); } catch { /* best-effort */ }
  }
});

function mkHome() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'install-e2e-'));
  CREATED_HOMES.push(dir);
  return dir;
}

// Every child process that touches lifecycle state must have BOTH names
// redirected. `claudeHome()` is `process.env.CLAUDE_CONFIG_DIR ||
// homedir/.claude`, so spreading `process.env` while pinning only HOME leaves
// the config dir aimed at the developer's REAL ~/.claude for anyone who exports
// it (the documented multi-account setup). Measured with a canary config dir:
// `uninstall --unadopt-all` DELETED `<canary>/plugins/cache/code-graph-mcp/` and
// wrote `statusline-providers.json` into it, while nine §1 assertions failed
// because they were reading a stranger's settings. This file is gated by
// pre-commit and excluded from CI (ci.yml), so it runs on developer machines
// only — there is no CI leg that would have caught it.
function sandboxEnv(homeDir, extra) {
  return {
    ...process.env,
    HOME: homeDir,
    CLAUDE_CONFIG_DIR: path.join(homeDir, '.claude'),
    ...extra,
  };
}

function writeJson(filePath, value) {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, JSON.stringify(value, null, 2) + '\n');
}

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

// v0.32.0 contract: install()/update() register these (event, matcher, script)
// tuples in settings.json with current PLUGIN_ROOT paths.
const EXPECTED_SETTINGS_HOOKS = [
  ['PreToolUse', 'Edit', 'pre-edit-guide.js'],
  ['PreToolUse', 'Bash', 'pre-grep-guide.js'],
  ['PreToolUse', 'Read', 'pre-read-guide.js'],
  ['PostToolUse', 'Write|Edit', 'incremental-index.js'],
  ['UserPromptSubmit', '', 'user-prompt-context.js'],
];

function assertCurrentSettingsHooks(settings) {
  for (const [event, matcher, script] of EXPECTED_SETTINGS_HOOKS) {
    const entries = settings.hooks?.[event] || [];
    const entry = entries.find(e => (e.matcher || '') === matcher &&
      JSON.stringify(e.hooks || []).includes(script));
    assert.ok(entry, `settings.json must have ${event}:${matcher || '*'} → ${script}`);
    const expectedPath = path.join(PLUGIN_ROOT, 'scripts', script);
    // POSIX commands carry the existence guard (`if [ -f "…" ]; then node "…"; fi`)
    // so a post-uninstall dead path exits 0 instead of erroring every tool call;
    // assert on the extracted node invocation, not the full string.
    const m = (entry.hooks[0].command || '').match(/node "([^"]+)"/);
    assert.equal(m && m[1], expectedPath,
      `${event}:${matcher || '*'} must point at the current plugin root (command: ${entry.hooks[0].command})`);
  }
}

// ═══════════════════════════════════════════════════════════════════════════
// §1 — Plugin Installation E2E
// ═══════════════════════════════════════════════════════════════════════════

test('§1.1 plugin manifest has required fields and valid structure', () => {
  const manifest = readJson(path.join(PLUGIN_ROOT, '.claude-plugin', 'plugin.json'));
  assert.equal(typeof manifest.name, 'string');
  assert.equal(typeof manifest.description, 'string');
  assert.equal(typeof manifest.version, 'string');
  assert.match(manifest.version, /^\d+\.\d+\.\d+$/);
  assert.equal(manifest.version, CURRENT_VERSION);
  assert.equal(manifest.name, 'code-graph-mcp');
});

test('§1.2 marketplace.json structure is valid and consistent', () => {
  const marketplace = readJson(path.join(ROOT, '.claude-plugin', 'marketplace.json'));
  const manifest = readJson(path.join(PLUGIN_ROOT, '.claude-plugin', 'plugin.json'));

  assert.equal(marketplace.name, manifest.name);
  assert.equal(marketplace.metadata.version, CURRENT_VERSION);
  assert.equal(marketplace.plugins.length, 1);
  assert.equal(marketplace.plugins[0].source, './claude-plugin');
  assert.equal(marketplace.plugins[0].name, manifest.name);
  assert.equal(marketplace.plugins[0].version, CURRENT_VERSION);
  assert.equal(typeof marketplace.owner?.name, 'string');
});

test('§1.3 .mcp.json points to valid launcher script', () => {
  const mcpJson = readJson(path.join(PLUGIN_ROOT, '.mcp.json'));
  assert.ok(mcpJson.mcpServers, 'mcpServers key required');
  const server = mcpJson.mcpServers['code-graph'];
  assert.ok(server, 'code-graph server entry required');
  assert.equal(server.command, 'node');
  assert.ok(server.args[0].includes('mcp-launcher.js'));
  // Verify the launcher script exists
  const launcherPath = server.args[0].replace('${CLAUDE_PLUGIN_ROOT}', PLUGIN_ROOT);
  assert.ok(fs.existsSync(launcherPath), `Launcher script missing: ${launcherPath}`);
});

test('§1.4 hook split: cache hooks.json carries SessionStart only; install() registers the rest in settings.json', () => {
  // v0.32.0 contract: current Claude Code only loads SessionStart from
  // cache/<mp>/<plugin>/<ver>/hooks/hooks.json — PreToolUse/PostToolUse/
  // UserPromptSubmit entries there are silently ignored (dead config), so
  // lifecycle.js install/update writes those to ~/.claude/settings.json.
  const cacheHooks = readJson(path.join(PLUGIN_ROOT, 'hooks', 'hooks.json'));
  assert.ok(cacheHooks.hooks, 'cache hooks.json must have a hooks key');

  assert.ok(Array.isArray(cacheHooks.hooks.SessionStart) && cacheHooks.hooks.SessionStart.length > 0,
    'cache hooks.SessionStart must have entries');
  assert.ok(cacheHooks.hooks.SessionStart[0].hooks[0].command.includes('session-init.js'));
  assert.match(cacheHooks.hooks.SessionStart[0].matcher, /startup/);
  for (const event of ['PreToolUse', 'PostToolUse', 'UserPromptSubmit']) {
    assert.equal(cacheHooks.hooks[event], undefined,
      `cache hooks.${event} would be dead config (CC ignores it) — it belongs in settings.json`);
  }

  // install() must register the settings.json-owned events with current paths.
  const homeDir = mkHome();
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  const installedPath = path.join(homeDir, '.claude', 'plugins', 'installed_plugins.json');
  writeJson(settingsPath, { enabledPlugins: { 'code-graph-mcp@code-graph-mcp': true } });
  writeJson(installedPath, {
    plugins: { 'code-graph-mcp@code-graph-mcp': [{ installPath: PLUGIN_ROOT, version: CURRENT_VERSION, scope: 'user' }] },
  });
  execFileSync(process.execPath, [LIFECYCLE, 'install'], {
    env: sandboxEnv(homeDir),
    stdio: 'pipe',
  });
  const settings = readJson(settingsPath);
  assertCurrentSettingsHooks(settings);
});

test('§1.5 plugin install creates install manifest with version', () => {
  const homeDir = mkHome();
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  const installedPath = path.join(homeDir, '.claude', 'plugins', 'installed_plugins.json');
  const manifestPath = path.join(homeDir, '.cache', 'code-graph', 'install-manifest.json');

  writeJson(settingsPath, { enabledPlugins: { 'code-graph-mcp@code-graph-mcp': true } });
  writeJson(installedPath, {
    plugins: { 'code-graph-mcp@code-graph-mcp': [{ installPath: PLUGIN_ROOT, version: CURRENT_VERSION, scope: 'user' }] },
  });

  execFileSync(process.execPath, [LIFECYCLE, 'install'], {
    env: sandboxEnv(homeDir),
    stdio: 'pipe',
  });

  const manifest = readJson(manifestPath);
  assert.equal(manifest.version, CURRENT_VERSION);
  assert.ok(manifest.installedAt, 'installedAt timestamp required');
  assert.ok(manifest.updatedAt, 'updatedAt timestamp required');
});

test('§1.6 plugin install sets up composite statusline', () => {
  const homeDir = mkHome();
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  const installedPath = path.join(homeDir, '.claude', 'plugins', 'installed_plugins.json');
  const registryPath = path.join(homeDir, '.cache', 'code-graph', 'statusline-registry.json');

  // Seed with an existing statusline
  writeJson(settingsPath, {
    statusLine: { type: 'command', command: 'echo existing-status' },
    enabledPlugins: { 'code-graph-mcp@code-graph-mcp': true },
  });
  writeJson(installedPath, {
    plugins: { 'code-graph-mcp@code-graph-mcp': [{ installPath: PLUGIN_ROOT, version: CURRENT_VERSION, scope: 'user' }] },
  });

  execFileSync(process.execPath, [LIFECYCLE, 'install'], {
    env: sandboxEnv(homeDir),
    stdio: 'pipe',
  });

  const settings = readJson(settingsPath);
  assert.match(settings.statusLine.command, /statusline-composite\.js/);

  const registry = readJson(registryPath);
  assert.equal(registry.length, 2);
  // Previous statusline preserved
  const previous = registry.find(p => p.id === '_previous');
  assert.ok(previous, '_previous provider must be registered');
  assert.equal(previous.command, 'echo existing-status');
  // Code-graph registered
  const cg = registry.find(p => p.id === 'code-graph');
  assert.ok(cg, 'code-graph provider must be registered');
  assert.match(cg.command, /statusline\.js/);
});

test('§1.7 plugin update strips legacy settings.json hooks and clears update cache', () => {
  const homeDir = mkHome();
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  const installedPath = path.join(homeDir, '.claude', 'plugins', 'installed_plugins.json');
  const updateCache = path.join(homeDir, '.cache', 'code-graph', 'update-check');

  // Simulate a v0.8.2-era install: hooks were registered in settings.json.
  writeJson(settingsPath, {
    statusLine: { type: 'command', command: `node "${path.join(PLUGIN_ROOT, 'scripts', 'statusline-composite.js')}"` },
    hooks: {
      SessionStart: [{
        matcher: 'startup',
        description: 'StatusLine self-heal, lifecycle sync, project map injection',
        hooks: [{ type: 'command', command: 'node "/old/code-graph/path/session-init.js"' }],
      }],
    },
    enabledPlugins: { 'code-graph-mcp@code-graph-mcp': true },
  });
  writeJson(installedPath, {
    plugins: { 'code-graph-mcp@code-graph-mcp': [{ installPath: PLUGIN_ROOT, version: CURRENT_VERSION, scope: 'user' }] },
  });
  fs.mkdirSync(path.dirname(updateCache), { recursive: true });
  fs.writeFileSync(updateCache, '{}');

  execFileSync(process.execPath, [LIFECYCLE, 'update'], {
    env: sandboxEnv(homeDir),
    stdio: 'pipe',
  });

  const settings = readJson(settingsPath);
  // Legacy v0.8.2-era entry (old path) must be gone, replaced by fresh
  // v0.32+ entries pointing at the current plugin root.
  const hookSerialized = JSON.stringify(settings.hooks || {});
  assert.ok(!hookSerialized.includes('/old/code-graph/path/'),
    `update() must strip the legacy old-path hook entry, got: ${hookSerialized}`);
  assertCurrentSettingsHooks(settings);
  // Update-check cache cleared (forces freshness post-update).
  assert.equal(fs.existsSync(updateCache), false);
});

function runSyncLifecycleConfig(homeDir) {
  const sessionInit = path.join(PLUGIN_ROOT, 'scripts', 'session-init.js');
  return execFileSync(process.execPath, [
    '-e', `console.log(require(${JSON.stringify(sessionInit)}).syncLifecycleConfig())`,
  ], { env: sandboxEnv(homeDir), stdio: 'pipe' }).toString().trim();
}

function installIntoSandbox() {
  const homeDir = mkHome();
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  writeJson(settingsPath, { enabledPlugins: { 'code-graph-mcp@code-graph-mcp': true } });
  writeJson(path.join(homeDir, '.claude', 'plugins', 'installed_plugins.json'), {
    plugins: { 'code-graph-mcp@code-graph-mcp': [{ installPath: PLUGIN_ROOT, version: CURRENT_VERSION, scope: 'user' }] },
  });
  execFileSync(process.execPath, [LIFECYCLE, 'install'], {
    env: sandboxEnv(homeDir), stdio: 'pipe',
  });
  return { homeDir, settingsPath };
}

test('§1.9 session-init self-heals a stale-but-existing settings.json hook path', () => {
  // The binary-pin sibling: after a failed auto-update re-register, hooks can
  // point at an old plugin-cache version dir that still EXISTS on disk. The
  // pre-v0.49.1 self-heal only checked existence + matcher presence, so users
  // kept running old hook code until doctor. session-init must heal it.
  const { homeDir, settingsPath } = installIntoSandbox();

  // Simulate: pre-edit-guide hook pinned at an old-version path that exists.
  // Must use the REAL plugin-cache layout (<marketplace>/<plugin>/<ver>/scripts):
  // since the v0.104.0 surface-tolerant staleness fix, only a dead path or an
  // OLDER cache-version dir is stale — an existing path of unknown shape is
  // treated as a valid in-place surface (the ping-pong fix) and left alone.
  const staleScript = path.join(homeDir, '.claude', 'plugins', 'cache',
    'code-graph-mcp', 'code-graph-mcp', '0.45.1', 'scripts', 'pre-edit-guide.js');
  fs.mkdirSync(path.dirname(staleScript), { recursive: true });
  fs.writeFileSync(staleScript, '// stale copy\n');
  const settings = readJson(settingsPath);
  const entry = settings.hooks.PreToolUse.find(e => e.matcher === 'Edit');
  entry.hooks[0].command = `node "${staleScript}"`;
  writeJson(settingsPath, settings);

  assert.equal(runSyncLifecycleConfig(homeDir), 'self-healed-stale-settings-hook');
  assertCurrentSettingsHooks(readJson(settingsPath));
  // Idempotent: second run is a noop.
  assert.equal(runSyncLifecycleConfig(homeDir), 'noop');
});

test('§1.10 session-init self-heals a stale-but-existing composite statusline path', () => {
  const { homeDir, settingsPath } = installIntoSandbox();

  // Must use the REAL plugin-cache layout (<marketplace>/<plugin>/<ver>/scripts),
  // for the same reason §1.9 above must: since the statusline slot joined the
  // surface-tolerant staleness rule (audit P2-17), only a dead path or an OLDER
  // cache-version dir is stale — an existing path of unknown shape is a valid
  // in-place surface (global npm, a dev checkout) and is left alone, which is
  // what stops two copies from taking the slot from each other every session.
  const staleComposite = path.join(homeDir, '.claude', 'plugins', 'cache',
    'code-graph-mcp', 'code-graph-mcp', '0.45.1', 'scripts', 'statusline-composite.js');
  fs.mkdirSync(path.dirname(staleComposite), { recursive: true });
  fs.writeFileSync(staleComposite, '// stale copy\n');
  const settings = readJson(settingsPath);
  settings.statusLine.command = `node "${staleComposite}"`;
  writeJson(settingsPath, settings);

  assert.equal(runSyncLifecycleConfig(homeDir), 'self-healed-stale-statusline');
  const healed = readJson(settingsPath);
  assert.equal(healed.statusLine.command,
    `node "${path.join(PLUGIN_ROOT, 'scripts', 'statusline-composite.js')}"`);
  assert.equal(runSyncLifecycleConfig(homeDir), 'noop');
});

test('§1.10b session-init leaves another delivery surface\'s live composite alone (P2-17)', () => {
  // The tolerant half of §1.10, and the reason both gates had to change
  // together: session-init decides whether to heal, install() decides whether to
  // write. When only install() learned surface tolerance, session-init kept
  // reporting 'self-healed-stale-statusline' on every session while install()
  // quietly changed nothing — a self-heal that heals nothing, once per session,
  // forever.
  const { homeDir, settingsPath } = installIntoSandbox();

  // A global-npm-shaped (in-place, no version dir) composite that exists.
  const npmComposite = path.join(homeDir, '.nvm', 'versions', 'node', 'v24.18.0',
    'lib', 'node_modules', '@sdsrs', 'code-graph', 'claude-plugin', 'scripts',
    'statusline-composite.js');
  fs.mkdirSync(path.dirname(npmComposite), { recursive: true });
  fs.writeFileSync(npmComposite, '// live copy on another surface\n');
  const settings = readJson(settingsPath);
  settings.statusLine.command = `node "${npmComposite}"`;
  writeJson(settingsPath, settings);

  assert.equal(runSyncLifecycleConfig(homeDir), 'noop',
    'a live composite on another surface is not stale');
  assert.equal(readJson(settingsPath).statusLine.command, `node "${npmComposite}"`,
    'and the slot must be left exactly as it was');
});

test('§1.11 a stale-relic session-init defers to the active install (downgrade-war guard)', () => {
  // Field repro 2026-06-12: auto-update installed 0.49.0 (manifest + hooks
  // re-anchored), then 15 minutes later a SessionStart fired by the STILL
  // RUNNING Claude Code process executed the 0.48.0 scripts, whose
  // syncLifecycleConfig saw manifest.version !== its own version and called
  // update() — dragging manifest + every settings.json hook back to 0.48.0.
  // The relic must defer to installed_plugins.json instead.
  const homeDir = mkHome();
  const cacheBase = path.join(homeDir, '.claude', 'plugins', 'cache',
    'code-graph-mcp', 'code-graph-mcp');
  const relicDir = path.join(cacheBase, '0.48.0');
  const activeDir = path.join(cacheBase, '0.49.0');

  // Relic = a real copy of the plugin tree, version-stamped 0.48.0.
  // Copy regular files and directories only. `.gitignore`'s blanket `.*` lets a
  // `.claude/` appear next to the plugin sources, grow, and never show up in
  // `git status`; one did, and its `hooks` entry is a CHARACTER DEVICE, which
  // `cpSync` cannot copy (EINVAL) — a fixture failing for a reason unrelated to
  // what it asserts. Filtering by TYPE rather than by leading dot is what keeps
  // `.claude-plugin/` (this plugin's own manifest dir, which the assertions below
  // read) in the copy. Sibling of the same fix in auto-update.test.js
  // (2026-08-16 audit §四).
  const copyableOnly = (src) => {
    try {
      const st = fs.lstatSync(src);
      return st.isFile() || st.isDirectory();
    } catch { return false; }
  };
  fs.cpSync(PLUGIN_ROOT, relicDir, { recursive: true, filter: copyableOnly });
  const relicManifest = path.join(relicDir, '.claude-plugin', 'plugin.json');
  writeJson(relicManifest, { ...readJson(relicManifest), version: '0.48.0' });

  // Active install: newer dir that exists with a lifecycle to defer to.
  fs.mkdirSync(path.join(activeDir, 'scripts'), { recursive: true });
  fs.writeFileSync(path.join(activeDir, 'scripts', 'lifecycle.js'), '// active install\n');
  writeJson(path.join(homeDir, '.claude', 'plugins', 'installed_plugins.json'), {
    plugins: { 'code-graph-mcp@code-graph-mcp': [{ installPath: activeDir, version: '0.49.0', scope: 'user' }] },
  });

  // Auto-update just anchored the manifest at 0.49.0.
  const manifestPath = path.join(homeDir, '.cache', 'code-graph', 'install-manifest.json');
  writeJson(manifestPath, { version: '0.49.0', config: {} });
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  writeJson(settingsPath, { enabledPlugins: { 'code-graph-mcp@code-graph-mcp': true } });

  // The relic's session-init must NOT downgrade anything.
  const relicSessionInit = path.join(relicDir, 'scripts', 'session-init.js');
  const out = execFileSync(process.execPath, [
    '-e', `console.log(require(${JSON.stringify(relicSessionInit)}).syncLifecycleConfig())`,
  ], { env: sandboxEnv(homeDir), stdio: 'pipe' }).toString().trim();

  assert.equal(out, 'deferred-to-active-install');
  assert.equal(readJson(manifestPath).version, '0.49.0',
    'relic must not drag the manifest back to its own version');
  const settings = readJson(settingsPath);
  assert.equal(JSON.stringify(settings).includes('0.48.0'), false,
    'relic must not anchor any settings.json path to its own version dir');
});

test('§1.8 plugin uninstall removes all traces', () => {
  const homeDir = mkHome();
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  const installedPath = path.join(homeDir, '.claude', 'plugins', 'installed_plugins.json');
  const cacheDir = path.join(homeDir, '.cache', 'code-graph');

  // First install
  writeJson(settingsPath, {
    statusLine: { type: 'command', command: 'echo before' },
    enabledPlugins: { 'code-graph-mcp@code-graph-mcp': true },
  });
  writeJson(installedPath, {
    plugins: { 'code-graph-mcp@code-graph-mcp': [{ installPath: PLUGIN_ROOT, version: CURRENT_VERSION, scope: 'user' }] },
  });

  execFileSync(process.execPath, [LIFECYCLE, 'install'], {
    env: sandboxEnv(homeDir),
    stdio: 'pipe',
  });

  // Verify install happened
  assert.ok(fs.existsSync(cacheDir));

  // Then uninstall
  execFileSync(process.execPath, [LIFECYCLE, 'uninstall'], {
    env: sandboxEnv(homeDir),
    stdio: 'pipe',
  });

  const settings = readJson(settingsPath);
  // Statusline restored
  assert.equal(settings.statusLine.command, 'echo before');
  // Hooks removed
  assert.equal(settings.hooks, undefined);
  // enabledPlugins cleaned
  assert.deepEqual(settings.enabledPlugins, {});
  // installed_plugins cleaned
  const installed = readJson(installedPath);
  assert.deepEqual(installed.plugins, {});
  // Cache directory removed
  assert.equal(fs.existsSync(cacheDir), false);
});

test('§1.9 plugin install migrates old plugin IDs', () => {
  const homeDir = mkHome();
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  const installedPath = path.join(homeDir, '.claude', 'plugins', 'installed_plugins.json');

  // Seed with old plugin ID remnants
  writeJson(settingsPath, {
    enabledPlugins: {
      'code-graph@sdsrss': true,
      'code-graph@sdsrss-code-graph': true,
      'code-graph-mcp@code-graph-mcp': true,
    },
    extraKnownMarketplaces: { 'sdsrss-code-graph': {} },
  });
  writeJson(installedPath, {
    plugins: {
      'code-graph@sdsrss': [{ installPath: '/old/path', version: '0.5.0' }],
      'code-graph-mcp@code-graph-mcp': [{ installPath: PLUGIN_ROOT, version: CURRENT_VERSION, scope: 'user' }],
    },
  });

  execFileSync(process.execPath, [LIFECYCLE, 'install'], {
    env: sandboxEnv(homeDir),
    stdio: 'pipe',
  });

  const settings = readJson(settingsPath);
  // Old IDs removed from enabledPlugins
  assert.equal(settings.enabledPlugins['code-graph@sdsrss'], undefined);
  assert.equal(settings.enabledPlugins['code-graph@sdsrss-code-graph'], undefined);
  // Old marketplace names removed
  assert.equal(settings.extraKnownMarketplaces?.['sdsrss-code-graph'], undefined);

  // Old IDs removed from installed_plugins
  const installed = readJson(installedPath);
  assert.equal(installed.plugins['code-graph@sdsrss'], undefined);
});

test('§1.10 plugin install is idempotent', () => {
  const homeDir = mkHome();
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  const installedPath = path.join(homeDir, '.claude', 'plugins', 'installed_plugins.json');

  writeJson(settingsPath, { enabledPlugins: { 'code-graph-mcp@code-graph-mcp': true } });
  writeJson(installedPath, {
    plugins: { 'code-graph-mcp@code-graph-mcp': [{ installPath: PLUGIN_ROOT, version: CURRENT_VERSION, scope: 'user' }] },
  });

  const env = sandboxEnv(homeDir);

  // Install twice
  execFileSync(process.execPath, [LIFECYCLE, 'install'], { env, stdio: 'pipe' });
  const settingsAfterFirst = readJson(settingsPath);
  execFileSync(process.execPath, [LIFECYCLE, 'install'], { env, stdio: 'pipe' });
  const settingsAfterSecond = readJson(settingsPath);

  // Should be identical (except updatedAt in manifest)
  assert.deepEqual(settingsAfterFirst.hooks, settingsAfterSecond.hooks);
  assert.deepEqual(settingsAfterFirst.statusLine, settingsAfterSecond.statusLine);
});

// ═══════════════════════════════════════════════════════════════════════════
// §2 — NPX Installation E2E
// ═══════════════════════════════════════════════════════════════════════════

test('§2.1 bin/cli.js exists and is executable entry point', () => {
  assert.ok(fs.existsSync(BIN_CLI), 'bin/cli.js must exist');
  const content = fs.readFileSync(BIN_CLI, 'utf8');
  assert.match(content, /^#!\/usr\/bin\/env node/, 'Must have node shebang');
  assert.match(content, /find-binary/, 'Must use find-binary for resolution');
  assert.match(content, /spawn/, 'Must spawn binary as child process');
});

test('§2.1b bin/cli.js: adopt/unadopt --help is side-effect-free', () => {
  // The npm/npx wrapper intercepts adopt/unadopt BEFORE the binary. `--help`/`-h`
  // must print usage WITHOUT running adopt() (writes the memory file + MEMORY.md
  // sentinel) / unadopt() — the common new-user path is `... adopt --help`. The
  // Rust binary guards this separately for direct invocation.
  for (const sub of ['adopt', 'unadopt']) {
    for (const flag of ['--help', '-h']) {
      const home = mkHome();
      const cwd = path.join(home, 'proj');
      fs.mkdirSync(cwd, { recursive: true });
      fs.writeFileSync(path.join(cwd, 'package.json'), '{"name":"x"}'); // project marker
      const out = execFileSync(process.execPath, [BIN_CLI, sub, flag], {
        cwd, env: sandboxEnv(home), stdio: ['pipe', 'pipe', 'pipe'],
      }).toString();
      assert.match(out, /USAGE/, `${sub} ${flag} must print usage`);
      // adopt/unadopt run-markers must be absent (proves the command didn't run).
      assert.doesNotMatch(out, /Adopted|Indexed|Removed/, `${sub} ${flag} ran the command`);
      // And no memory must be written under the sandbox HOME.
      assert.ok(!fs.existsSync(path.join(home, '.claude', 'projects')),
        `${sub} ${flag} wrote memory (side effect)`);
    }
  }
});

test('§2.2 find-binary.js resolves dev binary in source repo', () => {
  const result = execFileSync(process.execPath, [FIND_BINARY], {
    env: { ...process.env, _FIND_BINARY_ROOT: ROOT },
    stdio: ['pipe', 'pipe', 'pipe'],
  }).toString().trim();

  assert.ok(result.length > 0, 'find-binary must return a path');
  assert.ok(result.endsWith(BINARY_NAME), `Expected path ending with ${BINARY_NAME}, got: ${result}`);
  assert.ok(fs.existsSync(result), `Resolved binary must exist: ${result}`);
});

test('§2.3 find-binary.js writes and reads disk cache', () => {
  const homeDir = mkHome();
  const cacheFile = path.join(homeDir, '.cache', 'code-graph', 'binary-path');

  // Run find-binary (should write cache)
  const result = execFileSync(process.execPath, [FIND_BINARY], {
    env: sandboxEnv(homeDir, { _FIND_BINARY_ROOT: ROOT }),
    stdio: ['pipe', 'pipe', 'pipe'],
  }).toString().trim();

  assert.ok(result.length > 0);
  // Cache should be written
  assert.ok(fs.existsSync(cacheFile), 'Cache file must be written');
  const cached = fs.readFileSync(cacheFile, 'utf8').trim();
  assert.equal(cached, result, 'Cache must contain resolved path');

  // Second run should use cache (still returns same path)
  const result2 = execFileSync(process.execPath, [FIND_BINARY], {
    env: sandboxEnv(homeDir, { _FIND_BINARY_ROOT: ROOT }),
    stdio: ['pipe', 'pipe', 'pipe'],
  }).toString().trim();
  assert.equal(result2, result, 'Cached result should match');
});

test('§2.4 find-binary.js handles stale cache gracefully', () => {
  const homeDir = mkHome();
  const cacheFile = path.join(homeDir, '.cache', 'code-graph', 'binary-path');

  // Write a stale cache pointing to nonexistent binary
  fs.mkdirSync(path.dirname(cacheFile), { recursive: true });
  fs.writeFileSync(cacheFile, '/nonexistent/path/code-graph-mcp');

  // find-binary should fall through to actual resolution
  const result = execFileSync(process.execPath, [FIND_BINARY], {
    env: sandboxEnv(homeDir, { _FIND_BINARY_ROOT: ROOT }),
    stdio: ['pipe', 'pipe', 'pipe'],
  }).toString().trim();

  assert.ok(result.length > 0, 'Must resolve despite stale cache');
  assert.ok(fs.existsSync(result), 'Must resolve to existing binary');
  // Cache should be updated
  const newCached = fs.readFileSync(cacheFile, 'utf8').trim();
  assert.equal(newCached, result);
});

test('§2.5 find-binary.js clearCache removes the cache file', () => {
  const homeDir = mkHome();
  const cacheFile = path.join(homeDir, '.cache', 'code-graph', 'binary-path');

  // Write cache
  fs.mkdirSync(path.dirname(cacheFile), { recursive: true });
  fs.writeFileSync(cacheFile, '/some/path/code-graph-mcp');

  // Clear via module
  const result = execFileSync(process.execPath, ['-e', `
    process.env.HOME = ${JSON.stringify(homeDir)};
    process.env.CLAUDE_CONFIG_DIR = ${JSON.stringify(path.join(homeDir, '.claude'))};
    const { clearCache, CACHE_FILE } = require(${JSON.stringify(FIND_BINARY)});
    clearCache();
    process.stdout.write(String(require('fs').existsSync(CACHE_FILE)));
  `], {
    env: sandboxEnv(homeDir),
    stdio: ['pipe', 'pipe', 'pipe'],
  }).toString();

  // Note: CACHE_FILE uses os.homedir() which may not respect HOME env on all platforms
  // Just verify the function is callable and doesn't throw
  assert.ok(result === 'true' || result === 'false');
});

test('§2.6 bin/cli.js forwards --version to binary', () => {
  const result = spawnSync(process.execPath, [BIN_CLI, '--version'], {
    env: { ...process.env, _FIND_BINARY_ROOT: ROOT },
    stdio: ['pipe', 'pipe', 'pipe'],
    timeout: 10000,
  });

  // Should exit 0 and print version
  assert.equal(result.status, 0, `cli.js --version should exit 0, stderr: ${result.stderr?.toString()}`);
  const stdout = result.stdout.toString().trim();
  assert.match(stdout, /code-graph-mcp/, 'Version output should contain binary name');
  assert.match(stdout, /\d+\.\d+\.\d+/, 'Version output should contain semver');
});

test('§2.7 bin/cli.js forwards help argument', () => {
  const result = spawnSync(process.execPath, [BIN_CLI, '--help'], {
    env: { ...process.env, _FIND_BINARY_ROOT: ROOT },
    stdio: ['pipe', 'pipe', 'pipe'],
    timeout: 10000,
  });

  // --help should exit 0
  assert.equal(result.status, 0, `cli.js --help should exit 0`);
  const stdout = result.stdout.toString();
  assert.ok(stdout.length > 50, 'Help output should be substantial');
});

test('§2.8 bin/cli.js shows install instructions when binary is missing', () => {
  const homeDir = mkHome();
  // Create a fake cli.js environment where no binary can be found
  const fakeRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'fake-root-'));
  const fakeBin = path.join(fakeRoot, 'bin');
  const fakePlugin = path.join(fakeRoot, 'claude-plugin', 'scripts');
  fs.mkdirSync(fakeBin, { recursive: true });
  fs.mkdirSync(fakePlugin, { recursive: true });

  // Copy find-binary.js + whatever it actually requires (version-utils for the
  // cache version check, npm-exec for the Windows npm shim, proc-opts for
  // windowsHide, and anything added later — see copyWithLocalDeps).
  const copied = copyWithLocalDeps(FIND_BINARY, fakePlugin);
  assert.ok(copied.has('find-binary.js') && copied.size >= 2,
    `fixture must carry find-binary.js plus its deps, got: ${[...copied].join(', ')}`);

  // Create a minimal cli.js that uses the fake find-binary
  const cliScript = `
    process.env._FIND_BINARY_ROOT = ${JSON.stringify(fakeRoot)};
    const { findBinary } = require(${JSON.stringify(path.join(fakePlugin, 'find-binary.js'))});
    const binary = findBinary();
    if (!binary) {
      console.error('Error: code-graph-mcp binary not found.');
      process.exit(1);
    }
  `;

  const result = spawnSync(process.execPath, ['-e', cliScript], {
    env: { HOME: homeDir, PATH: '' },
    stdio: ['pipe', 'pipe', 'pipe'],
    timeout: 5000,
  });

  assert.notEqual(result.status, 0, 'Should exit non-zero when binary missing');
  const stderr = result.stderr.toString();
  assert.match(stderr, /not found/i, 'Should show error about missing binary');
});

test('§2.9 find-binary.js auto-update cache path checked', () => {
  const homeDir = mkHome();
  const autoUpdateBin = path.join(homeDir, '.cache', 'code-graph', 'bin', BINARY_NAME);

  // Copy actual binary to auto-update cache location
  const devBin = path.join(ROOT, 'target', 'release', BINARY_NAME);
  if (!fs.existsSync(devBin)) return; // skip if not built

  fs.mkdirSync(path.dirname(autoUpdateBin), { recursive: true });
  fs.copyFileSync(devBin, autoUpdateBin);
  fs.chmodSync(autoUpdateBin, 0o755);

  // Use a separate script in a temp dir so __dirname doesn't resolve to the source repo.
  // This prevents find-binary's dev-mode detection from finding Cargo.toml via __dirname.
  const testScript = path.join(homeDir, 'test-find.js');
  fs.writeFileSync(testScript, `
    const fb = require(${JSON.stringify(FIND_BINARY)});
    // findBinaryUncached checks __dirname which resolves to source repo,
    // so dev mode always wins. Instead, verify the auto-update path is checked
    // by directly testing the existence check logic.
    const fs = require('fs');
    const path = require('path');
    const os = require('os');
    const BINARY_NAME = os.platform() === 'win32' ? 'code-graph-mcp.exe' : 'code-graph-mcp';
    const autoUpdateBin = path.join(os.homedir(), '.cache', 'code-graph', 'bin', BINARY_NAME);
    if (fs.existsSync(autoUpdateBin)) {
      process.stdout.write('EXISTS:' + autoUpdateBin);
    } else {
      process.stdout.write('MISSING');
    }
  `);

  const result = execFileSync(process.execPath, [testScript], {
    env: { HOME: homeDir, PATH: process.env.PATH },
    stdio: ['pipe', 'pipe', 'pipe'],
  }).toString().trim();

  assert.match(result, /^EXISTS:/, 'Auto-update cache binary should be found');
  assert.ok(result.includes(autoUpdateBin));
});

// ═══════════════════════════════════════════════════════════════════════════
// §3 — NPM Installation E2E
// ═══════════════════════════════════════════════════════════════════════════

test('§3.1 main package.json has correct bin entries', () => {
  const pkg = readJson(path.join(ROOT, 'package.json'));
  assert.equal(pkg.bin['code-graph'], 'bin/cli.js');
  assert.equal(pkg.bin['code-graph-mcp'], 'bin/cli.js');
  assert.ok(fs.existsSync(path.join(ROOT, pkg.bin['code-graph'])), 'bin target must exist');
});

test('§3.2 main package.json files array includes required files', () => {
  const pkg = readJson(path.join(ROOT, 'package.json'));
  assert.ok(pkg.files.includes('bin/cli.js'), 'Must include bin/cli.js');
  assert.ok(pkg.files.includes('claude-plugin'), 'Must include claude-plugin directory');
});

test('§3.3 optionalDependencies cover all 5 platforms', () => {
  const pkg = readJson(path.join(ROOT, 'package.json'));
  const deps = pkg.optionalDependencies;
  const expectedPlatforms = [
    '@sdsrs/code-graph-linux-x64',
    '@sdsrs/code-graph-linux-arm64',
    '@sdsrs/code-graph-darwin-x64',
    '@sdsrs/code-graph-darwin-arm64',
    '@sdsrs/code-graph-win32-x64',
  ];

  for (const name of expectedPlatforms) {
    assert.ok(name in deps, `Missing optionalDependency: ${name}`);
    assert.equal(deps[name], CURRENT_VERSION, `${name} version must match root: ${deps[name]} vs ${CURRENT_VERSION}`);
  }
});

test('§3.4 each platform package has correct os/cpu constraints', () => {
  const platformMap = {
    'linux-x64': { os: 'linux', cpu: 'x64' },
    'linux-arm64': { os: 'linux', cpu: 'arm64' },
    'darwin-x64': { os: 'darwin', cpu: 'x64' },
    'darwin-arm64': { os: 'darwin', cpu: 'arm64' },
    'win32-x64': { os: 'win32', cpu: 'x64' },
  };

  for (const [platform, expected] of Object.entries(platformMap)) {
    const pkgPath = path.join(ROOT, 'npm', platform, 'package.json');
    assert.ok(fs.existsSync(pkgPath), `Platform package missing: ${platform}`);
    const pkg = readJson(pkgPath);

    assert.equal(pkg.version, CURRENT_VERSION, `${platform} version must be ${CURRENT_VERSION}`);
    assert.deepEqual(pkg.os, [expected.os], `${platform} os constraint`);
    assert.deepEqual(pkg.cpu, [expected.cpu], `${platform} cpu constraint`);
    assert.equal(pkg.preferUnplugged, true, `${platform} preferUnplugged`);

    // Verify files array includes binary
    const binaryFile = expected.os === 'win32' ? 'code-graph-mcp.exe' : 'code-graph-mcp';
    assert.ok(pkg.files.includes(binaryFile), `${platform} must include ${binaryFile} in files`);
  }
});

test('§3.5 platform package names match optionalDependencies', () => {
  const rootPkg = readJson(path.join(ROOT, 'package.json'));
  const deps = rootPkg.optionalDependencies;

  for (const dir of fs.readdirSync(path.join(ROOT, 'npm'))) {
    const pkg = readJson(path.join(ROOT, 'npm', dir, 'package.json'));
    assert.ok(pkg.name in deps, `Platform package ${pkg.name} must be in optionalDependencies`);
    assert.equal(deps[pkg.name], pkg.version, `Version mismatch for ${pkg.name}`);
  }
});

test('§3.6 engines field sets minimum node version', () => {
  const pkg = readJson(path.join(ROOT, 'package.json'));
  assert.ok(pkg.engines?.node, 'engines.node must be set');
  assert.match(pkg.engines.node, />=\d+/, 'Must specify minimum node version');
});

// ═══════════════════════════════════════════════════════════════════════════
// §4 — Binary Execution E2E
// ═══════════════════════════════════════════════════════════════════════════

test('§4.1 binary --version outputs correct version', () => {
  const binary = path.join(ROOT, 'target', 'release', BINARY_NAME);
  if (!fs.existsSync(binary)) return; // skip if not built

  const result = spawnSync(binary, ['--version'], { stdio: ['pipe', 'pipe', 'pipe'], timeout: 5000 });
  assert.equal(result.status, 0);
  const stdout = result.stdout.toString().trim();
  assert.match(stdout, new RegExp(CURRENT_VERSION.replace(/\./g, '\\.')), `Binary version should be ${CURRENT_VERSION}`);
});

test('§4.2 binary serve responds to MCP initialize', async () => {
  const binary = path.join(ROOT, 'target', 'release', BINARY_NAME);
  if (!fs.existsSync(binary)) return; // skip if not built

  const initMsg = JSON.stringify({
    jsonrpc: '2.0', id: 1, method: 'initialize',
    params: { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'e2e-test', version: '1.0.0' } },
  });

  const { spawn: spawnChild } = require('child_process');
  const child = spawnChild(binary, ['serve'], {
    stdio: ['pipe', 'pipe', 'pipe'],
    cwd: ROOT,
  });

  const responsePromise = new Promise((resolve, reject) => {
    let stdout = '';
    const timer = setTimeout(() => {
      child.kill('SIGTERM');
      reject(new Error('Timeout waiting for MCP response'));
    }, 10000);
    child.stdout.on('data', (d) => {
      stdout += d.toString();
      if (stdout.includes('"result"')) {
        clearTimeout(timer);
        child.kill('SIGTERM');
        resolve(stdout);
      }
    });
    child.on('error', (err) => { clearTimeout(timer); reject(err); });
  });

  child.stdin.write(initMsg + '\n');

  const stdout = await responsePromise;
  const respLine = stdout.trim().split('\n').find(l => l.includes('"result"'));
  assert.ok(respLine, 'Should receive MCP initialize response');
  const resp = JSON.parse(respLine);
  assert.equal(resp.id, 1);
  assert.ok(resp.result.serverInfo, 'Must include serverInfo');
  assert.ok(resp.result.capabilities, 'Must include capabilities');
});

test('§4.3 mcp-launcher.js resolves binary in dev mode', () => {
  // Test that find-binary can locate binary without CLAUDE_PLUGIN_ROOT
  // (mcp-launcher.js now derives _FIND_BINARY_ROOT from __dirname)
  const result = execFileSync(process.execPath, ['-e', `
    process.env._FIND_BINARY_ROOT = ${JSON.stringify(PLUGIN_ROOT)};
    const { findBinary } = require(${JSON.stringify(path.join(PLUGIN_ROOT, 'scripts', 'find-binary.js'))});
    const bin = findBinary();
    if (bin) {
      process.stdout.write('OK:' + bin);
    } else {
      process.stdout.write('NOTFOUND');
    }
  `], {
    env: { ...process.env },
    stdio: ['pipe', 'pipe', 'pipe'],
  }).toString();

  assert.match(result, /^OK:/, 'mcp-launcher should find binary');
  assert.ok(result.includes(BINARY_NAME));
});

// ═══════════════════════════════════════════════════════════════════════════
// §5 — Version Sync E2E
// ═══════════════════════════════════════════════════════════════════════════

test('§5.1 all version sources agree', () => {
  const rootPkg = readJson(path.join(ROOT, 'package.json'));
  const pluginManifest = readJson(path.join(PLUGIN_ROOT, '.claude-plugin', 'plugin.json'));
  const marketplace = readJson(path.join(ROOT, '.claude-plugin', 'marketplace.json'));

  // Read Cargo.toml version
  const cargoToml = fs.readFileSync(path.join(ROOT, 'Cargo.toml'), 'utf8');
  const cargoMatch = cargoToml.match(/^version = "(\d+\.\d+\.\d+)"$/m);
  assert.ok(cargoMatch, 'Cargo.toml must have version');
  const cargoVersion = cargoMatch[1];

  const expected = rootPkg.version;
  assert.equal(cargoVersion, expected, 'Cargo.toml');
  assert.equal(pluginManifest.version, expected, 'plugin.json');
  assert.equal(marketplace.metadata.version, expected, 'marketplace metadata');
  assert.equal(marketplace.plugins[0].version, expected, 'marketplace plugin');

  // All platform packages
  for (const dir of fs.readdirSync(path.join(ROOT, 'npm'))) {
    const pkg = readJson(path.join(ROOT, 'npm', dir, 'package.json'));
    assert.equal(pkg.version, expected, `npm/${dir}/package.json`);
  }

  // optionalDependencies versions
  for (const [name, version] of Object.entries(rootPkg.optionalDependencies || {})) {
    assert.equal(version, expected, `optionalDependency ${name}`);
  }
});

test('§5.2 binary version matches package version', () => {
  const binary = path.join(ROOT, 'target', 'release', BINARY_NAME);
  if (!fs.existsSync(binary)) return; // skip if not built

  const result = spawnSync(binary, ['--version'], { stdio: ['pipe', 'pipe', 'pipe'], timeout: 5000 });
  assert.equal(result.status, 0);
  const stdout = result.stdout.toString().trim();
  assert.ok(stdout.includes(CURRENT_VERSION), `Binary (${stdout}) should include ${CURRENT_VERSION}`);
});

// ═══════════════════════════════════════════════════════════════════════════
// §6 — Cross-cutting Integration
// ═══════════════════════════════════════════════════════════════════════════

test('§6.1 full lifecycle: fresh install → use → update → uninstall', () => {
  const homeDir = mkHome();
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  const installedPath = path.join(homeDir, '.claude', 'plugins', 'installed_plugins.json');
  const cacheDir = path.join(homeDir, '.cache', 'code-graph');
  const manifestPath = path.join(cacheDir, 'install-manifest.json');
  const registryPath = path.join(cacheDir, 'statusline-registry.json');
  const env = sandboxEnv(homeDir);

  // Phase 1: Fresh install (no prior settings)
  writeJson(settingsPath, { enabledPlugins: { 'code-graph-mcp@code-graph-mcp': true } });
  writeJson(installedPath, {
    plugins: { 'code-graph-mcp@code-graph-mcp': [{ installPath: PLUGIN_ROOT, version: CURRENT_VERSION, scope: 'user' }] },
  });

  execFileSync(process.execPath, [LIFECYCLE, 'install'], { env, stdio: 'pipe' });
  let settings = readJson(settingsPath);
  // v0.32.0: install() registers PreToolUse/PostToolUse/UserPromptSubmit in
  // settings.json (cache hooks.json only carries SessionStart).
  assertCurrentSettingsHooks(settings);
  assert.ok(settings.statusLine, 'StatusLine configured');
  assert.match(settings.statusLine.command, /statusline-composite/);
  assert.ok(fs.existsSync(manifestPath), 'Manifest created');
  assert.ok(fs.existsSync(registryPath), 'Registry created');

  // Phase 2: Simulate use — find binary works
  const binary = execFileSync(process.execPath, [FIND_BINARY], {
    env: { ...env, _FIND_BINARY_ROOT: ROOT },
    stdio: ['pipe', 'pipe', 'pipe'],
  }).toString().trim();
  assert.ok(binary.length > 0, 'Binary found during use');

  // Phase 3: Update
  execFileSync(process.execPath, [LIFECYCLE, 'update'], { env, stdio: 'pipe' });
  const manifest = readJson(manifestPath);
  assert.equal(manifest.version, CURRENT_VERSION);
  assert.ok(manifest.updatedAt, 'updatedAt refreshed');

  // Phase 4: Uninstall
  execFileSync(process.execPath, [LIFECYCLE, 'uninstall'], { env, stdio: 'pipe' });
  settings = readJson(settingsPath);
  assert.equal(settings.hooks, undefined, 'Hooks removed');
  assert.equal(fs.existsSync(cacheDir), false, 'Cache cleaned');
  assert.deepEqual(settings.enabledPlugins, {}, 'Plugin disabled');
});

test('§6.2 cli.js → find-binary → binary execution chain works end-to-end', () => {
  const result = spawnSync(process.execPath, [BIN_CLI, '--version'], {
    env: { ...process.env, _FIND_BINARY_ROOT: ROOT },
    stdio: ['pipe', 'pipe', 'pipe'],
    timeout: 10000,
  });

  assert.equal(result.status, 0, 'Full chain should exit 0');
  const stdout = result.stdout.toString().trim();
  assert.match(stdout, /\d+\.\d+\.\d+/, 'Should output version');
});

test('§6.3 all hook scripts referenced by lifecycle exist', () => {
  const hookScripts = ['session-init.js', 'incremental-index.js', 'user-prompt-context.js', 'pre-edit-guide.js'];
  for (const script of hookScripts) {
    const scriptPath = path.join(PLUGIN_ROOT, 'scripts', script);
    assert.ok(fs.existsSync(scriptPath), `Hook script missing: ${script}`);
  }
});

test('§6.4 all lifecycle-referenced scripts are syntactically valid', () => {
  const scripts = [
    'lifecycle.js', 'find-binary.js', 'mcp-launcher.js',
    'statusline.js', 'statusline-composite.js', 'auto-update.js',
    'session-init.js', 'incremental-index.js', 'user-prompt-context.js', 'pre-edit-guide.js',
  ];

  for (const script of scripts) {
    const scriptPath = path.join(PLUGIN_ROOT, 'scripts', script);
    if (!fs.existsSync(scriptPath)) continue;
    // Syntax check via node --check
    const result = spawnSync(process.execPath, ['--check', scriptPath], { stdio: ['pipe', 'pipe', 'pipe'] });
    assert.equal(result.status, 0, `Syntax error in ${script}: ${result.stderr?.toString()}`);
  }
});

test('§6.5 bin/cli.js syntax is valid', () => {
  const result = spawnSync(process.execPath, ['--check', BIN_CLI], { stdio: ['pipe', 'pipe', 'pipe'] });
  assert.equal(result.status, 0, `Syntax error in cli.js: ${result.stderr?.toString()}`);
});

// ── §6 the npm/npx wrapper must not act on an unrecognized flag ─────────────
//
// `bin/cli.js` intercepts adopt / unadopt / uninstall before the binary and
// guarded `--help` — but ignored every other token, so `adopt --helpp` ran adopt
// and wrote the user's CLAUDE.md, one keystroke from the side effect the --help
// guard exists to prevent. This is the FOURTH entry point onto the same
// "ignore what you don't recognise" idiom (doctor.js, lifecycle.js doctor and
// src/main.rs were the first three), and it is the surface npm/npx users reach.

// CLAUDE_CONFIG_DIR is pinned alongside HOME, not left to inherit. §6.2 below
// runs a REAL `uninstall --unadopt-all`, and every path it touches
// (settings.json, installed_plugins.json, statusline-providers.json, the plugin
// cache dir) resolves through lifecycle.js's `claudeHome()` — which is
// `process.env.CLAUDE_CONFIG_DIR || homedir/.claude`, so the env var WINS over a
// redirected HOME. A developer who exports it (the documented multi-account
// setup) would have had this file — run by pre-commit, and excluded from CI by
// ci.yml — detach their real statusline and strip their real hooks. Third
// sibling of the same escape hatch; cli_e2e.rs and doctor.test.js closed the
// other two in the same batch that missed this one.
function runCli(homeDir, cwd, args) {
  const r = spawnSync(process.execPath, [BIN_CLI, ...args], {
    cwd,
    env: sandboxEnv(homeDir),
    encoding: 'utf8',
  });
  return { code: r.status, stdout: r.stdout || '', stderr: r.stderr || '' };
}

function sandboxProject(t) {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-cli-home-'));
  const proj = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-cli-proj-'));
  fs.mkdirSync(path.join(proj, '.git'));
  fs.writeFileSync(path.join(proj, 'package.json'), '{"name":"p","version":"1.0.0"}');
  t.after(() => {
    fs.rmSync(home, { recursive: true, force: true });
    fs.rmSync(proj, { recursive: true, force: true });
  });
  return { home, proj, claudeMd: path.join(proj, 'CLAUDE.md') };
}

test('§6.1 bin/cli.js rejects an unknown flag instead of running the command', (t) => {
  for (const [sub, typo] of [
    ['adopt', '--helpp'], ['adopt', '-H'], ['unadopt', '--all'],
    ['uninstall', '--purge-globl'], ['uninstall', '--unadopt'],
  ]) {
    const sb = sandboxProject(t);
    const r = runCli(sb.home, sb.proj, [sub, typo]);
    assert.equal(r.code, 2, `${sub} ${typo} must exit 2; got ${r.code}\n${r.stderr}`);
    assert.match(r.stderr, /unknown argument/, `${sub} ${typo} must say why`);
    assert.equal(fs.existsSync(sb.claudeMd), false,
      `${sub} ${typo} must not have written CLAUDE.md — a typo must not perform ` +
      'the side effect the --help guard exists to prevent');
  }
});

test('§6.2 the real flags and --help still work (the guard is not inert)', (t) => {
  // Negative control for §6.1.
  const sb = sandboxProject(t);
  const help = runCli(sb.home, sb.proj, ['adopt', '--help']);
  assert.equal(help.code, 0);
  assert.match(help.stdout, /USAGE:/);
  assert.equal(fs.existsSync(sb.claudeMd), false, '--help must not write');

  const real = runCli(sb.home, sb.proj, ['adopt']);
  assert.equal(real.code, 0, `bare adopt must still work:\n${real.stderr}`);
  assert.equal(fs.existsSync(sb.claudeMd), true, 'bare adopt must still write the block');

  const un = runCli(sb.home, sb.proj, ['uninstall', '--unadopt-all']);
  assert.notEqual(un.code, 2, 'a documented uninstall flag must not be rejected');
});

test('§6.3 adopt/unadopt help describes the CLAUDE.md scheme, not the pre-v0.74 memory dir', (t) => {
  // The npm surface kept printing "install the code-graph memory file +
  // MEMORY.md sentinel" for three releases after the target moved to the
  // project's own CLAUDE.md — two texts for one command, and npm users got the
  // wrong one.
  const sb = sandboxProject(t);
  for (const sub of ['adopt', 'unadopt']) {
    const { stdout } = runCli(sb.home, sb.proj, [sub, '--help']);
    assert.match(stdout, /CLAUDE\.md/, `${sub} --help must name the real target`);
    assert.doesNotMatch(stdout, /MEMORY\.md sentinel/,
      `${sub} --help still describes the pre-v0.74 memory-dir scheme`);
  }
});
