'use strict';
// JS-12 (audit 2026-08-29): hook entry points had no top-level catch. An
// unhandled throw printed a node stack trace into the user's session and exited
// non-zero — and for a PreToolUse hook a non-zero exit is a DECISION, so a
// crash could read as a verdict.
const test = require('node:test');
const assert = require('node:assert/strict');
const { spawnSync } = require('child_process');
const fs = require('fs');
const path = require('path');

const HERE = __dirname;
const runNode = (code) =>
  spawnSync(process.execPath, ['-e', code], { cwd: HERE, encoding: 'utf8' });

test('a throw after install() exits 0 with one line, not a stack trace', () => {
  const res = runNode(
    `require('./hook-fail-open').installHookFailOpen('PreToolUse:Bash');` +
    `throw Object.assign(new Error('boom'), { code: 'EBOOM' });`
  );
  assert.equal(res.status, 0, 'a crashed hook must not answer with an exit code');
  assert.equal(res.stdout, '', 'and must not emit a half-formed decision on stdout');
  assert.match(res.stderr, /\[code-graph\] PreToolUse:Bash hook error \(EBOOM\): boom/);
  assert.doesNotMatch(res.stderr, /at Object\.<anonymous>|node:internal/,
    `a stack trace is what this replaced; got: ${res.stderr}`);
});

test('an async rejection is covered too — that is the one that actually happens', () => {
  const res = runNode(
    `require('./hook-fail-open').installHookFailOpen('PostToolUse:Bash');` +
    `Promise.reject(new Error('late'));`
  );
  assert.equal(res.status, 0);
  assert.match(res.stderr, /PostToolUse:Bash hook error/);
});

test('EPIPE is silent — the consumer already went away', () => {
  const res = runNode(
    `require('./hook-fail-open').installHookFailOpen('statusLine');` +
    `throw Object.assign(new Error('write EPIPE'), { code: 'EPIPE' });`
  );
  assert.equal(res.status, 0);
  assert.equal(res.stderr, '', `nothing to report to a closed pipe; got: ${res.stderr}`);
});

test('every registered hook entry point installs the fail-open handler', () => {
  // The list is DERIVED from what lifecycle actually registers plus the two
  // statusline entry points, not typed out here: a hand-written list is how a
  // ninth hook joins without one.
  const { buildSettingsHookEntries } = require('./lifecycle');
  // Name pattern accepts `_` and uppercase: the earlier `[a-z0-9-]+` would have
  // skipped such a script in silence, which is the failure mode this whole test
  // exists to prevent (pre-tag review).
  const SCRIPT = /scripts[/\\]([A-Za-z0-9_-]+\.js)/;
  const registered = new Set();
  for (const entries of Object.values(buildSettingsHookEntries())) {
    for (const entry of entries) {
      for (const h of entry.hooks) {
        const m = SCRIPT.exec(h.command);
        if (m) registered.add(m[1]);
      }
    }
  }
  // The plugin-manifest family (SessionStart today) is DERIVED from hooks.json
  // rather than listed here, so a second script added there joins this guard
  // automatically — the CHANGELOG claims a ninth hook cannot join without a
  // handler, and a hardcoded list would not have made that true.
  const manifest = fs.readFileSync(path.join(HERE, '..', 'hooks', 'hooks.json'), 'utf8');
  const fromManifest = [...manifest.matchAll(new RegExp(SCRIPT.source, 'g'))].map((m) => m[1]);
  assert.ok(fromManifest.length >= 1,
    'no hook scripts found in hooks.json — the scan lost its grip on the file');
  for (const f of fromManifest) registered.add(f);
  // statusLine is configured through neither: it is a top-level settings key.
  for (const extra of ['statusline.js', 'statusline-composite.js']) {
    registered.add(extra);
  }

  assert.ok(registered.size >= 9,
    `only ${registered.size} entry points found — the derivation lost its grip: ${[...registered]}`);

  const missing = [...registered].filter((f) => {
    const src = fs.readFileSync(path.join(HERE, f), 'utf8');
    // session-init.js predates this helper and wraps its own main in a
    // try/catch; either mechanism satisfies "does not crash the session".
    return !src.includes('installHookFailOpen') && !/catch \(e\) \{[\s\S]{0,400}hook error/.test(src);
  });
  assert.deepEqual(missing, [],
    `these hook entry points can still exit non-zero with a stack trace: ${missing}`);
});
