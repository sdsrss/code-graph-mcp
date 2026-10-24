'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync, execFileSync } = require('node:child_process');

// These tests redirect HOME for their children; CLAUDE_CONFIG_DIR would win
// over it (claude-config.js) and point them at the real config. Dropped at
// module load so every spawn below inherits the sandbox, not the live dir.
delete process.env.CLAUDE_CONFIG_DIR;

const {
  extractSignatures, allSignatures, definitionHeaders, signatureChanged, findCallSiteLine, computeStopReport,
  formatStopContext, followUpOf, reportRecords, MAX_FOLLOWUP_FILES,
} = require('./stop-impact');

// --- extractSignatures ------------------------------------------------------

test('extractSignatures: Rust header up to the brace, whitespace-insensitive', () => {
  const a = extractSignatures('pub fn compute(x: i32) -> i32 {\n    x + 1\n}\n', 'compute', '.rs');
  const b = extractSignatures('pub fn compute(\n    x: i32,\n) -> i32 {\n    x + 2\n}\n', 'compute', '.rs');
  assert.deepEqual(a, ['pubfncompute(x:i32)->i32']);
  assert.deepEqual(a, b, 'a formatter re-wrapping the params is not a signature change');
  const c = extractSignatures('pub fn compute(x: i32, y: i32) -> i32 {\n}\n', 'compute', '.rs');
  assert.ok(signatureChanged(a, c));
  const ret = extractSignatures('pub fn compute(x: i32) -> std::io::Result<i32> {\n}\n', 'compute', '.rs');
  assert.ok(signatureChanged(a, ret), 'a return-type change counts, `::` included');
});

test('extractSignatures: Python stops at the depth-0 colon, annotations kept', () => {
  const a = extractSignatures('def load(path: str) -> dict:\n    return {}\n', 'load', '.py');
  assert.deepEqual(a, ['defload(path:str)->dict']);
  const b = extractSignatures('def load(path: str, strict: bool) -> dict:\n    pass\n', 'load', '.py');
  assert.ok(signatureChanged(a, b, 'load'));
});

test('signatureChanged: optional parameters appended at the end break no caller (Q2)', () => {
  const a = extractSignatures('def load(path: str) -> dict:\n    return {}\n', 'load', '.py');
  const b = extractSignatures('def load(path: str, strict: bool = False) -> dict:\n    pass\n', 'load', '.py');
  assert.equal(signatureChanged(a, b, 'load'), false);
  // Without the symbol there is no parameter list to locate: every difference counts.
  assert.equal(signatureChanged(a, b), true);
  // Matched per definition: one compatible extension per old header.
  const two = ['defload(a)', 'defload(b)'];
  assert.equal(signatureChanged(two, ['defload(a,x=1)', 'defload(b)'], 'load'), false);
  assert.equal(signatureChanged(two, ['defload(a,x=1)'], 'load'), true, 'the other definition is gone');
  assert.equal(signatureChanged(['defload(a)', 'defload(a)'], ['defload(a,x=1)'], 'load'), true,
    'one new header extends one old header, not two');
});

test('extractSignatures: JS function, arrow binding and class method; calls are not definitions', () => {
  const src = [
    'function parse(a) {', '  return a;', '}',
    'const build = (x, y) => x + y;',
    'class K {', '  render(props) {', '    return parse(props);', '  }', '}',
    'parse(1);', 'render(() => {', '});', 'return build(1, 2);',
  ].join('\n');
  assert.deepEqual(extractSignatures(src, 'parse', '.js'), ['functionparse(a)']);
  assert.deepEqual(extractSignatures(src, 'build', '.js'), ['constbuild=(x,y)']);
  assert.deepEqual(extractSignatures(src, 'render', '.js'), ['render(props)']);
});

test('extractSignatures: a match on a line over 2,000 characters gives no reading (pre-tag review H2)', () => {
  const head = 'function abc(t) {';
  const withLine = (n) => head + ' '.repeat(n - head.length) + '\n  return t;\n}\n';
  assert.equal(withLine(2000).indexOf('\n'), 2000);
  assert.deepEqual(extractSignatures(withLine(2000), 'abc', '.js'), ['functionabc(t)']);
  assert.equal(extractSignatures(withLine(2001), 'abc', '.js'), null);
  // A long line without a match does not matter.
  assert.deepEqual(extractSignatures('x'.repeat(5000) + '\n' + withLine(20), 'abc', '.js'), ['functionabc(t)']);
  // A one-line 1.5 MB bundle is refused at its first match instead of
  // re-finding its line per match (1ffc45c: about 9 s of CPU here).
  const unit = 'function abc(t){return t+1}var a=abc(1);';
  const bundle = unit.repeat(Math.ceil((1536 * 1024) / unit.length));
  const t0 = process.cpuUsage();
  const got = extractSignatures(bundle, 'abc', '.js');
  const d = process.cpuUsage(t0);
  assert.equal(got, null);
  const ms = (d.user + d.system) / 1e3;
  assert.ok(ms < 1000, `1.5 MB one-line bundle took ${ms.toFixed(0)} ms of CPU`);
});

// Q1: an edit that names no definition (Write, `sed -i`) baselines every
// definition in the file at once. One pass per pattern, and for every name it
// finds the same reading as extractSignatures for that name.
const ALL_FIXTURES = {
  '.py': 'import os\n\ndef load(path, strict=False):\n    return read(path)\n\nclass K:\n    def load(self, p):\n        if p:\n            return 1\n\n    async def fetch(self):\n        pass\n\n# def commented(x):\nx = load(1)\n',
  '.ts': 'export function parse(a: string): T {\n  if (a) {\n    return build(a);\n  }\n}\nconst build = (x, y) => x + y;\nclass K {\n  render(props) {\n    for (const p of props) {\n    }\n    return parse(props);\n  }\n}\nwhile (x) {\n}\n',
  '.rs': 'pub fn compute(x: i32) -> i32 {\n    helper(x)\n}\nimpl From<A> for X {\n    fn from(a: A) -> X { X }\n}\nimpl From<B> for X {\n    fn from(b: B) -> X { X }\n}\n',
  '.go': 'func (s *S) Compute(x int) (int, error) {\n}\n\nfunc helper() {\n}\n',
  '.java': '  public int compute(int x) {\n    return helper(x);\n  }\n  private static String name() {\n    return "a";\n  }\n',
};
const ALL_WANT = {
  '.py': ['fetch', 'load'],
  '.ts': ['build', 'parse', 'render'],
  '.rs': ['compute', 'from'],
  '.go': ['Compute', 'helper'],
  '.java': ['compute', 'name'],
};

for (const [ext, text] of Object.entries(ALL_FIXTURES)) {
  test(`allSignatures ${ext}: every definition, and each reads as extractSignatures reads it`, () => {
    const all = allSignatures(text, ext);
    assert.deepEqual([...all.keys()].sort(), ALL_WANT[ext]);
    for (const [name, sigs] of all) assert.deepEqual(sigs, extractSignatures(text, name, ext), name);
  });
}

test('allSignatures: names come in file order, whichever pattern found them', () => {
  const text = 'class K {\n  render(p) {\n  }\n}\nconst build = (x) => x;\nfunction parse(a) {\n}\n';
  assert.deepEqual([...allSignatures(text, '.js').keys()], ['render', 'build', 'parse']);
});

test('allSignatures: a header with no reading leaves out that name only', () => {
  const text = 'fn compute(' + 'a: u8, '.repeat(120) + ') {}\nfn other(x: u8) {}\n';
  assert.equal(extractSignatures(text, 'compute', '.rs'), null);
  assert.deepEqual([...allSignatures(text, '.rs')], [['other', ['fnother(x:u8)']]]);
});

// pre-edit-guide reads which of a hunk's definitions an edit changed, and
// where each starts (tasks/specs/pre-edit-pick-by-reader.md).
test('definitionHeaders: every definition in text order, with its line start and key', () => {
  const text = 'class A:\n    def fetch(self, k):\n        return k\nclass B:\n    def fetch(self, k, d=None):\n';
  const r = definitionHeaders(text, '.py');
  assert.deepEqual(r.defs, [
    { name: 'fetch', at: text.indexOf('    def fetch(self, k)'), key: 'deffetch(self,k)' },
    { name: 'fetch', at: text.indexOf('    def fetch(self, k, d'), key: 'deffetch(self,k,d=None)' },
  ]);
  assert.equal(r.keywordOnly, true);
  for (const [name, sigs] of allSignatures(text, '.py')) {
    assert.deepEqual(r.defs.filter((d) => d.name === name).map((d) => d.key).sort(), sigs, 'the keys allSignatures reads');
  }
});

