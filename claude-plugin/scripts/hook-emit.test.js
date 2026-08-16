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

// P2 (2026-08-16 audit §四): the three injection hooks had no byte ceiling while
// `cg-answer.js`, which emits alongside them, has capped at 4000 since it was
// written. Their payloads are built from unbounded lists — pre-edit-guide joins
// every direct caller's `name (file)` onto one line — so editing a heavily-called
// symbol pushed a multi-kilobyte wall into the model's context on every Edit.
test('injected context is capped, on every envelope, with the cut announced', () => {
  const { capContext, MAX_INJECTED_BYTES, emitPreToolContext, emitPreToolAllowContext, emitPostToolContext } =
    require('./hook-emit');

  // Under the cap: byte-identical passthrough. Without this the cap could be a
  // rewriter that mangles ordinary payloads.
  const small = '[code-graph:impact] foo() — Risk: LOW\n  1 direct caller\n';
  assert.equal(capContext(small), small);

  const huge = Array.from({ length: 2000 }, (_, i) => `  caller_${i} (src/a/very/long/path/file_${i}.ts)`).join('\n');
  for (const [name, emit] of [
    ['PreToolUse', emitPreToolContext],
    ['PreToolUse allow', emitPreToolAllowContext],
    ['PostToolUse', emitPostToolContext],
  ]) {
    const ctx = JSON.parse(emit(huge)).hookSpecificOutput.additionalContext;
    assert.ok(
      Buffer.byteLength(ctx, 'utf8') <= MAX_INJECTED_BYTES,
      `${name}: ${Buffer.byteLength(ctx, 'utf8')} bytes exceeds the ${MAX_INJECTED_BYTES} cap`,
    );
    assert.match(ctx, /truncated at \d+ bytes/, `${name}: a silent cut is worse than an announced one`);
  }

  // Multi-byte safety: paths and symbol names are not always ASCII, and a cut
  // through a codepoint would inject U+FFFD into the model's context.
  const cjk = '符号'.repeat(5000);
  const cut = capContext(cjk);
  assert.ok(Buffer.byteLength(cut, 'utf8') <= MAX_INJECTED_BYTES);
  assert.ok(!cut.includes('�'), 'must not slice through a multi-byte codepoint');
});
