'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const { emitPreToolContext, emitPreToolAllowContext, emitPostToolContext } = require('./hook-emit');

test('emitPreToolContext carries additionalContext with NO permissionDecision', () => {
  const out = JSON.parse(emitPreToolContext('hello')).hookSpecificOutput;
  assert.equal(out.hookEventName, 'PreToolUse');
  assert.equal(out.additionalContext, 'hello');
  assert.ok(!('permissionDecision' in out),
    'the neutral envelope must not touch the tool call\'s permission flow');
});

test('emitPreToolAllowContext still carries the allow elevation (read-only tools)', () => {
  const out = JSON.parse(emitPreToolAllowContext('hint')).hookSpecificOutput;
  assert.equal(out.permissionDecision, 'allow');
  assert.equal(out.additionalContext, 'hint');
});

test('emitPostToolContext is permission-neutral', () => {
  const out = JSON.parse(emitPostToolContext('answer')).hookSpecificOutput;
  assert.equal(out.hookEventName, 'PostToolUse');
  assert.ok(!('permissionDecision' in out));
});

// ── Drift guard: who may skip the user's permission prompt ──────────────
// `permissionDecision: 'allow'` is documented as "skip the interactive
// permission prompt". A hook that sends it has answered the prompt on the
// user's behalf, so it is defensible ONLY for a read-only tool. pre-edit-guide
// shipped it for Edit — a WRITE — for four releases because nothing enforced the
// boundary (audit 2026-08-16 P0-2). This is that enforcement: adding the allow
// envelope to any other hook fails here, and the new hook's author has to argue
// the case in this list rather than inherit it silently.
const ALLOW_ELEVATION_ALLOWLIST = new Set(['pre-read-guide.js']);

test('only read-only hooks may use the allow+additionalContext envelope', () => {
  const offenders = [];
  for (const name of fs.readdirSync(__dirname)) {
    if (!name.endsWith('.js') || name.endsWith('.test.js')) continue;
    if (name === 'hook-emit.js') continue; // the definition itself
    const src = fs.readFileSync(path.join(__dirname, name), 'utf8');
    // Ignore prose: only a real call/import of the helper counts.
    const uses = /emitPreToolAllowContext\s*[(,}]/.test(src.replace(/^\s*\/\/.*$/gm, ''));
    if (uses && !ALLOW_ELEVATION_ALLOWLIST.has(name)) offenders.push(name);
  }
  assert.deepEqual(offenders, [],
    `these hooks elevate a tool call to auto-allowed: ${offenders.join(', ')}`);
});

test('no hook hand-rolls an allow decision outside hook-emit.js', () => {
  const offenders = [];
  for (const name of fs.readdirSync(__dirname)) {
    if (!name.endsWith('.js') || name.endsWith('.test.js') || name === 'hook-emit.js') continue;
    const src = fs.readFileSync(path.join(__dirname, name), 'utf8').replace(/^\s*\/\/.*$/gm, '');
    if (/permissionDecision\s*:\s*['"]allow['"]/.test(src)) offenders.push(name);
  }
  assert.deepEqual(offenders, [],
    `inline allow envelopes bypass the shared boundary: ${offenders.join(', ')}`);
});