test('definitionHeaders: keywordOnly only where every definition carries the keyword', () => {
  for (const ext of ['.rs', '.py', '.pyi', '.go', '.php', '.scala', '.sc', '.rb', '.lua']) {
    assert.equal(definitionHeaders('', ext).keywordOnly, true, ext);
  }
  // JS/TS read one-line methods only; C-family has no keyword; Kotlin's
  // `fun String.x()` is not read.
  for (const ext of ['.js', '.ts', '.tsx', '.mjs', '.c', '.cpp', '.java', '.cs', '.kt', '.kts']) {
    assert.equal(definitionHeaders('', ext).keywordOnly, false, ext);
  }
});

test('definitionHeaders: text order across names, not grouped by name', () => {
  const text = 'fn alpha(a: u8) {}\nfn beta(b: u8) {}\nfn alpha(c: u16) {}\n';
  assert.deepEqual(definitionHeaders(text, '.rs').defs.map((d) => d.name), ['alpha', 'beta', 'alpha']);
});

test('definitionHeaders: keywordLeads reads the keyword right before a name', () => {
  const rs = definitionHeaders('', '.rs');
  for (const before of ['fn ', '    pub(crate) async fn ', '#[inline] pub fn ', 'impl Conv for Cfg { fn ', '"fn ']) {
    assert.equal(rs.keywordLeads(before), true, before);
  }
  for (const before of ['    match ', '    if !x.', '    let v = ', '', 'fnord ', 'xfn ']) {
    assert.equal(rs.keywordLeads(before), false, before);
  }
  assert.equal(definitionHeaders('', '.go').keywordLeads('func (s *Server) '), true, 'Go receiver');
  assert.equal(definitionHeaders('', '.go').keywordLeads('go '), false);
  assert.equal(definitionHeaders('', '.scala').keywordLeads('class '), true, 'a Scala class constructor');
  assert.equal(definitionHeaders('', '.rb').keywordLeads('items.'), false, 'a call with a block');
  assert.equal(definitionHeaders('', '.php').keywordLeads('#[Route("/x")] public function '), true);
  assert.equal(definitionHeaders('', '.cpp').keywordLeads('static int '), false, 'no keyword in C-family');
});

test('definitionHeaders: typed only for the C-family profile', () => {
  for (const ext of ['.c', '.h', '.cpp', '.hpp', '.java', '.cs']) assert.equal(definitionHeaders('', ext).typed, true, ext);
  for (const ext of ['.rs', '.py', '.js', '.ts', '.go', '.kt', '.rb']) assert.equal(definitionHeaders('', ext).typed, false, ext);
});

test('definitionHeaders: no reading when any header has none, the language is not read, or a line is over-long', () => {
  assert.equal(definitionHeaders('fn compute(' + 'a: u8, '.repeat(120) + ') {}\nfn other(x: u8) {}\n', '.rs'), null);
  assert.equal(definitionHeaders('function f() {}\n', '.sh'), null);
  assert.equal(definitionHeaders('function abc(t){return t+1}' + 'var a=1;'.repeat(400), '.js'), null);
});

test('allSignatures: no reading for an unsupported language or a one-line bundle', () => {
  assert.equal(allSignatures('def run(a), do: a\n', '.ex'), null);
  const unit = 'function abc(t){return t+1}var a=abc(1);';
  const bundle = unit.repeat(Math.ceil((1536 * 1024) / unit.length));
  const t0 = process.cpuUsage();
  assert.equal(allSignatures(bundle, '.js'), null);
  const d = process.cpuUsage(t0);
  assert.ok((d.user + d.system) / 1e3 < 1000, 'refused at its first match, not scanned per match');
  assert.deepEqual([...allSignatures('', '.py').keys()], []);
});

test('extractSignatures: comments and absent symbols yield nothing', () => {
  assert.deepEqual(extractSignatures('// fn compute(x: i32) {\n', 'compute', '.rs'), []);
  assert.deepEqual(extractSignatures('fn other() {}\n', 'compute', '.rs'), []);
  assert.deepEqual(extractSignatures(null, 'compute', '.rs'), []);
});

test('findCallSiteLine: first mention at or after the caller start, else the start', () => {
  const text = 'use crate::a::compute;\n\npub fn caller_b() -> i32 {\n    let v = 2;\n    compute(v)\n}\n';
  assert.equal(findCallSiteLine(text, 'compute', 3), 5);
  assert.equal(findCallSiteLine(text, 'nothing_here', 3), 3);
  assert.equal(findCallSiteLine(null, 'compute', 7), 7);
});

// --- computeStopReport (IO injected) ----------------------------------------

const HEAD_A = 'pub fn compute(x: i32) -> i32 {\n    x + 1\n}\n';
const SIG_A = 'pub fn compute(x: i32, y: i32) -> i32 {\n    x + y\n}\n';
const BODY_A = 'pub fn compute(x: i32) -> i32 {\n    x + 2\n}\n';
// What pre-edit-guide records with a symbol: its signatures before that Edit.
const sigsOf = (text) => extractSignatures(text, 'compute', '.rs');

function world({ work = SIG_A, mtimes = {}, refs } = {}) {
  const calls = [];
  return {
    calls,
    workText: (f) => (f === 'src/a.rs' ? work : null),
    callers: (symbol, file) => { calls.push(symbol); return refs || [{ file: 'src/b.rs', line: 5, name: 'caller_b' }]; },
    mtimeMs: (f) => (f in mtimes ? mtimes[f] : 1000),
  };
}
const EMPTY = { lastStopAt: null, reported: [] };
const EDIT_A = [
  { ts: 5000, file: 'src/a.rs', symbol: null, sigs: null },
  { ts: 5001, file: 'src/a.rs', symbol: 'compute', sigs: sigsOf(HEAD_A) },
];

test('stop: signature change in a.rs with an untouched caller in b.rs → one b.rs:line line', () => {
  const r = computeStopReport({ edits: EDIT_A, state: EMPTY, now: 9000, ...world() });
  assert.deepEqual(r.lines, ['  compute() in src/a.rs: src/b.rs:5 (caller_b)']);
  assert.deepEqual(r.state, { lastStopAt: 9000, reported: ['src/a.rs#compute'] });
  const text = formatStopContext(r.lines);
  assert.match(text, /src\/b\.rs:5/);
  assert.doesNotMatch(text, /\b(?:must|should|you)\b/i, 'facts only');
});

test('stop: caller file also edited this turn → silent (logged edit, or mtime inside the turn)', () => {
  const logged = [...EDIT_A, { ts: 5002, file: 'src/b.rs', symbol: null, sigs: null }];
  assert.deepEqual(computeStopReport({ edits: logged, state: EMPTY, now: 9000, ...world() }).lines, []);
  // Not logged (Write, `sed -i`, a formatter) but modified after the turn began.
  const viaMtime = world({ mtimes: { 'src/b.rs': 6000 } });
  assert.deepEqual(computeStopReport({ edits: EDIT_A, state: EMPTY, now: 9000, ...viaMtime }).lines, []);
});

test('stop: mtime exactly at the turn start counts as touched', () => {
  const r = computeStopReport({ edits: EDIT_A, state: { lastStopAt: 4000, reported: [] }, now: 9000,
    ...world({ mtimes: { 'src/b.rs': 4000 } }) });
  assert.deepEqual(r.lines, []);
});

test('stop: the first turn starts at the session\'s FIRST logged edit, not its last', () => {
  // b.rs changed between the first and the last edit of the turn: touched.
  const edits = [...EDIT_A, { ts: 7000, file: 'src/c.rs', symbol: null, sigs: null }];
  const r = computeStopReport({ edits, state: EMPTY, now: 9000, ...world({ mtimes: { 'src/b.rs': 6000 } }) });
  assert.deepEqual(r.lines, []);
});

test('stop: second Stop in the same session → silent, even after a new edit of the symbol', () => {
  const first = computeStopReport({ edits: EDIT_A, state: EMPTY, now: 9000, ...world() });
  assert.equal(first.lines.length, 1);
  const again = computeStopReport({ edits: EDIT_A, state: first.state, now: 9500, ...world() });
  assert.deepEqual(again.lines, [], 'no edits since the last Stop');
  // A record whose baseline still differs: silent only because it was reported.
  const reEdit = [...EDIT_A, { ts: 9600, file: 'src/a.rs', symbol: 'compute', sigs: sigsOf(HEAD_A) }];
  assert.deepEqual(computeStopReport({ edits: reEdit, state: first.state, now: 9900, ...world() }).lines, [],
    'once per symbol per session');
});

