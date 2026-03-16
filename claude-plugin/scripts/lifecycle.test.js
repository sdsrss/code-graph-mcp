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