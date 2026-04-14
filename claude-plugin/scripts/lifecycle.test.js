'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { execFileSync } = require('child_process');

const lifecyclePath = path.join(__dirname, 'lifecycle.js');
const statuslinePath = path.join(__dirname, 'statusline.js');

function mkHome() {
  return fs.mkdtempSync(path.join(os.tmpdir(), 'code-graph-home-'));
}

function writeJson(filePath, value) {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, JSON.stringify(value, null, 2) + '\n');
}

function seedDisabledComposite(homeDir) {
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  const registryPath = path.join(homeDir, '.cache', 'code-graph', 'statusline-registry.json');
  writeJson(settingsPath, {
    statusLine: { type: 'command', command: 'node "/plugin/statusline-composite.js"' },
    enabledPlugins: { 'code-graph-mcp@code-graph-mcp': false },
  });
  writeJson(registryPath, [
    { id: '_previous', command: 'echo previous-status', needsStdin: true },
    { id: 'code-graph', command: 'node "/plugin/statusline.js"', needsStdin: false },
  ]);
  return { settingsPath, registryPath };
}

function seedOrphanedComposite(homeDir) {
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  const registryPath = path.join(homeDir, '.cache', 'code-graph', 'statusline-registry.json');
  const installedPath = path.join(homeDir, '.claude', 'plugins', 'installed_plugins.json');
  writeJson(settingsPath, {
    statusLine: { type: 'command', command: 'node "/plugin/statusline-composite.js"' },
    enabledPlugins: {},
  });
  writeJson(installedPath, { plugins: {} });
  writeJson(registryPath, [
    { id: '_previous', command: 'echo previous-status', needsStdin: true },
    { id: 'code-graph', command: 'node "/plugin/statusline.js"', needsStdin: false },
  ]);
  return { settingsPath, registryPath };
}

test('cleanupDisabledStatusline restores previous statusline and removes registry', () => {
  const homeDir = mkHome();
  const { settingsPath, registryPath } = seedDisabledComposite(homeDir);

  const out = execFileSync(process.execPath, ['-e', `
    const { cleanupDisabledStatusline } = require(${JSON.stringify(lifecyclePath)});
    process.stdout.write(JSON.stringify(cleanupDisabledStatusline()));
  `], { env: { ...process.env, HOME: homeDir } }).toString();

  assert.deepEqual(JSON.parse(out), { cleaned: true, settingsChanged: true });
  const settings = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
  assert.equal(settings.statusLine.command, 'echo previous-status');
  assert.equal(fs.existsSync(registryPath), false);
});

test('statusline exits cleanly and self-heals when plugin is disabled', () => {
  const homeDir = mkHome();
  const { settingsPath, registryPath } = seedDisabledComposite(homeDir);
  const projectDir = fs.mkdtempSync(path.join(os.tmpdir(), 'code-graph-project-'));
  fs.mkdirSync(path.join(projectDir, '.code-graph'), { recursive: true });
  fs.writeFileSync(path.join(projectDir, '.code-graph', 'index.db'), '');

  const stdout = execFileSync(process.execPath, [statuslinePath], {
    env: { ...process.env, HOME: homeDir },
    cwd: projectDir,
  }).toString();

  assert.equal(stdout, '');
  const settings = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
  assert.equal(settings.statusLine.command, 'echo previous-status');
  assert.equal(fs.existsSync(registryPath), false);
});

test('cleanupDisabledStatusline also heals orphaned statusline after uninstall', () => {
  const homeDir = mkHome();
  const { settingsPath, registryPath } = seedOrphanedComposite(homeDir);

  const out = execFileSync(process.execPath, ['-e', `
    const { cleanupDisabledStatusline } = require(${JSON.stringify(lifecyclePath)});
    process.stdout.write(JSON.stringify(cleanupDisabledStatusline()));
  `], { env: { ...process.env, HOME: homeDir } }).toString();

  assert.deepEqual(JSON.parse(out), { cleaned: true, settingsChanged: true });
  const settings = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
  assert.equal(settings.statusLine.command, 'echo previous-status');
  assert.equal(fs.existsSync(registryPath), false);
});