test('stop H1: a later turn compares with its own baseline, not HEAD', () => {
  // Turn 1 changed the signature (and was silent, e.g. its caller was fixed).
  // Turn 2 edits only the body: its record's baseline is turn 1's result.
  const turn2 = [...EDIT_A, { ts: 9600, file: 'src/a.rs', symbol: 'compute', sigs: sigsOf(SIG_A) }];
  const body2 = SIG_A.replace('x + y', 'x + y + 1');
  const r = computeStopReport({ edits: turn2, state: { lastStopAt: 9000, reported: [] }, now: 9900, ...world({ work: body2 }) });
  assert.deepEqual(r.lines, []);
});

test('stop H1: the baseline is the turn\'s FIRST record of the symbol', () => {
  // Two edits of compute this turn; the second was recorded after the first
  // had already changed the signature.
  const edits = [...EDIT_A, { ts: 5003, file: 'src/a.rs', symbol: 'compute', sigs: sigsOf(SIG_A) }];
  const r = computeStopReport({ edits, state: EMPTY, now: 9000, ...world() });
  assert.equal(r.lines.length, 1, 'changed against the first record');
});

test('stop: body-only change → silent; new, vanished, or unreadable definition → silent', () => {
  assert.deepEqual(computeStopReport({ edits: EDIT_A, state: EMPTY, now: 9000, ...world({ work: BODY_A }) }).lines, []);
  assert.deepEqual(computeStopReport({ edits: EDIT_A, state: EMPTY, now: 9000, ...world({ work: 'fn other() {}\n' }) }).lines, []);
  const withSigs = (sigs) => [EDIT_A[0], { ...EDIT_A[1], sigs }];
  assert.deepEqual(computeStopReport({ edits: withSigs([]), state: EMPTY, now: 9000, ...world() }).lines, [],
    'added this turn');
  assert.deepEqual(computeStopReport({ edits: withSigs(null), state: EMPTY, now: 9000, ...world() }).lines, [],
    'no baseline (unsupported language, pre-repair log line)');
});

test('stop Q2: a defaulted parameter appended → silent; a required one → reported', () => {
  const before = 'def load(p):\n    pass\n';
  const edits = [{ ts: 5001, file: 'src/a.py', symbol: 'load', sigs: extractSignatures(before, 'load', '.py') }];
  const run = (work) => computeStopReport({ edits, state: EMPTY, now: 9000, ...world(),
    workText: (f) => (f === 'src/a.py' ? work : null) }).lines;
  assert.deepEqual(run('def load(p, strict=False):\n    pass\n'), []);
  assert.deepEqual(run('def load(p, strict):\n    pass\n'), ['  load() in src/a.py: src/b.rs:5 (caller_b)']);
});

test('stop: edits before the previous Stop belong to an earlier turn', () => {
  const r = computeStopReport({ edits: EDIT_A, state: { lastStopAt: 8000, reported: [] }, now: 9000, ...world() });
  assert.deepEqual(r.lines, []);
  assert.equal(r.state.lastStopAt, 9000);
});

test('stop: callers are capped, sorted, and the rest named with the command that lists them', () => {
  const refs = Array.from({ length: 11 }, (_, i) => ({ file: `src/c${String(i).padStart(2, '0')}.rs`, line: 1, name: `f${i}` }));
  const r = computeStopReport({ edits: EDIT_A, state: EMPTY, now: 9000, ...world({ refs }) });
  assert.equal(r.lines.length, 1);
  assert.equal((r.lines[0].match(/src\/c\d\d\.rs:1/g) || []).length, 8);
  assert.match(r.lines[0], /and 3 more \(code-graph-mcp refs compute --file src\/a\.rs\)$/);
});

// D#560: an isolated agent's edits land in its parent's log as paths through
// the worktree, and the parent's Stop reads them from the main checkout. The
// report names files by those paths (the parent can open them); the command it
// suggests asks the index, which knows the file by its path in the worktree.
test('D#560: a worktree file is reported by its path, and the command asks for its index key', () => {
  const wtA = '.claude/worktrees/wt/src/a.rs';
  const refs = Array.from({ length: 11 }, (_, i) =>
    ({ file: `.claude/worktrees/wt/src/c${String(i).padStart(2, '0')}.rs`, line: 1, name: `f${i}` }));
  const r = computeStopReport({
    edits: EDIT_A.map((e) => ({ ...e, file: wtA })), state: EMPTY, now: 9000,
    workText: (f) => (f === wtA ? SIG_A : null),
    callers: () => refs,
    mtimeMs: () => 1000,
    indexKeyOf: (f) => f.replace(/^\.claude\/worktrees\/wt\//, ''),
  });
  assert.equal(r.lines.length, 1);
  assert.ok(r.lines[0].startsWith('  compute() in .claude/worktrees/wt/src/a.rs: .claude/worktrees/wt/src/c00.rs:1'), r.lines[0]);
  assert.match(r.lines[0], /and 3 more \(code-graph-mcp refs compute --file src\/a\.rs\)$/);
});

// A session whose own cwd is the worktree (EnterWorktree) reads the report
// from there, where `.claude/worktrees/wt/src/b.rs` does not exist (review of
// D#560). Names are shown from the session's tree; the log keeps its keys.
test('D#560: a session working in the worktree is shown paths from the worktree', () => {
  const wtA = '.claude/worktrees/wt/src/a.rs';
  const r = computeStopReport({
    edits: EDIT_A.map((e) => ({ ...e, file: wtA })), state: EMPTY, now: 9000,
    workText: (f) => (f === wtA ? SIG_A : null),
    callers: () => [{ file: '.claude/worktrees/wt/src/b.rs', line: 6, name: 'caller_b' }],
    mtimeMs: () => 1000,
    displayOf: (f) => (f.startsWith('.claude/worktrees/wt/') ? f.slice('.claude/worktrees/wt/'.length) : f),
  });
  assert.equal(r.lines[0], '  compute() in src/a.rs: src/b.rs:6 (caller_b)');
  assert.deepEqual(r.files, ['.claude/worktrees/wt/src/b.rs'], 'the follow-up keeps the log\'s keys');
});

test('stop: one caller line per file:line; a module-level caller has no name', () => {
  const refs = [
    { file: 'src/b.rs', line: 5, name: 'caller_b' },
    { file: 'src/b.rs', line: 5, name: 'caller_b' },
    { file: 'src/c.rs', line: 2, name: '<module>' },
  ];
  const r = computeStopReport({ edits: EDIT_A, state: EMPTY, now: 9000, ...world({ refs }) });
  assert.deepEqual(r.lines, ['  compute() in src/a.rs: src/b.rs:5 (caller_b), src/c.rs:2']);
});

test('stop M1: the symbol cap bounds refs queries over CHANGED symbols only; the rest are named', () => {
  const n = 10;
  const fns = (sig) => Array.from({ length: n }, (_, i) => `pub fn s${i}(x: i32${sig}) {\n}\n`).join('');
  const before = fns('');
  const after = fns(', y: i32');
  // Eight body-only edits first, then ten real changes.
  const edits = [];
  for (let i = 0; i < 8; i++) edits.push({ ts: 5000 + i, file: 'src/z.rs', symbol: `body${i}`, sigs: [`pubfnbody${i}()`] });
  for (let i = 0; i < n; i++) {
    edits.push({ ts: 6000 + i, file: 'src/a.rs', symbol: `s${i}`, sigs: extractSignatures(before, `s${i}`, '.rs') });
  }
  const w = world({ work: after });
  const bodies = Array.from({ length: 8 }, (_, i) => `pub fn body${i}() {\n  ${i}\n}\n`).join('');
  w.workText = (f) => (f === 'src/a.rs' ? after : f === 'src/z.rs' ? bodies : null);
  const r = computeStopReport({ edits, state: EMPTY, now: 9000, ...w });
  assert.equal(w.calls.length, 8, `refs queries: ${w.calls.join(',')}`);
  assert.deepEqual(w.calls, ['s0', 's1', 's2', 's3', 's4', 's5', 's6', 's7']);
  assert.equal(r.lines.length, 9);
  assert.equal(r.lines[8], '  2 more changed, callers not checked: s8() in src/a.rs, s9() in src/a.rs');
  assert.equal(r.state.reported.length, 8, 'the unchecked two are not marked reported');

  // The over-cap line names files as the reader sees them too (D#560: a
  // session working in the worktree; review of 0.178.0).
  const wtA = '.claude/worktrees/wt/src/a.rs';
  const ww = world({ work: after });
  ww.workText = (f) => (f === wtA ? after : null);
  const rw = computeStopReport({
    edits: edits.filter((e) => e.file === 'src/a.rs').map((e) => ({ ...e, file: wtA })),
    state: EMPTY, now: 9000, ...ww,
    displayOf: (f) => (f.startsWith('.claude/worktrees/wt/') ? f.slice('.claude/worktrees/wt/'.length) : f),
  });
  assert.equal(rw.lines[8], '  2 more changed, callers not checked: s8() in src/a.rs, s9() in src/a.rs');
});

test('stop M3: every repo-derived token in the text is shell-quoted', () => {
  const edited = 'src/a$(touch PWNED).rs';
  const refs = Array.from({ length: 9 }, (_, i) => ({ file: `src/m${i} x.rs`, line: 3, name: `c${i}` }));
  refs[0] = { file: 'src/b`id`.rs', line: 3, name: "it's" };
  const w = world({ refs });
  w.workText = (f) => (f === edited ? SIG_A : null);
  const edits = [{ ts: 5001, file: edited, symbol: 'compute', sigs: sigsOf(HEAD_A) }];
  const [line] = computeStopReport({ edits, state: EMPTY, now: 9000, ...w }).lines;
  assert.ok(line.startsWith("  compute() in 'src/a$(touch PWNED).rs': 'src/b`id`.rs':3"), line);
  assert.ok(line.includes("'src/b`id`.rs':3 ('it'\\''s')"), line);
  assert.ok(line.includes("'src/m1 x.rs':3 (c1)"), line);
  assert.ok(line.endsWith("(code-graph-mcp refs compute --file 'src/a$(touch PWNED).rs')"), line);
});

// --- Spawned end to end: real git, real binary, sandbox HOME/TMPDIR ---------

function realBinary() {
  try { return require('./find-binary').findBinary(); } catch { return null; }
}
const BIN = process.platform === 'win32' ? null : realBinary();
const e2eSkip = !BIN && 'needs a code-graph-mcp binary and a POSIX shell';

const DEFAULT_FILES = {
  'src/lib.rs': 'pub mod a;\npub mod b;\n',
  'src/a.rs': HEAD_A,
  'src/b.rs': 'use crate::a::compute;\n\npub fn caller_b() -> i32 {\n    let v = 2;\n    compute(v)\n}\n',
};

function sandboxRepo(t, files = DEFAULT_FILES) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-stop-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const home = path.join(root, 'home');
  const cache = path.join(home, '.cache', 'code-graph');
  fs.mkdirSync(cache, { recursive: true });
  fs.writeFileSync(path.join(cache, 'install-manifest.json'), '{"version":"9.9.9","config":{}}');
  fs.writeFileSync(path.join(cache, 'binary-path'), BIN);
  const tmp = path.join(root, 'tmp');
  fs.mkdirSync(tmp);
  const repo = path.join(root, 'repo');
  for (const [f, c] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(repo, f)), { recursive: true });
    fs.writeFileSync(path.join(repo, f), c);
  }
  fs.writeFileSync(path.join(repo, '.gitignore'), '.code-graph/\n');
  const env = {
    ...process.env,
    HOME: home, USERPROFILE: home,
    TMPDIR: tmp, TMP: tmp, TEMP: tmp,
    CODE_GRAPH_QUIET_HOOKS: '0',
    CODE_GRAPH_DISABLE_MODEL_DOWNLOAD: '1',
    GIT_CONFIG_NOSYSTEM: '1',
  };
  const git = (...args) => execFileSync('git', ['-c', 'user.email=t@t', '-c', 'user.name=t', ...args],
    { cwd: repo, env, stdio: 'pipe' });
  git('init', '-q');
  git('add', '-A');
  git('commit', '-qm', 'init');
  // Every fixture file predates the session by an hour, so "touched this
  // turn" is decided by the edit log and the edits the test makes — never by
  // a same-millisecond mtime.
  const past = new Date(Date.now() - 3600 * 1000);
  for (const f of Object.keys(files)) fs.utimesSync(path.join(repo, f), past, past);
  const index = () => execFileSync(BIN, ['incremental-index'], { cwd: repo, env, stdio: 'pipe' });
  index();
  return { root, repo, env, tmp, index };
}

