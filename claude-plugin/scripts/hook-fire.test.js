'use strict';
// Layer-A "does the hook really fire" smoke test (v0.67.0). Distinct from
// hooks.test.js (which inspects registration STRINGS) and the per-hook unit
// tests (which import predicates): this spawns each REGISTERED hook script the
// way Claude Code would — node + a synthetic CC stdin payload — and asserts it
// runs end-to-end without erroring. Catches the "registered but inert on this
// machine" class (broken require-chain, node-version, corrupt install) that
// string/predicate tests can't see. See feedback_pretooluse_dark_under_green_health.md.
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { spawnSync } = require('child_process');
const { verifyHooksFire } = require('./lifecycle');
const { hookFireWarning, analyzeHookDark } = require('./session-init');

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

test('verifyHooksFire: all real registered hooks run cleanly (exit 0)', () => {
  const { ok, results } = verifyHooksFire();
  // 3 PreToolUse + 2 PostToolUse (incremental-index + compound-grep inject) + 1 UserPromptSubmit = 6 settings.json hooks
  assert.ok(results.length >= 6, `expected >=6 hook probes, got ${results.length}`);
  for (const r of results) {
    assert.ok(r.ok, `hook ${r.label} (${r.script}) did not fire cleanly: code=${r.code} err=${r.error}`);
  }
  assert.equal(ok, true);
});

test('verifyHooksFire: the grep hook actually engages (emits a decision)', () => {
  const { results } = verifyHooksFire();
  const grep = results.find(r => /pre-grep-guide/.test(r.script));
  assert.ok(grep, 'no grep hook probe found');
  assert.ok(grep.emitted,
    'pre-grep-guide produced no output on an engaging grep payload — the firing path did not engage');
});

// The sibling of the assertion above, and the one that was missing. `ok` is
// `exit === 0`, and a hook that reads a field the host does not send exits 0 in
// silence — so this probe reported the UserPromptSubmit surface healthy for its
// entire dead lifetime while `emitted` sat right there in the same result object,
// recorded and unread. The payload was never the problem: `hookFirePayload('')`
// has always returned `{prompt:…}`, the shape Claude Code really sends
// (audit 2026-08-29 JS-01).
test('verifyHooksFire: the UserPromptSubmit hook actually engages (emits context)', () => {
  const { results } = verifyHooksFire();
  const upc = results.find(r => /user-prompt-context/.test(r.script));
  assert.ok(upc, 'no user-prompt-context hook probe found');
  assert.ok(upc.emitted,
    'user-prompt-context produced no output on a real UserPromptSubmit payload — ' +
    'the injection surface is dark, and exit 0 does not say so');
});

test('verifyHooksFire: reports a broken hook script (teeth)', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-hookfire-teeth-'));
  const broken = path.join(dir, 'broken-hook.js');
  fs.writeFileSync(broken, 'throw new Error("boom at runtime");\n');
  try {
    const { ok, results } = verifyHooksFire({ hooks: [{ label: 'broken', script: broken, payload: {} }] });
    assert.equal(ok, false, 'a hook that throws must make ok=false');
    assert.equal(results[0].ok, false);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('verifyHooksFire: missing hook script is reported, not thrown (teeth)', () => {
  const { ok, results } = verifyHooksFire({
    hooks: [{ label: 'gone', script: path.join(os.tmpdir(), 'definitely-not-here-xyz.js'), payload: {} }],
  });
  assert.equal(ok, false);
  assert.equal(results[0].ok, false);
});

// ── Layer A surface: hookFireWarning (pure interpreter of cached state) ──

test('hookFireWarning: ok / absent state → no warning', () => {
  assert.equal(hookFireWarning({ ok: true, failures: [] }), null);
  assert.equal(hookFireWarning(null), null);
  assert.equal(hookFireWarning({ ok: false, failures: [] }), null); // no names → nothing to say
});

test('hookFireWarning: failed state names the failed hook + points to doctor', () => {
  const w = hookFireWarning({ ok: false, failures: ['PreToolUse:Bash'] });
  assert.match(w, /PreToolUse:Bash/);
  assert.match(w, /doctor/);
});

// ── Layer B dispatch canary: analyzeHookDark (pure) ──

test('analyzeHookDark: edit fires repeatedly but grep/read never → warns', () => {
  const lines = ['{"hook":"edit"}', '{"hook":"edit"}', '{"hook":"edit"}'].join('\n');
  assert.match(analyzeHookDark(lines), /grep\/read/);
});

test('analyzeHookDark: any grep/read event present → no warning', () => {
  const lines = ['{"hook":"edit"}', '{"hook":"edit"}', '{"hook":"edit"}', '{"hook":"read","action":"observe"}'].join('\n');
  assert.equal(analyzeHookDark(lines), null);
});

test('analyzeHookDark: below the edit threshold / empty → no warning (low false-positive)', () => {
  assert.equal(analyzeHookDark('{"hook":"edit"}\n{"hook":"edit"}'), null);
  assert.equal(analyzeHookDark(''), null);
  assert.equal(analyzeHookDark('garbage\n{not json}'), null);
});

// ── CLI wiring: `lifecycle.js verify-hooks-fire` writes the state file ──

test('CLI verify-hooks-fire runs and writes hook-fire-state.json (HOME-redirected)', () => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-hf-home-'));
  try {
    const r = spawnSync(process.execPath, [path.join(__dirname, 'lifecycle.js'), 'verify-hooks-fire'], {
      env: { ...process.env, HOME: home }, encoding: 'utf8', timeout: 30000,
    });
    assert.equal(r.status, 0, `CLI exit ${r.status}: ${r.stderr}`);
    assert.match(r.stdout, /Hook firing: (OK|FAIL)/);
    const statePath = path.join(home, '.cache', 'code-graph', 'hook-fire-state.json');
    assert.ok(fs.existsSync(statePath), 'hook-fire-state.json was not written');
    const state = JSON.parse(fs.readFileSync(statePath, 'utf8'));
    assert.equal(typeof state.ok, 'boolean');
    assert.ok(state.ts, 'state missing timestamp');
  } finally {
    fs.rmSync(home, { recursive: true, force: true });
  }
});

// ── doctor wiring: runDiagnostics surfaces a "Hook firing" check ──

test('doctor runDiagnostics includes a Hook firing check', () => {
  const { runDiagnostics } = require('./doctor');
  const results = runDiagnostics();
  const hf = results.find(r => r.name === 'Hook firing');
  assert.ok(hf, 'doctor did not report a "Hook firing" check');
  assert.ok(['ok', 'warn'].includes(hf.status), `unexpected status ${hf.status}`);
});
