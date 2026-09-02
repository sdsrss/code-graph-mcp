'use strict';
const { test } = require('node:test');
const assert = require('node:assert');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { recordRecommendation, REC_FILE } = require('./recommendation-log');

function tmpProject(t, withCodeGraph) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-rec-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  if (withCodeGraph) fs.mkdirSync(path.join(dir, '.code-graph'));
  return dir;
}

test('recordRecommendation appends a JSON line with ts + fields', (t) => {
  const cwd = tmpProject(t, true);
  assert.equal(recordRecommendation(cwd, { hook: 'grep', action: 'deny' }), true);
  const content = fs.readFileSync(path.join(cwd, '.code-graph', REC_FILE), 'utf8');
  const lines = content.trim().split('\n');
  assert.equal(lines.length, 1);
  const rec = JSON.parse(lines[0]);
  assert.equal(rec.hook, 'grep');
  assert.equal(rec.action, 'deny');
  assert.ok(typeof rec.ts === 'string' && rec.ts.length > 0, 'ts should be a timestamp');
});

test('recordRecommendation is a no-op (no dir created) when .code-graph absent', (t) => {
  const cwd = tmpProject(t, false);
  assert.equal(recordRecommendation(cwd, { hook: 'grep', action: 'hint' }), false);
  // Must NOT create the dir or file — zero footprint in non-project cwd.
  assert.equal(fs.existsSync(path.join(cwd, '.code-graph')), false);
});

test('recordRecommendation is a no-op when .code-graph/.no-metrics sentinel present', (t) => {
  const cwd = tmpProject(t, true);
  // Without the sentinel it records normally...
  assert.equal(recordRecommendation(cwd, { hook: 'grep', action: 'deny' }), true);
  const before = fs.readFileSync(path.join(cwd, '.code-graph', REC_FILE), 'utf8');
  // ...then the project marks itself metrics-silent (a dev/dogfood checkout)...
  fs.writeFileSync(path.join(cwd, '.code-graph', '.no-metrics'), '');
  // ...and subsequent recordings are suppressed, leaving the file byte-unchanged.
  assert.equal(recordRecommendation(cwd, { hook: 'grep', action: 'hint' }), false);
  const after = fs.readFileSync(path.join(cwd, '.code-graph', REC_FILE), 'utf8');
  assert.equal(after, before, 'sentinel must suppress further recordings');
});

test('recordRecommendation appends across calls (one line each)', (t) => {
  const cwd = tmpProject(t, true);
  recordRecommendation(cwd, { hook: 'grep', action: 'hint' });
  recordRecommendation(cwd, { hook: 'read', action: 'hint' });
  recordRecommendation(cwd, { hook: 'grep', action: 'deny' });
  const lines = fs.readFileSync(path.join(cwd, '.code-graph', REC_FILE), 'utf8').trim().split('\n');
  assert.equal(lines.length, 3);
  const hooks = lines.map((l) => JSON.parse(l).hook);
  assert.deepEqual(hooks, ['grep', 'read', 'grep']);
});

test('recordRecommendation rotates the file when it exceeds the size cap', (t) => {
  const cwd = tmpProject(t, true);
  const file = path.join(cwd, '.code-graph', REC_FILE);
  // Pre-fill > 1MB of prior events.
  const filler = 'y'.repeat(1024);
  let blob = '';
  for (let i = 0; i < 1200; i++) blob += `{"old":${i},"pad":"${filler}"}\n`;
  fs.writeFileSync(file, blob);
  assert.ok(fs.statSync(file).size > 1048576, 'precondition: file over 1MB');

  // One more recorded event must trigger rotation (rotate-before-append).
  assert.equal(recordRecommendation(cwd, { hook: 'grep', action: 'deny' }), true);

  const size = fs.statSync(file).size;
  assert.ok(size < 600000, `rotated file should be well under 1MB, got ${size}`);
  const lines = fs.readFileSync(file, 'utf8').trim().split('\n');
  // The just-recorded line is last and intact; the first surviving line is whole JSON.
  const last = JSON.parse(lines[lines.length - 1]);
  assert.equal(last.action, 'deny');
  assert.doesNotThrow(() => JSON.parse(lines[0]), 'first surviving line must be a whole JSON line');
});

// JS twin of the Rust `record_cli_use_refuses_to_append_through_a_symlink` /
// `a_symlinked_code_graph_dir_is_refused_before_anything_is_written`
// (src/cli/tests.rs). The Rust pass closed four write sites and left this one
// open; audit 2026-09-02 P1-1 measured a 1,200,020-byte file outside the tree
// dropping to 67 bytes after ONE PreToolUse hook, first line destroyed.
test('recordRecommendation refuses to write through a symlinked jsonl', (t) => {
  const cwd = tmpProject(t, true);
  const victim = path.join(cwd, 'victim.conf');
  const original = 'SECRET-HEADER-LINE\n' + 'x'.repeat(1_200_000) + '\n';
  fs.writeFileSync(victim, original);
  const link = path.join(cwd, '.code-graph', REC_FILE);
  fs.symlinkSync(victim, link);

  assert.equal(recordRecommendation(cwd, { hook: 'read', action: 'observe' }), false);
  // Byte-level: the rotator's `writeFileSync` is what truncated the target.
  assert.equal(fs.readFileSync(victim, 'utf8'), original, 'link target must be byte-identical');
  assert.ok(fs.lstatSync(link).isSymbolicLink(), 'the link itself must be left alone');

  // Positive control in the SAME process: proves the assertions above are not
  // vacuously green because `recordRecommendation` stopped writing at all.
  const plain = tmpProject(t, true);
  assert.equal(recordRecommendation(plain, { hook: 'read', action: 'observe' }), true);
  assert.match(fs.readFileSync(path.join(plain, '.code-graph', REC_FILE), 'utf8'), /"observe"/);
});

test('recordRecommendation refuses a symlinked .code-graph directory', (t) => {
  const cwd = tmpProject(t, false);
  const outside = path.join(cwd, 'outside');
  fs.mkdirSync(outside);
  fs.symlinkSync(outside, path.join(cwd, '.code-graph'));

  // A symlinked dir holding ORDINARY files defeats the per-file guard: the write
  // lands on a real regular file that is simply not where the hook thinks it is.
  assert.equal(recordRecommendation(cwd, { hook: 'read', action: 'observe' }), false);
  assert.deepEqual(fs.readdirSync(outside), [], 'nothing may be written outside the project');
});