function hook(sb, script, payload) {
  return spawnSync(process.execPath, [path.join(__dirname, script)], {
    input: JSON.stringify(payload), cwd: sb.repo, env: sb.env, encoding: 'utf8', timeout: 20000,
  });
}

// What Claude Code does for one Edit: PreToolUse (logs the edit), the edit
// itself, PostToolUse (the incremental index; run directly here).
function edit(sb, session, file, oldString, newString) {
  const abs = path.join(sb.repo, file);
  const pre = hook(sb, 'pre-edit-guide.js', {
    session_id: session, hook_event_name: 'PreToolUse', tool_name: 'Edit', cwd: sb.repo,
    tool_input: { file_path: abs, old_string: oldString, new_string: newString },
  });
  assert.equal(pre.status, 0, pre.stderr);
  const text = fs.readFileSync(abs, 'utf8');
  assert.ok(text.includes(oldString), `${file} does not contain the old_string`);
  fs.writeFileSync(abs, text.replace(oldString, newString));
  sb.index();
}

// Q1: a Write over an existing file goes through the same PreToolUse hook.
function write(sb, session, file, content) {
  const abs = path.join(sb.repo, file);
  const pre = hook(sb, 'pre-edit-guide.js', {
    session_id: session, hook_event_name: 'PreToolUse', tool_name: 'Write', cwd: sb.repo,
    tool_input: { file_path: abs, content },
  });
  assert.equal(pre.status, 0, pre.stderr);
  assert.equal(pre.stdout.trim(), '', 'a Write is logged, never answered');
  fs.writeFileSync(abs, content);
  sb.index();
}

// Q1: `sed -i` / `perl -pi` reach PreToolUse as a Bash call.
function bash(sb, session, command) {
  const pre = hook(sb, 'pre-grep-guide.js', {
    session_id: session, hook_event_name: 'PreToolUse', tool_name: 'Bash', cwd: sb.repo,
    tool_input: { command },
  });
  assert.equal(pre.status, 0, pre.stderr);
  execFileSync('sh', ['-c', command], { cwd: sb.repo, env: sb.env, stdio: 'pipe' });
  sb.index();
}

function stop(sb, session, extra = {}) {
  const r = hook(sb, 'stop-impact.js', {
    session_id: session, hook_event_name: 'Stop', cwd: sb.repo, stop_hook_active: false, ...extra,
  });
  assert.equal(r.status, 0, r.stderr);
  return r.stdout ? JSON.parse(r.stdout) : null;
}

test('e2e: signature change with an untouched caller → exactly one b.rs:line, then silent', { skip: e2eSkip }, (t) => {
  const sb = sandboxRepo(t);
  edit(sb, 'sess-1', 'src/a.rs', 'pub fn compute(x: i32) -> i32 {', 'pub fn compute(x: i32, y: i32) -> i32 {');

  // A continuation caused by a Stop hook is never re-checked.
  assert.equal(stop(sb, 'sess-1', { stop_hook_active: true }), null);

  const out = stop(sb, 'sess-1');
  assert.ok(out, 'expected a Stop envelope');
  assert.equal(out.hookSpecificOutput.hookEventName, 'Stop');
  assert.equal(out.decision, undefined, 'feedback, not a block');
  const ctx = out.hookSpecificOutput.additionalContext;
  assert.equal((ctx.match(/src\/b\.rs:\d+/g) || []).length, 1, ctx);
  assert.match(ctx, /src\/b\.rs:5 \(caller_b\)/);

  assert.equal(stop(sb, 'sess-1'), null, 'second Stop in the same session is silent');
  // A different session keeps its own ledger, and compares with the signature
  // ITS turn started from: a body-only edit there changed no signature, so it
  // is silent even though the working tree still differs from HEAD (review
  // H1: this used to report, from a HEAD comparison).
  const header = 'pub fn compute(x: i32, y: i32) -> i32 {\n';
  edit(sb, 'sess-2', 'src/a.rs', header + '    x + 1', header + '    x + 1 + 0');
  assert.equal(stop(sb, 'sess-2'), null);
  // sess-3 changes the signature again, and is told.
  edit(sb, 'sess-3', 'src/a.rs', header, 'pub fn compute(x: i32, y: i64) -> i32 {\n');
  const third = stop(sb, 'sess-3');
  assert.ok(third, 'sess-3 changed the signature this turn');
  assert.match(third.hookSpecificOutput.additionalContext, /src\/b\.rs:5/);
});

