'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { runGrepAnswer, truncateAtLine } = require('./cg-answer');

// Stub "binary": a node script that reacts to its first real arg so one stub
// covers hits / no-hits / error / timeout cases.
let stubDir;
let stubPath;

test.before(() => {
  stubDir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-answer-test-'));
  stubPath = path.join(stubDir, 'cg-stub.js');
  fs.writeFileSync(stubPath, `#!/usr/bin/env node
'use strict';
const pattern = process.argv[3] || '';
if (pattern === 'HangForever') { setTimeout(() => {}, 60000); }
else if (pattern === 'ExplodePlease') { process.exit(3); }
else if (pattern === 'NothingHere') {
  process.stdout.write('[code-graph] No matches for: NothingHere\\n');
} else {
  process.stdout.write(
    'src/storage/db.rs:42  fn ' + pattern + '() {\\n' +
    '  -> fn ' + pattern + ' (lines 42-60)\\n' +
    'args=' + JSON.stringify(process.argv.slice(2)) + '\\n');
}
`);
});

test.after(() => {
  fs.rmSync(stubDir, { recursive: true, force: true });
});

// Wrap the stub so spawnSync can exec it directly: binary = node, leading arg
// trick is not possible (runGrepAnswer controls args), so expose via a shim
// shell-free approach: point binary at node and prepend the script through
// _CG_ANSWER_BINARY handling is binary-only. Instead make the stub itself
// executable with a node shebang and rely on exec.
function stubBinary() {
  fs.chmodSync(stubPath, 0o755);
  return stubPath;
}

test('runGrepAnswer: hits → status hits with stdout text', () => {
  const r = runGrepAnswer({ cwd: stubDir, pattern: 'fts5_search', binary: stubBinary() });
  assert.equal(r.status, 'hits');
  assert.match(r.text, /fn fts5_search/);
});

test('runGrepAnswer: passes grep subcommand, pattern and path as argv', () => {
  const r = runGrepAnswer({
    cwd: stubDir, pattern: 'fts5_search', searchPath: 'src/storage/', binary: stubBinary(),
  });
  assert.equal(r.status, 'hits');
  assert.match(r.text, /args=\["grep","fts5_search","src\/storage\/"\]/);
});

test('runGrepAnswer: omits path argv when no searchPath', () => {
  const r = runGrepAnswer({ cwd: stubDir, pattern: 'fts5_search', binary: stubBinary() });
  assert.match(r.text, /args=\["grep","fts5_search"\]/);
});

test('runGrepAnswer: CLI "[code-graph] No matches" → status no-hits', () => {
  const r = runGrepAnswer({ cwd: stubDir, pattern: 'NothingHere', binary: stubBinary() });
  assert.equal(r.status, 'no-hits');
});

test('runGrepAnswer: nonzero exit → unavailable', () => {
  const r = runGrepAnswer({ cwd: stubDir, pattern: 'ExplodePlease', binary: stubBinary() });
  assert.equal(r.status, 'unavailable');
});

test('runGrepAnswer: missing binary → unavailable', () => {
  const r = runGrepAnswer({ cwd: stubDir, pattern: 'fts5_search', binary: null });
  assert.equal(r.status, 'unavailable');
});

test('runGrepAnswer: nonexistent binary path → unavailable', () => {
  const r = runGrepAnswer({
    cwd: stubDir, pattern: 'fts5_search', binary: path.join(stubDir, 'nope-bin'),
  });
  assert.equal(r.status, 'unavailable');
});

test('runGrepAnswer: timeout → unavailable', () => {
  const r = runGrepAnswer({
    cwd: stubDir, pattern: 'HangForever', binary: stubBinary(), timeoutMs: 300,
  });
  assert.equal(r.status, 'unavailable');
});

test('runGrepAnswer: empty pattern → unavailable (never spawns)', () => {
  const r = runGrepAnswer({ cwd: stubDir, pattern: '', binary: stubBinary() });
  assert.equal(r.status, 'unavailable');
});

test('runGrepAnswer: oversized pattern (>200ch) → unavailable (never spawns)', () => {
  const r = runGrepAnswer({ cwd: stubDir, pattern: 'A'.repeat(201), binary: stubBinary() });
  assert.equal(r.status, 'unavailable');
});

test('runGrepAnswer: long output is truncated with marker', () => {
  // Stub echoes args= line; force truncation via tiny maxBytes
  const r = runGrepAnswer({
    cwd: stubDir, pattern: 'fts5_search', binary: stubBinary(), maxBytes: 30,
  });
  assert.equal(r.status, 'hits');
  assert.equal(r.truncated, true);
  assert.ok(Buffer.byteLength(r.text, 'utf8') <= 30);
});

// ── truncateAtLine (pure) ───────────────────────────────────────────

test('truncateAtLine: under limit → unchanged, not truncated', () => {
  const { text, truncated } = truncateAtLine('a\nb\nc', 100);
  assert.equal(text, 'a\nb\nc');
  assert.equal(truncated, false);
});

test('truncateAtLine: cuts at a line boundary', () => {
  const input = 'line-one\nline-two\nline-three\n';
  const { text, truncated } = truncateAtLine(input, 20);
  assert.equal(truncated, true);
  // 20-byte budget fits 'line-one\nline-two' (17B); the half-cut 'li' is dropped
  assert.equal(text, 'line-one\nline-two');
});

test('truncateAtLine: single oversized line → hard cut', () => {
  const { text, truncated } = truncateAtLine('x'.repeat(50), 10);
  assert.equal(truncated, true);
  assert.equal(Buffer.byteLength(text, 'utf8'), 10);
});
