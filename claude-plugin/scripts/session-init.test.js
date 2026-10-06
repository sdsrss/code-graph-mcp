'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');

const os = require('os');
const { launchBackgroundAutoUpdate, isHighIntentSource, syncLifecycleConfig, ensureIndexFresh, indexNeedsRevalidation, verifyBinary, computeQuietHooks, shouldInjectMap, missingBinaryMessage } = require('./session-init');

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
  // A second SessionStart in the same project: the `sb` a previous call returned.
  reuse = null,
  // false: the project has no .code-graph/ (and so no recommendations.jsonl).
  codeGraphDir = true,
} = {}) {
  const os = require('os');
  const { spawnSync } = require('child_process');
  const sb = reuse || fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  if (!reuse) {
    SESSION_INIT_SANDBOXES.push(sb);
    t.after(() => fs.rmSync(sb, { recursive: true, force: true }));
  }
  const home = sb;
  const cfg = path.join(sb, '.claude');
  const proj = path.join(sb, 'proj');
  fs.mkdirSync(path.join(cfg, 'plugins'), { recursive: true });
  fs.mkdirSync(codeGraphDir ? path.join(proj, '.code-graph') : proj, { recursive: true });
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
  // grep/read events → it must emit its "may be dark" warning as a notice.
  if (codeGraphDir) {
    fs.writeFileSync(path.join(proj, '.code-graph', 'recommendations.jsonl'),
      ['{"hook":"edit"}', '{"hook":"edit"}', '{"hook":"edit"}', ''].join('\n'));
  }

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
  return { res, proj, home, sb };
}

// SessionStart writes ONE JSON value on stdout (decision D5): the user-facing
// notices as `systemMessage`, model context as `additionalContext`. Its stderr
// is never shown when it exits 0, so that is not where a notice may go.
function noticeOf(res) {
  const out = (res.stdout || '').trim();
  if (!out) return '';
  return JSON.parse(out).systemMessage || '';
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
  assert.match(noticeOf(res), /manifest could not be written \(EACCES\)/,
    `the unwritable manifest must be surfaced; stdout was:\n${res.stdout}`);
  assert.match(noticeOf(res), /every session/,
    'the message must name the consequence, not just the error code');
});

// Decision D4: SessionStart no longer writes CLAUDE.md, so the adoption notices
// ("Installed …", "Refreshed …", the unrecorded-registry note) are gone. What is
// left is a block that has drifted from the shipped template: its guidance is
// out of date, and only the user can refresh or remove it.
const STALE_STUB = `
  const ad = require(${JSON.stringify(path.join(__dirname, 'adopt.js'))});
  ad.maybeAutoAdopt = () => ({ attempted: false, reason: 'stale' });
`;

test('a stale adoption block is reported with a refresh and a remove command', (t) => {
  const { res } = runSessionInitHook(t, { prefix: 'cg-si-stale-', preloadSrc: STALE_STUB });
  assert.equal(res.status, 0, `hook must still exit 0; stderr:\n${res.stderr}`);
  const n = noticeOf(res);
  assert.match(n, /out-of-date code-graph block/, `stdout was:\n${res.stdout}`);
  assert.match(n, /Refresh it: node '[^']+adopt\.js' adopt/);
  assert.match(n, /Remove it: {2}node '[^']+adopt\.js' unadopt/);
});

// 0.164.0 known gap: a project adopted by 0.163 showed the notice at every
// session start. It is shown once per project for one shipped template; a
// template that changes again is news, so it is shown again.
const staleStub = (fingerprint) => `
  const ad = require(${JSON.stringify(path.join(__dirname, 'adopt.js'))});
  ad.maybeAutoAdopt = () => ({ attempted: false, reason: 'stale', fingerprint: ${JSON.stringify(fingerprint)} });
`;
const STALE_RE = /out-of-date code-graph block/;

test('the stale-block notice is shown once per project for one shipped template', (t) => {
  const first = runSessionInitHook(t, { prefix: 'cg-si-stale-once-', preloadSrc: staleStub('aaaa1111') });
  assert.equal(first.res.status, 0, `stderr:\n${first.res.stderr}`);
  assert.match(noticeOf(first.res), STALE_RE, 'the first session must show it');

  const second = runSessionInitHook(t, { reuse: first.sb, preloadSrc: staleStub('aaaa1111') });
  assert.equal(second.res.status, 0, `stderr:\n${second.res.stderr}`);
  assert.doesNotMatch(noticeOf(second.res), STALE_RE, 'the same template must not be reported twice');

  const third = runSessionInitHook(t, { reuse: first.sb, preloadSrc: staleStub('bbbb2222') });
  assert.match(noticeOf(third.res), STALE_RE, 'a newer shipped template must be reported again');
});

