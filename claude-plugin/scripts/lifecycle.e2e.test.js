'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { execFileSync, spawnSync } = require('child_process');

const repoRoot = path.resolve(__dirname, '..', '..');
const pluginRoot = path.resolve(__dirname, '..');
const lifecycleCli = path.join(__dirname, 'lifecycle.js');
const compositeCli = path.join(__dirname, 'statusline-composite.js');
const currentVersion = JSON.parse(fs.readFileSync(path.join(repoRoot, 'package.json'), 'utf8')).version;

// `CLAUDE_CONFIG_DIR` is dropped from THIS process before anything runs.
//
// The sandboxes below redirect HOME and spawn with `{...process.env}`, which
// passes the variable straight through — and `claudeHome()` is
// `CLAUDE_CONFIG_DIR || homedir/.claude`, so the env var WINS over the
// redirected HOME. For a developer who exports it (the documented multi-profile
// setup) these tests wrote into their LIVE config: measured, a fabricated
// `9.9.9` plugin version landed in the real plugins cache. Deleting it here
// covers every spawn site at once; the few tests that need the variable set it
// explicitly in their own child env, which still wins.
delete process.env.CLAUDE_CONFIG_DIR;

// Same argument, second variable. `runScript` builds every child env as
// `{ ...process.env, HOME: homeDir }`, and `uninstall()` step 6.5 deletes
// `<os.tmpdir()>/code-graph-mcp` wholesale — the machine-global dir holding live
// cooldown flags and `update-*` staging. Every other path it removes is
// HOME-derived, so redirecting HOME alone read as sandboxed while this one
// `rmSync` reached the real directory: measured, a full-suite run destroyed it
// every time, and the failures surfaced in whichever sibling test file was
// running in parallel rather than here.
//
// The two tests that assert ON that deletion (`uninstall removes the shared tmp
// dir`, `SessionStart reclaims aged cgTmpDir residue`) pass TMPDIR explicitly in
// their own child env, which still wins over this.
// Guarded by tests/hardening.rs `js_test_suite_leaves_the_shared_tmp_dir_intact`.
const TMP_SANDBOX = fs.mkdtempSync(path.join(os.tmpdir(), 'code-graph-e2e-tmp-'));
process.env.TMPDIR = TMP_SANDBOX;
process.env.TMP = TMP_SANDBOX;
process.env.TEMP = TMP_SANDBOX;
test.after(() => {
  try { fs.rmSync(TMP_SANDBOX, { recursive: true, force: true }); } catch { /* best effort */ }
});

// Per-test cleanup PLUS a run-end sweep. The per-test `t.after` alone left
// exactly one `code-graph-e2e-*` directory behind per run — 154 had accumulated
// in ~/.claude/tmp/ — and the survivor held only
// `.cache/code-graph/{binary-path,hook-fire-state.json}` with no `.claude`, so
// it was RE-created after its cleanup ran, not missed by it. The sweep below
// runs after every test in this file and catches those. Measured after: 0 leaked
// across 4 isolated runs and 2 full-suite runs, down from +1 every run. A
// straggler landing after the very last test would still slip through, which is
// why this is a second net rather than a replacement for the per-test hook.
const E2E_HOMES = [];
function mkHome(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'code-graph-e2e-'));
  E2E_HOMES.push(dir);
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
}

test.after(() => {
  for (const dir of E2E_HOMES) {
    try { fs.rmSync(dir, { recursive: true, force: true }); } catch { /* best effort */ }
  }
});

