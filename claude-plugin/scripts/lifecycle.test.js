'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { execFileSync } = require('child_process');

// `CLAUDE_CONFIG_DIR` is dropped from THIS process before anything runs.
//
// Every sandbox below redirects HOME and then spawns with `{...process.env}`,
// which passes the variable straight through — and `claudeHome()` is
// `CLAUDE_CONFIG_DIR || homedir/.claude`, so the env var WINS over the
// redirected HOME. For a developer who exports it (the documented multi-profile
// setup) these tests wrote into their LIVE config: measured, a `9.9.9` plugin
// version landed in the real plugins cache. Deleting it here fixes every spawn
// site at once instead of 40 call sites, and `tests/hardening.rs`'s
// `js_test_files_neutralize_claude_config_dir` keeps new files from skipping it.
delete process.env.CLAUDE_CONFIG_DIR;

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

  // Disabled (not uninstalled) → settings are cleaned but the cache must
  // survive: the user may re-enable, and re-download costs ~40MB.
  assert.deepEqual(JSON.parse(out), { cleaned: true, settingsChanged: true, cacheRemoved: false });
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

  // Genuine uninstall → the composite render is the ONLY plugin code that still
  // runs (CC stopped loading hooks.json), so it must also reclaim the cache.
  assert.deepEqual(JSON.parse(out), { cleaned: true, settingsChanged: true, cacheRemoved: true });
  const settings = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
  assert.equal(settings.statusLine.command, 'echo previous-status');
  assert.equal(fs.existsSync(registryPath), false);
  assert.equal(fs.existsSync(path.join(homeDir, '.cache', 'code-graph')), false);
});

test('isPluginUninstalled distinguishes a genuine uninstall from a temporary disable', (t) => {
  // Orphaned composite (installed_plugins exists, no code-graph record) = uninstalled.
  const uninstalledHome = mkHome(t);
  seedOrphanedComposite(uninstalledHome);
  // enabledPlugins[id]=false = user toggled it off; may re-enable → NOT uninstalled.
  const disabledHome = mkHome(t);
  seedDisabledComposite(disabledHome);

  const probe = (home) => JSON.parse(execFileSync(process.execPath, ['-e', `
    const { isPluginUninstalled } = require(${JSON.stringify(lifecyclePath)});
    process.stdout.write(JSON.stringify(isPluginUninstalled()));
  `], { env: { ...process.env, HOME: home } }).toString());

  assert.equal(probe(uninstalledHome), true, 'orphaned/no-record → uninstalled');
  assert.equal(probe(disabledHome), false, 'explicit disable → not uninstalled (re-enable safe)');
});