// The rules file (rules-file.js): Claude Code read its rules before this hook
// wrote the file, so the session that creates it gets the text once — and only
// that session; one that finds it in place has already loaded it.
const contextOf = (res) => {
  const out = (res.stdout || '').trim();
  if (!out) return '';
  const o = JSON.parse(out);
  return (o.hookSpecificOutput && o.hookSpecificOutput.additionalContext) || '';
};
const rulesStub = (result) => `
  require(${JSON.stringify(path.join(__dirname, 'rules-file.js'))}).maybeWriteRulesFile = () => (${JSON.stringify(result)});
`;

test('the session that creates the rules file is handed its text once', (t) => {
  const created = runSessionInitHook(t, {
    prefix: 'cg-si-rules-',
    preloadSrc: rulesStub({ action: 'created', text: 'RULES-TEXT-CANARY' }),
  });
  assert.equal(created.res.status, 0, `stderr:\n${created.res.stderr}`);
  assert.match(contextOf(created.res), /RULES-TEXT-CANARY/);
  for (const action of ['unchanged', 'updated', 'skipped', 'refused']) {
    const r = runSessionInitHook(t, {
      reuse: created.sb, preloadSrc: rulesStub({ action, text: 'RULES-TEXT-CANARY' }),
    });
    assert.doesNotMatch(contextOf(r.res), /RULES-TEXT-CANARY/, `${action}: the file itself is loaded already`);
  }
});

test('a throwing rules-file writer does not take SessionStart down', (t) => {
  const { res } = runSessionInitHook(t, {
    prefix: 'cg-si-rules-throw-',
    preloadSrc: `require(${JSON.stringify(path.join(__dirname, 'rules-file.js'))}).maybeWriteRulesFile = () => { throw new Error('boom'); };`,
  });
  assert.equal(res.status, 0, `stderr:\n${res.stderr}`);
});

test('the stale-block notice keeps showing where it cannot be recorded', (t) => {
  // No .code-graph/ in the project: creating one would put an unexcluded
  // directory in `git status`, the side effect D3/D4 removed. Not recorded, so
  // shown every session, as in 0.164.0.
  // No binary either, so no index build can create the directory mid-test.
  const noDirStub = staleStub('aaaa1111') + `
    require(${JSON.stringify(path.join(__dirname, 'find-binary.js'))}).findBinary = () => null;
  `;
  const first = runSessionInitHook(t, { prefix: 'cg-si-stale-nodir-', preloadSrc: noDirStub, codeGraphDir: false });
  const second = runSessionInitHook(t, { reuse: first.sb, preloadSrc: noDirStub, codeGraphDir: false });
  assert.match(noticeOf(first.res), STALE_RE);
  assert.match(noticeOf(second.res), STALE_RE, 'without .code-graph/ nothing may silence it');
  assert.equal(fs.existsSync(path.join(first.proj, '.code-graph')), false,
    'the notice must not create the directory to record itself');

  // The marker path is taken by a directory: the write fails, and a failed
  // write must not read as "already shown".
  const sb2 = runSessionInitHook(t, { prefix: 'cg-si-stale-eisdir-', preloadSrc: staleStub('aaaa1111') });
  fs.rmSync(path.join(sb2.proj, '.code-graph', 'stale-block-notice'), { force: true });
  fs.mkdirSync(path.join(sb2.proj, '.code-graph', 'stale-block-notice'));
  const again = runSessionInitHook(t, { reuse: sb2.sb, preloadSrc: staleStub('aaaa1111') });
  assert.equal(again.res.status, 0, `stderr:\n${again.res.stderr}`);
  assert.match(noticeOf(again.res), STALE_RE);
});

