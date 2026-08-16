'use strict';
// The composite is the registered statusLine command: it receives Claude Code's
// JSON context on stdin and fans out to each provider. This pins the cwd bridge:
// the code-graph provider keys its gate on process.cwd(), but Claude Code may
// spawn the statusline from a cwd unrelated to the session. The composite must
// extract the authoritative cwd from stdin and forward it (CODE_GRAPH_STATUSLINE_CWD)
// so the provider resolves the right project regardless of the spawn's cwd.
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { cwdFromStdin, runProvider } = require('./statusline-composite');

test('cwdFromStdin reads the top-level cwd field', () => {
  assert.equal(cwdFromStdin('{"cwd":"/a/b"}'), '/a/b');
});

test('cwdFromStdin falls back to workspace.current_dir', () => {
  assert.equal(cwdFromStdin('{"workspace":{"current_dir":"/c/d"}}'), '/c/d');
});

test('cwdFromStdin prefers top-level cwd over workspace.current_dir', () => {
  assert.equal(cwdFromStdin('{"cwd":"/a","workspace":{"current_dir":"/c"}}'), '/a');
});

test('cwdFromStdin returns null for empty / non-JSON / cwd-less payloads', () => {
  assert.equal(cwdFromStdin(''), null);
  assert.equal(cwdFromStdin('not json'), null);
  assert.equal(cwdFromStdin('{}'), null);
  assert.equal(cwdFromStdin('{"workspace":{}}'), null);
});

test('cwdFromStdin returns null for a non-string cwd (no bogus env path)', () => {
  // A malformed payload must not coerce a number/object into an env path that
  // resolves to nowhere and silently blanks the segment. Only a real string wins.
  assert.equal(cwdFromStdin('{"cwd":123}'), null);
  assert.equal(cwdFromStdin('{"cwd":{"x":1}}'), null);
  assert.equal(cwdFromStdin('{"cwd":""}'), null);
  assert.equal(cwdFromStdin('{"workspace":{"current_dir":42}}'), null);
});

test('runProvider forwards the stdin cwd to the provider as CODE_GRAPH_STATUSLINE_CWD', (t) => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-composite-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const fixture = path.join(dir, 'echo-cwd.js');
  fs.writeFileSync(fixture, "process.stdout.write('CWD='+(process.env.CODE_GRAPH_STATUSLINE_CWD||'NONE'));");
  const out = runProvider(`node ${JSON.stringify(fixture)}`, false, '{"cwd":"/x/y"}');
  assert.equal(out, 'CWD=/x/y');
});

// ── Timeout kill signal (P1-17) ────────────────────────────────────────────
// `execFileSync(..., { timeout })` sends `killSignal` (default SIGTERM) and then
// WAITS for the child to die. A provider that traps SIGTERM therefore never
// returns: the statusline command hangs on every frame, every provider's segment
// (ours included) disappears, and the zombies accumulate. SIGKILL cannot be
// trapped, so the timeout stays a timeout.
test('runProvider returns even when the provider ignores SIGTERM (timeout must KILL)', (t) => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-composite-sigterm-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const fixture = path.join(dir, 'deaf-provider.js');
  fs.writeFileSync(fixture, [
    "process.on('SIGTERM', () => {});",   // trap and ignore, exactly like a shell `trap '' TERM`
    "process.on('SIGINT', () => {});",
    "setInterval(() => {}, 1000);",       // stay alive forever
  ].join('\n'));

  const started = Date.now();
  const out = runProvider(`node ${JSON.stringify(fixture)}`, false, '');
  const elapsed = Date.now() - started;

  assert.equal(out, null, 'a hung provider contributes no segment');
  assert.ok(elapsed < 6000,
    `runProvider must give up at its 3s timeout, not hang: took ${elapsed}ms`);
}, { timeout: 20000 });

test('a deaf provider does not take the other segments down with it', (t) => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-composite-mixed-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const deaf = path.join(dir, 'deaf.js');
  fs.writeFileSync(deaf, "process.on('SIGTERM', () => {});\nsetInterval(() => {}, 1000);");
  const ours = path.join(dir, 'ours.js');
  fs.writeFileSync(ours, "process.stdout.write('code-graph: ok');");

  const started = Date.now();
  const dead = runProvider(`node ${JSON.stringify(deaf)}`, false, '');
  const alive = runProvider(`node ${JSON.stringify(ours)}`, false, '');
  const elapsed = Date.now() - started;

  assert.equal(dead, null);
  assert.equal(alive, 'code-graph: ok', 'our own segment must still render');
  assert.ok(elapsed < 8000, `both providers together took ${elapsed}ms`);
}, { timeout: 20000 });

test('runProvider leaves CODE_GRAPH_STATUSLINE_CWD unset when stdin carries no cwd', (t) => {
  // Hermetic against an ambient var: with no stdin cwd, runProvider passes
  // process.env through unchanged, so a value inherited by the test runner would
  // leak into the child. Clear it for this case, restore after.
  const saved = process.env.CODE_GRAPH_STATUSLINE_CWD;
  delete process.env.CODE_GRAPH_STATUSLINE_CWD;
  t.after(() => { if (saved !== undefined) process.env.CODE_GRAPH_STATUSLINE_CWD = saved; });
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-composite-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const fixture = path.join(dir, 'echo-cwd.js');
  fs.writeFileSync(fixture, "process.stdout.write('CWD='+(process.env.CODE_GRAPH_STATUSLINE_CWD||'NONE'));");
  const out = runProvider(`node ${JSON.stringify(fixture)}`, false, '');
  assert.equal(out, 'CWD=NONE');
});