// D#560: an agent isolated in a linked worktree (`.claude/worktrees/<slug>`) has
// no index of its own; its hooks read the main checkout's index and log its
// edits under its parent's session id, so the parent's Stop — run from the main
// checkout — checks them. Before the fix both the pre-edit impact and this
// check asked the index for `.claude/worktrees/wt/src/a.rs` and got nothing.
// b.rs plus nine more callers, c0.rs of which the agent edits: one past
// MAX_CALLERS_LISTED remains, so the report also carries the command that
// lists the rest — the one place it names a file to the index rather than to
// the reader.
const WT_FILES = { ...DEFAULT_FILES };
WT_FILES['src/lib.rs'] = 'pub mod a;\npub mod b;\n' +
  Array.from({ length: 9 }, (_, i) => `pub mod c${i};\n`).join('');
for (let i = 0; i < 9; i++) {
  WT_FILES[`src/c${i}.rs`] = `use crate::a::compute;\n\npub fn caller_c${i}() -> i32 {\n    compute(${i})\n}\n`;
}

test('e2e D#560: a signature change in a linked worktree is checked against the worktree\'s callers', { skip: e2eSkip }, (t) => {
  const sb = sandboxRepo(t, WT_FILES);
  execFileSync('git', ['worktree', 'add', '-q', '.claude/worktrees/wt', '-b', 'wt'],
    { cwd: sb.repo, env: sb.env, stdio: 'pipe' });
  const wt = path.join(sb.repo, '.claude', 'worktrees', 'wt');
  // The worktree's copy of the caller has its call a line lower than the main
  // checkout's (line 5), so the line reported says which copy was read.
  const bWt = path.join(wt, 'src', 'b.rs');
  fs.writeFileSync(bWt,
    'use crate::a::compute;\n\npub fn caller_b() -> i32 {\n    let v = 2;\n    let w = v;\n    compute(w)\n}\n');
  const past = new Date(Date.now() - 3600 * 1000);
  fs.utimesSync(bWt, past, past);

  const aWt = path.join(wt, 'src', 'a.rs');
  const oldString = 'pub fn compute(x: i32) -> i32 {';
  const newString = 'pub fn compute(x: i32, y: i32) -> i32 {';
  const pre = spawnSync(process.execPath, [path.join(__dirname, 'pre-edit-guide.js')], {
    input: JSON.stringify({
      session_id: 'sess-wt', hook_event_name: 'PreToolUse', tool_name: 'Edit', cwd: wt,
      tool_input: { file_path: aWt, old_string: oldString, new_string: newString },
    }),
    cwd: wt, env: sb.env, encoding: 'utf8', timeout: 20000,
  });
  assert.equal(pre.status, 0, pre.stderr);
  assert.match(pre.stdout, /code-graph:impact\] compute\(\)[\s\S]*caller_b \(src\/b\.rs\)/, `pre-edit: ${pre.stdout}`);
  fs.writeFileSync(aWt, fs.readFileSync(aWt, 'utf8').replace(oldString, newString));
  // The agent also updates one caller, through the same hook.
  const c0Wt = path.join(wt, 'src', 'c0.rs');
  const preC0 = spawnSync(process.execPath, [path.join(__dirname, 'pre-edit-guide.js')], {
    input: JSON.stringify({
      session_id: 'sess-wt', hook_event_name: 'PreToolUse', tool_name: 'Edit', cwd: wt,
      tool_input: { file_path: c0Wt, old_string: '    compute(0)', new_string: '    compute(0, 1)' },
    }),
    cwd: wt, env: sb.env, encoding: 'utf8', timeout: 20000,
  });
  assert.equal(preC0.status, 0, preC0.stderr);
  fs.writeFileSync(c0Wt, fs.readFileSync(c0Wt, 'utf8').replace('    compute(0)', '    compute(0, 1)'));
  fs.utimesSync(c0Wt, past, past);  // left out by the edit log, not by its mtime

  const out = stop(sb, 'sess-wt');
  assert.ok(out, 'expected a Stop envelope');
  const ctx = out.hookSpecificOutput.additionalContext;
  assert.match(ctx, /compute\(\) in \.claude\/worktrees\/wt\/src\/a\.rs: \.claude\/worktrees\/wt\/src\/b\.rs:6 \(caller_b\)/, ctx);
  assert.doesNotMatch(ctx, /c0\.rs/, 'a caller the agent edited this turn is not listed');
  assert.match(ctx, /and 1 more \(code-graph-mcp refs compute --file src\/a\.rs\)/, ctx);
});

// The session itself works in the worktree (EnterWorktree): its Stop runs with
// the worktree as cwd, so the report names files from there.
test('e2e D#560: a session working in a linked worktree is shown paths from the worktree', { skip: e2eSkip }, (t) => {
  const sb = sandboxRepo(t);
  execFileSync('git', ['worktree', 'add', '-q', '.claude/worktrees/wt', '-b', 'wt'],
    { cwd: sb.repo, env: sb.env, stdio: 'pipe' });
  const wt = path.join(sb.repo, '.claude', 'worktrees', 'wt');
  const aWt = path.join(wt, 'src', 'a.rs');
  const oldString = 'pub fn compute(x: i32) -> i32 {';
  const newString = 'pub fn compute(x: i32, y: i32) -> i32 {';
  const pre = spawnSync(process.execPath, [path.join(__dirname, 'pre-edit-guide.js')], {
    input: JSON.stringify({
      session_id: 'sess-in-wt', hook_event_name: 'PreToolUse', tool_name: 'Edit', cwd: wt,
      tool_input: { file_path: aWt, old_string: oldString, new_string: newString },
    }),
    cwd: wt, env: sb.env, encoding: 'utf8', timeout: 20000,
  });
  assert.equal(pre.status, 0, pre.stderr);
  fs.writeFileSync(aWt, fs.readFileSync(aWt, 'utf8').replace(oldString, newString));

  const r = spawnSync(process.execPath, [path.join(__dirname, 'stop-impact.js')], {
    input: JSON.stringify({ session_id: 'sess-in-wt', hook_event_name: 'Stop', cwd: wt, stop_hook_active: false }),
    cwd: wt, env: sb.env, encoding: 'utf8', timeout: 20000,
  });
  assert.equal(r.status, 0, r.stderr);
  assert.ok(r.stdout, 'expected a Stop envelope');
  const ctx = JSON.parse(r.stdout).hookSpecificOutput.additionalContext;
  assert.match(ctx, /^ {2}compute\(\) in src\/a\.rs: src\/b\.rs:5 \(caller_b\)$/m, ctx);
});

// D#163: the roadmap's Stop metric ("reported, and adopted") needs both halves
// in recommendations.jsonl.
function stopRecords(sb) {
  let raw = '';
  try { raw = fs.readFileSync(path.join(sb.repo, '.code-graph', 'recommendations.jsonl'), 'utf8'); } catch { /* none */ }
  return raw.split('\n').filter(Boolean).map((l) => JSON.parse(l)).filter((r) => r.hook === 'stop');
}

test('e2e Q1: a signature changed by Write, sed -i or perl -pi reports the untouched caller', { skip: e2eSkip }, (t) => {
  const changed = HEAD_A.replace('compute(x: i32)', 'compute(x: i32, y: i32)');
  const sb = sandboxRepo(t);
  write(sb, 'w', 'src/a.rs', changed);
  assert.match(stop(sb, 'w').hookSpecificOutput.additionalContext, /compute\(\) in src\/a\.rs: src\/b\.rs:5 \(caller_b\)/);

  const sb2 = sandboxRepo(t);
  bash(sb2, 's', "sed -i 's/compute(x: i32)/compute(x: i32, y: i32)/' src/a.rs");
  assert.match(stop(sb2, 's').hookSpecificOutput.additionalContext, /compute\(\) in src\/a\.rs: src\/b\.rs:5/);

  const sb3 = sandboxRepo(t);
  bash(sb3, 'p', "perl -pi -e 's/compute\\(x: i32\\)/compute(x: i32, y: i32)/' src/a.rs");
  assert.match(stop(sb3, 'p').hookSpecificOutput.additionalContext, /compute\(\) in src\/a\.rs: src\/b\.rs:5/);
});