// issue #41: the reporter uses the plugin only and has no `code-graph-mcp` on
// PATH, so a remedy that spends the bare name is unrunnable. The notice is
// per-machine and ephemeral, so unlike the CLAUDE.md block it may (and must)
// name the path this install actually resolved.
test('the stale-block remedy names a command this install can actually run', (t) => {
  const { res } = runSessionInitHook(t, { prefix: 'cg-si-reverse-', preloadSrc: STALE_STUB });
  assert.equal(res.status, 0, `hook must still exit 0; stderr:\n${res.stderr}`);

  // Pull the command out of the message and run it, rather than matching a
  // shape. An earlier version of this test stubbed findBinary to `/bin/true`
  // and asserted the string — it stayed green while the real command exited 1
  // with "adopt.js not found" on every install layout except a dev checkout.
  const m = noticeOf(res).match(/Remove it:\s+(node '[^']+' unadopt)/);
  assert.ok(m, `the remove hint must be present and quoted; stdout was:\n${res.stdout}`);
  const script = m[1].match(/'([^']+)'/)[1];
  assert.ok(fs.existsSync(script), `the hint points at a file that does not exist: ${script}`);
  assert.equal(path.basename(script), 'adopt.js',
    `unadopt is JS-dispatched — the hint must name adopt.js, not a binary that ` +
    `re-execs it from a directory the plugin cache does not have: ${script}`);

  const { spawnSync } = require('child_process');
  const probeHome = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-si-reverse-run-'));
  t.after(() => fs.rmSync(probeHome, { recursive: true, force: true }));
  const ran = spawnSync(process.execPath, [script, 'unadopt'], {
    cwd: probeHome,
    encoding: 'utf8',
    // CLAUDE_CONFIG_DIR too, not just HOME: this probe runs a DESTRUCTIVE
    // subcommand, and a developer who exports that variable would otherwise have
    // it act on their real Claude config. The hardening guard caught exactly this.
    env: {
      ...process.env,
      HOME: probeHome,
      USERPROFILE: probeHome,
      CLAUDE_CONFIG_DIR: path.join(probeHome, '.claude'),
    },
  });
  assert.equal(ran.status, 0,
    `the printed command must RUN, not just read well. stdout:\n${ran.stdout}\nstderr:\n${ran.stderr}`);
});

// The hint must not depend on binary resolution at all — `adopt`/`unadopt` are
// dispatched through adopt.js, which sits next to this hook in every install
// layout, so a missing binary changes nothing about how you undo an adoption.
test('the stale-block remedy does not change when no binary resolved', (t) => {
  const findBinary = JSON.stringify(path.join(__dirname, 'find-binary.js'));
  const { res } = runSessionInitHook(t, {
    prefix: 'cg-si-reverse-none-',
    preloadSrc: STALE_STUB + `
      const fb = require(${findBinary});
      fb.findBinary = () => null;
    `,
  });
  assert.equal(res.status, 0, `hook must still exit 0; stderr:\n${res.stderr}`);
  assert.match(noticeOf(res), /Remove it:\s+node '[^']+adopt\.js' unadopt/,
    `no binary must not degrade the hint; stdout was:\n${res.stdout}`);
});

// A corrupt index answers every hook with nothing. A reader never rebuilds it
// (it reports and preserves), and health-check's `reason:"corrupt"` used to read
// as "not stale", so the hooks stayed dark and nobody was told (hook audit
// 2026-09-28 P1-7). SessionStart now starts the indexer and says so.
test('a corrupt index is rebuilt in the background and the user is told', (t) => {
  const os = require('os');
  const stubDir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-si-corrupt-bin-'));
  t.after(() => fs.rmSync(stubDir, { recursive: true, force: true }));
  const marker = path.join(stubDir, 'rebuilt');
  const stub = path.join(stubDir, 'code-graph-mcp');
  fs.writeFileSync(stub, `#!/bin/sh
case "$1" in
  health-check) echo '{"healthy":false,"reason":"corrupt","issue":"index database is corrupt"}'; exit 1 ;;
  incremental-index) touch ${JSON.stringify(marker)} ;;
esac
exit 0
`, { mode: 0o755 });
  const { res } = runSessionInitHook(t, {
    prefix: 'cg-si-corrupt-',
    indexDb: true,
    preloadSrc: `
      const fb = require(${JSON.stringify(path.join(__dirname, 'find-binary.js'))});
      fb.findBinary = () => ${JSON.stringify(stub)};
    `,
  });
  assert.equal(res.status, 0, `hook must still exit 0; stderr:\n${res.stderr}`);
  assert.match(noticeOf(res), /index at \.code-graph\/index\.db was corrupt/, `stdout was:\n${res.stdout}`);
  const deadline = Date.now() + 5000;
  while (!fs.existsSync(marker) && Date.now() < deadline) { /* the rebuild is detached */ }
  assert.ok(fs.existsSync(marker), 'the indexer must have been started to rebuild it');
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
  assert.match(noticeOf(res), /may be dark/,
    'the seeded recommendations.jsonl sits at the project root; a subdir session ' +
    `must still find it. stdout was:\n${res.stdout}`);
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
  assert.doesNotMatch(noticeOf(res), /may be dark/,
    'without an indexed ancestor there is no file to read and nothing to conclude');
});

test('a clean session start emits neither disclosure', (t) => {
  const { res } = runSessionInitHook(t, { prefix: 'cg-si-clean-' });
  assert.equal(res.status, 0, `stderr:\n${res.stderr}`);
  assert.doesNotMatch(noticeOf(res), /manifest could not be written/);
  assert.doesNotMatch(noticeOf(res), /out-of-date code-graph block/);
});