function nonEmptyHooksJson() {
  return {
    hooks: {
      SessionStart: [{
        matcher: 'startup',
        hooks: [{ type: 'command', command: 'node "/plugin/session-init.js"' }],
      }],
    },
  };
}

test('findStalePluginHooksJson detects non-empty cache and marketplace copies', () => {
  const homeDir = mkHome();
  const mpHooks = path.join(homeDir, '.claude', 'plugins', 'marketplaces', 'code-graph-mcp', 'claude-plugin', 'hooks', 'hooks.json');
  const mpManifest = path.join(homeDir, '.claude', 'plugins', 'marketplaces', 'code-graph-mcp', '.claude-plugin', 'marketplace.json');
  const cacheHooks = path.join(homeDir, '.claude', 'plugins', 'cache', 'code-graph-mcp', 'code-graph-mcp', '0.7.17', 'hooks', 'hooks.json');

  writeJson(mpManifest, { name: 'code-graph-mcp' });
  writeJson(mpHooks, nonEmptyHooksJson());
  writeJson(cacheHooks, nonEmptyHooksJson());

  const out = execFileSync(process.execPath, ['-e', `
    const { findStalePluginHooksJson } = require(${JSON.stringify(lifecyclePath)});
    process.stdout.write(JSON.stringify(findStalePluginHooksJson()));
  `], { env: { ...process.env, HOME: homeDir } }).toString();

  const stale = JSON.parse(out).sort();
  assert.equal(stale.length, 2);
  assert.ok(stale.some(p => p === mpHooks));
  assert.ok(stale.some(p => p === cacheHooks));
});

test('clearStalePluginCacheHooks empties non-empty hooks.json copies', () => {
  const homeDir = mkHome();
  const cacheHooks = path.join(homeDir, '.claude', 'plugins', 'cache', 'code-graph-mcp', 'code-graph-mcp', '0.7.17', 'hooks', 'hooks.json');
  writeJson(cacheHooks, nonEmptyHooksJson());

  const out = execFileSync(process.execPath, ['-e', `
    const { clearStalePluginCacheHooks } = require(${JSON.stringify(lifecyclePath)});
    process.stdout.write(JSON.stringify(clearStalePluginCacheHooks()));
  `], { env: { ...process.env, HOME: homeDir } }).toString();

  const cleared = JSON.parse(out);
  assert.deepEqual(cleared, [cacheHooks]);

  const payload = JSON.parse(fs.readFileSync(cacheHooks, 'utf8'));
  assert.deepEqual(payload.hooks, {});
  assert.ok(payload._note && payload._note.includes('cleared'));
});

test('clearStalePluginCacheHooks is idempotent and skips already-empty copies', () => {
  const homeDir = mkHome();
  const cacheHooks = path.join(homeDir, '.claude', 'plugins', 'cache', 'code-graph-mcp', 'code-graph-mcp', '0.7.17', 'hooks', 'hooks.json');
  writeJson(cacheHooks, { hooks: {} });

  const out = execFileSync(process.execPath, ['-e', `
    const { clearStalePluginCacheHooks } = require(${JSON.stringify(lifecyclePath)});
    process.stdout.write(JSON.stringify(clearStalePluginCacheHooks()));
  `], { env: { ...process.env, HOME: homeDir } }).toString();

  assert.deepEqual(JSON.parse(out), []);
});

test('scanPluginHooksJsonCopies ignores unrelated marketplaces', () => {
  const homeDir = mkHome();
  const otherMp = path.join(homeDir, '.claude', 'plugins', 'marketplaces', 'some-other-plugin', 'claude-plugin', 'hooks', 'hooks.json');
  const otherManifest = path.join(homeDir, '.claude', 'plugins', 'marketplaces', 'some-other-plugin', '.claude-plugin', 'marketplace.json');
  writeJson(otherManifest, { name: 'some-other-plugin' });
  writeJson(otherMp, nonEmptyHooksJson());

  const out = execFileSync(process.execPath, ['-e', `
    const { scanPluginHooksJsonCopies } = require(${JSON.stringify(lifecyclePath)});
    process.stdout.write(JSON.stringify(scanPluginHooksJsonCopies()));
  `], { env: { ...process.env, HOME: homeDir } }).toString();

  assert.deepEqual(JSON.parse(out), []);
});