test('e2e Q1: a body-only Write or sed -i stays silent', { skip: e2eSkip }, (t) => {
  const sb = sandboxRepo(t);
  write(sb, 'w', 'src/a.rs', BODY_A);
  assert.equal(stop(sb, 'w'), null);
  const sb2 = sandboxRepo(t);
  bash(sb2, 's', "sed -i 's/x + 1/x + 2/' src/a.rs");
  assert.equal(stop(sb2, 's'), null);
});

test('e2e: a report is recorded, and the next Stop records that a listed caller file was edited', { skip: e2eSkip }, (t) => {
  const sb = sandboxRepo(t);
  edit(sb, 's', 'src/a.rs', 'pub fn compute(x: i32) -> i32 {', 'pub fn compute(x: i32, y: i32) -> i32 {');
  assert.ok(stop(sb, 's'), 'the signature change is reported');
  assert.deepEqual(stopRecords(sb).map((r) => [r.action, r.symbols, r.callers]), [['stop_check', 1, 1]]);

  // The turn continues on the feedback, fixes the caller, and ends with
  // stop_hook_active: that Stop is not re-checked, but it is the follow-up.
  edit(sb, 's', 'src/b.rs', '    compute(v)', '    compute(v, 1)');
  assert.equal(stop(sb, 's', { stop_hook_active: true }), null, 'a continuation is never re-checked');
  const after = stopRecords(sb);
  assert.deepEqual(after.map((r) => [r.action, r.adopted, r.listed, r.edited]),
    [['stop_check', undefined, undefined, undefined], ['stop_followup', true, 1, 1]]);

  // One follow-up per report.
  assert.equal(stop(sb, 's', { stop_hook_active: true }), null);
  assert.equal(stopRecords(sb).length, 2);
});

test('e2e: a report whose callers stay untouched records adopted:false at the next Stop', { skip: e2eSkip }, (t) => {
  const sb = sandboxRepo(t);
  edit(sb, 's', 'src/a.rs', 'pub fn compute(x: i32) -> i32 {', 'pub fn compute(x: i32, y: i32) -> i32 {');
  assert.ok(stop(sb, 's'));
  assert.equal(stop(sb, 's'), null, 'already reported');
  assert.deepEqual(stopRecords(sb).map((r) => [r.action, r.adopted]),
    [['stop_check', undefined], ['stop_followup', false]]);
});

test('followUpOf: a listed file edited after the report is adoption; earlier or unlisted is not', () => {
  const pending = { at: 9000, files: ['src/b.rs', 'src/c.rs'] };
  const mtimes = (m) => (f) => (f in m ? m[f] : 1000);
  assert.deepEqual(followUpOf(pending, [{ ts: 9500, file: 'src/b.rs' }], mtimes({})),
    { adopted: true, listed: 2, edited: 1 });
  assert.deepEqual(followUpOf(pending, [{ ts: 8000, file: 'src/b.rs' }, { ts: 9500, file: 'src/z.rs' }], mtimes({})),
    { adopted: false, listed: 2, edited: 0 }, 'an edit before the report, or of a file it did not list');
  assert.deepEqual(followUpOf(pending, [], mtimes({ 'src/c.rs': 9000 })),
    { adopted: true, listed: 2, edited: 1 }, 'an mtime at the report counts (Write, sed -i)');
});

test('report symbols/files: newly reported symbols only, every untouched caller file, none touched this turn', () => {
  const refs = [{ file: 'src/b.rs', line: 5, name: 'x' }, { file: 'src/c.rs', line: 2, name: 'y' },
    { file: 'src/d.rs', line: 9, name: 'z' }];
  const w = world({ refs, mtimes: { 'src/d.rs': 6000 } }); // d.rs changed inside this turn
  const r = computeStopReport({ edits: EDIT_A, state: { lastStopAt: null, reported: ['src/q.rs#other'] }, now: 9000, ...w });
  assert.equal(r.lines.length, 1);
  assert.equal(r.symbols, 1, 'a symbol reported by an earlier Stop is not counted again');
  assert.deepEqual(r.files, ['src/b.rs', 'src/c.rs'], 'one entry per caller file; d.rs was touched this turn');
});

test('reportRecords: every shown report is recorded; only one naming caller files leaves a follow-up', () => {
  assert.deepEqual(reportRecords({ lines: [], state: { lastStopAt: 9 } }), { check: null, pending: null });
  const onlyUnchecked = reportRecords({ lines: ['  1 more changed, callers not checked: g() in a.rs'],
    state: { lastStopAt: 9 }, symbols: 0, files: [] });
  assert.deepEqual(onlyUnchecked, { check: { hook: 'stop', action: 'stop_check', symbols: 0, callers: 0 }, pending: null });
  const many = Array.from({ length: MAX_FOLLOWUP_FILES + 6 }, (_, i) => `src/m${i}.rs`);
  const r = reportRecords({ lines: ['x'], state: { lastStopAt: 9 }, symbols: 2, files: many });
  assert.deepEqual(r.check, { hook: 'stop', action: 'stop_check', symbols: 2, callers: MAX_FOLLOWUP_FILES + 6 });
  assert.equal(r.pending.at, 9);
  assert.equal(r.pending.files.length, MAX_FOLLOWUP_FILES, 'the follow-up keeps a bounded list');
});

test('e2e: adoption is read from the edit log alone and from the mtime alone', { skip: e2eSkip }, (t) => {
  const past = new Date(Date.now() - 3600 * 1000);
  // Log only: an Edit is logged, then the file's mtime is put back before the report.
  const a = sandboxRepo(t);
  edit(a, 's', 'src/a.rs', 'pub fn compute(x: i32) -> i32 {', 'pub fn compute(x: i32, y: i32) -> i32 {');
  assert.ok(stop(a, 's'));
  edit(a, 's', 'src/b.rs', '    compute(v)', '    compute(v, 1)');
  fs.utimesSync(path.join(a.repo, 'src/b.rs'), past, past);
  stop(a, 's', { stop_hook_active: true });
  assert.deepEqual(stopRecords(a).map((r) => [r.action, r.adopted]), [['stop_check', undefined], ['stop_followup', true]]);
  // Mtime only: the file is written with no Edit (Write, sed -i).
  const b = sandboxRepo(t);
  edit(b, 's', 'src/a.rs', 'pub fn compute(x: i32) -> i32 {', 'pub fn compute(x: i32, y: i32) -> i32 {');
  assert.ok(stop(b, 's'));
  const bf = path.join(b.repo, 'src/b.rs');
  fs.writeFileSync(bf, fs.readFileSync(bf, 'utf8').replace('compute(v)', 'compute(v, 1)'));
  stop(b, 's', { stop_hook_active: true });
  assert.deepEqual(stopRecords(b).map((r) => [r.action, r.adopted]), [['stop_check', undefined], ['stop_followup', true]]);
});

test('e2e: caller also edited in the same turn → silent', { skip: e2eSkip }, (t) => {
  const sb = sandboxRepo(t);
  edit(sb, 's', 'src/a.rs', 'pub fn compute(x: i32) -> i32 {', 'pub fn compute(x: i32, y: i32) -> i32 {');
  edit(sb, 's', 'src/b.rs', '    compute(v)', '    compute(v, 1)');
  assert.equal(stop(sb, 's'), null);
});

test('e2e: body-only change → silent; not a git repo → silent; state stays in TMPDIR', { skip: e2eSkip }, (t) => {
  const sb = sandboxRepo(t);
  // old_string carries the header, as a real body edit usually does — that is
  // what lets pre-edit-guide name the symbol.
  edit(sb, 's', 'src/a.rs', 'pub fn compute(x: i32) -> i32 {\n    x + 1', 'pub fn compute(x: i32) -> i32 {\n    x + 2');
  assert.equal(stop(sb, 's'), null);

  const sb2 = sandboxRepo(t);
  fs.rmSync(path.join(sb2.repo, '.git'), { recursive: true, force: true });
  edit(sb2, 's', 'src/a.rs', 'pub fn compute(x: i32) -> i32 {', 'pub fn compute(x: i32, y: i32) -> i32 {');
  assert.equal(stop(sb2, 's'), null);

  // Both files this feature writes live in the redirected cgTmpDir, nowhere else.
  const cg = path.join(sb.tmp, 'code-graph-mcp');
  const names = fs.readdirSync(cg).filter((n) => /^\.cg-(edits|stop)-/.test(n)).sort();
  assert.equal(names.length, 2, names.join(', '));
  assert.match(names[0], /^\.cg-edits-[0-9a-f]{12}-s\.jsonl$/);
  assert.match(names[1], /^\.cg-stop-[0-9a-f]{12}-s\.json$/);
});

