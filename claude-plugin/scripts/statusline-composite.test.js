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
const { cwdFromStdin, runProvider, parseCommand, needsShell } = require('./statusline-composite');

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

// --- provider command splitting (audit 2026-08-22 P2-9) --------------------
//
// Claude Code runs `statusLine.command` through a shell; the composite runs it
// through `execFileSync`, so the composite has to do the splitting itself. It
// used to be one regex that understood exactly one double-quoted word after the
// executable and `split(/\s+/)` for everything else — so a `_previous` command
// whose path contains a space was torn apart, `execFileSync` threw ENOENT, and
// the catch swallowed it. The user's original statusline disappeared silently,
// which is the one thing the `_previous` slot exists to prevent.

test('parseCommand keeps quoted paths whole, wherever they appear', () => {
  assert.deepEqual(parseCommand('node /path/x.js'), ['node', '/path/x.js']);
  assert.deepEqual(
    parseCommand('node "/path with space/x.js"'),
    ['node', '/path with space/x.js'],
  );
  // A quoted EXECUTABLE — the old regex required the quote to come second.
  assert.deepEqual(
    parseCommand('"/Program Files/tools/line.exe" --a b'),
    ['/Program Files/tools/line.exe', '--a', 'b'],
  );
  // A quoted argument that is not the first — likewise unreachable before.
  assert.deepEqual(
    parseCommand('node /x.js --label "my project"'),
    ['node', '/x.js', '--label', 'my project'],
  );
  assert.deepEqual(
    parseCommand("node '/single quoted/x.js'"),
    ['node', '/single quoted/x.js'],
  );
  assert.deepEqual(parseCommand('node /x.js  --a   --b'), ['node', '/x.js', '--a', '--b']);
});

test('parseCommand leaves Windows path separators alone', () => {
  // A general backslash escape would turn this into `C:Usersmeline.exe` —
  // a repair that breaks the platform it did not test on. Backslash escapes
  // only space, quote and backslash.
  assert.deepEqual(
    parseCommand('C:\\Users\\me\\line.exe --a'),
    ['C:\\Users\\me\\line.exe', '--a'],
  );
  assert.deepEqual(parseCommand('node /my\\ dir/x.js'), ['node', '/my dir/x.js']);
});

test('parseCommand refuses to guess at an unterminated quote', () => {
  assert.equal(parseCommand('node "/unterminated'), null);
  assert.equal(parseCommand(''), null);
  assert.equal(parseCommand('   '), null);
});

test('needsShell fires on constructs execFileSync cannot run, not on paths', () => {
  if (process.platform === 'win32') return; // no `sh` there; direct path only
  assert.equal(needsShell('foo | cut -c1-40'), true);
  assert.equal(needsShell('a && b'), true);
  assert.equal(needsShell('echo $(date)'), true);
  assert.equal(needsShell('x > /tmp/y'), true);
  // Ordinary commands and paths must stay on the direct-exec path, which is
  // where the timeout's SIGKILL definitely reaches the provider itself.
  assert.equal(needsShell('node "/path with space/x.js"'), false);
  assert.equal(needsShell('C:\\Users\\me\\line.exe'), false);
  assert.equal(needsShell('~/bin/line.sh --flag'), false);
});

test('runProvider runs a provider whose path contains a space', (t) => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-statusline-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const spaced = path.join(dir, 'my provider');
  fs.mkdirSync(spaced);
  const fixture = path.join(spaced, 'line.js');
  fs.writeFileSync(fixture, "process.stdout.write('SEGMENT');");
  assert.equal(runProvider(`node "${fixture}"`, false, ''), 'SEGMENT');
});

test('runProvider runs a piped provider command', (t) => {
  if (process.platform === 'win32') { t.skip('no /bin/sh'); return; }
  // Before, this was split into ['echo','hello','|','tr',…] and threw ENOENT.
  assert.equal(runProvider('echo hello | tr a-z A-Z', false, ''), 'HELLO');
});