test('removeCacheResidue deletes ~/.cache/code-graph and is idempotent', (t) => {
  const homeDir = mkHome(t);
  const cacheDir = path.join(homeDir, '.cache', 'code-graph');
  writeJson(path.join(cacheDir, 'bin', 'marker.json'), { v: 1 });
  fs.writeFileSync(path.join(cacheDir, 'update-state.json'), '{}');

  const run = () => execFileSync(process.execPath, ['-e', `
    const { removeCacheResidue } = require(${JSON.stringify(lifecyclePath)});
    process.stdout.write(JSON.stringify(removeCacheResidue()));
  `], { env: { ...process.env, HOME: homeDir } }).toString();

  assert.equal(run(), 'true');
  assert.equal(fs.existsSync(cacheDir), false, 'cache dir removed');
  assert.equal(run(), 'true', 'second call is a no-op success (idempotent force-rm)');
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

  // The row must carry the command the product ACTUALLY writes for `code-graph`:
  // `codeGraphStatuslineCommand()` (statusline.js), which is what both
  // `registerStatuslineProvider('code-graph', …)` call sites pass. Not
  // `compositeCommand()` (statusline-composite.js) — that is only ever the value
  // of `settings.statusLine.command`. A first version of this fixture used the
  // composite and so tested the keep-branch on an input the product can never
  // produce, hiding a filter that dropped the live install's own entry
  // (v0.118.0 pre-tag review).
  const { codeGraphStatuslineCommand } = require('./lifecycle');
  const liveComposite = codeGraphStatuslineCommand();

  // Seed both files, then simulate user wiping ~/.cache/code-graph/
  writeJson(registryPath, [
    { id: '_previous', command: 'echo gsd', needsStdin: true },
    { id: 'code-graph', command: liveComposite, needsStdin: false },
  ]);
  writeJson(backupPath, [
    { id: '_previous', command: 'echo gsd', needsStdin: true },
    { id: 'code-graph', command: liveComposite, needsStdin: false },
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

// P2 (2026-08-16 audit §四): the durable backup lives in `~/.claude/`, so it
// outlives the plugin cache — including an uninstall that REFUSED to rewrite the
// registry (that refusal is deliberate: rewriting an unreadable registry is how
// the user's providers got destroyed once already). The next install then
// self-healed the previous install's `code-graph` entry back to life, pointing at
// a versioned cache directory that no longer exists: a zombie in the composite
// chain. `_previous` and third-party entries must still come back — those are the
// user's data and the reason the backup exists.
test('self-heal does not resurrect a stale code-graph entry from a dead install', (t) => {
  const homeDir = mkHome(t);
  const cacheDir = path.join(homeDir, '.cache', 'code-graph');
  const registryPath = path.join(cacheDir, 'statusline-registry.json');
  const backupPath = path.join(homeDir, '.claude', 'statusline-providers.json');

  writeJson(backupPath, [
    { id: '_previous', command: 'echo the-users-own-statusline', needsStdin: true },
    { id: 'gsd', command: 'node /opt/gsd/statusline.js', needsStdin: false },
    // A previous install's composite, under a cache version that is gone.
    { id: 'code-graph', command: 'node "/home/u/.claude/plugins/cache/code-graph@0.1.0/scripts/statusline-composite.js"', needsStdin: false },
  ]);
  assert.equal(fs.existsSync(registryPath), false, 'cache is gone, as after uninstall');

  const out = execFileSync(process.execPath, ['-e', `
    const { readRegistry } = require(${JSON.stringify(lifecyclePath)});
    process.stdout.write(JSON.stringify(readRegistry()));
  `], { env: { ...process.env, HOME: homeDir } }).toString();

  const restored = JSON.parse(out);
  const ids = restored.map(p => p.id).sort();
  assert.deepEqual(ids, ['_previous', 'gsd'],
    'the dead code-graph entry must not come back; the user\'s own and third-party entries must');
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

// ════════════════════════════════════════════════════════════════════
// P1-12 — the statusline registry is a READ-MODIFY-WRITE of user data
// ════════════════════════════════════════════════════════════════════
// It holds `_previous` (the statusline the user had before we installed) and
// every third-party provider that registered through us. readRegistry() used the
// LENIENT reader, so "unreadable" and "absent" both became `[]` — and the very
// next write persisted that empty list over BOTH the primary copy and the
// durable backup. One unreadable file, and the user's original statusline is
// unrecoverable. Same class as the settings.json fix two hundred lines above it.

const REGISTRY_SEED = [
  { id: '_previous', command: 'echo the-users-own-statusline', needsStdin: true },
  { id: 'gsd', command: 'node /gsd.cjs', needsStdin: true },
];

function seedRegistryPair(homeDir, value = REGISTRY_SEED) {
  const registryPath = path.join(homeDir, '.cache', 'code-graph', 'statusline-registry.json');
  const backupPath = path.join(homeDir, '.claude', 'statusline-providers.json');
  writeJson(registryPath, value);
  writeJson(backupPath, value);
  return { registryPath, backupPath };
}

function readBytes(p) {
  const saved = fs.statSync(p).mode;
  fs.chmodSync(p, 0o600);
  const bytes = fs.readFileSync(p);
  fs.chmodSync(p, saved);
  return bytes;
}

test('registerStatuslineProvider refuses to rewrite an UNREADABLE registry (P1-12)', (t) => {
  const homeDir = mkHome(t);
  const { registryPath, backupPath } = seedRegistryPair(homeDir);
  const before = { primary: fs.readFileSync(registryPath), backup: fs.readFileSync(backupPath) };

  // EACCES on both copies — a stray `sudo`, a restrictive umask, a borrowed HOME.
  fs.chmodSync(registryPath, 0o000);
  fs.chmodSync(backupPath, 0o000);
  t.after(() => { try { fs.chmodSync(registryPath, 0o600); fs.chmodSync(backupPath, 0o600); } catch { /* gone */ } });

  const out = execFileSync(process.execPath, ['-e', `
    const { registerStatuslineProvider } = require(${JSON.stringify(lifecyclePath)});
    process.stdout.write(String(registerStatuslineProvider('code-graph', 'node /new/statusline.js', false)));
  `], { env: { ...process.env, HOME: homeDir } }).toString();

  assert.equal(out, 'false', 'an unusable registry must report "nothing registered", not success');
  assert.deepEqual(readBytes(registryPath), before.primary,
    'the primary registry must be byte-identical — the user\'s _previous + third-party entries are in it');
  assert.deepEqual(readBytes(backupPath), before.backup,
    'the durable backup is the ONLY recovery path; it must survive too');
});

test('unregisterStatuslineProvider refuses on an unreadable registry (uninstall path)', (t) => {
  const homeDir = mkHome(t);
  const { registryPath, backupPath } = seedRegistryPair(homeDir);
  const before = { primary: fs.readFileSync(registryPath), backup: fs.readFileSync(backupPath) };
  fs.chmodSync(registryPath, 0o000);
  fs.chmodSync(backupPath, 0o000);
  t.after(() => { try { fs.chmodSync(registryPath, 0o600); fs.chmodSync(backupPath, 0o600); } catch { /* gone */ } });

  const out = execFileSync(process.execPath, ['-e', `
    const { unregisterStatuslineProvider } = require(${JSON.stringify(lifecyclePath)});
    process.stdout.write(String(unregisterStatuslineProvider('code-graph')));
  `], { env: { ...process.env, HOME: homeDir } }).toString();

  assert.equal(out, 'false');
  assert.deepEqual(readBytes(registryPath), before.primary);
  assert.deepEqual(readBytes(backupPath), before.backup);
});

test('detachStatuslineIntegration refuses on an unreadable registry instead of deleting the statusline slot (P1-12 sibling)', (t) => {
  const homeDir = mkHome(t);
  const { registryPath, backupPath } = seedRegistryPair(homeDir);
  fs.chmodSync(registryPath, 0o000);
  fs.chmodSync(backupPath, 0o000);
  t.after(() => { try { fs.chmodSync(registryPath, 0o600); fs.chmodSync(backupPath, 0o600); } catch { /* gone */ } });

  // The user's live statusline points at our composite; with the registry
  // unreadable, detach cannot know `_previous` exists — it must leave the
  // slot alone rather than fall into the delete branch.
  const out = execFileSync(process.execPath, ['-e', `
    const { detachStatuslineIntegration } = require(${JSON.stringify(lifecyclePath)});
    const settings = { statusLine: { type: 'command', command: 'node ' + ${JSON.stringify(path.join(homeDir, '.cache', 'code-graph', 'statusline-composite.js'))} } };
    const changed = detachStatuslineIntegration(settings);
    process.stdout.write(JSON.stringify({ changed, hasSlot: !!settings.statusLine }));
  `], { env: { ...process.env, HOME: homeDir } }).toString();

  assert.deepEqual(JSON.parse(out), { changed: false, hasSlot: true },
    'an unreadable registry must refuse the detach — deleting statusLine here orphans the user\'s _previous forever');
});

test('a CORRUPT registry (valid JSON, wrong shape) is also refused, not overwritten', (t) => {
  const homeDir = mkHome(t);
  const registryPath = path.join(homeDir, '.cache', 'code-graph', 'statusline-registry.json');
  const backupPath = path.join(homeDir, '.claude', 'statusline-providers.json');
  // Half-written / hand-edited: parses, but is not the array we own.
  fs.mkdirSync(path.dirname(registryPath), { recursive: true });
  fs.mkdirSync(path.dirname(backupPath), { recursive: true });
  fs.writeFileSync(registryPath, '{"providers": [{"id":"gsd"}]}\n');
  fs.writeFileSync(backupPath, '{"providers": [{"id":"gsd"}]}\n');
  const before = { primary: fs.readFileSync(registryPath), backup: fs.readFileSync(backupPath) };

  const out = execFileSync(process.execPath, ['-e', `
    const { registerStatuslineProvider } = require(${JSON.stringify(lifecyclePath)});
    process.stdout.write(String(registerStatuslineProvider('code-graph', 'node /new/statusline.js', false)));
  `], { env: { ...process.env, HOME: homeDir } }).toString();

  assert.equal(out, 'false');
  assert.deepEqual(fs.readFileSync(registryPath), before.primary);
  assert.deepEqual(fs.readFileSync(backupPath), before.backup);
});

test('the refusal is scoped: a MISSING registry is still a normal fresh registration', (t) => {
  // Negative control for the two tests above. If "refuse" leaked into the absent
  // case, first-install would silently never register the code-graph provider and
  // the statusline would be dark for everyone — a guard that broke the product.
  const homeDir = mkHome(t);
  const registryPath = path.join(homeDir, '.cache', 'code-graph', 'statusline-registry.json');

  const out = execFileSync(process.execPath, ['-e', `
    const { registerStatuslineProvider } = require(${JSON.stringify(lifecyclePath)});
    process.stdout.write(String(registerStatuslineProvider('code-graph', 'node /new/statusline.js', false)));
  `], { env: { ...process.env, HOME: homeDir } }).toString();

  assert.equal(out, 'true');
  assert.deepEqual(JSON.parse(fs.readFileSync(registryPath, 'utf8')),
    [{ id: 'code-graph', command: 'node /new/statusline.js', needsStdin: false }]);
});

test('an unreadable PRIMARY still self-heals from a readable backup (no data loss either way)', (t) => {
  const homeDir = mkHome(t);
  const { registryPath, backupPath } = seedRegistryPair(homeDir);
  const backupBefore = fs.readFileSync(backupPath);
  fs.chmodSync(registryPath, 0o000);
  t.after(() => { try { fs.chmodSync(registryPath, 0o600); } catch { /* gone */ } });

  const out = execFileSync(process.execPath, ['-e', `
    const { registerStatuslineProvider, readRegistry } = require(${JSON.stringify(lifecyclePath)});
    const ok = registerStatuslineProvider('code-graph', 'node /new/statusline.js', false);
    process.stdout.write(JSON.stringify({ ok, ids: readRegistry().map(p => p.id) }));
  `], { env: { ...process.env, HOME: homeDir } }).toString();

  // The primary is unusable, so nothing may be written over it; the entries the
  // backup still holds must NOT be reported as gone.
  const res = JSON.parse(out);
  assert.equal(res.ok, false);
  assert.deepEqual(res.ids, ['_previous', 'gsd'],
    'the user\'s providers must still be visible via the durable backup');
  assert.deepEqual(fs.readFileSync(backupPath), backupBefore, 'backup untouched');
});

test('the third-party statusline-chain CLI exits 2 on an unusable registry (no false success)', (t) => {
  const homeDir = mkHome(t);
  const { registryPath } = seedRegistryPair(homeDir);
  fs.chmodSync(registryPath, 0o000);
  t.after(() => { try { fs.chmodSync(registryPath, 0o600); } catch { /* gone */ } });
  const chainPath = path.join(__dirname, 'statusline-chain.js');
  const { spawnSync } = require('child_process');
  const env = { ...process.env, HOME: homeDir };

  for (const argv of [['register', 'gsd', 'node /gsd.cjs'], ['unregister', 'gsd']]) {
    const r = spawnSync(process.execPath, [chainPath, ...argv], { env, encoding: 'utf8' });
    assert.equal(r.status, 2, `${argv[0]} must fail loudly, not print "unchanged"/"not-found" at exit 0`);
    assert.match(r.stderr, /cannot be read as a provider list/);
    assert.doesNotMatch(r.stdout, /registered|unregistered|unchanged|not-found/);
  }
});

test('uninstall REPORTS an unusable installed_plugins.json instead of skipping it silently', (t) => {
  // The write here was always gated on a successful parse, so nothing was ever
  // clobbered — but the silent skip left Claude Code listing a plugin whose
  // cache directory the same run then deleted, while uninstall reported success.
  const homeDir = mkHome(t);
  const installedPath = path.join(homeDir, '.claude', 'plugins', 'installed_plugins.json');
  fs.mkdirSync(path.dirname(installedPath), { recursive: true });
  fs.writeFileSync(installedPath, '{"plugins": {"code-graph-mcp@code-graph-mcp": [truncated\n');
  const before = fs.readFileSync(installedPath);

  const out = execFileSync(process.execPath, ['-e', `
    const { uninstall } = require(${JSON.stringify(lifecyclePath)});
    process.stdout.write(JSON.stringify(uninstall({ runNpm: () => true, scanGlobalPkgs: () => [] })));
  `], { env: { ...process.env, HOME: homeDir }, stdio: ['pipe', 'pipe', 'pipe'] });

  assert.equal(JSON.parse(out.toString()).installedPluginsUnusable, true);
  assert.deepEqual(fs.readFileSync(installedPath), before, 'and the file itself is untouched');
});

test('uninstall neutralizes the statusline slot even when the registry refuses a write', (t) => {
  // Pre-tag review of the detach-refusal fix: refusing is right for the
  // statusline render (it retries next frame), but uninstall is ONE-SHOT and
  // deletes statusline-composite.js in the same run. Treating the refusal as
  // "nothing to change" left settings.statusLine pointing at a script that no
  // longer exists, with no plugin code left to repair it — the user's status
  // line is then permanently broken. Corrupt primary + healthy backup is the
  // shape that regressed: the backup still knows `_previous`.
  const homeDir = mkHome(t);
  const registryPath = path.join(homeDir, '.cache', 'code-graph', 'statusline-registry.json');
  const backupPath = path.join(homeDir, '.claude', 'statusline-providers.json');
  fs.mkdirSync(path.dirname(registryPath), { recursive: true });
  fs.mkdirSync(path.dirname(backupPath), { recursive: true });
  fs.writeFileSync(registryPath, '{"providers": [ truncated\n');   // corrupt → refuse
  writeJson(backupPath, [
    { id: '_previous', command: 'echo the-users-own-statusline', needsStdin: true },
    { id: 'code-graph', command: 'node /cache/statusline-composite.js', needsStdin: false },
  ]);
  const backupBefore = fs.readFileSync(backupPath);
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  writeJson(settingsPath, {
    statusLine: { type: 'command', command: 'node ' + path.join(homeDir, '.cache', 'code-graph', 'statusline-composite.js') },
  });

  execFileSync(process.execPath, ['-e', `
    const { uninstall } = require(${JSON.stringify(lifecyclePath)});
    uninstall({ runNpm: () => true, scanGlobalPkgs: () => [] });
  `], { env: { ...process.env, HOME: homeDir }, stdio: ['pipe', 'pipe', 'pipe'] });

  const after = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
  const slot = after.statusLine && after.statusLine.command;
  assert.ok(!slot || !slot.includes('statusline-composite'),
    `uninstall must not leave the slot pointing at the composite it just deleted, got: ${slot}`);
  assert.equal(slot, 'echo the-users-own-statusline',
    'the durable backup still holds _previous on a corrupt primary — restore it');
  // The primary lives under the plugin cache, which uninstall deletes wholesale
  // — so the byte-level "never rewritten" check belongs on the durable backup,
  // the copy that outlives the cache and is the user's only recovery path.
  assert.deepEqual(fs.readFileSync(backupPath), backupBefore,
    'refusing means we never rewrite the registry — the durable backup must be byte-identical');
});

// ════════════════════════════════════════════════════════════════════
// v0.32.0 — settings.json hook registration (replaces the v0.8.3 strip)
// ════════════════════════════════════════════════════════════════════

test('install() registers PreToolUse/PostToolUse/UserPromptSubmit hooks in settings.json', (t) => {
  const homeDir = mkHome(t);
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  writeJson(settingsPath, {
    statusLine: { type: 'command', command: 'echo previous-status' },
  });

  execFileSync(process.execPath, [lifecyclePath, 'install'], {
    env: { ...process.env, HOME: homeDir },
  });

  const after = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
  assert.ok(after.hooks, 'install() must add hooks block');
  assert.ok(after.hooks.PreToolUse, 'PreToolUse must be registered');
  assert.ok(after.hooks.PostToolUse, 'PostToolUse must be registered');
  assert.ok(after.hooks.UserPromptSubmit, 'UserPromptSubmit must be registered');

  // Verify the matchers we promised exist
  const ptuMatchers = after.hooks.PreToolUse.map(e => e.matcher);
  for (const m of ['Edit', 'Bash', 'Read']) {
    assert.ok(ptuMatchers.includes(m), `PreToolUse matcher ${m} missing; got ${JSON.stringify(ptuMatchers)}`);
  }

  // Every registered entry must carry the description marker for cleanup
  for (const entries of Object.values(after.hooks)) {
    for (const e of entries) {
      if (e.description) {
        assert.ok(e.description.includes('[code-graph-mcp'),
          `entry without our marker leaked through: ${JSON.stringify(e.description)}`);
      }
    }
  }

  // statusLine composite still set
  assert.match(after.statusLine.command, /statusline-composite/);
});

test('install() strips legacy code-graph hooks AND writes fresh ones (migration path)', (t) => {
  const homeDir = mkHome(t);
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  // Seed with v0.8.2-era legacy entries that should be cleaned up
  writeJson(settingsPath, {
    hooks: legacyHooksFromPlugin(),
  });

  execFileSync(process.execPath, [lifecyclePath, 'install'], {
    env: { ...process.env, HOME: homeDir },
  });

  const after = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
  // Legacy stale paths should be gone — no `/stale/cache/0.8.2/` survivors
  const serialized = JSON.stringify(after.hooks || {});
  assert.ok(!serialized.includes('/stale/cache/'),
    'legacy stale paths must be evicted: ' + serialized);
  // BUT fresh entries (v0.32.0 markers) should be present
  assert.ok(serialized.includes('[code-graph-mcp v0.32+]'),
    'fresh v0.32+ entries should be installed');
});

test('install() is idempotent on settings.json (second call no-op)', (t) => {
  const homeDir = mkHome(t);
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');

  execFileSync(process.execPath, [lifecyclePath, 'install'], {
    env: { ...process.env, HOME: homeDir },
  });
  const first = fs.readFileSync(settingsPath, 'utf8');

  execFileSync(process.execPath, [lifecyclePath, 'install'], {
    env: { ...process.env, HOME: homeDir },
  });
  const second = fs.readFileSync(settingsPath, 'utf8');

  assert.equal(first, second, 'second install() must produce byte-identical settings.json');
});

test('install() preserves foreign plugin hooks (other plugins\' entries survive)', (t) => {
  const homeDir = mkHome(t);
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  // Seed with an unrelated plugin's hooks alongside ours
  writeJson(settingsPath, {
    hooks: {
      PreToolUse: [{
        matcher: 'Bash',
        description: 'some-other-plugin Bash inspector',
        hooks: [{ type: 'command', command: 'node /opt/other-plugin/bash-check.js', timeout: 3 }],
      }],
      PostToolUse: [{
        matcher: '*',
        description: 'foreign post-tool logger',
        hooks: [{ type: 'command', command: 'bash /opt/foreign/post.sh', timeout: 5 }],
      }],
    },
  });

  execFileSync(process.execPath, [lifecyclePath, 'install'], {
    env: { ...process.env, HOME: homeDir },
  });

  const after = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
  // Foreign entries must still be there
  const ptu = after.hooks.PreToolUse;
  const otherBash = ptu.find(e => e.description === 'some-other-plugin Bash inspector');
  assert.ok(otherBash, 'foreign Bash hook was stripped — never strip non-code-graph entries');

  const ptoFor = after.hooks.PostToolUse.find(e => e.description === 'foreign post-tool logger');
  assert.ok(ptoFor, 'foreign PostToolUse hook was stripped');

  // Ours are also there
  assert.ok(after.hooks.PreToolUse.some(e => e.matcher === 'Edit' && e.description?.includes('[code-graph-mcp')));
});

test('registerHooksToSettings is idempotent when called directly', () => {
  // Pure-function direct call, no process spawn
  const { registerHooksToSettings } = require('./lifecycle.js');
  const settings = {};
  const changed1 = registerHooksToSettings(settings);
  const snapshot1 = JSON.stringify(settings);
  const changed2 = registerHooksToSettings(settings);
  const snapshot2 = JSON.stringify(settings);
  assert.equal(changed1, true, 'first call must report change');
  assert.equal(changed2, false, 'second call must report no-change (idempotent)');
  assert.equal(snapshot1, snapshot2, 'settings must be byte-identical after second call');
});

test('removeHooksFromSettings cleans up v0.32+ entries (uninstall path)', () => {
  const { registerHooksToSettings, removeHooksFromSettings } = require('./lifecycle.js');
  const settings = {};
  registerHooksToSettings(settings);
  // Sanity: have entries
  assert.ok(settings.hooks.PreToolUse && settings.hooks.PreToolUse.length > 0);

  const changed = removeHooksFromSettings(settings);
  assert.equal(changed, true);
  assert.ok(!settings.hooks || Object.keys(settings.hooks).length === 0,
    'all our entries must be removed; got: ' + JSON.stringify(settings.hooks));
});

test('uninstall() removes settings.json hook entries end-to-end', (t) => {
  const homeDir = mkHome(t);
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');

  execFileSync(process.execPath, [lifecyclePath, 'install'], {
    env: { ...process.env, HOME: homeDir },
  });
  const afterInstall = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
  assert.ok(afterInstall.hooks?.PreToolUse, 'install must have created hooks');

  execFileSync(process.execPath, [lifecyclePath, 'uninstall'], {
    env: { ...process.env, HOME: homeDir },
  });
  const afterUninstall = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
  // Our hooks should be gone (foreign ones would survive but we didn't seed any)
  const serialized = JSON.stringify(afterUninstall.hooks || {});
  assert.ok(!serialized.includes('[code-graph-mcp'),
    'uninstall must strip all our entries; got: ' + serialized);
});

test('hook commands use absolute paths (no ${CLAUDE_PLUGIN_ROOT} in settings.json)', (t) => {
  // settings.json hook commands run with env-pollution risk per
  // feedback_plugin_env_isolation.md — they must NOT depend on
  // ${CLAUDE_PLUGIN_ROOT} (different plugins overwrite each other's value).
  const homeDir = mkHome(t);
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');

  execFileSync(process.execPath, [lifecyclePath, 'install'], {
    env: { ...process.env, HOME: homeDir },
  });
  const after = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
  const serialized = JSON.stringify(after.hooks || {});
  assert.ok(!serialized.includes('${CLAUDE_PLUGIN_ROOT}'),
    'settings.json hook commands must not reference ${CLAUDE_PLUGIN_ROOT}: ' + serialized);
});

// ════════════════════════════════════════════════════════════════════
// v0.32.2 — update() upgrade-path integration tests (reviewer Rec #2)
// ════════════════════════════════════════════════════════════════════
// Covers the actual v0.31.x → v0.32.x migration path that runs in
// production via session-init.js syncLifecycleConfig detecting a manifest
// version mismatch and calling update(). Previously only install() was
// tested end-to-end; the upgrade path shared the registerHooksToSettings
// code internally but had no integration test exercising the wiring.

test('update() from v0.31.x manifest registers fresh hooks in empty settings.json', (t) => {
  const homeDir = mkHome(t);
  const manifestPath = path.join(homeDir, '.cache', 'code-graph', 'install-manifest.json');
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');

  // Seed v0.31.2 manifest state. updatedAt is the v0.31.2 release date.
  writeJson(manifestPath, {
    version: '0.31.2',
    installedAt: '2026-03-16T18:56:17.656Z',
    updatedAt: '2026-05-23T16:46:39.353Z',
    config: { statusLine: false },
  });
  // settings.json empty (mirrors real v0.31.x state — pre-v0.32.0 strategy
  // was "strip from settings.json, rely on plugin-cache hooks.json").
  writeJson(settingsPath, {});

  const out = execFileSync(process.execPath, [lifecyclePath, 'update'], {
    env: { ...process.env, HOME: homeDir },
  }).toString();
  assert.match(out, /Updated 0\.31\.2 → /, 'CLI output must show version transition');

  // Manifest version was bumped to current
  const manifestAfter = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
  assert.notEqual(manifestAfter.version, '0.31.2', 'manifest version must advance');
  assert.ok(/^\d+\.\d+\.\d+$/.test(manifestAfter.version),
    `manifest version must be semver, got ${manifestAfter.version}`);

  // settings.json got the v0.32+ hook entries
  const settingsAfter = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
  assert.ok(settingsAfter.hooks, 'update() must populate hooks block');
  assert.ok(settingsAfter.hooks.PreToolUse, 'PreToolUse must be registered');
  assert.ok(settingsAfter.hooks.PostToolUse, 'PostToolUse must be registered');
  assert.ok(settingsAfter.hooks.UserPromptSubmit, 'UserPromptSubmit must be registered');

  // Every entry must carry the v0.32+ marker
  for (const entries of Object.values(settingsAfter.hooks)) {
    for (const e of entries) {
      assert.ok(e.description && e.description.includes('[code-graph-mcp v0.32+'),
        `update() entry without v0.32+ marker: ${JSON.stringify(e.description)}`);
    }
  }
});

test('update() from v0.31.x evicts legacy v0.7/v0.8 entries with stale paths', (t) => {
  const homeDir = mkHome(t);
  const manifestPath = path.join(homeDir, '.cache', 'code-graph', 'install-manifest.json');
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');

  writeJson(manifestPath, {
    version: '0.31.2',
    installedAt: '2026-03-16T18:56:17.656Z',
    config: { statusLine: false },
  });
  // Seed with legacy v0.8.2-era entries that should be evicted on update.
  writeJson(settingsPath, {
    hooks: legacyHooksFromPlugin(),
  });

  execFileSync(process.execPath, [lifecyclePath, 'update'], {
    env: { ...process.env, HOME: homeDir },
  });

  const settingsAfter = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
  const serialized = JSON.stringify(settingsAfter.hooks || {});
  // Stale paths must be gone
  assert.ok(!serialized.includes('/stale/cache/'),
    'legacy stale paths must be evicted by update(): ' + serialized);
  // Fresh v0.32+ entries must be present
  assert.ok(serialized.includes('[code-graph-mcp v0.32+'),
    'fresh v0.32+ entries must be installed by update()');
});

test('update() preserves foreign plugin hooks during upgrade', (t) => {
  const homeDir = mkHome(t);
  const manifestPath = path.join(homeDir, '.cache', 'code-graph', 'install-manifest.json');
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');

  writeJson(manifestPath, {
    version: '0.31.2',
    config: { statusLine: false },
  });
  // Seed with an unrelated plugin's hooks — must survive our update().
  writeJson(settingsPath, {
    hooks: {
      PreToolUse: [{
        matcher: 'Bash',
        description: 'foreign-plugin Bash watcher',
        hooks: [{ type: 'command', command: 'node /opt/foreign/bash.js', timeout: 3 }],
      }],
    },
  });

  execFileSync(process.execPath, [lifecyclePath, 'update'], {
    env: { ...process.env, HOME: homeDir },
  });

  const settingsAfter = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
  const ptu = settingsAfter.hooks.PreToolUse;
  assert.ok(ptu.some(e => e.description === 'foreign-plugin Bash watcher'),
    'foreign Bash hook must survive update() — never strip non-code-graph entries');
  // And our own entries must coexist
  assert.ok(ptu.some(e => e.description && e.description.includes('[code-graph-mcp v0.32+')),
    'update() must add our v0.32+ entries alongside the foreign one');
});

// ════════════════════════════════════════════════════════════════════
// v0.32.2 — healthCheck post-repair re-verification
// (Reviewer M3: repaired:true was set blindly after install() without
//  re-scanning to confirm the issues actually resolved.)
// ════════════════════════════════════════════════════════════════════

function runHealthCheckInChild(homeDir) {
  const code = `
    const lc = require(${JSON.stringify(lifecyclePath)});
    process.stdout.write(JSON.stringify(lc.healthCheck()));
  `;
  const out = execFileSync(process.execPath, ['-e', code], {
    env: { ...process.env, HOME: homeDir },
    encoding: 'utf8',
  });
  return JSON.parse(out);
}

test('healthCheck on a clean state returns healthy:true and never sets remaining', (t) => {
  const homeDir = mkHome(t);
  // No settings.json, no registry — clean slate.
  const r = runHealthCheckInChild(homeDir);
  assert.equal(r.healthy, true, 'fresh empty state must be healthy');
  assert.deepEqual(r.issues, [], 'no issues on empty state');
  assert.equal(r.repaired, false, 'no repair runs when nothing was broken');
  assert.equal(r.remaining, undefined, 'no remaining field when no repair attempted');
});

test('healthCheck repaired:true ONLY after post-repair re-scan returns clean', (t) => {
  const homeDir = mkHome(t);
  // Seed a hook entry whose path is broken AND carries our marker. install()
  // will overwrite our entries with fresh absolute paths derived from
  // __dirname (which is real in the test env), so the re-scan should be clean.
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  writeJson(settingsPath, {
    hooks: {
      PreToolUse: [{
        matcher: 'Edit',
        description: '[code-graph-mcp v0.32+] PreToolUse re-routed via settings.json (cache hooks.json silently ignored for this event by current CC)',
        hooks: [{ type: 'command', command: 'node "/nonexistent/code-graph-mcp/pre-edit-guide.js"' }],
      }],
    },
  });

  const r = runHealthCheckInChild(homeDir);
  assert.equal(r.healthy, false, 'pre-repair scan must have flagged the broken path');
  assert.ok(r.issues.length >= 1, 'pre-repair issues must list the broken hook');
  assert.equal(r.repaired, true, 'install() rewrote our entry → post-scan clean → repaired:true');
  assert.deepEqual(r.remaining, [], 'remaining must be empty when repair succeeded');
});

test('healthCheck repaired:false when install() cannot resolve a flagged path', (t) => {
  const homeDir = mkHome(t);
  // Seed the registry with a non-`_previous` third-party provider whose path
  // is broken. install() only manages the 'code-graph' registry entry, so
  // the third-party entry survives untouched and the post-repair re-scan
  // still flags it. This is the canonical "auto-repair could not fix it"
  // path — previously the function lied and returned repaired:true anyway.
  const registryPath = path.join(homeDir, '.cache', 'code-graph', 'statusline-registry.json');
  writeJson(registryPath, [
    { id: 'third-party-statusline', command: 'node "/nonexistent/foreign/sl.js"', needsStdin: false },
  ]);

  const r = runHealthCheckInChild(homeDir);
  assert.equal(r.healthy, false, 'broken third-party path must be flagged on entry');
  assert.ok(r.issues.some(i => i.type === 'registry' && i.id === 'third-party-statusline'),
    'pre-repair issue list must contain the third-party entry');
  assert.equal(r.repaired, false,
    'install() does not touch third-party providers → re-scan still broken → repaired must be false');
  assert.ok(Array.isArray(r.remaining), 'remaining must be present when install() was attempted');
  assert.ok(r.remaining.some(i => i.id === 'third-party-statusline'),
    'remaining must still contain the un-fixable third-party entry');
});

test('scanForBrokenPaths is exported and returns the issue structure', (t) => {
  // Direct unit test of the extracted scanner — no install() side effects.
  // Verifies the contract M3 relies on: a pure function whose return
  // shape is what healthCheck composes its result from.
  const homeDir = mkHome(t);
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  writeJson(settingsPath, {
    hooks: {
      PreToolUse: [{
        matcher: 'Edit',
        description: '[code-graph-mcp v0.32+] PreToolUse re-routed via settings.json (cache hooks.json silently ignored for this event by current CC)',
        hooks: [{ type: 'command', command: 'node "/nonexistent/code-graph-mcp/pre-edit-guide.js"' }],
      }],
    },
  });

  const code = `
    const lc = require(${JSON.stringify(lifecyclePath)});
    process.stdout.write(JSON.stringify(lc.scanForBrokenPaths()));
  `;
  const out = execFileSync(process.execPath, ['-e', code], {
    env: { ...process.env, HOME: homeDir },
    encoding: 'utf8',
  });
  const issues = JSON.parse(out);
  assert.ok(Array.isArray(issues));
  assert.ok(issues.some(i => i.type === 'hook' && i.event === 'PreToolUse' && i.path.includes('/nonexistent/')),
    'scanForBrokenPaths must report the seeded broken hook entry');
});
// ── isStaleRelicContext (v0.49.1 downgrade-war guard) ──────────────────────

test('isStaleRelicContext: relic in plugins cache defers to a different active install', (t) => {
  const { isStaleRelicContext } = require('./lifecycle');
  const cacheRoot = '/home/u/.claude/plugins/cache';
  const relicRoot = `${cacheRoot}/code-graph-mcp/code-graph-mcp/0.48.0`;
  const activeRoot = `${cacheRoot}/code-graph-mcp/code-graph-mcp/0.49.0`;

  // The downgrade-war case: running from old cache dir, active points elsewhere.
  assert.equal(isStaleRelicContext({
    pluginRoot: relicRoot, cacheRoot, activePath: activeRoot,
    existsSync: () => true,
  }), true);

  // Running FROM the active install → full self-heal rights.
  assert.equal(isStaleRelicContext({
    pluginRoot: activeRoot, cacheRoot, activePath: activeRoot,
    existsSync: () => true,
  }), false);

  // Dev checkout / npm install (pluginRoot outside the plugins cache) → exempt.
  assert.equal(isStaleRelicContext({
    pluginRoot: '/repo/code-graph-mcp/claude-plugin', cacheRoot, activePath: activeRoot,
    existsSync: () => true,
  }), false);

  // No installed_plugins record → exempt (nothing authoritative to defer to).
  assert.equal(isStaleRelicContext({
    pluginRoot: relicRoot, cacheRoot, activePath: null,
    existsSync: () => true,
  }), false);

  // Active path recorded but its lifecycle.js is gone (cache wiped) → the
  // relic is the only working copy left; keep self-heal rights.
  assert.equal(isStaleRelicContext({
    pluginRoot: relicRoot, cacheRoot, activePath: activeRoot,
    existsSync: () => false,
  }), false);
});

test('cleanupOldCacheVersions keeps an in-use version even beyond the keep window', (t) => {
  const { cleanupOldCacheVersions } = require('./lifecycle.js');
  const cacheParent = fs.mkdtempSync(path.join(os.tmpdir(), 'code-graph-cache-'));
  t.after(() => fs.rmSync(cacheParent, { recursive: true, force: true }));
  const pluginDir = path.join(cacheParent, 'code-graph-mcp');
  // Seven versions, oldest -> newest by mtime.
  const vers = ['0.78.0', '0.80.2', '0.80.3', '0.81.0', '0.81.1', '0.81.2', '0.81.3'];
  vers.forEach((v, i) => {
    const scripts = path.join(pluginDir, v, 'scripts');
    fs.mkdirSync(scripts, { recursive: true });
    fs.writeFileSync(path.join(scripts, 'mcp-launcher.js'), '// stub');
    const ts = (i + 1) * 3600; // distinct, increasing mtimes
    fs.utimesSync(path.join(pluginDir, v), ts, ts);
  });
  // A live MCP server is running from the OLDEST version (beyond keep=5) — this
  // is the v0.80.2 reconnect-(-32000) scenario.
  const inUse = path.join(pluginDir, '0.78.0');
  const fakeCmdlines = [`node ${path.join(inUse, 'scripts', 'mcp-launcher.js')} `];

  cleanupOldCacheVersions(5, () => fakeCmdlines, cacheParent);

  assert.equal(fs.existsSync(inUse), true,
    'in-use version must survive prune even when it is the oldest');
  assert.equal(fs.existsSync(path.join(pluginDir, '0.80.2')), false,
    'a non-in-use version beyond the keep window is still pruned');
  assert.equal(fs.existsSync(path.join(pluginDir, '0.81.3')), true,
    'newest version (within keep window) is kept');
});

test('cleanupOldCacheVersions prunes beyond keep when nothing is in use', (t) => {
  const { cleanupOldCacheVersions } = require('./lifecycle.js');
  const cacheParent = fs.mkdtempSync(path.join(os.tmpdir(), 'code-graph-cache-'));
  t.after(() => fs.rmSync(cacheParent, { recursive: true, force: true }));
  const pluginDir = path.join(cacheParent, 'code-graph-mcp');
  const vers = ['0.78.0', '0.80.2', '0.80.3', '0.81.0', '0.81.1', '0.81.2', '0.81.3'];
  vers.forEach((v, i) => {
    fs.mkdirSync(path.join(pluginDir, v), { recursive: true });
    const ts = (i + 1) * 3600;
    fs.utimesSync(path.join(pluginDir, v), ts, ts);
  });
  // No live process references any version → recency-only pruning (pre-guard).
  cleanupOldCacheVersions(5, () => [], cacheParent);

  assert.equal(fs.existsSync(path.join(pluginDir, '0.78.0')), false, 'oldest pruned');
  assert.equal(fs.existsSync(path.join(pluginDir, '0.80.2')), false, '2nd-oldest pruned');
  assert.equal(fs.existsSync(path.join(pluginDir, '0.80.3')), true, 'within keep window kept');
  assert.equal(
    fs.readdirSync(pluginDir).filter(n => fs.statSync(path.join(pluginDir, n)).isDirectory()).length,
    5, 'exactly keep=5 versions remain');
});

// ── uninstall: global npm packages + residue guidance ───────────────────────
//
// The launcher's background install runs `npm install -g` on the user's
// behalf and writes global-install-marker.json. uninstall() must remove those
// packages when the marker proves plugin ownership (or --purge-global), and
// must NEVER touch a user's own global install without either.

function seedUninstallHome(t) {
  const homeDir = mkHome(t);
  const cacheDir = path.join(homeDir, '.cache', 'code-graph');
  fs.mkdirSync(cacheDir, { recursive: true });
  writeJson(path.join(homeDir, '.claude', 'settings.json'), {});
  return { homeDir, cacheDir };
}

function runUninstallInSubprocess(homeDir, { marker, purgeGlobal = false, pkgs }) {
  // In-subprocess stub: scanGlobalPkgs returns `pkgs` until runNpm "removes"
  // them; records the npm args it would have run.
  const script = `
    const { uninstall } = require(${JSON.stringify(lifecyclePath)});
    let installed = ${JSON.stringify(pkgs)};
    const npmCalls = [];
    const r = uninstall({
      purgeGlobal: ${JSON.stringify(purgeGlobal)},
      scanGlobalPkgs: () => installed,
      runNpm: (args) => { npmCalls.push(args); installed = []; return true; },
    });
    process.stdout.write(JSON.stringify({ r, npmCalls }));
  `;
  return JSON.parse(execFileSync(process.execPath, ['-e', script], {
    env: { ...process.env, HOME: homeDir },
  }).toString());
}

test('uninstall removes plugin-installed global packages (marker present)', (t) => {
  const { homeDir, cacheDir } = seedUninstallHome(t);
  writeJson(path.join(cacheDir, 'global-install-marker.json'),
    { installedBy: 'code-graph-mcp launcher', version: '1.2.3' });
  writeJson(path.join(cacheDir, 'adopted-projects.json'), ['/proj/a', '/proj/b']);
  const pkgs = ['@sdsrs/code-graph', '@sdsrs/code-graph-linux-x64'];

  const { r, npmCalls } = runUninstallInSubprocess(homeDir, { marker: true, pkgs });

  assert.equal(r.pluginInstalledGlobals, true);
  assert.deepEqual(npmCalls, [['uninstall', '-g', ...pkgs]]);
  assert.deepEqual(r.globalPkgsRemoved, pkgs);
  assert.deepEqual(r.globalPkgsRemaining, []);
  assert.deepEqual(r.adoptedProjects, ['/proj/a', '/proj/b'],
    'adoption inventory read before the cache wipe');
  assert.equal(fs.existsSync(cacheDir), false, 'cache dir removed');
});

test('uninstall leaves user-installed globals alone without marker; --purge-global overrides', (t) => {
  const pkgs = ['@sdsrs/code-graph', '@sdsrs/code-graph-linux-x64'];

  // No marker, no flag → report only, never run npm.
  const a = seedUninstallHome(t);
  const resA = runUninstallInSubprocess(a.homeDir, { pkgs });
  assert.equal(resA.r.pluginInstalledGlobals, false);
  assert.deepEqual(resA.npmCalls, [], 'must not uninstall a user-owned global install');
  assert.deepEqual(resA.r.globalPkgsRemoved, []);
  assert.deepEqual(resA.r.globalPkgsRemaining, pkgs, 'remaining packages surfaced for manual cleanup');

  // No marker + explicit --purge-global → removal authorized by the user.
  const b = seedUninstallHome(t);
  const resB = runUninstallInSubprocess(b.homeDir, { purgeGlobal: true, pkgs });
  assert.deepEqual(resB.npmCalls, [['uninstall', '-g', ...pkgs]]);
  assert.deepEqual(resB.r.globalPkgsRemoved, pkgs);
});

// ── detach: third-party statusline providers must survive ───────────────────

function seedThirdPartyRegistry(homeDir) {
  const registryPath = path.join(homeDir, '.cache', 'code-graph', 'statusline-registry.json');
  writeJson(registryPath, [
    { id: '_previous', command: 'echo previous-status', needsStdin: true },
    { id: 'code-graph', command: 'node "/plugin/statusline.js"', needsStdin: false },
    { id: 'gsd', command: 'node "/gsd/statusline.js"', needsStdin: true },
  ]);
  return registryPath;
}

test('uninstall hands the statusLine slot to a surviving third-party provider', (t) => {
  const homeDir = mkHome(t);
  seedOrphanedComposite(homeDir);
  seedThirdPartyRegistry(homeDir);

  const out = execFileSync(process.execPath, ['-e', `
    const { cleanupDisabledStatusline } = require(${JSON.stringify(lifecyclePath)});
    process.stdout.write(JSON.stringify(cleanupDisabledStatusline()));
  `], { env: { ...process.env, HOME: homeDir } }).toString();

  assert.deepEqual(JSON.parse(out), { cleaned: true, settingsChanged: true, cacheRemoved: true });
  const settings = JSON.parse(fs.readFileSync(path.join(homeDir, '.claude', 'settings.json'), 'utf8'));
  // Our composite dies with the uninstall — the slot must go to the third
  // party, NOT to _previous (which would silently drop gsd's segment).
  assert.equal(settings.statusLine.command, 'node "/gsd/statusline.js"');
  // Durable backup keeps the surviving providers (primary died with the cache).
  const backup = JSON.parse(fs.readFileSync(path.join(homeDir, '.claude', 'statusline-providers.json'), 'utf8'));
  const ids = backup.map((p) => p.id).sort();
  assert.deepEqual(ids, ['_previous', 'gsd'], 'code-graph gone; third party + _previous retained');
});

test('temporary disable keeps the composite when a third-party provider is registered', (t) => {
  const homeDir = mkHome(t);
  seedDisabledComposite(homeDir);
  seedThirdPartyRegistry(homeDir);

  execFileSync(process.execPath, ['-e', `
    const { cleanupDisabledStatusline } = require(${JSON.stringify(lifecyclePath)});
    cleanupDisabledStatusline();
  `], { env: { ...process.env, HOME: homeDir } });

  const settings = JSON.parse(fs.readFileSync(path.join(homeDir, '.claude', 'settings.json'), 'utf8'));
  // Composite script survives a mere disable and keeps rendering gsd.
  assert.ok(settings.statusLine.command.includes('statusline-composite'),
    'composite retained so the third-party segment keeps rendering');
});

// ── install: statusline slot ping-pong stand-down ───────────────────────────

test('install stands down after 3 displacements; explicit install re-claims', (t) => {
  const homeDir = mkHome(t);
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  const manifestPath = path.join(homeDir, '.cache', 'code-graph', 'install-manifest.json');

  const script = `
    const fs = require('fs');
    const { install, readManifest } = require(${JSON.stringify(lifecyclePath)});
    const settingsPath = ${JSON.stringify(settingsPath)};
    const foreign = { type: 'command', command: 'node "/peer-plugin/statusline.js"' };
    const seedForeign = () => {
      const s = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
      s.statusLine = foreign;
      fs.writeFileSync(settingsPath, JSON.stringify(s));
    };
    const slot = () => JSON.parse(fs.readFileSync(settingsPath, 'utf8')).statusLine.command;
    const outcomes = [];
    for (let i = 0; i < 3; i++) {
      seedForeign();
      install();
      outcomes.push(slot().includes('statusline-composite') ? 'claimed' : 'stood-down');
    }
    const m = readManifest();
    seedForeign();
    install({ reclaimStatusline: true });
    outcomes.push(slot().includes('statusline-composite') ? 'claimed' : 'stood-down');
    process.stdout.write(JSON.stringify({ outcomes, displaced: m.config.statuslineDisplaced, owned: m.config.statusLine }));
  `;
  writeJson(settingsPath, { statusLine: { type: 'command', command: 'node "/peer-plugin/statusline.js"' } });
  writeJson(manifestPath, { version: '0.0.1', config: { statusLine: true } });

  const r = JSON.parse(execFileSync(process.execPath, ['-e', script], {
    env: { ...process.env, HOME: homeDir },
  }).toString());

  // Two re-claims, then stand-down; explicit install() re-claims again.
  assert.deepEqual(r.outcomes, ['claimed', 'claimed', 'stood-down', 'claimed']);
  assert.equal(r.displaced, 3, 'third displacement recorded before standing down');
  assert.equal(r.owned, false, 'ownership released on stand-down (stops the counter)');
});

test('stand-down re-arms once the slot is empty again (P2-22)', (t) => {
  // Stand-down exists to end a tug-of-war. When the other provider is
  // uninstalled the slot goes empty and there is nobody left to fight — but the
  // counter was write-only, so the plugin stayed statusline-less for the life of
  // the manifest and the only way back was an env var nobody knows to set.
  const homeDir = mkHome(t);
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  const manifestPath = path.join(homeDir, '.cache', 'code-graph', 'install-manifest.json');

  const script = `
    const fs = require('fs');
    const { install, readManifest } = require(${JSON.stringify(lifecyclePath)});
    const settingsPath = ${JSON.stringify(settingsPath)};
    const slot = () => (JSON.parse(fs.readFileSync(settingsPath, 'utf8')).statusLine || {}).command || '';
    // Already stood down (displaced past the threshold, ownership released) and
    // the competitor has since been uninstalled: no statusLine key at all.
    install();
    const m = readManifest();
    process.stdout.write(JSON.stringify({
      claimed: slot().includes('statusline-composite'),
      displaced: m.config.statuslineDisplaced,
      owned: m.config.statusLine,
    }));
  `;
  writeJson(settingsPath, {});
  writeJson(manifestPath, {
    version: '0.0.1',
    config: { statusLine: false, statuslineDisplaced: 5 },
  });

  const r = JSON.parse(execFileSync(process.execPath, ['-e', script], {
    env: { ...process.env, HOME: homeDir },
  }).toString());

  assert.equal(r.claimed, true, 'an empty slot must be claimed — nobody is displacing us');
  assert.equal(r.displaced, 0, 'the displacement counter re-arms');
  assert.equal(r.owned, true, 'ownership is taken again');
});

test('stand-down HOLDS while a foreign provider still occupies the slot (P2-22)', (t) => {
  // The negative control for the re-arm above: an occupied slot must still be
  // left alone, or the re-arm would simply delete stand-down.
  const homeDir = mkHome(t);
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  const manifestPath = path.join(homeDir, '.cache', 'code-graph', 'install-manifest.json');

  const script = `
    const fs = require('fs');
    const { install } = require(${JSON.stringify(lifecyclePath)});
    const settingsPath = ${JSON.stringify(settingsPath)};
    install();
    const s = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
    process.stdout.write(JSON.stringify({ cmd: (s.statusLine || {}).command || '' }));
  `;
  writeJson(settingsPath, { statusLine: { type: 'command', command: 'node "/peer-plugin/statusline.js"' } });
  writeJson(manifestPath, {
    version: '0.0.1',
    config: { statusLine: false, statuslineDisplaced: 5 },
  });

  const r = JSON.parse(execFileSync(process.execPath, ['-e', script], {
    env: { ...process.env, HOME: homeDir },
  }).toString());

  assert.equal(r.cmd, 'node "/peer-plugin/statusline.js"',
    'a slot another provider holds must stay theirs while stood down');
});

// ── uninstall --unadopt-all ─────────────────────────────────────────────────

test('uninstall --unadopt-all sweeps every registered adopted project', (t) => {
  const homeDir = mkHome(t);
  writeJson(path.join(homeDir, '.claude', 'settings.json'), {});
  const proj = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-unadoptall-'));
  t.after(() => fs.rmSync(proj, { recursive: true, force: true }));
  fs.mkdirSync(path.join(proj, '.git'));

  const script = `
    const { adopt } = require(${JSON.stringify(path.join(__dirname, 'adopt.js'))});
    const { uninstall } = require(${JSON.stringify(lifecyclePath)});
    const a = adopt({ cwd: ${JSON.stringify(proj)} });
    const r = uninstall({ unadoptAll: true, scanGlobalPkgs: () => [], runNpm: () => true });
    process.stdout.write(JSON.stringify({ adopted: a.ok, r }));
  `;
  const { adopted, r } = JSON.parse(execFileSync(process.execPath, ['-e', script], {
    env: { ...process.env, HOME: homeDir },
  }).toString());

  assert.equal(adopted, true);
  assert.equal(r.unadopted.length, 1);
  assert.equal(r.unadopted[0].cleaned, true);
  assert.deepEqual(r.adoptedProjects, [], 'registry re-read shows nothing left to hand-clean');
  assert.equal(fs.existsSync(path.join(proj, 'CLAUDE.md')), false, 'managed CLAUDE.md removed (we created it)');
  assert.equal(fs.existsSync(path.join(proj, '.claude', 'plugin_code_graph_mcp.md')), false, 'detail file removed');
});