// --- Review repairs (P1 #3 review, 2026-09-28) ------------------------------

// M2: what the text heuristic concludes for one old → new copy of a file.
// 'changed' = a definition header of `symbol` differs; 'same' = found, none
// differs; 'silent' = no verdict (unsupported language or no definition).
function verdict(oldText, newText, symbol, ext) {
  const a = extractSignatures(oldText, symbol, ext);
  const b = extractSignatures(newText, symbol, ext);
  if (!a || !b || a.length === 0 || b.length === 0) return 'silent';
  return signatureChanged(a, b, symbol) ? 'changed' : 'same';
}

const SHAPES = [
  // The reviewer's false positives: a body-only change read as a signature change.
  ['py: `with f(...)` call is not a definition', '.py', 'load',
    'def load(p):\n    with load(1) as f:\n        pass\n',
    'def load(p):\n    with load(1, 2) as f:\n        pass\n', 'same'],
  ['py: `elif f(...)` call is not a definition', '.py', 'ok',
    'def ok(n):\n    if n:\n        pass\n    elif ok(1):\n        pass\n',
    'def ok(n):\n    if n:\n        pass\n    elif ok(2):\n        pass\n', 'same'],
  ['rust: a new same-named impl adds a definition, changes none', '.rs', 'from',
    'impl From<A> for X {\n    fn from(a: A) -> X { X }\n}\n',
    'impl From<A> for X {\n    fn from(a: A) -> X { X }\n}\nimpl From<B> for X {\n    fn from(b: B) -> X { X }\n}\n', 'same'],
  ['kotlin: expression-bodied fun, body edited', '.kt', 'area',
    'fun area(r: Double) =\n    r * r * 3.14\n\nfun other() {\n}\n',
    'fun area(r: Double) =\n    r * r * 3.1416\n\nfun other() {\n}\n', 'same'],
  ['kotlin: one-line expression body edited', '.kt', 'area',
    'fun area(r: Double) = r * r * 3.14\n', 'fun area(r: Double) = r * r * 3.1416\n', 'same'],
  ['scala: expression-bodied def, body edited', '.scala', 'f',
    'def f(): Int = 1\n\ndef g() = {\n}\n',
    'def f(): Int = 2\n\ndef g() = {\n}\n', 'same'],
  // The reviewer's false negative.
  ['ts: generic bound holding `{` — a new parameter is seen', '.ts', 'pick',
    'function pick<T extends {id:number}>(a: T) {\n  return a;\n}\n',
    'function pick<T extends {id:number}>(a: T, b: number) {\n  return a;\n}\n', 'changed'],
  // Real changes in the same languages stay visible.
  ['py: parameter added', '.py', 'load', 'def load(p):\n    pass\n', 'def load(p, q):\n    pass\n', 'changed'],
  ['kotlin: parameter added to an expression body', '.kt', 'area',
    'fun area(r: Double) =\n    r * r\n', 'fun area(r: Double, k: Double) =\n    r * r * k\n', 'changed'],
  ['kotlin: bodyless interface fun, the NEXT fun changes', '.kt', 'area',
    'interface S {\n    fun area(r: Double): Double\n}\nfun g() {\n  1\n}\n',
    'interface S {\n    fun area(r: Double): Double\n}\nfun g(x: Int) {\n  1\n}\n', 'same'],
  ['go: bodyless declaration, the NEXT func changes', '.go', 'Compute',
    'func Compute(x int) int\n\nfunc other() {\n}\n', 'func Compute(x int) int\n\nfunc other(y int) {\n}\n', 'same'],
  ['c#: expression-bodied member, body edited', '.cs', 'F',
    '  public int F(int x) => x + 1;\n', '  public int F(int x) => x + 2;\n', 'same'],
  ['header not ended within the character budget → silent', '.rs', 'compute',
    'fn compute(' + 'a: u8, '.repeat(120) + ') {}\n', 'fn compute(' + 'a: u8, '.repeat(121) + ') {}\n', 'silent'],
  ['scala: parameter added', '.scala', 'f', 'def f(): Int = 1\n', 'def f(x: Int): Int = x\n', 'changed'],
  ['rust: one impl of two changes its parameter', '.rs', 'from',
    'impl From<A> for X {\n    fn from(a: A) -> X { X }\n}\nimpl From<B> for X {\n    fn from(b: B) -> X { X }\n}\n',
    'impl From<A> for X {\n    fn from(a: &A) -> X { X }\n}\nimpl From<B> for X {\n    fn from(b: B) -> X { X }\n}\n', 'changed'],
  ['rust: a `match` guard calling the fn is not a definition', '.rs', 'compute',
    'fn compute(x: i32) -> i32 {\n    match x {\n        a if compute(a) > 0 => 1,\n        _ => 0,\n    }\n}\n',
    'fn compute(x: i32) -> i32 {\n    match x {\n        a if compute(a - 1) > 0 => 1,\n        _ => 0,\n    }\n}\n', 'same'],
  ['ruby: body-only change (newline ends the header)', '.rb', 'run',
    'def run(a, b)\n  a + b\nend\n', 'def run(a, b)\n  a - b\nend\n', 'same'],
  ['ruby: endless def, body edited', '.rb', 'run', 'def run(a) = a + 1\n', 'def run(a) = a + 2\n', 'same'],
  ['ruby: parameter added', '.rb', 'run', 'def run(a)\n  a\nend\n', 'def run(a, b)\n  a\nend\n', 'changed'],
  ['lua: one-line function, body edited', '.lua', 'run',
    'local function run(a) return a + 1 end\n', 'local function run(a) return a + 2 end\n', 'same'],
  ['c++: constructor initializer list edited', '.cpp', 'Foo',
    'explicit Foo(int x) : a(x), b{x} {\n}\n', 'explicit Foo(int x) : a(x + 1), b{x} {\n}\n', 'same'],
  ['c++: a label before a call is not a type', '.cpp', 'compute',
    'int compute(int x) {\nretry: compute(1);\n}\n', 'int compute(int x) {\nretry: compute(2);\n}\n', 'same'],
  ['c++: parameter added', '.cpp', 'compute', 'int compute(int x) {\n}\n', 'int compute(int x, int y) {\n}\n', 'changed'],
  ['java: parameter type changed', '.java', 'compute',
    '  public int compute(int x) {\n  }\n', '  public int compute(long x) {\n  }\n', 'changed'],
  ['go: parameter added', '.go', 'Compute',
    'func (s *S) Compute(x int) (int, error) {\n}\n', 'func (s *S) Compute(x int, y int) (int, error) {\n}\n', 'changed'],
  ['js: arrow binding parameter added', '.js', 'build',
    'const build = (x) => x;\n', 'const build = (x, y) => x + y;\n', 'changed'],
  // Q2: optional parameters appended at the end of the list leave every
  // existing call valid. Anything else that moves stays a change.
  ['py: defaulted parameter appended', '.py', 'load',
    'def load(p):\n    pass\n', 'def load(p, strict=False):\n    pass\n', 'same'],
  ['py: *args and **kwargs appended', '.py', 'load',
    'def load(self, p):\n    pass\n', 'def load(self, p, *args, **kwargs):\n    pass\n', 'same'],
  ['py: keyword-only parameters with defaults appended', '.py', 'load',
    'def load(p):\n    pass\n', 'def load(p, *, strict=False):\n    pass\n', 'same'],
  ['py: re-wrapped with a trailing comma', '.py', 'load',
    'def load(p):\n    pass\n', 'def load(\n    p,\n    strict: bool = False,\n) -> None:\n    pass\n', 'changed'],
  ['py: re-wrapped with a trailing comma, no return type added', '.py', 'load',
    'def load(p):\n    pass\n', 'def load(\n    p,\n    strict: bool = False,\n):\n    pass\n', 'same'],
  ['py: first parameter of an empty list, defaulted', '.py', 'load',
    'def load():\n    pass\n', 'def load(p=None):\n    pass\n', 'same'],
  ['py: keyword-only parameter WITHOUT a default', '.py', 'load',
    'def load(p):\n    pass\n', 'def load(p, *, strict):\n    pass\n', 'changed'],
  ['py: defaulted parameter inserted before an existing one', '.py', 'load',
    'def load(a, b=1):\n    pass\n', 'def load(a, c=2, b=1):\n    pass\n', 'changed'],
  ['py: defaulted parameter renamed', '.py', 'load',
    'def load(a, b=1):\n    pass\n', 'def load(a, c=1):\n    pass\n', 'changed'],
  ['ts: optional parameter appended', '.ts', 'pick',
    'function pick(a: T) {\n}\n', 'function pick(a: T, b?: number) {\n}\n', 'same'],
  ['ts: rest parameter appended to a generic function', '.ts', 'pick',
    'function pick<T>(a: T): T {\n}\n', 'function pick<T>(a: T, ...rest: T[]): T {\n}\n', 'same'],
  ['ts: defaulted parameter appended to an arrow binding', '.ts', 'build',
    'const build = (x) => x;\n', 'const build = (x, y = () => 1) => x;\n', 'same'],
  ['ts: appended parameter of function type, no default', '.ts', 'build',
    'const build = (x) => x;\n', 'const build = (x, cb: (a: number) => void) => x;\n', 'changed'],
  ['ts: defaulted parameter whose generic type holds an arrow type', '.ts', 'pick',
    'function pick(a: T) {\n}\n', 'function pick(a: T, m: Map<(k: K) => V, number> = new Map()) {\n}\n', 'same'],
  ['ts: last parameter type changed, then optionals appended', '.ts', 'pick',
    'function pick(a: T) {\n}\n', 'function pick(a: T[] = [], b?: number) {\n}\n', 'changed'],
  ['ts: optional parameter appended to the RETURN type, not the parameters', '.ts', 'pick',
    'function pick(a: T): (x: number) => void {\n}\n', 'function pick(a: T): (x: number, y?: number) => void {\n}\n', 'changed'],
  ['go: variadic parameter appended', '.go', 'Compute',
    'func (s *S) Compute(x int) (int, error) {\n}\n', 'func (s *S) Compute(x int, opts ...Option) (int, error) {\n}\n', 'same'],
  ['c++: defaulted parameter appended', '.cpp', 'compute',
    'int compute(int x) {\n}\n', 'int compute(int x, int y = 0) {\n}\n', 'same'],
  ['kotlin: defaulted parameter appended', '.kt', 'area',
    'fun area(r: Double): Double {\n}\n', 'fun area(r: Double, k: Double = 1.0): Double {\n}\n', 'same'],
  ['rust: no optional parameters exist; a tuple return grows', '.rs', 'split',
    'fn split(s: &str) -> (A, B) {\n}\n', 'fn split(s: &str) -> (A, B, C) {\n}\n', 'changed'],
  // No exact reading for these: say nothing rather than guess.
  ['elixir: unsupported → silent', '.ex', 'run',
    'def run(a), do: a + 1\n', 'def run(a), do: a + 2\n', 'silent'],
  ['unknown extension → silent', '.txt', 'run', 'def run(a):\n', 'def run(a, b):\n', 'silent'],
];

