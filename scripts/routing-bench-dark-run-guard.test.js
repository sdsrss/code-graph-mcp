#!/usr/bin/env node
'use strict';
const { test } = require('node:test');
const assert = require('node:assert');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

// ENG-01 (audit 2026-08-29). The routing bench went dark on 2026-08-02 when its
// `OPENROUTER_API_KEY` secret disappeared, and reported SUCCESS on every weekly
// schedule and all 14 release tags through v0.126.2 before anyone noticed. The
// step already wrote a "SKIPPED — no P@1 was measured" step summary; that was
// not enough, because nobody opens a green run to read its summary. A
// disclosure you have to click through is not a signal.
//
// The unattended triggers now fail instead. This guard is BEHAVIORAL in the
// same sense as test-discovery-drift-guard.test.js: it extracts the step's REAL
// shell script out of the workflow and runs it under bash with the key unset,
// asserting the exit code per trigger. A textual "does the source contain
// exit 1" assertion would pass for a gate wired to the wrong event name, which
// is precisely the mistake worth catching.

const ROOT = path.resolve(__dirname, '..');
const WORKFLOW = path.join(ROOT, '.github/workflows/routing-bench.yml');

/**
 * The `run: |` block of the named step, dedented — the real script, not a copy.
 * Indentation-based rather than a YAML parse so the guard needs no dependency.
 */
function stepScript(stepName) {
  const lines = fs.readFileSync(WORKFLOW, 'utf8').split('\n');
  const nameAt = lines.findIndex((l) => l.trim() === `- name: ${stepName}`);
  assert.ok(nameAt >= 0, `step "${stepName}" not found in ${WORKFLOW}`);
  const runAt = lines.findIndex((l, i) => i > nameAt && l.trim() === 'run: |');
  assert.ok(runAt > nameAt, `step "${stepName}" has no \`run: |\` block`);
  const indent = lines[runAt].match(/^\s*/)[0].length + 2;
  const body = [];
  for (let i = runAt + 1; i < lines.length; i++) {
    const line = lines[i];
    if (line.trim() === '') { body.push(''); continue; }
    if (line.match(/^\s*/)[0].length < indent) break;
    body.push(line.slice(indent));
  }
  const script = body.join('\n');
  assert.ok(
    script.includes('OPENROUTER_API_KEY'),
    'extracted the wrong block — the anchors moved, and a guard that reads the ' +
      'wrong text passes vacuously'
  );
  return script;
}

/** Run the step's script with the key unset, under one trigger. Returns exit code. */
function runWithoutKey({ repository, event }) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-bench-gate-'));
  const env = {
    ...process.env,
    OPENROUTER_API_KEY: '',
    GITHUB_REPOSITORY: repository,
    GITHUB_EVENT_NAME: event,
    GITHUB_OUTPUT: path.join(dir, 'output'),
    GITHUB_STEP_SUMMARY: path.join(dir, 'summary'),
  };
  fs.writeFileSync(env.GITHUB_OUTPUT, '');
  fs.writeFileSync(env.GITHUB_STEP_SUMMARY, '');
  const r = spawnSync('bash', ['-c', stepScript('Run routing benchmark')], {
    env,
    cwd: dir,
    encoding: 'utf8',
  });
  const summary = fs.readFileSync(env.GITHUB_STEP_SUMMARY, 'utf8');
  const output = fs.readFileSync(env.GITHUB_OUTPUT, 'utf8');
  fs.rmSync(dir, { recursive: true, force: true });
  return { code: r.status, summary, output, stdout: r.stdout || '' };
}

test('a missing key FAILS the unattended triggers, where nobody is watching', () => {
  for (const event of ['schedule', 'push']) {
    const r = runWithoutKey({ repository: 'sdsrss/code-graph-mcp', event });
    assert.equal(
      r.code,
      1,
      `on \`${event}\` a missing key must fail the run — a green run list is what hid ` +
        `four weeks and 14 release tags of zero measurement. stdout:\n${r.stdout}`
    );
  }
});

test('a missing key stays green on manual dispatch and on forks', () => {
  const cases = [
    { repository: 'sdsrss/code-graph-mcp', event: 'workflow_dispatch', why: 'a human is reading this output' },
    { repository: 'someone/code-graph-mcp', event: 'schedule', why: 'a fork without the secret is a documented benign no-op' },
    { repository: 'someone/code-graph-mcp', event: 'push', why: 'same, on a fork release tag' },
  ];
  for (const { repository, event, why } of cases) {
    const r = runWithoutKey({ repository, event });
    assert.equal(r.code, 0, `${repository} / ${event} must stay green — ${why}. stdout:\n${r.stdout}`);
  }
});

test('either way the skip is recorded where the result is read', () => {
  const r = runWithoutKey({ repository: 'someone/code-graph-mcp', event: 'schedule' });
  assert.match(r.summary, /no P@1 was measured/, `step summary was:\n${r.summary}`);
  assert.match(r.output, /skipped=true/, `step output was:\n${r.output}`);
});

// Permanent negative control. The gate is one `if` over two env vars; a guard
// that only ran the positive cases would still pass if the condition were
// inverted, or keyed on an event name this workflow never receives.
test('the guard fires when the gate is disarmed', () => {
  const script = stepScript('Run routing benchmark');
  const disarmed = script.replace(/exit 1/, 'exit 0');
  assert.notEqual(disarmed, script, 'negative control cut nothing — the gate no longer exits 1');
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-bench-gate-neg-'));
  const r = spawnSync('bash', ['-c', disarmed], {
    env: {
      ...process.env,
      OPENROUTER_API_KEY: '',
      GITHUB_REPOSITORY: 'sdsrss/code-graph-mcp',
      GITHUB_EVENT_NAME: 'schedule',
      GITHUB_OUTPUT: path.join(dir, 'output'),
      GITHUB_STEP_SUMMARY: path.join(dir, 'summary'),
    },
    cwd: dir,
    encoding: 'utf8',
  });
  fs.rmSync(dir, { recursive: true, force: true });
  assert.equal(
    r.status,
    0,
    'negative control is inert: disarming the gate should make the unattended run go green again, ' +
      'which is the regression the tests above exist to catch'
  );
});
