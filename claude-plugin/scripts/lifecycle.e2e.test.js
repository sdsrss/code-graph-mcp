'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { execFileSync } = require('child_process');

const repoRoot = path.resolve(__dirname, '..', '..');
const pluginRoot = path.resolve(__dirname, '..');
const lifecycleCli = path.join(__dirname, 'lifecycle.js');
const compositeCli = path.join(__dirname, 'statusline-composite.js');
const currentVersion = JSON.parse(fs.readFileSync(path.join(repoRoot, 'package.json'), 'utf8')).version;

function mkHome() {
  return fs.mkdtempSync(path.join(os.tmpdir(), 'code-graph-e2e-'));
}

function writeJson(filePath, value) {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, JSON.stringify(value, null, 2) + '\n');
}

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function runScript(homeDir, scriptPath, args = [], options = {}) {
  const env = { ...process.env, HOME: homeDir };
  // Do NOT set CLAUDE_PLUGIN_ROOT — lifecycle.js derives PLUGIN_ROOT from __dirname
  // to avoid env var leakage from other plugins in shared hook execution context.
  delete env.CLAUDE_PLUGIN_ROOT;
  return execFileSync(process.execPath, [scriptPath, ...args], {
    cwd: options.cwd || repoRoot,
    env,
    input: options.input,
    stdio: ['pipe', 'pipe', 'pipe'],
  }).toString();
}

test('lifecycle CLI handles install, disable self-heal, re-enable, and uninstall', () => {
  const homeDir = mkHome();
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  const installedPath = path.join(homeDir, '.claude', 'plugins', 'installed_plugins.json');
  const registryPath = path.join(homeDir, '.cache', 'code-graph', 'statusline-registry.json');
  const manifestPath = path.join(homeDir, '.cache', 'code-graph', 'install-manifest.json');
  const cacheDir = path.join(homeDir, '.cache', 'code-graph');

  writeJson(settingsPath, {
    statusLine: { type: 'command', command: 'echo previous-status' },
    enabledPlugins: { 'code-graph-mcp@code-graph-mcp': true },
  });
  writeJson(installedPath, {
    plugins: {
      'code-graph-mcp@code-graph-mcp': [{
        installPath: pluginRoot,
        version: currentVersion,
        scope: 'user',
      }],
    },
  });

  runScript(homeDir, lifecycleCli, ['install']);
  let settings = readJson(settingsPath);
  let registry = readJson(registryPath);
  let manifest = readJson(manifestPath);
  assert.match(settings.statusLine.command, /statusline-composite\.js/);
  assert.equal(registry[0].id, '_previous');
  assert.equal(registry[1].id, 'code-graph');
  assert.equal(manifest.version, currentVersion);

  settings.enabledPlugins['code-graph-mcp@code-graph-mcp'] = false;
  writeJson(settingsPath, settings);
  runScript(homeDir, compositeCli, [], { input: '{}' });
  settings = readJson(settingsPath);
  assert.equal(settings.statusLine.command, 'echo previous-status');
  assert.equal(fs.existsSync(registryPath), false);

  settings.enabledPlugins['code-graph-mcp@code-graph-mcp'] = true;
  writeJson(settingsPath, settings);
  runScript(homeDir, lifecycleCli, ['install']);
  settings = readJson(settingsPath);
  registry = readJson(registryPath);
  assert.match(settings.statusLine.command, /statusline-composite\.js/);
  assert.equal(registry.length, 2);

  runScript(homeDir, lifecycleCli, ['uninstall']);
  settings = readJson(settingsPath);
  const installed = readJson(installedPath);
  assert.equal(settings.statusLine.command, 'echo previous-status');
  assert.deepEqual(settings.enabledPlugins, {});
  assert.deepEqual(installed.plugins, {});
  assert.equal(fs.existsSync(cacheDir), false);
});