test('SessionStart fails OPEN when adoption throws: exit 0, later steps still run', (t) => {
  const { res } = runSessionInitHook(t, { adoptThrows: true });

  assert.equal(res.status, 0,
    `a SessionStart hook must never exit non-zero on a bad CLAUDE.md; stderr:\n${res.stderr}`);
  assert.match(noticeOf(res), /may be dark/,
    'detectHookDark runs AFTER adoption — its warning proves the rest of the sequence still executed');
  assert.doesNotMatch(res.stderr, /^\s*at .*session-init\.js/m,
    'a raw node stack trace in the user\'s session is not a report');
});

test('the fail-open wrapper is scoped: a normal run still reaches the same late steps', (t) => {
  // Negative control for the test above. If the wrapper (or the stub) were what
  // produced the "may be dark" line, this run would prove nothing.
  const { res } = runSessionInitHook(t, { adoptThrows: false, prefix: 'cg-si-normal-' });
  assert.equal(res.status, 0);
  assert.match(noticeOf(res), /may be dark/);
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

// ── The SessionStart recent-change blast radius is gone (decision D6) ─────────
// In 2026-09 sessions it was followed up 0 of 35 times, its `affected` command
// never ran, and a comment-only edit reported 291 of 366 files impacted. A
// source check, because the section was assembled from several helpers and any
// one left behind would bring the text back.
test('SessionStart no longer injects a recent-change blast radius', () => {
  const mod = require('./session-init');
  for (const name of ['injectRecentImpact', 'formatRecentImpact', 'shouldInjectRecentImpact']) {
    assert.ok(!(name in mod), `${name} must be gone`);
  }
  const src = fs.readFileSync(path.join(__dirname, 'session-init.js'), 'utf8');
  assert.doesNotMatch(src, /blast radius from the AST index/);
  assert.doesNotMatch(src, /CODE_GRAPH_NO_RECENT_IMPACT/);
});

// Decision D2: an install from before 0.164 left its hooks in settings.json.
// Run as the plugin (Claude Code sets CLAUDE_PLUGIN_ROOT for a hooks.json
// SessionStart), the hook must take them out — hooks.json already carries them,
// and both copies would fire every hook twice — and leave the user's own alone.
test('a plugin SessionStart removes our hooks from settings.json and keeps the user\'s', (t) => {
  const os = require('os');
  const { execFileSync } = require('child_process');
  const sb = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-si-d2-'));
  t.after(() => fs.rmSync(sb, { recursive: true, force: true }));
  const cfg = path.join(sb, '.claude');
  const proj = path.join(sb, 'proj');
  fs.mkdirSync(path.join(cfg, 'plugins'), { recursive: true });
  fs.mkdirSync(proj, { recursive: true });
  fs.writeFileSync(path.join(proj, 'package.json'), '{"name":"p"}');
  const base = { ...process.env, HOME: sb, USERPROFILE: sb, CLAUDE_CONFIG_DIR: cfg, CODE_GRAPH_NO_AUTO_UPDATE: '1' };
  delete base.CLAUDE_PLUGIN_ROOT;
  // The pre-0.164 state: install from a surface hooks.json does not reach.
  execFileSync(process.execPath, [path.join(__dirname, 'lifecycle.js'), 'install'], { env: base, cwd: proj, stdio: 'ignore' });
  const settingsFile = path.join(cfg, 'settings.json');
  const before = JSON.parse(fs.readFileSync(settingsFile, 'utf8'));
  assert.ok(before.hooks && before.hooks.PreToolUse, 'precondition: hooks registered in settings.json');
  before.hooks.PreToolUse.push({ matcher: 'Bash', hooks: [{ type: 'command', command: 'echo mine' }] });
  fs.writeFileSync(settingsFile, JSON.stringify(before, null, 2));

  const si = path.join(__dirname, 'session-init.js');
  const res = JSON.parse(execFileSync(process.execPath, ['-e',
    `process.stdout.write(JSON.stringify(require(${JSON.stringify(si)}).runSessionInit({source:'startup'})))`],
    { env: { ...base, CLAUDE_PLUGIN_ROOT: path.resolve(__dirname, '..') }, cwd: proj }).toString());

  assert.equal(res.lifecycle, 'removed-settings-hooks');
  const after = JSON.parse(fs.readFileSync(settingsFile, 'utf8'));
  assert.deepEqual(after.hooks, { PreToolUse: [{ matcher: 'Bash', hooks: [{ type: 'command', command: 'echo mine' }] }] },
    'only the user hook is left');
  assert.match(after.statusLine && after.statusLine.command, /statusline-composite/, 'the statusline stays');
});
