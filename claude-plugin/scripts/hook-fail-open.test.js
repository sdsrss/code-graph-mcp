'use strict';
// JS-12 (audit 2026-08-29): hook entry points had no top-level catch. An
// unhandled throw printed a node stack trace into the user's session and exited
// non-zero — and for a PreToolUse hook a non-zero exit is a DECISION, so a
// crash could read as a verdict.
const test = require('node:test');
const assert = require('node:assert/strict');
const { spawnSync } = require('child_process');
const fs = require('fs');
const os = require('os');
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

// ── JS-03 (audit 2026-09-05): the process deadline ─────────────────────────
//
// Each hook's internal timeouts were sized alone and run in SERIES, so their
// sum was 2–3× the budget Claude Code kills the hook at. Being killed is not a
// missing hint: PreToolUse reports a hook error on the USER's tool call.

test('remainingMs is bounded by the hook budget, not by the caller default', () => {
  const res = runNode(
    `const h = require('./hook-fail-open');
     h.armHookDeadline('pre-grep-guide.js');   // registered budget: 3 s
     console.log(JSON.stringify({ big: h.remainingMs(60000), small: h.remainingMs(50) }));`
  );
  assert.equal(res.status, 0, `stderr=${res.stderr}`);
  const out = JSON.parse(res.stdout);
  assert.ok(out.big > 0 && out.big <= 3000,
    `a 60 s caller default must be cut to what is left of the 3 s budget; got ${out.big}`);
  assert.equal(out.small, 50,
    'a caller asking for less than the remainder keeps its own tighter bound');
});

test('remainingMs is an integer — child_process rejects a fractional timeout', () => {
  // `process.uptime()` is fractional, so the first version of this returned
  // 2565.27… and every `spawnSync` threw ERR_OUT_OF_RANGE. The runners catch
  // their own exceptions, so the hooks went on exiting 0 while silently
  // answering nothing — a regression that only pre-grep-guide's e2e suite saw.
  const res = runNode(
    `const h = require('./hook-fail-open');
     h.armHookDeadline('pre-grep-guide.js');
     const t = h.remainingMs(60000);
     require('child_process').spawnSync(process.execPath, ['-e', '0'], { timeout: t });
     console.log(String(Number.isInteger(t)));`
  );
  assert.equal(res.status, 0, `a fractional timeout throws here; stderr=${res.stderr}`);
  assert.equal(res.stdout.trim(), 'true');
});

test('an unarmed process (a hook module under test) keeps the caller default', () => {
  const res = runNode(
    `console.log(String(require('./hook-fail-open').remainingMs(60000)));`
  );
  assert.equal(res.stdout.trim(), '60000',
    'requiring a hook in a test must not clamp its spawns to a budget nobody armed');
});

test('remainingMs returns null — not 0 — once the budget is spent', () => {
  const res = runNode(
    `const h = require('./hook-fail-open');
     h.armHookDeadline('pre-grep-guide.js');
     Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 2800);
     console.log(String(h.remainingMs(2000)));`
  );
  assert.equal(res.status, 0, `stderr=${res.stderr}`);
  assert.equal(res.stdout.trim(), 'null',
    'null means DO NOT RUN; a numeric 0 reads as "no timeout" to node, which is ' +
    'exactly the unbounded child this guard exists to prevent');
});

test('serial answers stop spawning instead of outliving the hook budget', () => {
  // The shape from post-grep-inject: callgraph over each symbol in the pattern,
  // then show, then grep — each with cg-answer's own 2 s timeout, all inside a
  // 3 s hook. Before the clamp, three hanging children spent ~6 s.
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-deadline-test-'));
  const stub = path.join(dir, 'hang.js');
  fs.writeFileSync(stub, "#!/usr/bin/env node\n'use strict';\nsetTimeout(() => {}, 60000);\n");
  fs.chmodSync(stub, 0o755);
  try {
    const env = { ...process.env };
    // A sibling test file raises this for its own harness; the budget under
    // test is the product default.
    delete env._CG_ANSWER_TIMEOUT_MS;
    const res = spawnSync(process.execPath, ['-e',
      `const h = require('./hook-fail-open');
       h.armHookDeadline('pre-grep-guide.js');
       const { runGrepAnswer } = require('./cg-answer');
       const t0 = Date.now();
       const seen = [];
       for (let i = 0; i < 3; i++) {
         seen.push(runGrepAnswer({ cwd: process.cwd(), pattern: 'sym' + i,
                                   binary: ${JSON.stringify(stub)} }).status);
       }
       console.log(JSON.stringify({ ms: Date.now() - t0, seen }));`
    ], { cwd: HERE, encoding: 'utf8', env });
    assert.equal(res.status, 0, `stderr=${res.stderr}`);
    const out = JSON.parse(res.stdout);
    assert.deepEqual(out.seen, ['unavailable', 'unavailable', 'unavailable'],
      'a hanging binary degrades to the static path, armed or not');
    assert.ok(out.ms < 3200,
      `three hanging answers must not outlive the 3 s budget; took ${out.ms} ms`);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
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