for (const [name, ext, symbol, before, after, want] of SHAPES) {
  test(`M2 signature shapes: ${name}`, () => {
    assert.equal(verdict(before, after, symbol, ext), want);
  });
}

// e2e H1: a caller fixed in the same turn as the signature change must not
// come back when a LATER turn only edits the function body.
test('e2e H1: callers fixed in turn 1, body-only edit in turn 2 → silent both turns', { skip: e2eSkip }, (t) => {
  const sb = sandboxRepo(t);
  edit(sb, 's', 'src/a.rs', 'pub fn compute(x: i32) -> i32 {', 'pub fn compute(x: i32, y: i32) -> i32 {');
  edit(sb, 's', 'src/b.rs', '    compute(v)', '    compute(v, 1)');
  assert.equal(stop(sb, 's'), null, 'turn 1: the only caller was updated');
  const header = 'pub fn compute(x: i32, y: i32) -> i32 {\n';
  edit(sb, 's', 'src/a.rs', header + '    x + 1', header + '    x + y + 1');
  const t2 = stop(sb, 's');
  assert.equal(t2, null, t2 && t2.hookSpecificOutput.additionalContext);
});

// e2e M1: eight body-only edits before the one real change must not use up
// the per-Stop symbol budget.
test('e2e M1: the 9th edited symbol, the only signature change, is reported', { skip: e2eSkip }, (t) => {
  const fns = [];
  const callers = [];
  for (let i = 0; i < 9; i++) {
    fns.push(`pub fn f${i}xx(x: i32) -> i32 {\n    x + ${i}\n}\n`);
    callers.push(`use crate::a::f${i}xx;\npub fn c${i}() -> i32 {\n    f${i}xx(1)\n}\n`);
  }
  const sb = sandboxRepo(t, { 'src/lib.rs': 'pub mod a;\npub mod b;\n', 'src/a.rs': fns.join('\n'), 'src/b.rs': callers.join('') });
  for (let i = 0; i < 8; i++) {
    edit(sb, 's', 'src/a.rs', `pub fn f${i}xx(x: i32) -> i32 {\n    x + ${i}`, `pub fn f${i}xx(x: i32) -> i32 {\n    x + ${i} + 0`);
  }
  edit(sb, 's', 'src/a.rs', 'pub fn f8xx(x: i32) -> i32 {', 'pub fn f8xx(x: i32, y: i32) -> i32 {');
  const out = stop(sb, 's');
  assert.ok(out, 'expected the f8xx report');
  const ctx = out.hookSpecificOutput.additionalContext;
  assert.match(ctx, /f8xx\(\) in src\/a\.rs: src\/b\.rs:\d+ \(c8\)/);
  assert.doesNotMatch(ctx, /f[0-7]xx\(\)/, 'body-only edits are not reported');
});

// e2e M3: repo-derived tokens in the injected text are shell-quoted — the
// edited file in the suggested command and every caller file.
test('e2e M3: hostile file names are shell-quoted in the injected text', { skip: e2eSkip }, (t) => {
  const edited = 'src/a$(touch PWNED).rs';
  const files = { 'src/lib.rs': 'pub mod a;\n', [edited]: HEAD_A };
  const caller = (name, arg) => `use crate::a::compute;\npub fn ${name}() -> i32 {\n    compute(${arg})\n}\n`;
  for (let i = 0; i < 8; i++) files[`src/m${i}.rs`] = caller(`cm${i}`, i);
  files['src/m$(id).rs'] = caller('cmx', 9);
  const sb = sandboxRepo(t, files);
  edit(sb, 's', edited, 'pub fn compute(x: i32) -> i32 {', 'pub fn compute(x: i32, y: i32) -> i32 {');
  const out = stop(sb, 's');
  assert.ok(out, 'expected a report');
  const ctx = out.hookSpecificOutput.additionalContext;
  assert.ok(ctx.includes("code-graph-mcp refs compute --file 'src/a$(touch PWNED).rs'"), ctx);
  assert.ok(ctx.includes("'src/m$(id).rs':3"), ctx);
  // No `$(` outside single quotes anywhere in the text.
  assert.doesNotMatch(ctx.replace(/'[^']*'/g, "''"), /\$\(/, ctx);
  assert.equal(fs.existsSync(path.join(sb.repo, 'PWNED')), false);
});

// Review L7 survivors M08 and M15: two documented silences no test pinned.
test('e2e: CODE_GRAPH_QUIET_HOOKS=1 silences Stop; a session with no edit log writes nothing', { skip: e2eSkip }, (t) => {
  const sb = sandboxRepo(t);
  edit(sb, 's', 'src/a.rs', 'pub fn compute(x: i32) -> i32 {', 'pub fn compute(x: i32, y: i32) -> i32 {');
  const quiet = hook({ ...sb, env: { ...sb.env, CODE_GRAPH_QUIET_HOOKS: '1' } }, 'stop-impact.js',
    { session_id: 's', hook_event_name: 'Stop', cwd: sb.repo, stop_hook_active: false });
  assert.equal(quiet.status, 0, quiet.stderr);
  assert.equal(quiet.stdout, '');
  const cg = path.join(sb.tmp, 'code-graph-mcp');
  const stateFiles = () => fs.readdirSync(cg).filter((n) => n.startsWith('.cg-stop-'));
  assert.deepEqual(stateFiles(), [], 'a quiet Stop does not advance the state either');

  assert.equal(stop(sb, 'never-edited'), null);
  assert.deepEqual(stateFiles(), [], 'no edit log → no state file');
  assert.ok(stop(sb, 's'), 'the logged session is still reported once unquieted');
});