function writeJson(filePath, value) {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, JSON.stringify(value, null, 2) + '\n');
}

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function runScript(homeDir, scriptPath, args = [], options = {}) {
  const env = { ...process.env, HOME: homeDir, USERPROFILE: homeDir };
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

// Same spawn, but hands back STDERR too. `runScript` throws away stderr and
// `execFileSync` only surfaces it on a non-zero exit — which is exactly the case
// that never happens for a script whose whole job is to swallow provider errors
// and still render. No timeout is passed: a fractional one silently kills
// spawnSync (memory #7), and the caller here has no deadline to enforce.
function runScriptCaptured(homeDir, scriptPath, args = [], options = {}) {
  const env = { ...process.env, HOME: homeDir, USERPROFILE: homeDir, ...(options.env || {}) };
  delete env.CLAUDE_PLUGIN_ROOT;
  const r = spawnSync(process.execPath, [scriptPath, ...args], {
    cwd: options.cwd || repoRoot,
    env,
    input: options.input,
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  return { stdout: (r.stdout || '').toString(), stderr: (r.stderr || '').toString() };
}

test('lifecycle CLI handles install, disable self-heal, re-enable, and uninstall', (t) => {
  const homeDir = mkHome(t);
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

test('lifecycle install writes to CLAUDE_CONFIG_DIR instead of ~/.claude when set', (t) => {
  // Multi-account isolation: a user with CLAUDE_CONFIG_DIR=~/work-claude
  // expects all plugin config (settings.json, installed_plugins.json,
  // statusline-providers backup) to land under that directory, not the
  // default ~/.claude. Default path must remain untouched.
  const homeDir = mkHome(t);
  const configDir = fs.mkdtempSync(path.join(os.tmpdir(), 'code-graph-cfgdir-'));
  t.after(() => fs.rmSync(configDir, { recursive: true, force: true }));

  const cfgSettings = path.join(configDir, 'settings.json');
  const cfgInstalled = path.join(configDir, 'plugins', 'installed_plugins.json');
  const cfgBackup = path.join(configDir, 'statusline-providers.json');
  const defaultSettings = path.join(homeDir, '.claude', 'settings.json');

  writeJson(cfgSettings, {
    statusLine: { type: 'command', command: 'echo prior-work-status' },
    enabledPlugins: { 'code-graph-mcp@code-graph-mcp': true },
  });
  writeJson(cfgInstalled, {
    plugins: {
      'code-graph-mcp@code-graph-mcp': [{
        installPath: pluginRoot,
        version: currentVersion,
        scope: 'user',
      }],
    },
  });

  // Run install with CLAUDE_CONFIG_DIR set; HOME points elsewhere.
  const env = { ...process.env, HOME: homeDir, USERPROFILE: homeDir, CLAUDE_CONFIG_DIR: configDir };
  delete env.CLAUDE_PLUGIN_ROOT;
  execFileSync(process.execPath, [lifecycleCli, 'install'], {
    cwd: repoRoot, env, stdio: ['pipe', 'pipe', 'pipe'],
  });

  // Config landed in the override dir...
  const settings = readJson(cfgSettings);
  assert.match(settings.statusLine.command, /statusline-composite\.js/);
  assert.equal(fs.existsSync(cfgBackup), true,
    'statusline-providers backup should land in CLAUDE_CONFIG_DIR');

  // ...and default ~/.claude was never touched.
  assert.equal(fs.existsSync(defaultSettings), false,
    'default ~/.claude/settings.json must not be written when override is set');
});

test('composite expands a leading ~ in a _previous command instead of dropping it (issue #24)', (t) => {
  // A user whose prior statusline used a leading ~ (valid in settings.json, which
  // Claude Code runs through a shell). install() captures it verbatim as _previous.
  // The composite runs providers via execFileSync (no shell), so without tilde
  // expansion the command throws ENOENT and is silently swallowed — the user's
  // original statusline vanishes.
  const homeDir = mkHome(t);
  const prevScript = path.join(homeDir, '.claude', 'utils', 'statusline.sh');
  fs.mkdirSync(path.dirname(prevScript), { recursive: true });
  fs.writeFileSync(prevScript, '#!/bin/sh\necho "PREV-STATUSLINE-OK"\n');
  fs.chmodSync(prevScript, 0o755);

  const registryPath = path.join(homeDir, '.cache', 'code-graph', 'statusline-registry.json');
  writeJson(registryPath, [
    { id: '_previous', command: '~/.claude/utils/statusline.sh', needsStdin: true },
  ]);

  // CODE_GRAPH_STATUSLINE_DEBUG: this assertion has been intermittently red
  // (measured 2/8 on an unmodified tree, 0/8 on another day — noise, not a
  // trend) and carried NOTHING to diagnose it with, because runProvider's catch
  // makes "the script failed to exec" and "the script printed nothing" the same
  // observation from out here. The env var turns the swallowed error into a
  // stderr line; the assertion below quotes it, so the next red names its own
  // cause instead of restarting the guessing. Leading candidates, none yet
  // observed: ETXTBSY (fits — write-then-exec, fails in ~70 ms like this one
  // does), ENOENT, EACCES.
  const { stdout, stderr } = runScriptCaptured(homeDir, compositeCli, [], {
    input: '{}',
    env: { CODE_GRAPH_STATUSLINE_DEBUG: '1' },
  });
  assert.match(stdout, /PREV-STATUSLINE-OK/,
    'a _previous command using a leading ~ must be tilde-expanded, not silently dropped'
    + `\n  composite stderr: ${stderr.trim() || '(empty — the provider ran and printed nothing)'}`);
});

test('a dropped provider names its reason on stderr under CODE_GRAPH_STATUSLINE_DEBUG', (t) => {
  // Positive control for the diagnostic the test above depends on. A debug
  // channel that has never been observed to print is worth exactly as much as
  // the silent catch it replaced: the next intermittent red would still arrive
  // with "(empty)" and no way to tell an unset env var from a provider that
  // really did run and print nothing.
  const homeDir = mkHome(t);
  const registryPath = path.join(homeDir, '.cache', 'code-graph', 'statusline-registry.json');
  writeJson(registryPath, [
    { id: '_previous', command: path.join(homeDir, 'nope', 'missing.sh'), needsStdin: true },
  ]);

  const on = runScriptCaptured(homeDir, compositeCli, [], {
    input: '{}',
    env: { CODE_GRAPH_STATUSLINE_DEBUG: '1' },
  });
  assert.match(on.stderr, /\[statusline] provider '_previous' dropped: ENOENT/,
    `a provider that cannot exec must say why when the debug var is set (stderr: ${JSON.stringify(on.stderr)})`);

  // …and stays silent by default: this ships, and a user's statusline must not
  // grow a diagnostic line because a third-party provider is broken.
  const off = runScriptCaptured(homeDir, compositeCli, [], { input: '{}' });
  assert.equal(off.stderr.includes('[statusline]'), false,
    `the debug channel must be off unless asked for (stderr: ${JSON.stringify(off.stderr)})`);
});

test('expandTilde mirrors shell tilde expansion (only a leading ~ / ~/)', () => {
  const composite = require('./statusline-composite');
  const home = os.homedir();
  assert.equal(composite.expandTilde('~'), home);
  assert.equal(composite.expandTilde('~/.claude/utils/statusline.sh'),
    path.join(home, '.claude', 'utils', 'statusline.sh'));
  assert.equal(composite.expandTilde('/abs/path/script.sh'), '/abs/path/script.sh');
  assert.equal(composite.expandTilde('node'), 'node');
  assert.equal(composite.expandTilde('~user/script.sh'), '~user/script.sh',
    'other-user home dirs are not resolved');
  assert.equal(composite.expandTilde('a~/b'), 'a~/b',
    'only a leading ~ expands, not a mid-string ~');
});


test('a corrupt settings.json is backed up, never silently overwritten', (t) => {
  // readJson collapsed ENOENT and SyntaxError into the same `null`, so
  // `readJson(settingsPath()) || {}` handed install() an empty object and the
  // next atomic write replaced the whole file. One trailing comma — the most
  // common hand-edit slip — cost the user their model / env / permissions /
  // enabledPlugins and their own hooks, with no copy left anywhere.
  const homeDir = mkHome(t);
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  const corrupt = [
    '{',
    '  "model": "opus",',
    '  "env": { "FOO": "bar" },',
    '  "permissions": { "allow": ["Bash(ls:*)"] },',
    '  "enabledPlugins": { "code-graph-mcp@code-graph-mcp": true },',   // <- trailing comma below
    '  "hooks": { "SessionStart": [{ "hooks": [{ "type": "command", "command": "echo mine" }] }] },',
    '}',
  ].join('\n');
  fs.mkdirSync(path.dirname(settingsPath), { recursive: true });
  fs.writeFileSync(settingsPath, corrupt);

  runScript(homeDir, lifecycleCli, ['install']);

  const backups = fs.readdirSync(path.dirname(settingsPath))
    .filter((f) => f.startsWith('settings.json.corrupt-'));
  assert.equal(backups.length, 1,
    `install() must preserve an unparseable settings.json before rebuilding it (found: ${backups.join(', ')})`);
  assert.equal(
    fs.readFileSync(path.join(path.dirname(settingsPath), backups[0]), 'utf8'),
    corrupt,
    'the backup must be the original bytes, verbatim');

  // Backing up is only half the contract — the install must then actually
  // happen. Without this, a regression where install() backs up and then bails
  // (exactly the shape of the new refuse-path) would still pass the assertion
  // above, and the plugin would be silently inert.
  const rebuilt = readJson(settingsPath);
  assert.match(rebuilt.statusLine.command, /statusline-composite\.js/,
    'the rebuilt settings.json must carry the composite statusLine');
  assert.ok(rebuilt.hooks && Object.keys(rebuilt.hooks).length > 0,
    'the rebuilt settings.json must carry the plugin hooks');
});

// `chmod 000` is meaningless for uid 0 — root reads anything, so the refuse
// path would never be exercised and the test would assert the opposite of the
// truth. Skip loudly rather than silently pass.
const asRoot = typeof process.getuid === 'function' && process.getuid() === 0;

test('an UNREADABLE settings.json is left untouched, not rebuilt', { skip: asRoot && 'running as root' }, (t) => {
  // The first version of this fix split ENOENT from SyntaxError and mapped every
  // OTHER read error to "missing" — so a settings.json the process cannot read
  // was still rebuilt from `{}`, silently, with no backup possible. Real trigger:
  // one `sudo claude` leaves ~/.claude/settings.json root-owned 0600, and the
  // next ordinary SessionStart destroys it. `missing` must mean ENOENT alone.
  const homeDir = mkHome(t);
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  writeJson(settingsPath, {
    model: 'opus',
    env: { FOO: 'bar' },
    hooks: { SessionStart: [{ hooks: [{ type: 'command', command: 'echo mine' }] }] },
  });
  const original = fs.readFileSync(settingsPath);
  fs.chmodSync(settingsPath, 0o000);
  t.after(() => { try { fs.chmodSync(settingsPath, 0o600); } catch { /* already gone */ } });

  let exitCode = 0;
  try {
    runScript(homeDir, lifecycleCli, ['install']);
  } catch (err) {
    exitCode = err.status;
  }

  fs.chmodSync(settingsPath, 0o600);
  assert.deepEqual(fs.readFileSync(settingsPath), original,
    'an unreadable settings.json must survive byte-for-byte');
  const backups = fs.readdirSync(path.dirname(settingsPath))
    .filter((f) => f.startsWith('settings.json.corrupt-'));
  assert.deepEqual(backups, [],
    'no backup is possible when the file cannot be read — and none should be faked');
  assert.notEqual(exitCode, 0,
    'refusing to install must not report success (`install && …` chains read exit 0 as done)');
});

test('an EMPTY settings.json is treated as absent, not corrupt', (t) => {
  // A zero-byte file is what a crash mid-write leaves behind. It carries nothing
  // worth preserving, so classifying it corrupt would litter ~/.claude with an
  // empty `.corrupt-*` copy on the way to the same rebuild.
  const homeDir = mkHome(t);
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  fs.mkdirSync(path.dirname(settingsPath), { recursive: true });
  fs.writeFileSync(settingsPath, '  \n');

  runScript(homeDir, lifecycleCli, ['install']);

  const backups = fs.readdirSync(path.dirname(settingsPath))
    .filter((f) => f.startsWith('settings.json.corrupt-'));
  assert.deepEqual(backups, [], 'an empty file has nothing to back up');
  assert.match(readJson(settingsPath).statusLine.command, /statusline-composite\.js/,
    'and it must still install normally');
});

test('update() refuses an unusable settings.json too, not just install()', (t) => {
  // install() and update() are separate entry points onto the same destructive
  // write. Fixing one and testing only that one is how the pair drifts.
  const homeDir = mkHome(t);
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  const corrupt = '{ "model": "opus", }';
  fs.mkdirSync(path.dirname(settingsPath), { recursive: true });
  fs.writeFileSync(settingsPath, corrupt);

  runScript(homeDir, lifecycleCli, ['update']);

  const backups = fs.readdirSync(path.dirname(settingsPath))
    .filter((f) => f.startsWith('settings.json.corrupt-'));
  assert.equal(backups.length, 1, 'update() must back up before rebuilding');
  assert.equal(
    fs.readFileSync(path.join(path.dirname(settingsPath), backups[0]), 'utf8'),
    corrupt,
    'update()`s backup must also be the original bytes');
});

test('a valid settings.json is never treated as corrupt', (t) => {
  // Negative control for the guard above: the backup path must not fire on the
  // normal case, or every SessionStart would litter ~/.claude with copies.
  const homeDir = mkHome(t);
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  writeJson(settingsPath, { model: 'opus', env: { FOO: 'bar' } });

  runScript(homeDir, lifecycleCli, ['install']);

  const backups = fs.readdirSync(path.dirname(settingsPath))
    .filter((f) => f.startsWith('settings.json.corrupt-'));
  assert.deepEqual(backups, [], 'a parseable settings.json must not be backed up');
  const after = readJson(settingsPath);
  assert.equal(after.model, 'opus', 'user keys survive a normal install');
  assert.deepEqual(after.env, { FOO: 'bar' });
});

test('a BOM-prefixed but otherwise valid settings.json is not treated as corrupt', (t) => {
  // A UTF-8 BOM is JS whitespace, so `.trim()` strips it, but `JSON.parse`
  // rejects it. PowerShell 5.1's Out-File / Set-Content emit a BOM by default,
  // so a Windows user editing settings.json by hand gets a valid file that this
  // reader would call corrupt — back it up and rebuild the live one.
  const homeDir = mkHome(t);
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  fs.mkdirSync(path.dirname(settingsPath), { recursive: true });
  fs.writeFileSync(settingsPath, '\uFEFF' + JSON.stringify({ model: 'opus', env: { FOO: 'bar' } }, null, 2));

  runScript(homeDir, lifecycleCli, ['install']);

  const backups = fs.readdirSync(path.dirname(settingsPath))
    .filter((f) => f.startsWith('settings.json.corrupt-'));
  assert.deepEqual(backups, [], 'a BOM is not corruption');
  const after = JSON.parse(fs.readFileSync(settingsPath, 'utf8').replace(/^\uFEFF/, ''));
  assert.equal(after.model, 'opus', 'user keys survive');
  assert.deepEqual(after.env, { FOO: 'bar' });
  assert.match(after.statusLine.command, /statusline-composite\.js/);
});

test('a rebuilt settings.json is reported as destructive, not as a clean repair', (t) => {
  // healthCheck() auto-calls install() for any issue, and for a BACKUPABLE
  // corrupt file install() succeeds — so the rescan came back clean and doctor
  // printed `Hooks ✅ 1 issue(s) auto-repaired` for a run that had just moved
  // the user's model / env / permissions into a `.corrupt-*` file it never
  // named. The repair is fine; describing it as clean is not.
  const homeDir = mkHome(t);
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  fs.mkdirSync(path.dirname(settingsPath), { recursive: true });
  fs.writeFileSync(settingsPath, '{ "model": "opus", "env": { "FOO": "bar" }, }');

  // doctor exits non-zero whenever it leaves any issue unresolved (here: the
  // sandbox has no cargo toolchain), so capture stdout rather than let
  // execFileSync throw on an exit code unrelated to what is under test.
  let out = '';
  try { out = runScript(homeDir, path.join(__dirname, 'doctor.js'), []); }
  catch (err) { out = (err.stdout || '').toString(); }

  const hooksRow = out.split('\n').find((l) => /^\s*Hooks\s/.test(l)) || '';
  assert.doesNotMatch(hooksRow, /auto-repaired/,
    `a rebuild that replaced the user's settings must not read as a clean repair: ${hooksRow}`);
  assert.match(hooksRow, /REBUILT/, `must say the file was rebuilt: ${hooksRow}`);
  assert.match(hooksRow, /settings\.json\.corrupt-/,
    `must name the backup holding the user's original: ${hooksRow}`);

  // And the backup really is there, holding the original bytes.
  const backups = fs.readdirSync(path.dirname(settingsPath))
    .filter((f) => f.startsWith('settings.json.corrupt-'));
  assert.equal(backups.length, 1);
  assert.match(fs.readFileSync(path.join(path.dirname(settingsPath), backups[0]), 'utf8'), /"model"/);
});

test('doctor does not claim missing hooks when settings.json is unreadable', { skip: asRoot && 'running as root' }, (t) => {
  // Sibling read left on the old collapsed-`null` idiom: an unusable file became
  // `{}`, which has no hooks, so the coverage probe reported "missing 6/6
  // settings.json entries" — a confident, wrong diagnosis in the SAME table as
  // the correct "settings.json unusable" row.
  const homeDir = mkHome(t);
  const claudeDir = path.join(homeDir, '.claude');
  const settingsPath = path.join(claudeDir, 'settings.json');
  fs.mkdirSync(claudeDir, { recursive: true });
  fs.writeFileSync(settingsPath, '{ "model": "opus", }');
  fs.chmodSync(claudeDir, 0o555);
  t.after(() => { try { fs.chmodSync(claudeDir, 0o755); } catch { /* gone */ } });

  let out = '';
  try { out = runScript(homeDir, path.join(__dirname, 'doctor.js'), []); }
  catch (err) { out = (err.stdout || '').toString(); }
  fs.chmodSync(claudeDir, 0o755);

  const covRow = out.split('\n').find((l) => /^\s*Hook coverage\s/.test(l)) || '';
  assert.ok(covRow, `the Hook coverage row must exist — a probe that cannot run is itself a finding.\n${out}`);
  assert.doesNotMatch(covRow, /missing \d+\/\d+/,
    `coverage is not determinable from an unreadable file, so it must not be reported as missing: ${covRow}`);
  assert.match(covRow, /not determinable/, covRow);
});

test('doctor --check-only never writes settings.json', (t) => {
  // `--check-only` is a SHIPPED read-only contract (CHANGELOG v0.82.1: "it never
  // reaches runRepairs"). The write was never in runRepairs: runDiagnostics
  // called healthCheck(), which calls install(), which REBUILDS an unusable
  // settings.json. Measured: 36 B -> 3318 B with the model key gone, while the
  // report said "Run without --check-only to fix."
  for (const content of ['{ "model": "opus", }', '[1,2,3]', 'null', '"hello"']) {
    const homeDir = mkHome(t);
    const settingsPath = path.join(homeDir, '.claude', 'settings.json');
    fs.mkdirSync(path.dirname(settingsPath), { recursive: true });
    fs.writeFileSync(settingsPath, content);
    const before = fs.readFileSync(settingsPath);

    try { runScript(homeDir, path.join(__dirname, 'doctor.js'), ['--check-only']); }
    catch { /* non-zero exit is expected when issues exist */ }

    assert.deepEqual(fs.readFileSync(settingsPath), before,
      `--check-only rewrote settings.json holding ${content}`);
    assert.deepEqual(
      fs.readdirSync(path.dirname(settingsPath)).filter((f) => f.startsWith('settings.json.corrupt-')),
      [],
      `--check-only created a backup for ${content} — it must not write at all`);
  }
});

test('SessionStart reports on STDOUT when it rebuilds settings.json', (t) => {
  // The honesty fix was wired into `doctor` — which a user runs deliberately —
  // and not into syncLifecycleConfig, which runs on EVERY SessionStart and calls
  // install() seven times. lifecycle.js logs the rebuild to stderr, which a
  // SessionStart hook discards, so from the user's side their model / env /
  // permissions vanished with no message at all.
  const homeDir = mkHome(t);
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  fs.mkdirSync(path.dirname(settingsPath), { recursive: true });
  fs.writeFileSync(settingsPath, '{"model":"opus","env":{"FOO":"bar"},}');

  let out = '';
  try {
    out = runScript(homeDir, path.join(__dirname, 'session-init.js'), [], {
      input: JSON.stringify({ source: 'startup' }),
    });
  } catch (err) { out = (err.stdout || '').toString(); }

  assert.match(out, /REBUILT/,
    `SessionStart must say so on stdout when it rebuilds settings.json.\n${out}`);
  assert.match(out, /settings\.json\.corrupt-/,
    `and must name the backup holding the original.\n${out}`);
});

// ── Contract audit 2026-07-27: shapes and permissions that reported success ──

test('a non-object `hooks` value is replaced, not silently discarded', (t) => {
  // `settings.hooks || {}` accepted an ARRAY, then assigned named properties
  // onto it — which JSON.stringify drops. install printed "settings=true" and
  // health printed "OK — all paths valid" while `"hooks": []` came back out with
  // zero of our six hooks registered: total, reported-as-success inertness.
  for (const badShape of [[], 'nonsense', 42, null]) {
    const homeDir = mkHome(t);
    const settingsPath = path.join(homeDir, '.claude', 'settings.json');
    writeJson(settingsPath, { model: 'opus', hooks: badShape });

    const out = runScript(homeDir, lifecycleCli, ['install']);
    assert.match(out, /Installed/, `install should succeed for hooks:${JSON.stringify(badShape)}`);

    const after = readJson(settingsPath);
    assert.equal(after.model, 'opus', 'unrelated user keys preserved');
    assert.equal(typeof after.hooks, 'object', 'hooks is an object');
    assert.ok(!Array.isArray(after.hooks), 'hooks is not an array');
    const registered = Object.values(after.hooks)
      .filter(Array.isArray)
      .reduce((n, entries) => n + entries.length, 0);
    assert.ok(registered > 0,
      `hooks:${JSON.stringify(badShape)} must not yield an install that registers nothing while reporting success`);
  }
});

test('an unwritable settings.json is reported and does not stamp the manifest', (t) => {
  const homeDir = mkHome(t);
  const claudeDir = path.join(homeDir, '.claude');
  const settingsPath = path.join(claudeDir, 'settings.json');
  writeJson(settingsPath, { model: 'opus' });
  const before = fs.readFileSync(settingsPath);
  const manifestBefore = path.join(homeDir, '.cache', 'code-graph', 'install-manifest.json');

  fs.chmodSync(claudeDir, 0o555);          // readable, not writable

  let stdout = '', stderr = '', code = 0;
  try {
    stdout = execFileSync(process.execPath, [lifecycleCli, 'install'], {
      cwd: repoRoot, env: { ...process.env, HOME: homeDir, USERPROFILE: homeDir }, stdio: ['pipe', 'pipe', 'pipe'],
    }).toString();
  } catch (err) {
    code = err.status; stdout = err.stdout.toString(); stderr = err.stderr.toString();
  }
  // Restore here, not in t.after: mkHome's cleanup hook runs first and would
  // fail to rm a 0555 directory, turning a passing test into a hook error.
  fs.chmodSync(claudeDir, 0o755);

  assert.notEqual(code, 0, 'must not exit 0 — a chained `install && …` would read it as success');
  assert.match(stderr, /\[code-graph\] cannot write/, 'names the real cause, not a raw fs stack');
  assert.doesNotMatch(stdout, /^Installed/m, 'must not claim it installed');
  assert.deepEqual(fs.readFileSync(settingsPath), before, 'settings byte-identical');
  assert.equal(fs.existsSync(manifestBefore), false,
    'no manifest stamp — a current-version manifest would make the next run skip the retry');
});

test('cache teardown preserves a registry that still names other projects', (t) => {
  const homeDir = mkHome(t);
  const registry = path.join(homeDir, '.cache', 'code-graph', 'adopted-projects.json');
  const binary = path.join(homeDir, '.cache', 'code-graph', 'bin', 'code-graph-mcp');
  fs.mkdirSync(path.dirname(binary), { recursive: true });
  fs.writeFileSync(binary, 'x'.repeat(1024));
  writeJson(registry, ['/repo/one', '/repo/two']);

  const out = execFileSync(process.execPath, ['-e', `
    process.env.HOME = ${JSON.stringify(homeDir)};
    process.env.USERPROFILE = ${JSON.stringify(homeDir)};
    const { removeCacheResidue } = require(${JSON.stringify(lifecycleCli)});
    console.log(JSON.stringify({ removed: removeCacheResidue() }));
  `], { env: { ...process.env, HOME: homeDir, USERPROFILE: homeDir }, cwd: repoRoot }).toString();

  assert.equal(JSON.parse(out.trim().split('\n').pop()).removed, true);
  assert.equal(fs.existsSync(binary), false, 'the ~40MB binary is still reclaimed');
  assert.equal(fs.existsSync(registry), true,
    'the registry is the ONLY record of which repos carry a managed CLAUDE.md block — ' +
    'wiping it strands every one of them with nothing left that knows where they are');
  assert.deepEqual(readJson(registry), ['/repo/one', '/repo/two']);
});

test('cache teardown leaves nothing behind when the registry is already empty', (t) => {
  // Negative control for the test above: preservation must not become residue.
  const homeDir = mkHome(t);
  const cacheDir = path.join(homeDir, '.cache', 'code-graph');
  fs.mkdirSync(path.join(cacheDir, 'bin'), { recursive: true });
  fs.writeFileSync(path.join(cacheDir, 'bin', 'code-graph-mcp'), 'x');
  writeJson(path.join(cacheDir, 'adopted-projects.json'), []);

  execFileSync(process.execPath, ['-e', `
    process.env.HOME = ${JSON.stringify(homeDir)};
    process.env.USERPROFILE = ${JSON.stringify(homeDir)};
    require(${JSON.stringify(lifecycleCli)}).removeCacheResidue();
  `], { env: { ...process.env, HOME: homeDir, USERPROFILE: homeDir }, cwd: repoRoot });

  assert.equal(fs.existsSync(cacheDir), false,
    'an empty registry strands nothing, so re-creating the dir would just be new residue');
});

test('the .corrupt-* backup is byte-identical, including non-UTF-8 bytes', (t) => {
  // Round-6 F4: the Buffer fix was correct but unguarded — every corrupt fixture
  // in this file was pure ASCII, so re-introducing the lossy `readFileSync(p,
  // 'utf8')` broke nothing. That is the same blind spot the CHANGELOG records
  // about the test this one replaces.
  const homeDir = mkHome(t);
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  fs.mkdirSync(path.dirname(settingsPath), { recursive: true });
  // Invalid UTF-8 (0xff, 0xfe, a bare latin-1 0xe9) inside an unparseable file.
  const original = Buffer.concat([
    Buffer.from('{"model":"opus","note":"'),
    Buffer.from([0xff, 0xfe, 0x20]),
    Buffer.from('caf'),
    Buffer.from([0xe9]),
    Buffer.from('","env":{"A":"b"},}'),
  ]);
  fs.writeFileSync(settingsPath, original);

  runScript(homeDir, lifecycleCli, ['install']);

  const backups = fs.readdirSync(path.dirname(settingsPath))
    .filter((f) => f.startsWith('settings.json.corrupt-'));
  assert.equal(backups.length, 1, 'exactly one backup was taken');
  const backup = fs.readFileSync(path.join(path.dirname(settingsPath), backups[0]));
  assert.deepEqual(backup, original,
    'the backup is the user\'s ONLY copy — a UTF-8 round-trip turns every invalid ' +
    'byte into U+FFFD and the original is then overwritten, so it must be copied ' +
    'as bytes, never as a decoded string');
  assert.equal(backup.length, original.length, 'no re-encoding growth');
});

test('settings.json with a non-UTF-8 byte is preserved before the rewrite', (t) => {
  // The byte-exactness work above covered only the CORRUPT branch. A file that
  // is VALID JSON but carries an invalid UTF-8 byte — a latin-1/cp1252 byte in
  // a path, which is what a non-ASCII username on a legacy code page produces —
  // classified as clean, so no backup was made. The object then round-tripped
  // `toString('utf8')` -> JSON.parse -> JSON.stringify -> atomic write, and
  // every bad byte became U+FFFD in the user's live file, permanently, with
  // nothing on stderr. Detection is a re-encode comparison: a lossless decode
  // round-trips to the original bytes, a lossy one cannot.
  const homeDir = mkHome(t);
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  const original = Buffer.concat([
    Buffer.from('{\n  "model": "opus",\n  "env": { "MY_PATH": "/home/andr', 'utf8'),
    Buffer.from([0xe9]),                       // latin-1 'é', not valid UTF-8
    Buffer.from('/bin" },\n  "permissions": { "allow": ["Bash(ls:*)"] }\n}\n', 'utf8'),
  ]);
  fs.mkdirSync(path.dirname(settingsPath), { recursive: true });
  fs.writeFileSync(settingsPath, original);

  runScript(homeDir, lifecycleCli, ['install']);

  const backups = fs.readdirSync(path.dirname(settingsPath))
    .filter((f) => f.startsWith('settings.json.corrupt-'));
  assert.equal(backups.length, 1,
    `install() must preserve the true bytes before a rewrite that cannot round-trip them (found: ${backups.join(', ')})`);
  assert.ok(
    fs.readFileSync(path.join(path.dirname(settingsPath), backups[0])).equals(original),
    'the backup must be byte-identical — a U+FFFD transcription is exactly what it exists to prevent');

  // And the install must still have happened: a regression that backs up and
  // then bails would satisfy the assertion above while leaving the plugin inert.
  const rebuilt = readJson(settingsPath);
  assert.equal(rebuilt.model, 'opus', 'the parsed value is usable and must be carried forward');
  assert.ok(rebuilt.hooks, 'install() must still register its hooks');
});

// ── Audit 2026-08-01: the two settings writers that bypassed the guarded pair ──
//
// install() and update() were fixed to read through readSettingsForWrite (the
// lossy-UTF8 detector + `.corrupt-*` backup) and write through tryWriteSettings.
// cleanupDisabledStatusline() and uninstall() kept the raw
// `readJson(settingsPath())` -> `writeJsonAtomic(settingsPath())` pair, so every
// protection the detector added was absent on exactly the paths that run when
// the user is DISABLING or REMOVING the plugin — the moment they are least
// likely to be watching, and the one where losing their model / env /
// permissions is least recoverable.

/** settings.json bytes that are valid JSON but cannot survive a UTF-8 round-trip. */
function lossySettings(extra) {
  return Buffer.concat([
    Buffer.from('{\n  "model": "opus",\n  "env": { "MY_PATH": "/home/andr', 'utf8'),
    Buffer.from([0xe9]),                       // latin-1 'é' — not valid UTF-8
    Buffer.from(`/bin" },\n${extra}\n}\n`, 'utf8'),
  ]);
}

function corruptBackupsIn(dir) {
  return fs.readdirSync(dir).filter((f) => f.startsWith('settings.json.corrupt-'));
}

test('uninstall() preserves non-UTF-8 bytes before rewriting settings.json', (t) => {
  const homeDir = mkHome(t);
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  const original = lossySettings('  "enabledPlugins": { "code-graph-mcp@code-graph-mcp": true }');
  fs.mkdirSync(path.dirname(settingsPath), { recursive: true });
  fs.writeFileSync(settingsPath, original);

  runScript(homeDir, lifecycleCli, ['uninstall']);

  const backups = corruptBackupsIn(path.dirname(settingsPath));
  assert.equal(backups.length, 1,
    `uninstall() must preserve the true bytes before a rewrite that cannot round-trip them (found: ${backups.join(', ')})`);
  assert.ok(fs.readFileSync(path.join(path.dirname(settingsPath), backups[0])).equals(original),
    'the backup is the user\'s only copy, so it must be byte-identical');
  // ...and the teardown must still have happened: backing up and then bailing
  // would satisfy the assertion above while leaving our config wired in.
  const after = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
  assert.equal(after.model, 'opus', 'unrelated user keys survive the teardown');
  assert.equal(Object.prototype.hasOwnProperty.call(after.enabledPlugins || {}, 'code-graph-mcp@code-graph-mcp'),
    false, 'and our enabledPlugins entry is gone');
});

/** A sandbox HOME in the disabled-but-still-wired state cleanupDisabledStatusline acts on. */
function inactiveHome(t, settingsBytes, { registry } = {}) {
  const homeDir = mkHome(t);
  const settingsPath = path.join(homeDir, '.claude', 'settings.json');
  fs.mkdirSync(path.dirname(settingsPath), { recursive: true });
  fs.writeFileSync(settingsPath, settingsBytes);
  // installed_plugins.json exists but does NOT list us => isPluginInactive.
  writeJson(path.join(homeDir, '.claude', 'plugins', 'installed_plugins.json'),
    { plugins: { 'someone-else@theirs': [{ scope: 'user' }] } });
  if (registry) {
    writeJson(path.join(homeDir, '.cache', 'code-graph', 'statusline-registry.json'), registry);
  }
  return { homeDir, settingsPath };
}

function callCleanup(homeDir) {
  return execFileSync(process.execPath, ['-e', `
    process.env.HOME = ${JSON.stringify(homeDir)};
    process.env.USERPROFILE = ${JSON.stringify(homeDir)};
    const r = require(${JSON.stringify(lifecycleCli)}).cleanupDisabledStatusline();
    console.log(JSON.stringify(r));
  `], { env: { ...process.env, HOME: homeDir, USERPROFILE: homeDir }, cwd: repoRoot, stdio: ['pipe', 'pipe', 'pipe'] }).toString();
}

test('cleanupDisabledStatusline() preserves non-UTF-8 bytes before rewriting settings.json', (t) => {
  const original = lossySettings(
    '  "statusLine": { "type": "command", "command": "node \\"/gone/statusline-composite.js\\"" }');
  const { homeDir, settingsPath } = inactiveHome(t, original);

  const out = callCleanup(homeDir);

  assert.equal(JSON.parse(out.trim().split('\n').pop()).cleaned, true, 'the teardown ran');
  const backups = corruptBackupsIn(path.dirname(settingsPath));
  assert.equal(backups.length, 1,
    `the disable path must preserve the bytes too (found: ${backups.join(', ')})`);
  assert.ok(fs.readFileSync(path.join(path.dirname(settingsPath), backups[0])).equals(original),
    'byte-identical, same contract as install()');
  const after = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
  assert.equal(after.model, 'opus', 'user keys survive');
  assert.equal(after.statusLine, undefined, 'and our composite was actually detached');
});

test('cleanupDisabledStatusline() takes no backup on the runs that change nothing', (t) => {
  // Negative control with teeth: this function runs on EVERY statusline render.
  // Reading it through readSettingsForWrite unconditionally would take a fresh
  // `.corrupt-*` copy of a lossy settings.json once per prompt — turning a data
  // -loss fix into a disk-filling one. The guarded read must happen only once a
  // write is certain.
  const original = lossySettings('  "statusLine": { "type": "command", "command": "node \\"/other/provider.js\\"" }');
  // No composite, no registry => isPluginInactive is false => nothing to do.
  const { homeDir, settingsPath } = inactiveHome(t, original);

  for (let i = 0; i < 3; i++) {
    assert.equal(JSON.parse(callCleanup(homeDir).trim().split('\n').pop()).cleaned, false);
  }

  assert.deepEqual(corruptBackupsIn(path.dirname(settingsPath)), [],
    'a render that changes nothing must not copy the file aside');
  assert.deepEqual(fs.readFileSync(settingsPath), original, 'and must not touch it at all');
});

test('the statusline renders instead of throwing when ~/.claude is read-only',
  { skip: asRoot && 'running as root' }, (t) => {
  // cleanupDisabledStatusline() ran at MODULE SCOPE in statusline.js with
  // nothing to catch it. On a read-only config dir (EROFS mount, a `sudo` that
  // left root ownership, restrictive umask) its registry/settings writes throw,
  // so the user's whole status line was replaced by a node stack trace — for a
  // cleanup they never asked for.
  const { homeDir } = inactiveHome(t,
    Buffer.from(JSON.stringify({
      model: 'opus',
      statusLine: { type: 'command', command: 'node "/gone/statusline-composite.js"' },
    }, null, 2)),
    // Two entries so the detach writes the registry file rather than unlinking it.
    { registry: [{ id: 'code-graph', command: 'node "/gone/statusline.js"', needsStdin: false },
                 { id: '_previous', command: 'echo prior', needsStdin: true }] });

  const claudeDir = path.join(homeDir, '.claude');
  const cacheDir = path.join(homeDir, '.cache', 'code-graph');
  const emptyCwd = path.join(homeDir, 'scratch');   // no .code-graph anywhere above it
  fs.mkdirSync(emptyCwd, { recursive: true });
  fs.chmodSync(cacheDir, 0o555);
  fs.chmodSync(claudeDir, 0o555);

  let code = 0, stderr = '';
  try {
    execFileSync(process.execPath, [path.join(__dirname, 'statusline.js')], {
      cwd: emptyCwd, env: { ...process.env, HOME: homeDir, USERPROFILE: homeDir }, stdio: ['pipe', 'pipe', 'pipe'],
    });
  } catch (err) {
    code = err.status;
    stderr = (err.stderr || '').toString();
  }
  // Restore before mkHome's cleanup hook, which cannot rm a 0555 directory.
  fs.chmodSync(claudeDir, 0o755);
  fs.chmodSync(cacheDir, 0o755);

  assert.equal(code, 0, `the statusline must not fail on a read-only config dir\n${stderr}`);
  assert.doesNotMatch(stderr, /EACCES|EROFS|at Object\.<anonymous>/,
    `a housekeeping failure must not surface as a stack trace\n${stderr}`);
});

test('scanForBrokenPaths reads a hook command that names the node interpreter by path', (t) => {
  // Six sites extracted the script path from a hook command; five used a strict
  // `/node\s+"([^"]+)"/` that requires the literal word `node` followed by
  // whitespace. A Windows install writes `"C:\Program Files\nodejs\node.exe"
  // "C:\…\hook.js"`, which that pattern cannot read — so the health scan found
  // no path, reported no issue, and a completely dead hook registration passed
  // as healthy. hookCmdScript (already used by compositeSlotIsStale and
  // surveyHookCoverage) handles both spellings; this is the sixth site adopting it.
  const homeDir = mkHome(t);
  const { SETTINGS_HOOK_DESC } = require('./lifecycle');
  const deadScript = 'C:\\plugins\\code-graph-mcp\\scripts\\user-prompt-context.js';
  writeJson(path.join(homeDir, '.claude', 'settings.json'), {
    hooks: {
      UserPromptSubmit: [{
        description: SETTINGS_HOOK_DESC.userPromptSubmit,
        matcher: '',
        hooks: [{ type: 'command', command: `"C:\\Program Files\\nodejs\\node.exe" "${deadScript}"` }],
      }],
    },
  });

  const out = execFileSync(process.execPath, ['-e', `
    process.env.HOME = ${JSON.stringify(homeDir)};
    process.env.USERPROFILE = ${JSON.stringify(homeDir)};
    console.log(JSON.stringify(require(${JSON.stringify(lifecycleCli)}).scanForBrokenPaths()));
  `], { env: { ...process.env, HOME: homeDir, USERPROFILE: homeDir }, cwd: repoRoot }).toString();

  const issues = JSON.parse(out.trim().split('\n').pop());
  assert.deepEqual(issues.filter((i) => i.type === 'hook'),
    [{ type: 'hook', event: 'UserPromptSubmit', path: deadScript }],
    'a dead hook path must be reported whichever way the command names node');
});

test('SessionStart reclaims aged cgTmpDir residue', (t) => {
  // Nothing ever deleted what cgTmpDir() collects — cooldown flags,
  // read-fanout state, and interrupted `update-*` download staging. Measured on
  // a working dev box before this landed: 281 entries, 232 of them over a day
  // old. The prune has to hang off a path that actually runs, and SessionStart
  // is the only one guaranteed to.
  const homeDir = mkHome(t);
  const tmpRoot = path.join(homeDir, 'tmp');
  const cgTmp = path.join(tmpRoot, 'code-graph-mcp');
  fs.mkdirSync(cgTmp, { recursive: true });

  const aged = path.join(cgTmp, '.code-graph-bash-ancient');
  const recent = path.join(cgTmp, '.code-graph-bash-recent');
  const agedStaging = path.join(cgTmp, 'update-1700000000000');
  fs.writeFileSync(aged, '');
  fs.writeFileSync(recent, '');
  fs.mkdirSync(agedStaging);
  fs.writeFileSync(path.join(agedStaging, 'payload.tar.gz'), 'x'.repeat(4096));
  const twoDaysAgo = (Date.now() - 2 * 24 * 3600 * 1000) / 1000;
  fs.utimesSync(aged, twoDaysAgo, twoDaysAgo);
  fs.utimesSync(agedStaging, twoDaysAgo, twoDaysAgo);

  try {
    execFileSync(process.execPath, [path.join(__dirname, 'session-init.js')], {
      cwd: homeDir,
      env: { ...process.env, HOME: homeDir, USERPROFILE: homeDir, TMPDIR: tmpRoot, TMP: tmpRoot, TEMP: tmpRoot,
             CODE_GRAPH_QUIET_HOOKS: '1' },
      input: JSON.stringify({ source: 'startup' }),
      stdio: ['pipe', 'pipe', 'pipe'],
    });
  } catch { /* SessionStart is best-effort; the prune must run regardless */ }

  assert.equal(fs.existsSync(aged), false, 'an aged cooldown flag must be reclaimed');
  assert.equal(fs.existsSync(agedStaging), false, 'and so must an aged update-* staging dir');
  assert.equal(fs.existsSync(recent), true, 'a flag inside the window must survive');
});

test('uninstall removes the shared tmp dir', (t) => {
  // The prune that keeps cgTmpDir() bounded stops running the moment the hooks
  // are gone, so an uninstall that leaves it behind leaves residue with no
  // remaining owner.
  const homeDir = mkHome(t);
  const tmpRoot = path.join(homeDir, 'tmp');
  const cgTmp = path.join(tmpRoot, 'code-graph-mcp');
  fs.mkdirSync(cgTmp, { recursive: true });
  fs.writeFileSync(path.join(cgTmp, '.code-graph-bash-recent'), '');   // fresh: prune would keep it

  execFileSync(process.execPath, [lifecycleCli, 'uninstall'], {
    cwd: repoRoot,
    env: { ...process.env, HOME: homeDir, USERPROFILE: homeDir, TMPDIR: tmpRoot, TMP: tmpRoot, TEMP: tmpRoot },
    stdio: ['pipe', 'pipe', 'pipe'],
  });

  assert.equal(fs.existsSync(cgTmp), false,
    'uninstall must reclaim the tmp dir, including entries the age-based prune would keep');
});
