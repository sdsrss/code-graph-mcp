'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { execFileSync } = require('child_process');

const lifecyclePath = path.join(__dirname, 'lifecycle.js');
const statuslinePath = path.join(__dirname, 'statusline.js');

function mkHome(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'code-graph-home-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
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

test('cleanupDisabledStatusline restores previous statusline and removes registry', (t) => {
  const homeDir = mkHome(t);
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

test('statusline exits cleanly and self-heals when plugin is disabled', (t) => {
  const homeDir = mkHome(t);
  const { settingsPath, registryPath } = seedDisabledComposite(homeDir);
  const projectDir = fs.mkdtempSync(path.join(os.tmpdir(), 'code-graph-project-'));
  t.after(() => fs.rmSync(projectDir, { recursive: true, force: true }));
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

test('cleanupDisabledStatusline also heals orphaned statusline after uninstall', (t) => {
  const homeDir = mkHome(t);
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

function legacyHooksFromPlugin() {
  return {
    SessionStart: [{
      matcher: 'startup|clear|compact',
      description: 'StatusLine self-heal, lifecycle sync, project map injection',
      hooks: [{ type: 'command', command: 'node "/stale/cache/0.8.2/claude-plugin/scripts/session-init.js"', timeout: 5 }],
    }],
    PostToolUse: [{
      matcher: 'tool == "Write" || tool == "Edit"',
      description: 'Auto-update code graph index after file edits',
      hooks: [{ type: 'command', command: 'node "/stale/code-graph/incremental-index.js"', timeout: 10 }],
    }],
  };
}

test('isOurHookEntry matches legacy description-tagged entries', () => {
  const entry = legacyHooksFromPlugin().SessionStart[0];
  const out = execFileSync(process.execPath, ['-e', `
    const { isOurHookEntry } = require(${JSON.stringify(lifecyclePath)});
    process.stdout.write(JSON.stringify(isOurHookEntry(${JSON.stringify(entry)})));
  `]).toString();
  assert.equal(JSON.parse(out), true);
});

test('isOurHookEntry matches script-name + path fallback (missing description)', () => {
  const entry = {
    matcher: 'tool == "Edit"',
    hooks: [{ type: 'command', command: 'node "/cache/code-graph-mcp/scripts/pre-edit-guide.js"' }],
  };
  const out = execFileSync(process.execPath, ['-e', `
    const { isOurHookEntry } = require(${JSON.stringify(lifecyclePath)});
    process.stdout.write(JSON.stringify(isOurHookEntry(${JSON.stringify(entry)})));
  `]).toString();
  assert.equal(JSON.parse(out), true);
});

test('isOurHookEntry leaves unrelated entries alone', () => {
  const entry = {
    matcher: 'startup',
    description: 'some other plugin hook',
    hooks: [{ type: 'command', command: 'node /some/other/script.js' }],
  };
  const out = execFileSync(process.execPath, ['-e', `
    const { isOurHookEntry } = require(${JSON.stringify(lifecyclePath)});
    process.stdout.write(JSON.stringify(isOurHookEntry(${JSON.stringify(entry)})));
  `]).toString();
  assert.equal(JSON.parse(out), false);
});

test('removeHooksFromSettings strips our entries but keeps unrelated hooks', () => {
  const settings = {
    hooks: {
      SessionStart: [
        legacyHooksFromPlugin().SessionStart[0],
        {
          matcher: 'startup',
          description: 'some other plugin hook',
          hooks: [{ type: 'command', command: 'node /some/other/script.js' }],
        },
      ],
      PostToolUse: [legacyHooksFromPlugin().PostToolUse[0]],
    },
  };

  const out = execFileSync(process.execPath, ['-e', `
    const { removeHooksFromSettings } = require(${JSON.stringify(lifecyclePath)});
    const s = ${JSON.stringify(settings)};
    const changed = removeHooksFromSettings(s);
    process.stdout.write(JSON.stringify({ changed, s }));
  `]).toString();

  const { changed, s } = JSON.parse(out);
  assert.equal(changed, true);
  // Only the unrelated SessionStart entry remains; PostToolUse removed entirely.
  assert.equal(s.hooks.SessionStart.length, 1);
  assert.equal(s.hooks.SessionStart[0].description, 'some other plugin hook');
  assert.ok(!s.hooks.PostToolUse, 'empty event key should be deleted');
});

test('writeRegistry mirrors entries to durable backup outside ~/.cache/', (t) => {
  const homeDir = mkHome(t);
  const registryPath = path.join(homeDir, '.cache', 'code-graph', 'statusline-registry.json');
  const backupPath = path.join(homeDir, '.claude', 'statusline-providers.json');

  execFileSync(process.execPath, ['-e', `
    const { registerStatuslineProvider } = require(${JSON.stringify(lifecyclePath)});
    registerStatuslineProvider('_previous', 'echo prev', true);
    registerStatuslineProvider('code-graph', 'node /cg.js', false);
  `], { env: { ...process.env, HOME: homeDir } });

  const primary = JSON.parse(fs.readFileSync(registryPath, 'utf8'));
  const backup = JSON.parse(fs.readFileSync(backupPath, 'utf8'));
  assert.deepEqual(primary, backup);
  assert.equal(primary.length, 2);
});

test('readRegistry self-heals primary from durable backup after cache wipe', (t) => {
  const homeDir = mkHome(t);
  const cacheDir = path.join(homeDir, '.cache', 'code-graph');
  const registryPath = path.join(cacheDir, 'statusline-registry.json');
  const backupPath = path.join(homeDir, '.claude', 'statusline-providers.json');

  // Seed both files, then simulate user wiping ~/.cache/code-graph/
  writeJson(registryPath, [
    { id: '_previous', command: 'echo gsd', needsStdin: true },
    { id: 'code-graph', command: 'node /cg.js', needsStdin: false },
  ]);
  writeJson(backupPath, [
    { id: '_previous', command: 'echo gsd', needsStdin: true },
    { id: 'code-graph', command: 'node /cg.js', needsStdin: false },
  ]);
  fs.rmSync(cacheDir, { recursive: true, force: true });
  assert.equal(fs.existsSync(registryPath), false);

  const out = execFileSync(process.execPath, ['-e', `
    const { readRegistry } = require(${JSON.stringify(lifecyclePath)});
    process.stdout.write(JSON.stringify(readRegistry()));
  `], { env: { ...process.env, HOME: homeDir } }).toString();

  const restored = JSON.parse(out);
  assert.equal(restored.length, 2);
  assert.equal(restored[0].id, '_previous');
  // Primary file rebuilt from backup
  assert.equal(fs.existsSync(registryPath), true);
});

test('writeRegistry([]) clears both primary and backup', (t) => {
  const homeDir = mkHome(t);
  const registryPath = path.join(homeDir, '.cache', 'code-graph', 'statusline-registry.json');
  const backupPath = path.join(homeDir, '.claude', 'statusline-providers.json');

  execFileSync(process.execPath, ['-e', `
    const { registerStatuslineProvider, unregisterStatuslineProvider } = require(${JSON.stringify(lifecyclePath)});
    registerStatuslineProvider('code-graph', 'node /cg.js', false);
    unregisterStatuslineProvider('code-graph');
  `], { env: { ...process.env, HOME: homeDir } });

  assert.equal(fs.existsSync(registryPath), false);
  assert.equal(fs.existsSync(backupPath), false);
});

test('statusline-chain CLI register/unregister/list + reserved-id guard', (t) => {
  const homeDir = mkHome(t);
  const chainPath = path.join(__dirname, 'statusline-chain.js');
  const env = { ...process.env, HOME: homeDir };

  const reg = execFileSync(process.execPath, [chainPath, 'register', 'gsd', 'node /gsd.cjs', '--stdin'], { env }).toString();
  assert.match(reg, /registered gsd/);

  const reRun = execFileSync(process.execPath, [chainPath, 'register', 'gsd', 'node /gsd.cjs', '--stdin'], { env }).toString();
  assert.match(reRun, /unchanged gsd/);

  const list = execFileSync(process.execPath, [chainPath, 'list'], { env }).toString();
  assert.match(list, /gsd \[stdin\]: node \/gsd\.cjs/);

  // Reserved ids rejected — both should exit 2 with stderr "reserved"
  const { spawnSync } = require('child_process');
  for (const rid of ['_previous', 'code-graph']) {
    const r = spawnSync(process.execPath, [chainPath, 'register', rid, 'x'], { env });
    assert.equal(r.status, 2, `${rid} should exit 2`);
    assert.match(r.stderr.toString(), /reserved/);
  }

  const un = execFileSync(process.execPath, [chainPath, 'unregister', 'gsd'], { env }).toString();
  assert.match(un, /unregistered gsd/);
});

test('install() removes legacy code-graph hooks from settings.json without re-registering', (t) => {
  const homeDir = mkHome(t);
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  writeJson(settingsPath, {
    statusLine: { type: 'command', command: 'echo previous-status' },
    hooks: legacyHooksFromPlugin(),
  });

  execFileSync(process.execPath, [lifecyclePath, 'install'], {
    env: { ...process.env, HOME: homeDir },
  });

  const after = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
  // No code-graph hook entries should remain — cache hooks.json is authoritative now.
  const serialized = JSON.stringify(after.hooks || {});
  assert.ok(!serialized.includes('code-graph'), 'settings.json must not retain code-graph hook entries');
  assert.ok(!serialized.includes('session-init.js'), 'settings.json must not retain session-init.js paths');
  // StatusLine composite is still registered (only channel available).
  assert.match(after.statusLine.command, /statusline-composite/);
});