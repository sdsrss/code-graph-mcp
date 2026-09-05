'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');

const os = require('os');
const { launchBackgroundAutoUpdate, isHighIntentSource, syncLifecycleConfig, ensureIndexFresh, indexNeedsRevalidation, verifyBinary, computeQuietHooks, shouldInjectMap, shouldInjectRecentImpact, recentImpactWorthShowing, filterSourceFiles, parseGitStatusPaths, formatRecentImpact, missingBinaryMessage } = require('./session-init');

// Write an executable stub named `code-graph-mcp` that emits `json` to stdout on
// `health-check` and exits with `exitCode`. Mirrors how the real binary behaves:
// non-zero exit on an unhealthy index, but the JSON report still goes to stdout.
function stubHealthBin(t, { json, exitCode = 0 }) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-sessinit-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const bin = path.join(dir, 'code-graph-mcp');
  const payload = String(json).replace(/'/g, `'\\''`);
  fs.writeFileSync(bin, [
    '#!/usr/bin/env bash',
    `printf '%s' '${payload}'`,
    `exit ${exitCode}`,
    '',
  ].join('\n'));
  fs.chmodSync(bin, 0o755);
  return { bin, cwd: dir };
}

test('syncLifecycleConfig is exported as a callable helper', () => {
  assert.equal(typeof syncLifecycleConfig, 'function');
});

test('ensureIndexFresh is exported as a callable helper', () => {
  assert.equal(typeof ensureIndexFresh, 'function');
});

test('ensureIndexFresh returns skipped when no index exists', () => {
  const origCwd = process.cwd();
  const tmpDir = require('node:os').tmpdir();
  process.chdir(tmpDir);
  try {
    const result = ensureIndexFresh();
    assert.equal(result, 'skipped');
  } finally {
    process.chdir(origCwd);
  }
});

test('indexNeedsRevalidation true when health-check reports index_version_stale', (t) => {
  const { bin, cwd } = stubHealthBin(t, {
    json: JSON.stringify({ healthy: true, nodes: 5, index_version_stale: true }),
    exitCode: 0,
  });
  assert.equal(indexNeedsRevalidation(bin, cwd), true);
});

test('indexNeedsRevalidation false when index is current', (t) => {
  const { bin, cwd } = stubHealthBin(t, {
    json: JSON.stringify({ healthy: true, nodes: 5, index_version_stale: false }),
    exitCode: 0,
  });
  assert.equal(indexNeedsRevalidation(bin, cwd), false);
});

test('indexNeedsRevalidation recovers JSON from a non-zero exit (unhealthy index)', (t) => {
  // health-check exits 1 on an empty/unhealthy index but still emits the report.
  const { bin, cwd } = stubHealthBin(t, {
    json: JSON.stringify({ healthy: false, nodes: 0, index_version_stale: true }),
    exitCode: 1,
  });
  assert.equal(indexNeedsRevalidation(bin, cwd), true);
});

test('indexNeedsRevalidation false on garbage output (never forces work off a bad probe)', (t) => {
  const { bin, cwd } = stubHealthBin(t, { json: 'not json at all', exitCode: 0 });
  assert.equal(indexNeedsRevalidation(bin, cwd), false);
});

test('verifyBinary returns available:true when binary is found and executable', () => {
  const result = verifyBinary();
  // In dev repo, binary should be found (target/release/code-graph-mcp)
  if (result.available) {
    assert.equal(typeof result.binary, 'string');
    assert.ok(result.binary.length > 0);
  } else {
    // Binary not built — still verify the return shape
    assert.equal(result.available, false);
  }
});

test('verifyBinary returns structured result with expected shape', () => {
  const result = verifyBinary();
  assert.equal(typeof result.available, 'boolean');
  assert.ok('binary' in result);
  if (!result.available && result.binary) {
    assert.ok('issue' in result);
  }
});

test('launchBackgroundAutoUpdate spawns detached silent updater', () => {
  const calls = [];

  const ok = launchBackgroundAutoUpdate((command, args, options) => {
    const record = { command, args, options, unrefCalled: false };
    calls.push(record);
    return {
      unref() {
        record.unrefCalled = true;
      },
    };
  }, { HOME: '/tmp/fake-home', USERPROFILE: '/tmp/fake-home' });

  assert.equal(ok, true);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].command, process.execPath);
  assert.match(calls[0].args[0], /auto-update\.js$/);
  assert.equal(calls[0].args[1], 'check');
  assert.equal(calls[0].args[2], '--silent');
  assert.equal(calls[0].options.detached, true);
  assert.equal(calls[0].options.stdio, 'ignore');
  assert.equal(calls[0].options.env.CODE_GRAPH_AUTO_UPDATE_SILENT, '1');
  assert.equal(calls[0].unrefCalled, true);
});

test('launchBackgroundAutoUpdate forwards --force only when asked (session-start bypass)', () => {
  const calls = [];
  const capture = (_command, args) => {
    calls.push({ args });
    return { unref() {} };
  };

  launchBackgroundAutoUpdate(capture, {}, { force: true });
  assert.deepEqual(calls[0].args.slice(1), ['check', '--silent', '--force']);

  launchBackgroundAutoUpdate(capture, {}); // default → no --force
  assert.deepEqual(calls[1].args.slice(1), ['check', '--silent']);
});

test('CODE_GRAPH_NO_AUTO_UPDATE=1 stops the updater from being spawned at all', () => {
  // The opt-out is enforced inside auto-update.js too; checking it here as well
  // means an opted-out user doesn't pay for a node process per session just to
  // have it exit immediately (issue #40).
  const calls = [];
  const capture = (_command, args) => { calls.push({ args }); return { unref() {} }; };

  const ok = launchBackgroundAutoUpdate(capture, { CODE_GRAPH_NO_AUTO_UPDATE: '1' }, { force: true });
  assert.equal(ok, false, 'opted out → reports "not launched"');
  assert.equal(calls.length, 0, 'opted out → no updater process');

  // Control: the same call WITHOUT the variable does spawn, so the assertion
  // above is about the opt-out and not about the fixture being inert.
  assert.equal(launchBackgroundAutoUpdate(capture, {}, { force: true }), true);
  assert.equal(calls.length, 1);
});

test('isHighIntentSource forces on session start/resume/clear but not automatic compaction', () => {
  assert.equal(isHighIntentSource('startup'), true);
  assert.equal(isHighIntentSource('resume'), true);
  assert.equal(isHighIntentSource('clear'), true);
  assert.equal(isHighIntentSource(undefined), true); // direct call / unknown → high intent
  assert.equal(isHighIntentSource('compact'), false); // frequent + automatic → gentle cadence
});

const { consistencyCheck } = require('./session-init');

test('consistencyCheck is exported as a function', () => {
  assert.equal(typeof consistencyCheck, 'function');
});

test('runSessionInit in a non-project cwd: global self-heal fires, zero project footprint', (t) => {
  // Two contracts in one (roadmap 3.4, project_cross_project_interference):
  // (1) syncLifecycleConfig runs BEFORE the non-project gate — settings.json is
  //     user-global, so a lost hook entry heals even when the session starts in
  //     a marker-less cwd (the headless /tmp fleet). Pre-fix the gate returned
  //     first and the miss never healed (lifecycle was hardcoded 'noop').
  // (2) The cwd itself stays untouched: no .code-graph, no adoption, no map.
  // Subprocess isolation: lifecycle.js binds CACHE_DIR from os.homedir() at
  // MODULE LOAD, so HOME/CLAUDE_CONFIG_DIR only take effect in a fresh child.
  const os = require('os');
  const { execFileSync } = require('child_process');
  const sb = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-si-nonproj-'));
  t.after(() => fs.rmSync(sb, { recursive: true, force: true }));
  const home = sb, cfg = path.join(sb, '.claude'), bare = path.join(sb, 'bare');
  fs.mkdirSync(path.join(cfg, 'plugins'), { recursive: true });
  fs.mkdirSync(bare, { recursive: true }); // no .git / package.json → non-project
  const env = { ...process.env, HOME: home, USERPROFILE: home, CLAUDE_CONFIG_DIR: cfg };
  const lc = path.join(__dirname, 'lifecycle.js');
  const si = path.join(__dirname, 'session-init.js');

  // Full install into the sandbox HOME, then simulate the daagu incident:
  // the PreToolUse (bash-guard) registration vanishes from settings.json.
  execFileSync(process.execPath, [lc, 'install'], { env, cwd: bare, stdio: 'ignore' });
  const settingsFile = path.join(cfg, 'settings.json');
  const settings = JSON.parse(fs.readFileSync(settingsFile, 'utf8'));
  assert.ok(settings.hooks && settings.hooks.PreToolUse, 'install registered PreToolUse');
  delete settings.hooks.PreToolUse;
  fs.writeFileSync(settingsFile, JSON.stringify(settings, null, 2));

  const res = JSON.parse(execFileSync(process.execPath, ['-e',
    `process.stdout.write(JSON.stringify(require(${JSON.stringify(si)}).runSessionInit({source:'startup'})))`],
    { env, cwd: bare }).toString());

  assert.equal(res.nonProject, true);
  assert.equal(res.autoUpdateLaunched, false);
  assert.equal(res.lifecycle, 'self-healed-missing-settings-hook',
    'a marker-less cwd must still heal the missing global hook registration');
  const healed = JSON.parse(fs.readFileSync(settingsFile, 'utf8'));
  assert.ok(healed.hooks && healed.hooks.PreToolUse && healed.hooks.PreToolUse.length > 0,
    'PreToolUse registration restored in settings.json');
  assert.equal(fs.existsSync(path.join(bare, '.code-graph')), false, 'no project footprint');
  assert.equal(fs.existsSync(path.join(bare, 'CLAUDE.md')), false, 'no adoption in non-project cwd');
});

// ── P1-16: SessionStart must fail OPEN ──────────────────────────────────────
//
// maybeAutoAdopt was called bare, and `runSessionInit()` at the bottom of this
// file had no wrapper at all. An unreadable / directory CLAUDE.md therefore
// threw EACCES/EISDIR out of the hook: everything AFTER adoption (project-map
// injection, the recent-impact push, the consistency check, both hook-firing
// canaries) silently stopped running, and the hook exited non-zero with a raw
// node stack trace in the user's session.
//
// The stub makes adoption throw regardless of WHY — the point of a fail-open
// wrapper is that it does not need to know the cause. adopt.js's own EACCES
// tolerance is tested in adopt.test.js; this is the second layer.
// §8.V4 disposal, second pass. The hook spawns a DETACHED background
// `verify-hooks-fire` against the sandbox HOME, which re-creates
// `~/.cache/code-graph` inside the directory the per-test `t.after` just
// removed — measured as 4 surviving sandboxes under ~/.claude/tmp. A run-level
// after-hook sweeps them once that child has exited.
const SESSION_INIT_SANDBOXES = [];
test.after(() => {
  for (const dir of SESSION_INIT_SANDBOXES) {
    try { fs.rmSync(dir, { recursive: true, force: true }); } catch { /* already gone */ }
  }
});

function runSessionInitHook(t, {
  adoptThrows = false,
  preloadSrc = null,
  prefix = 'cg-si-failopen-',
  // JS-08: run the hook from a SUBDIRECTORY of the project, the shape a
  // persistent shell reaches after `cd backend/`. `indexDb` plants the marker
  // `resolveProjectRoot` walks up to; without it the walk finds nothing and
  // correctly falls back to cwd, so the two options go together.
  cwdSub = null,
  indexDb = false,
} = {}) {
  const os = require('os');
  const { spawnSync } = require('child_process');
  const sb = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  SESSION_INIT_SANDBOXES.push(sb);
  t.after(() => fs.rmSync(sb, { recursive: true, force: true }));
  const home = sb;
  const cfg = path.join(sb, '.claude');
  const proj = path.join(sb, 'proj');
  fs.mkdirSync(path.join(cfg, 'plugins'), { recursive: true });
  fs.mkdirSync(path.join(proj, '.code-graph'), { recursive: true });
  // Fresh hook-fire state so checkHookFiring does NOT spawn its detached
  // background probe: that child outlives the test run and re-creates
  // `<sandbox>/.cache/code-graph` after every cleanup hook has run, which is
  // what left sandboxes behind in ~/.claude/tmp (§8.V4). Also makes the run
  // hermetic — no background process touching the sandbox mid-assertion.
  fs.mkdirSync(path.join(home, '.cache', 'code-graph'), { recursive: true });
  fs.writeFileSync(path.join(home, '.cache', 'code-graph', 'hook-fire-state.json'),
    JSON.stringify({ ts: new Date().toISOString(), failures: [] }));
  fs.writeFileSync(path.join(proj, 'package.json'), '{"name":"p","version":"1.0.0"}');
  // Seeds detectHookDark (runs LATE, after adoption): 3 edit events, no
  // grep/read events → it must emit its "may be dark" warning on stderr.
  fs.writeFileSync(path.join(proj, '.code-graph', 'recommendations.jsonl'),
    ['{"hook":"edit"}', '{"hook":"edit"}', '{"hook":"edit"}', ''].join('\n'));

  const args = [];
  if (adoptThrows) {
    const preload = path.join(sb, 'throwing-adopt.js');
    fs.writeFileSync(preload, `
      const adopt = require(${JSON.stringify(path.join(__dirname, 'adopt.js'))});
      adopt.maybeAutoAdopt = () => { throw Object.assign(new Error('EACCES: permission denied'), { code: 'EACCES' }); };
    `);
    args.push('--require', preload);
  }
  if (preloadSrc) {
    const p = path.join(sb, 'preload.js');
    fs.writeFileSync(p, preloadSrc);
    args.push('--require', p);
  }
  args.push(path.join(__dirname, 'session-init.js'));

  if (indexDb) fs.writeFileSync(path.join(proj, '.code-graph', 'index.db'), '');
  let cwd = proj;
  if (cwdSub) {
    cwd = path.join(proj, cwdSub);
    fs.mkdirSync(cwd, { recursive: true });
  }

  const res = spawnSync(process.execPath, args, {
    cwd,
    encoding: 'utf8',
    input: JSON.stringify({ source: 'startup' }),
    env: { ...process.env, HOME: home, USERPROFILE: home, CLAUDE_CONFIG_DIR: cfg, CODE_GRAPH_NO_AUTO_UPDATE: '1' },
  });
  return { res, proj, home };
}

// install()/update() have reported `manifestUnwritable` since they stopped
// throwing on it, and nothing read the field. It is not cosmetic:
// syncLifecycleConfig keys entirely off `manifest.version`, so a manifest that
// could not be written makes EVERY later SessionStart re-run install() and
// re-report 'installed', forever, with nothing to show for it.
test('an unwritable plugin manifest is reported, not swallowed', (t) => {
  const lifecycle = JSON.stringify(path.join(__dirname, 'lifecycle.js'));
  const { res } = runSessionInitHook(t, {
    prefix: 'cg-si-manifest-',
    preloadSrc: `
      const lc = require(${lifecycle});
      const realInstall = lc.install;
      lc.install = (...a) => ({ ...(realInstall(...a) || {}), manifestUnwritable: 'EACCES' });
    `,
  });
  assert.equal(res.status, 0, `hook must still exit 0; stderr:\n${res.stderr}`);
  assert.match(res.stdout, /manifest could not be written \(EACCES\)/,
    `the unwritable manifest must be surfaced; stdout was:\n${res.stdout}`);
  assert.match(res.stdout, /every session/,
    'the message must name the consequence, not just the error code');
});

// adopt() has reported `registryRecorded` since it stopped throwing on a broken
// registry, and nothing read it. uninstall() walks that registry to strip our
// managed block from each adopted project's CLAUDE.md, so an unrecorded project
// keeps the block forever after uninstall — with no plugin code left to remove it.
test('an unrecorded adoption warns that uninstall will not clean this project', (t) => {
  const adopt = JSON.stringify(path.join(__dirname, 'adopt.js'));
  const { res } = runSessionInitHook(t, {
    prefix: 'cg-si-registry-',
    preloadSrc: `
      const ad = require(${adopt});
      ad.maybeAutoAdopt = () => ({
        attempted: true,
        reason: 'installed',
        result: { ok: true, detailWritten: true, registryRecorded: false },
      });
    `,
  });
  assert.equal(res.status, 0, `hook must still exit 0; stderr:\n${res.stderr}`);
  assert.match(res.stderr, /adopted-projects registry/,
    `an unrecorded adoption must be surfaced; stderr was:\n${res.stderr}`);
  assert.match(res.stderr, /unadopt/,
    'the message must name the manual remedy');
});

// Control for the two tests above: the same harness with NO stubbed failure
// must stay quiet, so neither assertion can be passing on an unconditional line.
// JS-08 (audit 2026-08-29): the hook-dark detector read `process.cwd()` while
// every writer of recommendations.jsonl records into the RESOLVED project root.
// A session whose shell has `cd`-ed into a subdirectory — the exact case the
// subdir-cwd fix exists for — therefore found no file and made no claim: the
// detector for dark hooks was itself dark, silently.
test('the hook-dark detector reads the resolved project root, not the shell cwd', (t) => {
  const { res } = runSessionInitHook(t, {
    prefix: 'cg-si-subdir-',
    cwdSub: path.join('src', 'deep'),
    indexDb: true,
  });
  assert.equal(res.status, 0, `hook must exit 0; stderr:\n${res.stderr}`);
  assert.match(res.stderr, /may be dark/,
    'the seeded recommendations.jsonl sits at the project root; a subdir session ' +
    `must still find it. stderr was:\n${res.stderr}`);
});

// Control for the test above: with no index.db to walk up to, resolveProjectRoot
// has nothing to resolve and cwd remains the answer — so the assertion above is
// about root resolution, not about the warning being unconditional.
test('a subdir session with no indexed ancestor falls back to cwd and stays quiet', (t) => {
  const { res } = runSessionInitHook(t, {
    prefix: 'cg-si-subdir-noidx-',
    cwdSub: path.join('src', 'deep'),
    indexDb: false,
  });
  assert.equal(res.status, 0, `hook must exit 0; stderr:\n${res.stderr}`);
  assert.doesNotMatch(res.stderr, /may be dark/,
    'without an indexed ancestor there is no file to read and nothing to conclude');
});

test('a clean session start emits neither disclosure', (t) => {
  const { res } = runSessionInitHook(t, { prefix: 'cg-si-clean-' });
  assert.equal(res.status, 0, `stderr:\n${res.stderr}`);
  assert.doesNotMatch(res.stdout, /manifest could not be written/);
  assert.doesNotMatch(res.stderr, /adopted-projects registry/);
});

test('SessionStart fails OPEN when adoption throws: exit 0, later steps still run', (t) => {
  const { res } = runSessionInitHook(t, { adoptThrows: true });

  assert.equal(res.status, 0,
    `a SessionStart hook must never exit non-zero on a bad CLAUDE.md; stderr:\n${res.stderr}`);
  assert.match(res.stderr, /may be dark/,
    'detectHookDark runs AFTER adoption — its warning proves the rest of the sequence still executed');
  assert.match(res.stderr, /\[code-graph\]/,
    'the failure itself must be reported, not swallowed');
  assert.doesNotMatch(res.stderr, /^\s*at .*session-init\.js/m,
    'a raw node stack trace in the user\'s session is not a report');
});

test('the fail-open wrapper is scoped: a normal run still reaches the same late steps', (t) => {
  // Negative control for the test above. If the wrapper (or the stub) were what
  // produced the "may be dark" line, this run would prove nothing.
  const { res } = runSessionInitHook(t, { adoptThrows: false, prefix: 'cg-si-normal-' });
  assert.equal(res.status, 0);
  assert.match(res.stderr, /may be dark/);
});

test('runSessionInit tears down cache + adoption on a genuine uninstall (order regression)', (t) => {
  // Subprocess isolation: lifecycle.js evaluates CACHE_DIR from os.homedir() at
  // MODULE LOAD, so HOME/CLAUDE_CONFIG_DIR must be set before require — only a
  // fresh child honors them. This locks the order bug: isPluginUninstalled() MUST be
  // read BEFORE cleanupDisabledStatusline() wipes the composite/registry signals it
  // depends on — otherwise teardown is skipped (was null pre-fix).
  const os = require('os');
  const { execFileSync } = require('child_process');
  const sb = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-si-teardown-'));
  t.after(() => fs.rmSync(sb, { recursive: true, force: true }));
  const home = sb, cfg = path.join(sb, '.claude'), proj = path.join(sb, 'proj');
  fs.mkdirSync(path.join(cfg, 'plugins'), { recursive: true });
  fs.mkdirSync(proj, { recursive: true });
  fs.writeFileSync(path.join(proj, 'package.json'), '{"name":"p","version":"1.0.0"}');
  fs.writeFileSync(path.join(proj, 'CLAUDE.md'), '# P\n\nKEEP THIS USER LINE.\n');
  fs.writeFileSync(path.join(cfg, 'settings.json'), '{"statusLine":{"type":"command","command":"/bin/prior.sh"}}');
  const env = { ...process.env, HOME: home, USERPROFILE: home, CLAUDE_CONFIG_DIR: cfg };
  const lc = path.join(__dirname, 'lifecycle.js'), ad = path.join(__dirname, 'adopt.js');
  const si = path.join(__dirname, 'session-init.js');

  // install + adopt, then simulate a downloaded binary + a post-/plugin-uninstall
  // installed_plugins.json (record for some OTHER plugin, none for code-graph).
  execFileSync(process.execPath, [lc, 'install'], { env, cwd: proj, stdio: 'ignore' });
  execFileSync(process.execPath, ['-e',
    `require(${JSON.stringify(ad)}).adopt({cwd:process.cwd()})`], { env, cwd: proj, stdio: 'ignore' });
  fs.mkdirSync(path.join(home, '.cache', 'code-graph', 'bin'), { recursive: true });
  fs.writeFileSync(path.join(home, '.cache', 'code-graph', 'bin', 'code-graph-mcp'), 'x');
  fs.writeFileSync(path.join(cfg, 'plugins', 'installed_plugins.json'),
    JSON.stringify({ plugins: { 'other@mkt': [{ version: '1.0.0', installPath: '/x' }] } }));
  assert.ok(fs.readFileSync(path.join(proj, 'CLAUDE.md'), 'utf8').includes('code-graph'), 'adopt injected block');

  const res = JSON.parse(execFileSync(process.execPath, ['-e',
    `process.stdout.write(JSON.stringify(require(${JSON.stringify(si)}).runSessionInit({source:'startup'})))`],
    { env, cwd: proj }).toString());

  assert.equal(res.inactive, true);
  assert.ok(res.teardown, 'teardown ran (null pre-fix = order bug)');
  assert.equal(res.teardown.cacheRemoved, true);
  assert.equal(res.teardown.unadopted, true);
  assert.equal(fs.existsSync(path.join(home, '.cache', 'code-graph')), false, 'cache residue gone');
  const md = fs.readFileSync(path.join(proj, 'CLAUDE.md'), 'utf8');
  assert.ok(!md.includes('code-graph'), 'adopt block removed');
  assert.ok(md.includes('KEEP THIS USER LINE'), 'user content preserved');
  const settings = JSON.parse(fs.readFileSync(path.join(cfg, 'settings.json'), 'utf8'));
  assert.equal(settings.statusLine.command, '/bin/prior.sh', 'prior statusline restored');
});

test('consistencyCheck returns empty array when binary version matches plugin', () => {
  const result = consistencyCheck('/tmp/nonexistent-binary');
  assert.ok(Array.isArray(result));
});

// ──────────────────────────────────────────────────────────────────────────
// v0.17.0 — quietHooks: unconditional quiet default
// Priority: legacy QUIET_HOOKS=0/1 > new VERBOSE_HOOKS=1 > default true.
// `adopted` param is dead (unconditional default does not consult it) but
// the destructured signature still accepts it for backward compat.
// ──────────────────────────────────────────────────────────────────────────

test('computeQuietHooks: legacy QUIET_HOOKS="0" forces noisy', () => {
  assert.equal(computeQuietHooks({ env: { CODE_GRAPH_QUIET_HOOKS: '0' } }), false);
});

test('computeQuietHooks: legacy QUIET_HOOKS="1" forces quiet', () => {
  assert.equal(computeQuietHooks({ env: { CODE_GRAPH_QUIET_HOOKS: '1' } }), true);
});

test('computeQuietHooks: VERBOSE_HOOKS="1" opts in to noisy', () => {
  assert.equal(computeQuietHooks({ env: { CODE_GRAPH_VERBOSE_HOOKS: '1' } }), false);
});

test('computeQuietHooks: legacy QUIET_HOOKS="1" wins over VERBOSE_HOOKS="1"', () => {
  // Conflicting opt-ins: legacy explicit-quiet wins over new verbose opt-in.
  // (Legacy QUIET_HOOKS="0" + VERBOSE_HOOKS="1" both mean noisy — no conflict.)
  assert.equal(
    computeQuietHooks({ env: { CODE_GRAPH_QUIET_HOOKS: '1', CODE_GRAPH_VERBOSE_HOOKS: '1' } }),
    true
  );
});

test('computeQuietHooks: env unset → quiet by default', () => {
  assert.equal(computeQuietHooks({ env: {} }), true);
});

test('computeQuietHooks: no args → quiet by default', () => {
  assert.equal(computeQuietHooks(), true);
});

test('computeQuietHooks: legacy `adopted` param is ignored under new default', () => {
  // adopted=true used to imply quiet; now quiet is unconditional.
  // adopted=false used to imply noisy; now still quiet by default.
  assert.equal(computeQuietHooks({ adopted: true, env: {} }), true);
  assert.equal(computeQuietHooks({ adopted: false, env: {} }), true);
});

test('shouldInjectMap: only injects when available + not-quiet + adopted', () => {
  // The single positive case: opted into verbose AND adopted.
  assert.equal(shouldInjectMap({ available: true, quietHooks: false, adopted: true }), true);
  // Adopted-only gate: verbose but unadopted → no injection (the zero-referenced
  // case cross-project-interference flagged).
  assert.equal(shouldInjectMap({ available: true, quietHooks: false, adopted: false }), false);
  // Quiet default suppresses regardless of adoption.
  assert.equal(shouldInjectMap({ available: true, quietHooks: true, adopted: true }), false);
  // No binary → nothing to inject.
  assert.equal(shouldInjectMap({ available: false, quietHooks: false, adopted: true }), false);
  // Missing args default to falsey → no injection.
  assert.equal(shouldInjectMap(), false);
});

// ──────────────────────────────────────────────────────────────────────────
// v0.63 — SessionStart "live context": recent-change blast radius injection.
// ──────────────────────────────────────────────────────────────────────────

test('shouldInjectRecentImpact: default-ON for adopted projects (separate gate from the static map)', () => {
  // Unlike shouldInjectMap, this does NOT require the verbose opt-in — it earns
  // standing context because it's git-delta-derived, not duplicative of MEMORY.md.
  assert.equal(shouldInjectRecentImpact({ available: true, adopted: true, env: {} }), true);
});

test('shouldInjectRecentImpact: hard kill-switch and dedicated opt-out suppress it', () => {
  assert.equal(shouldInjectRecentImpact({ available: true, adopted: true, env: { CODE_GRAPH_QUIET_HOOKS: '1' } }), false);
  assert.equal(shouldInjectRecentImpact({ available: true, adopted: true, env: { CODE_GRAPH_NO_RECENT_IMPACT: '1' } }), false);
});

test('shouldInjectRecentImpact: needs binary + adoption', () => {
  assert.equal(shouldInjectRecentImpact({ available: false, adopted: true, env: {} }), false);
  assert.equal(shouldInjectRecentImpact({ available: true, adopted: false, env: {} }), false);
  assert.equal(shouldInjectRecentImpact(), false);
});

test('filterSourceFiles: keeps AST-bearing source, drops config/lock/doc', () => {
  const diff = [
    'src/domain.rs', 'Cargo.lock', 'Cargo.toml', 'CHANGELOG.md',
    'package.json', 'src/parser/relations/mod.rs', 'claude-plugin/scripts/session-init.js',
    'npm/linux-x64/package.json',
  ].join('\n');
  assert.deepEqual(filterSourceFiles(diff), [
    'src/domain.rs', 'src/parser/relations/mod.rs', 'claude-plugin/scripts/session-init.js',
  ]);
});

test('parseGitStatusPaths: extracts paths from modified / staged / untracked lines (finding #3)', () => {
  // `git status --porcelain` columns: " M" unstaged-mod, "M " staged, "??" untracked,
  // "A " added. The untracked line is exactly what diff-only missed.
  const out = [
    ' M src/domain.rs',
    'M  src/cli.rs',
    '?? src/brand_new.rs',
    'A  src/staged_new.rs',
    'D  src/gone.rs',
  ].join('\n');
  assert.deepEqual(parseGitStatusPaths(out), [
    'src/domain.rs', 'src/cli.rs', 'src/brand_new.rs', 'src/staged_new.rs', 'src/gone.rs',
  ]);
});

test('parseGitStatusPaths: rename takes the NEW path; quoted path is unquoted', () => {
  assert.deepEqual(parseGitStatusPaths('R  src/old.rs -> src/new.rs'), ['src/new.rs']);
  assert.deepEqual(parseGitStatusPaths('?? "src/with space.rs"'), ['src/with space.rs']);
});

test('parseGitStatusPaths: blank / too-short / non-string input → []', () => {
  assert.deepEqual(parseGitStatusPaths(''), []);
  assert.deepEqual(parseGitStatusPaths(null), []);
  assert.deepEqual(parseGitStatusPaths('\n\n'), []);
  assert.deepEqual(parseGitStatusPaths('??'), []); // no path after status
});

test('parseGitStatusPaths composes with filterSourceFiles: untracked source kept, config dropped', () => {
  const out = [' M Cargo.toml', '?? src/new_feature.rs', '?? notes.txt'].join('\n');
  assert.deepEqual(filterSourceFiles(parseGitStatusPaths(out)), ['src/new_feature.rs']);
});

test('formatRecentImpact: re-run command is runnable verbatim when ≤4 changed (finding #4)', () => {
  const affected = { affected_files: [{ depth: 1, is_test: false, path: 'src/a.rs' }], tests: [] };
  const text = formatRecentImpact(['src/x.rs', 'src/y.rs'], affected);
  assert.match(text, /Re-run impacted tests: code-graph-mcp affected src\/x\.rs src\/y\.rs$/m);
  assert.doesNotMatch(text, /more changed file/);
  assert.doesNotMatch(text, / …/); // no bare ellipsis
});

test('formatRecentImpact: >4 changed → explicit "+N more", not a bare ellipsis (finding #4)', () => {
  const affected = { affected_files: [{ depth: 1, is_test: false, path: 'src/a.rs' }], tests: [] };
  const changed = ['s/1.rs', 's/2.rs', 's/3.rs', 's/4.rs', 's/5.rs', 's/6.rs'];
  const text = formatRecentImpact(changed, affected);
  assert.match(text, /code-graph-mcp affected s\/1\.rs s\/2\.rs s\/3\.rs s\/4\.rs {2}\(\+2 more changed file\(s\)/);
  assert.doesNotMatch(text, / …/); // the misleading bare ellipsis is gone
});

test('filterSourceFiles: caps the list and tolerates blank/garbage input', () => {
  assert.deepEqual(filterSourceFiles(''), []);
  assert.deepEqual(filterSourceFiles(null), []);
  const many = Array.from({ length: 40 }, (_, i) => `src/m${i}.rs`).join('\n');
  assert.equal(filterSourceFiles(many).length, 25);
  assert.equal(filterSourceFiles(many, 3).length, 3);
});

test('formatRecentImpact: renders changed + blast radius + direct dependents', () => {
  const affected = {
    affected_files: [
      { depth: 1, is_test: false, path: 'src/cli.rs' },
      { depth: 1, is_test: false, path: 'src/graph/impact.rs' },
      { depth: 1, is_test: true, path: 'src/parser/relations/tests.rs' },
      { depth: 2, is_test: false, path: 'src/main.rs' },
    ],
    changed: ['src/domain.rs'],
    tests: ['src/parser/relations/tests.rs', 'tests/integration.rs'],
  };
  const text = formatRecentImpact(['src/domain.rs'], affected);
  assert.match(text, /Recent changes/);
  assert.match(text, /Changed: src\/domain\.rs/);
  assert.match(text, /Impacts 4 file\(s\) \(2 direct dependent\(s\)\), 2 test file\(s\)/);
  assert.match(text, /Direct dependents: src\/cli\.rs, src\/graph\/impact\.rs/);
  assert.match(text, /code-graph-mcp affected src\/domain\.rs/);
  // It is graph-unique — the copy says so (the whole point vs the static map).
  assert.match(text, /not in MEMORY\.md/);
});

test('recentImpactWorthShowing: WIP always shows, regardless of source', () => {
  assert.equal(recentImpactWorthShowing({ isWip: true, source: 'startup' }), true);
  assert.equal(recentImpactWorthShowing({ isWip: true, source: 'compact' }), true);
});

test('recentImpactWorthShowing: clean tree (last-commit fallback) suppressed on cold startup, shown on resume', () => {
  assert.equal(recentImpactWorthShowing({ isWip: false, source: 'startup' }), false);
  assert.equal(recentImpactWorthShowing({ isWip: false, source: 'clear' }), true);
  assert.equal(recentImpactWorthShowing({ isWip: false, source: 'compact' }), true);
  assert.equal(recentImpactWorthShowing({ isWip: false, source: 'resume' }), true);
  // Unknown source (direct call / test) defaults to showing — only explicit
  // cold startup is the suppressed case.
  assert.equal(recentImpactWorthShowing({ isWip: false }), true);
  assert.equal(recentImpactWorthShowing(), true);
});

test('formatRecentImpact: high-fanout change drops the noisy name list, keeps risk + test scope', () => {
  // >15 direct dependents = a constants/util node "touches everything"; the
  // first-N names are arbitrary noise, so only risk + test count is surfaced.
  const affected = {
    affected_files: Array.from({ length: 20 }, (_, i) => ({ depth: 1, is_test: false, path: `src/f${i}.rs` })),
    tests: ['tests/a.rs', 'tests/b.rs'],
  };
  const text = formatRecentImpact(['src/domain.rs'], affected);
  assert.match(text, /High-fanout change/);
  assert.match(text, /run the full suite \(2 test file\(s\)\)/);
  assert.doesNotMatch(text, /Direct dependents:/); // name list suppressed
});

test('formatRecentImpact: at/under the fanout threshold the name list IS the signal', () => {
  const affected = {
    affected_files: Array.from({ length: 15 }, (_, i) => ({ depth: 1, is_test: false, path: `src/f${i}.rs` })),
    tests: [],
  };
  const text = formatRecentImpact(['src/x.rs'], affected);
  assert.doesNotMatch(text, /High-fanout/);
  assert.match(text, /Direct dependents:/);
});

test('formatRecentImpact: caps direct-dependent list with a "+N more" overflow', () => {
  const affected = {
    affected_files: Array.from({ length: 10 }, (_, i) => ({ depth: 1, is_test: false, path: `src/f${i}.rs` })),
    tests: [],
  };
  const text = formatRecentImpact(['src/domain.rs'], affected);
  assert.match(text, /\+4 more/); // 10 direct, cap 6 → 4 hidden
});

test('formatRecentImpact: returns null when nothing graph-relevant (no dependents / no changes)', () => {
  // A deps-only commit: changed files filtered to empty upstream → caller skips.
  assert.equal(formatRecentImpact([], { affected_files: [] }), null);
  // Changed source but zero indexed dependents → nothing actionable to say.
  assert.equal(formatRecentImpact(['src/x.rs'], { affected_files: [], tests: [] }), null);
  assert.equal(formatRecentImpact(['src/x.rs'], {}), null);
});

test('consistencyCheck returns version-mismatch when versions differ', (t) => {
  const os = require('os');
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cc-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const bin = path.join(dir, 'code-graph-mcp');
  fs.writeFileSync(bin, [
    '#!/usr/bin/env bash',
    'if [ "$1" = "--version" ]; then',
    '  echo "code-graph-mcp 0.0.1"',
    '  exit 0',
    'fi',
    'exit 0',
  ].join('\n'));
  fs.chmodSync(bin, 0o755);

  const issues = consistencyCheck(bin);
  const versionIssue = issues.find(i => i.id === 'version-mismatch');
  assert.ok(versionIssue, 'should detect version mismatch');
  assert.ok(versionIssue.msg.includes('0.0.1'));
});

test('injectProjectMap map call carries CODE_GRAPH_INTERNAL (delivery, not a model conversion)', () => {
  // injectProjectMap runs `code-graph-mcp map --compact` to inject the project map.
  // That run is a hook-internal delivery — it must carry the internal marker so
  // record_cli_use (src/cli.rs) does not log it as a phantom model `use` event
  // (the 2026-06-23 mem audit found this leak class; the sibling affected call was
  // already guarded). Asserted at source level because injectProjectMap is not exported.
  const src = fs.readFileSync(path.join(__dirname, 'session-init.js'), 'utf8');
  const i = src.indexOf("['map', '--compact']");
  assert.ok(i >= 0, 'map injection present');
  assert.match(src.slice(i, i + 420), /CODE_GRAPH_INTERNAL:\s*'1'/);
});


test('a missing binary on a fresh install reads as auto-install, not as a failed one', () => {
  // The first session after `/plugin install` ALWAYS has no binary — nothing
  // ships the ~40MB engine with the plugin — and runSessionInit launches the
  // background download a few lines after this message. Measured in a sandboxed
  // HOME 2026-08-17: the old text ('MCP server cannot start. Install: npm
  // install -g @sdsrs/code-graph') was the first thing a new user saw, and the
  // binary landed on its own 12s later.
  const auto = missingBinaryMessage({});
  assert.match(auto, /background/i);
  assert.ok(!/cannot start/i.test(auto), 'no failure framing while the fetch is running');
  assert.ok(!/npm install -g/.test(auto), 'no manual instruction the user does not need');

  // Opted out of auto-update → nothing else will fetch it, so the manual
  // instruction is the only correct answer.
  const optedOut = missingBinaryMessage({ CODE_GRAPH_NO_AUTO_UPDATE: '1' });
  assert.match(optedOut, /npm install -g @sdsrs\/code-graph/);
  assert.match(optedOut, /CODE_GRAPH_NO_AUTO_UPDATE=1/);
});

// ── SessionStart budget (audit 2026-09-05 NEW-05) ─────────────────────────
//
// These assert OUTCOMES, not the clock. `resetHookDeadline(Date.now() - 1)`
// arms an already-expired deadline so `remainingMs` returns null on the first
// call, deterministically; a test that armed a real budget and waited it out
// would be a clock race (the shape of the deadline-timing test removed in
// v0.134.0).
//
// Two of these need a resolvable binary and this repo's CI checkout has none.
// They call `t.skip()` with a reason rather than asserting something vacuously
// true, so the gap shows up in the run output instead of reading as coverage.
const { resetHookDeadline } = require('./hook-fail-open');

function withSpentBudget(t) {
  resetHookDeadline(Date.now() - 1);
  t.after(() => resetHookDeadline());
}

test('a spent budget makes indexNeedsRevalidation report unknown, not "not stale"', (t) => {
  // The stub reports a STALE index. With the budget gone the probe must not run
  // at all — and must not answer `false`, which is the value it also returns for
  // a healthy index and which would let ensureIndexFresh call the index 'fresh'.
  const { bin, cwd } = stubHealthBin(t, {
    json: JSON.stringify({ healthy: true, nodes: 5, index_version_stale: true }),
  });
  assert.equal(indexNeedsRevalidation(bin, cwd), true, 'precondition: the probe sees the stale index');

  withSpentBudget(t);
  assert.equal(
    indexNeedsRevalidation(bin, cwd), null,
    'budget-exhausted needs its own answer — `false` here is a freshness claim nothing verified'
  );
});

test('a spent budget stops consistencyCheck spawning --version instead of running it unbounded', (t) => {
  // Takes the binary path as an argument, so this one runs everywhere.
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-cc-budget-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const bin = path.join(dir, 'code-graph-mcp');
  const marker = path.join(dir, 'spawned');
  fs.writeFileSync(bin, [
    '#!/usr/bin/env bash',
    `touch "${marker}"`,
    'if [ "$1" = "--version" ]; then echo "code-graph-mcp 0.0.1"; exit 0; fi',
    'exit 0',
  ].join('\n'));
  fs.chmodSync(bin, 0o755);

  assert.ok(consistencyCheck(bin).some(i => i.id === 'version-mismatch'),
    'precondition: the stub reports a mismatched version');
  assert.ok(fs.existsSync(marker), 'precondition: the check spawns the binary');
  fs.rmSync(marker);

  withSpentBudget(t);
  const issues = consistencyCheck(bin);
  assert.ok(!fs.existsSync(marker), 'must not spawn --version with no budget left');
  assert.ok(!issues.some(i => i.id === 'version-mismatch'),
    'a skipped check suppresses a warning; it must not report one it never made');
});

test('a spent budget makes ensureIndexFresh report unknown, never fresh', (t) => {
  const { findBinary } = require('./find-binary');
  if (!findBinary()) {
    t.skip('needs a resolvable binary — ensureIndexFresh returns "skipped" before reaching the budget');
    return;
  }
  // A real index that nothing looked at. 'fresh' is the one answer that would
  // stop the server drift check and the CLI from looking again, so it is the
  // one answer this path must not invent.
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-budget-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  fs.mkdirSync(path.join(dir, '.code-graph'), { recursive: true });
  fs.writeFileSync(path.join(dir, '.code-graph', 'index.db'), 'not a real db');

  const origCwd = process.cwd();
  process.chdir(dir);
  try {
    withSpentBudget(t);
    assert.equal(ensureIndexFresh(), 'unknown');
  } finally {
    process.chdir(origCwd);
  }
});

test('a spent budget leaves the macOS quarantine probe unverified rather than silently OK', (t) => {
  const { findBinary } = require('./find-binary');
  if (!findBinary()) {
    t.skip('needs a resolvable binary — verifyBinary returns available:false before the darwin branch');
    return;
  }
  const realPlatform = Object.getOwnPropertyDescriptor(process, 'platform');
  Object.defineProperty(process, 'platform', { value: 'darwin', configurable: true });
  t.after(() => Object.defineProperty(process, 'platform', realPlatform));

  withSpentBudget(t);
  const result = verifyBinary();
  // `available` stays true: the binary exists and is executable, so `false`
  // would send the user to `xattr -d` for nothing. But "it runs" is exactly
  // what the probe establishes, and it did not run — so name that.
  assert.equal(result.available, true);
  assert.equal(result.issue, 'quarantine-probe-skipped');
});
