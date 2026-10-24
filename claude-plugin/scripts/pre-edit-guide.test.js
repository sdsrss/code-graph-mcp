'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');

// Pre-edit-guide.js is a script with side effects (reads stdin, checks db).
// We test its PATTERNS directly without requiring the module.

// --- Function signature patterns, READ OUT OF the hook's source ---
//
// These used to be a hand-copied duplicate of the array in pre-edit-guide.js,
// with a "pattern-sync" test that only counted language comments (>= 7). That
// counts as no sync at all: the SEC-04 fix rewrote three of the eight regexes to
// bound their quantifiers, and every extraction test here would have gone on
// passing against the old unbounded copies — a suite testing a regex the hook
// does not use. Parsing the literal out of the source means a source edit is
// exercised by these tests on the next run, whatever it changes.
const SOURCE = require('node:fs').readFileSync(
  require('node:path').join(__dirname, 'pre-edit-guide.js'),
  'utf8'
);

function fnPatternsFromSource(source) {
  const start = source.indexOf('const fnPatterns = [');
  assert.ok(start !== -1, 'fnPatterns array not found in pre-edit-guide.js');
  const open = source.indexOf('[', start);
  const close = source.indexOf('\n];', open);
  assert.ok(close !== -1, 'unterminated fnPatterns array in pre-edit-guide.js');
  // eslint-disable-next-line no-new-func
  const patterns = new Function(`return ${source.slice(open, close + 2)}`)();
  assert.ok(
    Array.isArray(patterns) && patterns.every((p) => p instanceof RegExp),
    'fnPatterns must parse to an array of RegExp'
  );
  return patterns;
}

const fnPatterns = fnPatternsFromSource(SOURCE);

function extractFunctionName(code) {
  for (const pat of fnPatterns) {
    const m = code.match(pat);
    if (m) return m[1] || m[2];
  }
  return null;
}

function isCommonKeyword(s) {
  return /^(if|for|while|switch|catch|else|return|new|get|set|try)$/i.test(s);
}

// ── Rust ────────────────────────────────────────────────

test('fn-extract: Rust pub fn', () => {
  assert.equal(extractFunctionName('pub fn parse_code(input: &str) -> Vec<Node> {'), 'parse_code');
});

test('fn-extract: Rust pub async fn', () => {
  assert.equal(extractFunctionName('pub async fn handle_message(&self, msg: &str) -> Result<()> {'), 'handle_message');
});

test('fn-extract: Rust fn (no pub)', () => {
  assert.equal(extractFunctionName('fn helper_func(x: i32) -> i32 {'), 'helper_func');
});

// ── JavaScript/TypeScript ───────────────────────────────

test('fn-extract: JS function', () => {
  assert.equal(extractFunctionName('function handleRequest(req, res) {'), 'handleRequest');
});

test('fn-extract: JS export function', () => {
  assert.equal(extractFunctionName('export function processData(input) {'), 'processData');
});

test('fn-extract: JS async function', () => {
  assert.equal(extractFunctionName('async function fetchData(url) {'), 'fetchData');
});

test('fn-extract: JS export async function', () => {
  assert.equal(extractFunctionName('export async function loadConfig(path) {'), 'loadConfig');
});

test('fn-extract: JS arrow function (const)', () => {
  assert.equal(extractFunctionName('const handleError = (err) => {'), 'handleError');
});

test('fn-extract: JS arrow function (async)', () => {
  assert.equal(extractFunctionName('const fetchUser = async (id) => {'), 'fetchUser');
});

test('fn-extract: JS method', () => {
  assert.equal(extractFunctionName('  handleMessage(msg) {'), 'handleMessage');
});

// ── Python ──────────────────────────────────────────────

test('fn-extract: Python def', () => {
  assert.equal(extractFunctionName('def process_data(self, items):'), 'process_data');
});

test('fn-extract: Python async def', () => {
  assert.equal(extractFunctionName('async def fetch_data(url):'), 'fetch_data');
});

// ── Go ──────────────────────────────────────────────────

test('fn-extract: Go func', () => {
  assert.equal(extractFunctionName('func HandleRequest(w http.ResponseWriter, r *http.Request) {'), 'HandleRequest');
});

// ── Java/C#/Kotlin ──────────────────────────────────────

test('fn-extract: Java public method', () => {
  assert.equal(extractFunctionName('public void processItem(Item item) {'), 'processItem');
});

test('fn-extract: Java private method', () => {
  assert.equal(extractFunctionName('private String formatOutput(Data data) {'), 'formatOutput');
});

test('fn-extract: C# static method', () => {
  assert.equal(extractFunctionName('static int CalculateTotal(List<int> items) {'), 'CalculateTotal');
});

// ── PHP ─────────────────────────────────────────────────

test('fn-extract: PHP function', () => {
  assert.equal(extractFunctionName('function handleUpload($file) {'), 'handleUpload');
});

test('fn-extract: PHP public function', () => {
  assert.equal(extractFunctionName('public function getUser($id) {'), 'getUser');
});

// ── Ruby ────────────────────────────────────────────────

test('fn-extract: Ruby def', () => {
  assert.equal(extractFunctionName('def process_request(params)'), 'process_request');
});

// ── Keyword filter ──────────────────────────────────────

test('keyword-filter: common keywords rejected', () => {
  for (const kw of ['if', 'for', 'while', 'switch', 'catch', 'else', 'return', 'new', 'get', 'set', 'try']) {
    assert.ok(isCommonKeyword(kw), `"${kw}" should be rejected`);
  }
});

test('keyword-filter: real function names pass', () => {
  for (const name of ['parse_code', 'handleMessage', 'process_data', 'fetchUser']) {
    assert.ok(!isCommonKeyword(name), `"${name}" should pass`);
  }
});

// ── No false positives ──────────────────────────────────

test('fn-extract: plain code body returns null', () => {
  assert.equal(extractFunctionName('let x = 42;\nreturn x + 1;'), null);
});

test('fn-extract: comment returns null', () => {
  assert.equal(extractFunctionName('// This is a comment about the function'), null);
});

test('fn-extract: short strings return null', () => {
  assert.equal(extractFunctionName('x = 1'), null);
});

// ── Pattern consistency check ───────────────────────────
// Verify fnPatterns in this test match what's in pre-edit-guide.js

// ── Salience forcing (v0.63) ────────────────────────────
// pre-edit-guide.js top-level-exits on require (reads stdin / checks db), so we
// assert on the source text — same convention as pattern-sync below.

test('salience: impact summary forces a per-caller verdict before the edit', () => {
  const fs = require('node:fs');
  const path = require('node:path');
  const source = fs.readFileSync(path.join(__dirname, 'pre-edit-guide.js'), 'utf8');
  // mem lifts cite-recall to ~94% by making the model ACT on the injection; the
  // impact summary must do the same rather than be passively skimmed. Wording
  // references "each caller of X()" not "above" (finding #5) so it stays coherent
  // when only the caller COUNT is shown (callers[] empty but directCallers>=1).
  assert.match(source, /Before this edit: confirm each caller of/);
  assert.match(source, /still holds with your change, or note why it is unaffected/);
  assert.doesNotMatch(source, /caller\(s\) above you will update/); // old wording removed
});

test('pattern-sync: the suite runs the hook\'s own patterns, not a copy', () => {
  // The extraction tests above already prove the parsed array works; this states
  // the invariant the parse exists for, and pins the count so a pattern silently
  // disappearing still fails something.
  assert.equal(fnPatterns.length, 8, `Expected 8 patterns, got ${fnPatterns.length}`);
  assert.ok(
    !/const fnPatterns = \[[\s\S]*?\n\];/.test(
      require('node:fs').readFileSync(__filename, 'utf8').replace(/fnPatternsFromSource[\s\S]*$/, '')
    ),
    'this test file must not carry its own fnPatterns literal — parse the source instead'
  );
});

// SEC-04 (audit 2026-08-29). `old_string` is whatever the model is editing, so a
// benign bracket-free blob — base64, a hex dump, a minified bundle — used to
// stall this BLOCKING hook for seconds: three of the eight patterns ran an
// unbounded \w / \S run in front of a required literal, which backtracks from
// every start position. Measured at HEAD before the fix, on the same box that
// runs this test: 100 KB 2.8 s, 200 KB 11.0 s, 400 KB 43.4 s.
//
// This runs the patterns over the WHOLE 400 KB deliberately, bypassing the hook's
// own 8 KB window, so it fails if a quantifier loses its cap even though the
// window would have hidden it. Post-fix on that box the curve is linear —
// 10 KB 2.4 ms, 100 KB 21.9 ms, 400 KB 87.4 ms — while the hook's real cost for
// the same 400 KB input is 1.81 ms, because it only ever matches the window.
//
// The assertion is the RATIO, not a millisecond count. It was `ms < 250`, a
// threshold read off the 87.4 ms linear number on a 24-core dev box; the first
// 2-core CI runner to execute it did the same linear work in 269.7 ms and went
// red. An absolute budget calibrated on fast hardware is a machine-speed
// assertion wearing a complexity assertion's name — and the name is the one that
// is actually testable here. Quadrupling the input costs ~4x when the bounds
// hold and ~17x when they do not, so the ratio separates them by 4x on ANY box,
// while the loose absolute ceiling stays only as a backstop for a machine so slow
// the ratio itself gets noisy.
//
// Mutation-verified 2026-09-01 by restoring the three pre-fix patterns (the JS
// arrow, JS method and Java/C# arms, uncapped): 100 KB 2689.5 ms -> 400 KB
// 45532.4 ms, growth 16.9x, red. Capped, the same run is 22.1 ms -> 88.1 ms,
// growth 4.0x. Restoring the Java/C# arm ALONE stays linear (1.9 ms -> 7.4 ms,
// 4.0x) and correctly does not fire — the backtracking driver is the two JS
// arms, not that one.
//
// Timed in process CPU time, not wall time, and each size is the MINIMUM of up
// to REDOS_SAMPLES runs. Wall time made the ratio a measure of scheduler luck:
// under parallel `node --test` 2 of 20 full-suite runs went red on 2026-09-25 at
// 8.2x (100 KB 41.9 ms -> 400 KB 341.9 ms) and 8.6x (2.4 ms -> 20.4 ms), both
// linear work with the large sample preempted. Min-of-3 wall time alone still
// went red 1 in 20 in the suite (8.8x) and 7 in 30 run alone beside 12 busy
// loops, because contention outlasts three samples. CPU time does not accrue
// while descheduled; the minimum absorbs what is left (GC, cache). A sample past
// the absolute ceiling ends the loop — that is not noise, and it keeps the
// quadratic case at one run's cost.
const REDOS_GROWTH_LIMIT = 8; // linear 4.0x, quadratic 16.9x — sits between them
const REDOS_ABSOLUTE_CEILING_MS = 5000; // 57x the linear number, 9x below quadratic
const REDOS_SAMPLES = 3;
test('SEC-04: bracket-free word-dense input matches in linear time', () => {
  // CPU time of this process, not wall time: the claim is about the work the
  // regexes do, and wall time also bills every millisecond the scheduler spent
  // running someone else.
  const timeAt = (build, bytes) => {
    const input = build(bytes);
    const t0 = process.cpuUsage();
    for (const pat of fnPatterns) input.match(pat);
    const d = process.cpuUsage(t0);
    return (d.user + d.system) / 1e3;
  };
  for (const build of [
    (n) => 'a'.repeat(n),                        // one unbroken \w run
    (n) => 'foo_bar_baz_'.repeat(Math.ceil(n / 12)).slice(0, n), // underscore-dense
    (n) => 'public static void '.repeat(Math.ceil(n / 19)).slice(0, n), // feeds the \S+ pattern
  ]) {
    timeAt(build, 32 * 1024); // warm-up: don't bill regex compilation to the small sample
    let small = Infinity;
    let large = Infinity;
    for (let i = 0; i < REDOS_SAMPLES; i++) {
      small = Math.min(small, timeAt(build, 100 * 1024));
      const sample = timeAt(build, 400 * 1024);
      large = Math.min(large, sample);
      if (sample >= REDOS_ABSOLUTE_CEILING_MS) break;
    }
    // Floor the divisor: a sub-millisecond `small` would inflate the ratio, and
    // it can only be sub-millisecond when the bounds ARE holding (the quadratic
    // case takes seconds at 100 KB).
    const growth = large / Math.max(small, 1);
    assert.ok(
      growth < REDOS_GROWTH_LIMIT,
      `4x the input cost ${growth.toFixed(1)}x the time (100 KB ${small.toFixed(1)}ms -> ` +
        `400 KB ${large.toFixed(1)}ms); linear is ~4x, quadratic backtracking ~15x`
    );
    assert.ok(
      large < REDOS_ABSOLUTE_CEILING_MS,
      `400 KB of word-dense input took ${large.toFixed(1)}ms across all patterns`
    );
  }
});

test('SEC-04: the scan window is bounded in the hook itself', () => {
  // Belt to the quantifier caps' braces, and the part that bounds a pattern a
  // future author adds without reading the note.
  assert.match(SOURCE, /oldStr\.length > 8192 \? oldStr\.slice\(0, 8192\)/);
  assert.match(SOURCE, /for \(const pat of fnPatterns\) \{\n\s*const m = scanned\.match\(new RegExp\(pat\.source, `\$\{pat\.flags\}d`\)\)/);
});

// ── Covering-test targeting (edit-time PUSH) ────────────
// The pure formatter lives in covering-tests.js (unit-tested in covering-tests.test.js);
// these guard that the hook actually wires it in and records the forward signal.
// Source-grep, same convention as the salience/pattern-sync guards (hook exits on require).

test('covering-tests: hook requires and invokes the covering-tests formatter on test_callers', () => {
  const fs = require('node:fs');
  const path = require('node:path');
  const source = fs.readFileSync(path.join(__dirname, 'pre-edit-guide.js'), 'utf8');
  assert.match(source, /require\(['"]\.\/covering-tests['"]\)/);
  assert.match(source, /formatCoveringTests\(/);
  assert.match(source, /test_callers/);
});

test('covering-tests: edit injection records test_targets for the forward funnel', () => {
  const fs = require('node:fs');
  const path = require('node:path');
  const source = fs.readFileSync(path.join(__dirname, 'pre-edit-guide.js'), 'utf8');
  assert.match(source, /test_targets:/);
});

// ── Compound-grep sibling sweep: impact summary → additionalContext ──
// Bare `process.stdout.write(summary)` on a PreToolUse exit-0 lands in the debug
// log only and never reaches the model (CC docs v2026-06). The impact summary
// must ride the shared PreToolUse allow+additionalContext envelope instead.
// Source-grep, same convention as the salience/pattern-sync guards (hook exits
// on require: reads stdin, resolves the index).

test('emit: impact summary is delivered via the PreToolUse additionalContext envelope', () => {
  const fs = require('node:fs');
  const path = require('node:path');
  const source = fs.readFileSync(path.join(__dirname, 'pre-edit-guide.js'), 'utf8');
  assert.match(source, /require\(['"]\.\/hook-emit['"]\)/,
    'pre-edit-guide must use the shared hook-emit module (no inline envelope copy)');
  assert.match(source, /emitPreToolContext\(summary\)/,
    'the impact summary must be carried inside additionalContext, not bare stdout');
  assert.doesNotMatch(source, /emitPreToolAllowContext/,
    'Edit is a WRITE tool: this hook must never carry permissionDecision:"allow"');
  assert.doesNotMatch(source, /process\.stdout\.write\(summary\)\s*;/,
    'the bare stdout summary emission (debug-log-only) must be removed');
});

// ── Subprocess-level envelope test (P0-2) ───────────────────
// The guards above read the SOURCE. That is how a hook whose whole job is to
// print one JSON line went four releases emitting `permissionDecision: 'allow'`
// for Edit — a source grep proves which helper is called, never what the process
// actually writes. This spawns the real hook and parses its real stdout.
//
// Two stubs make the run hermetic (an --require preload patches the child's
// module cache BEFORE the hook's own requires destructure from it):
//   - find-binary   → a path that never runs, so no real binary is exec'd
//   - execFileSync  → a canned `impact --json` payload with 2 direct callers,
//                     which is the exact condition that opens the emit path
// HOME / TMPDIR / CLAUDE_CONFIG_DIR are redirected into the sandbox so the
// cooldown flag and recommendation log never touch the live ~/.claude (§8).
// The tmp redirect spells all THREE names. node's os.tmpdir() reads TMPDIR
// first on POSIX but only TEMP/TMP on Windows, where TMPDIR is ignored
// outright — so `TMPDIR` alone sandboxed this spawn on two platforms and left
// it inheriting the caller's tmp on the third. That is not hypothetical: the
// child's `.cg-impact-<cwd>-<symbol>` cooldown flag landed in the real shared
// `cgTmpDir()` on windows-latest and reddened
// `js_test_suite_leaves_the_shared_tmp_dir_intact` in CI for v0.126.1.

function runPreEditHook(t, {
  oldString = 'function processPayment(order) {',
  newString = 'x',
  relPath = 'src/payments.js',
  extraEnv = {},
  setup = () => {},
  stubExec = null,
  worktree = null,
  sessionId = null,
} = {}) {
  const fs = require('node:fs');
  const os = require('node:os');
  const path = require('node:path');
  const { spawnSync } = require('node:child_process');

  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-preedit-home-'));
  const proj = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-preedit-proj-'));
  // §8.V4: the creating test disposes of its own sandbox. Under Claude Code
  // os.tmpdir() IS ~/.claude/tmp, where orphans accumulate invisibly.
  t.after(() => {
    fs.rmSync(home, { recursive: true, force: true });
    fs.rmSync(proj, { recursive: true, force: true });
  });
  fs.mkdirSync(path.join(proj, '.code-graph'), { recursive: true });
  fs.writeFileSync(path.join(proj, '.code-graph', 'index.db'), '');
  fs.mkdirSync(path.join(home, 'tmp'), { recursive: true });
  // `worktree`: the hook runs in a linked worktree inside the project, where
  // Claude Code's EnterWorktree and isolated agents put it. It has no index of
  // its own, so the hook reads the main checkout's (D#560).
  let workDir = proj;
  if (worktree) {
    const gitdir = path.join(proj, '.git', 'worktrees', worktree);
    fs.mkdirSync(gitdir, { recursive: true });
    workDir = path.join(proj, '.claude', 'worktrees', worktree);
    fs.mkdirSync(workDir, { recursive: true });
    fs.writeFileSync(path.join(workDir, '.git'), `gitdir: ${gitdir}\n`);
  }
  setup(proj, workDir);

  const preload = path.join(home, 'stub-preload.js');
  fs.writeFileSync(preload, `
    'use strict';
    const cp = require('child_process');
    // grep --json answers as if every hit sat inside processPayment, so the
    // body-edit fallback (if the hook still had one) would resolve a symbol.
    cp.execFileSync = ${stubExec || `(bin, args) => args[0] === 'grep'
      ? JSON.stringify([{ file: 'src/payments.js', line: 3, container: { name: 'processPayment' } }])
      : JSON.stringify({
        direct_callers: 2, total_callers: 3, affected_files: 2, risk: 'medium',
        callers: [{ name: 'checkout', file: 'src/checkout.js', depth: 1 }],
        test_callers: [],
      })`};
    const fb = require(${JSON.stringify(path.join(__dirname, 'find-binary.js'))});
    fb.findBinary = () => ${JSON.stringify(path.join(home, 'never-executed-binary'))};
  `);

  const editedFile = path.resolve(workDir, relPath);
  const res = spawnSync(process.execPath, ['--require', preload, path.join(__dirname, 'pre-edit-guide.js')], {
    cwd: workDir,
    encoding: 'utf8',
    input: JSON.stringify({
      tool_name: 'Edit',
      ...(sessionId ? { session_id: sessionId } : {}),
      tool_input: { file_path: editedFile, old_string: oldString, new_string: newString },
    }),
    env: {
      ...process.env,
      HOME: home,
      USERPROFILE: home,
      TMPDIR: path.join(home, 'tmp'),
      TMP: path.join(home, 'tmp'),
      TEMP: path.join(home, 'tmp'),
      CLAUDE_CONFIG_DIR: path.join(home, '.claude'),
      ...extraEnv,
    },
  });
  return { res, home, proj };
}

test('emit(subprocess): the real hook emits additionalContext and NEVER auto-allows the Edit', (t) => {
  const { res } = runPreEditHook(t);
  assert.equal(res.status, 0, `hook exited ${res.status}: ${res.stderr}`);
  assert.notEqual(res.stdout.trim(), '',
    'the stubbed impact payload (2 direct callers) must open the emit path — ' +
    'an empty stdout here would make every assertion below vacuous');

  const payload = JSON.parse(res.stdout.trim());
  const out = payload.hookSpecificOutput;
  assert.equal(out.hookEventName, 'PreToolUse');
  assert.match(out.additionalContext, /code-graph:impact/,
    'the impact summary must ride additionalContext');

  // The P0: this hook fires on any symbol with >=1 caller, so an `allow` here
  // silently answers the user's Edit permission prompt for them.
  assert.ok(!('permissionDecision' in out),
    `PreToolUse(Edit) must carry no permissionDecision; got ${JSON.stringify(out.permissionDecision)}`);
  assert.doesNotMatch(res.stdout, /"allow"/);
});

test('D5: no impact while the startup index is being written', (t) => {
  // "2 callers, LOW" from a half-built index is a wrong risk, not a short one.
  const building = (proj) => require('node:fs').writeFileSync(
    require('node:path').join(proj, '.code-graph', 'indexing-status.json'),
    JSON.stringify({ s: 'indexing', d: 3, t: 50 }));
  const { res } = runPreEditHook(t, { setup: building });
  assert.equal(res.status, 0, res.stderr);
  assert.equal(res.stdout.trim(), '');
  // Control: the same edit with no build in progress injects (the first test above).
});

// ── Fires only when the edit changes a definition's header ──────────────────
// The contract at the top of pre-edit-guide.js: impact is for a signature being
// modified. Callers cannot break on a body-only edit, and the body-edit
// fallback that guessed the enclosing function by grep picked a wrong symbol
// for 108 of 608 TypeScript definitions in the 2026-09-28 hook audit.

test('scope: a body-only edit (no definition in old_string) injects nothing', (t) => {
  const { res } = runPreEditHook(t, {
    oldString: '  const total = computeTax(order) + shippingFee;',
    newString: '  const total = computeTax(order) + shippingFee + handlingFee;',
  });
  assert.equal(res.status, 0, res.stderr);
  assert.equal(res.stdout.trim(), '', `body edit must stay silent, got: ${res.stdout}`);
});

test('scope: an edit that keeps the definition header unchanged injects nothing', (t) => {
  const { res } = runPreEditHook(t, {
    oldString: 'function processPayment(order) {\n  return charge(order);',
    newString: 'function processPayment(order) {\n  return chargeWithRetry(order);',
  });
  assert.equal(res.status, 0, res.stderr);
  assert.equal(res.stdout.trim(), '', `unchanged header must stay silent, got: ${res.stdout}`);
});

test('scope: a changed signature still injects the impact summary', (t) => {
  const { res } = runPreEditHook(t, {
    oldString: 'function processPayment(order) {\n  return charge(order);',
    newString: 'function processPayment(order, currency) {\n  return charge(order, currency);',
  });
  assert.equal(res.status, 0, res.stderr);
  assert.match(res.stdout, /code-graph:impact\] processPayment\(\)/);
});

// Shapes 0.163.0's first-pattern-wins rule names right and efe41d0's
// earliest-match rule named wrong, which left both this hook and the Stop check
// blind to a signature change (pre-tag review 2026-09-29). The rule is
// 0.163.0's again; these pin it.
for (const [label, relPath, oldString, newString, name] of [
  ['a Rust doc comment\'s prose', 'src/a.rs',
    '/// Adds one; the function that every caller in b.rs uses.\npub fn target(a: i32) -> i32 {',
    '/// Adds one; the function that every caller in b.rs uses.\npub fn target(a: i32, b: i32) -> i32 {',
    'target'],
  ['a Rust `if` on a member call', 'src/a.rs',
    '    if v.is_empty() {\n        return 0;\n    }\n    v.len()\n}\n\npub fn target(a: i32) -> i32 {',
    '    if v.is_empty() {\n        return 0;\n    }\n    v.len()\n}\n\npub fn target(a: i32, b: i32) -> i32 {',
    'target'],
  ['a JS `if` statement', 'src/payments.js',
    '  if (x > 0) {\n    return 1;\n  }\n}\n\nfunction processPayment(order) {',
    '  if (x > 0) {\n    return 1;\n  }\n}\n\nfunction processPayment(order, currency) {',
    'processPayment'],
]) {
  test(`scope: ${label} ahead of a changed header does not name the symbol`, (t) => {
    const { res } = runPreEditHook(t, { relPath, oldString, newString });
    assert.equal(res.status, 0, res.stderr);
    assert.match(res.stdout, new RegExp(`code-graph:impact\\] ${name}\\(\\)`));
  });
}

// The hook's own loop, per language (second review round, 2026-09-29): the
// fn-extract tests above run the patterns first-match in array order, so they
// never saw that the first repair's allow-list of JS modifiers rejected every
// definition whose return type or keyword comes first. Each case changes the
// header and must inject its own name.
for (const [label, relPath, header, changed, name] of [
  ['C', 'src/parse.c', 'int parse_header(const char *buf, int n) {', 'int parse_header(const char *buf, int n, int flags) {', 'parse_header'],
  ['C++ out-of-line', 'src/db.cc', 'Status DBImpl::Write(const WriteOptions& o) {', 'Status DBImpl::Write(const WriteOptions& o, int x) {', 'Write'],
  ['Java package-private', 'src/A.java', 'void recalculate(int depth) {', 'void recalculate(int depth, int max) {', 'recalculate'],
  ['Kotlin', 'src/App.kt', 'fun processOrder(order: Int) {', 'fun processOrder(order: Int, rush: Boolean) {', 'processOrder'],
  ['Kotlin extension', 'src/Ext.kt', 'fun String.toSlug(sep: Char) {', 'fun String.toSlug(sep: Char, max: Int) {', 'toSlug'],
  ['C#', 'src/Repo.cs', 'public async Task SaveAsync(int order)\n{', 'public async Task SaveAsync(int order, bool flush)\n{', 'SaveAsync'],
  ['Dart', 'lib/w.dart', 'Widget build(BuildContext context) {', 'Widget build(BuildContext context, int n) {', 'build'],
  ['JS generator', 'src/store.js', '  *entries(prefix) {', '  *entries(prefix, limit) {', 'entries'],
  ['Rust after a comment ending in async', 'src/a.rs', '// TODO: make this async\nfn load_all(p: &Path) -> Vec<u8> {', '// TODO: make this async\nfn load_all(p: &Path, n: usize) -> Vec<u8> {', 'load_all'],
  ['PHP after a foreach', 'src/a.php', '    foreach ($items as $item) {\n        $x++;\n    }\n}\n\npublic function store($request) {', '    foreach ($items as $item) {\n        $x++;\n    }\n}\n\npublic function store($request, $opts) {', 'store'],
  ['JS after a describe callback', 'src/a.js', "describe('x', function () {\n  it('y');\n});\n\nfunction save(a) {", "describe('x', function () {\n  it('y');\n});\n\nfunction save(a, b) {", 'save'],
]) {
  test(`scope: a ${label} definition whose header changed names itself`, (t) => {
    const { res } = runPreEditHook(t, { relPath, oldString: header, newString: changed });
    assert.equal(res.status, 0, res.stderr);
    assert.match(res.stdout, new RegExp(`code-graph:impact\\] ${name}\\(\\)`), `got: ${res.stdout}`);
  });
}

test('scope: an edit to a file outside the project injects nothing', (t) => {
  const { res } = runPreEditHook(t, { relPath: '../elsewhere/payments.js' });
  assert.equal(res.status, 0, res.stderr);
  assert.equal(res.stdout.trim(), '', `outside-project edit must stay silent, got: ${res.stdout}`);
});

test('scope: CODE_GRAPH_QUIET_HOOKS=1 silences the impact summary', (t) => {
  const { res } = runPreEditHook(t, { extraEnv: { CODE_GRAPH_QUIET_HOOKS: '1' } });
  assert.equal(res.status, 0, res.stderr);
  assert.equal(res.stdout.trim(), '', `QUIET must silence the hook, got: ${res.stdout}`);
});

// ── Q4: same-file same-name definitions ─────────────────────────────────────
// `impact X --file F` refuses when F defines X more than once (two classes'
// `fetch`), listing each definition's node_id and start line. The hook used to go
// silent there; it now asks again with the node_id of the definition the edit
// changes — the last one starting at or before the edited header's line.

const TWO_GETS = 'class A:\n    def fetch(self, k):\n        return k\n\n\nclass B:\n    def fetch(self, k):\n        return k\n';

// The stub binary: refuses by name (exit 1 with the refusal on stdout, as the
// CLI does), answers by id with a caller named after the id it was asked for.
const STUB_SAME_FILE = `(bin, args) => {
  if (args.includes('--node-id')) {
    const id = args[args.indexOf('--node-id') + 1];
    return JSON.stringify({ direct_callers: 1, total_callers: 1, affected_files: 1, risk: 'low',
      callers: [{ name: 'caller_of_' + id, file: 'src/use.py', depth: 1 }], test_callers: [] });
  }
  const e = new Error('exit 1');
  e.status = 1;
  e.stdout = JSON.stringify({ error: "Ambiguous symbol 'fetch': 2 definitions in the same file (src/app.py).",
    suggestions: [
      { name: 'fetch', file_path: 'src/other.py', type: 'method', node_id: 99, start_line: 7 },
      { name: 'fetch', file_path: 'src/app.py', type: 'method', node_id: 11, start_line: 2 },
      { name: 'fetch', file_path: 'src/app.py', type: 'method', node_id: 22, start_line: 7 },
    ] });
  throw e;
}`;

function writeTwoGets(proj) {
  const fs = require('node:fs');
  const path = require('node:path');
  fs.mkdirSync(path.join(proj, 'src'), { recursive: true });
  fs.writeFileSync(path.join(proj, 'src', 'app.py'), TWO_GETS);
}

test('Q4: a same-file same-name edit injects the impact of the edited definition', (t) => {
  const second = runPreEditHook(t, {
    relPath: 'src/app.py', setup: writeTwoGets, stubExec: STUB_SAME_FILE,
    oldString: 'class B:\n    def fetch(self, k):', newString: 'class B:\n    def fetch(self, k, default=None):',
  });
  assert.equal(second.res.status, 0, second.res.stderr);
  assert.match(second.res.stdout, /caller_of_22/, 'B.fetch starts at line 7');
  const first = runPreEditHook(t, {
    relPath: 'src/app.py', setup: writeTwoGets, stubExec: STUB_SAME_FILE,
    oldString: '    def fetch(self, k):\n        return k\n\n\nclass B:', newString: '    def fetch(self, key):\n        return key\n\n\nclass B:',
  });
  assert.equal(first.res.status, 0, first.res.stderr);
  assert.match(first.res.stdout, /caller_of_11/, 'A.fetch starts at line 2');
});

// The refusal lists at most five definitions (`total` says how many exist, in
// source order). An edit past the last listed one may be an unlisted
// definition: picking the last listed named `go_5`'s caller for C7.run
// (pre-tag review 2026-09-29). Only there the hook stays silent.
const SEVEN_RUNS = Array.from({ length: 7 }, (_, i) =>
  `class C${i + 1}:\n    def run(self):\n        return ${i + 1}\n\n`).join('');
const STUB_CUT_LIST = `(bin, args) => {
  if (args.includes('--node-id')) {
    const id = args[args.indexOf('--node-id') + 1];
    return JSON.stringify({ direct_callers: 1, total_callers: 1, affected_files: 1, risk: 'low',
      callers: [{ name: 'caller_of_' + id, file: 'src/views.py', depth: 1 }], test_callers: [] });
  }
  const e = new Error('exit 1');
  e.status = 1;
  e.stdout = JSON.stringify({ error: "Ambiguous symbol 'run': 7 definitions in the same file (src/views.py). Showing the first 5 of 7.",
    suggestions: [2, 6, 10, 14, 18].map((line, i) =>
      ({ name: 'run', file_path: 'src/views.py', type: 'method', node_id: 101 + i, start_line: line })),
    total: 7 });
  throw e;
}`;
for (const [k, expect] of [[7, null], [2, 'caller_of_102'], [5, 'caller_of_105']]) {
  test(`Q4: C${k}.run with the refusal list cut at five → ${expect || 'silent'}`, (t) => {
    const { res } = runPreEditHook(t, {
      relPath: 'src/views.py', stubExec: STUB_CUT_LIST,
      setup: (proj) => {
        require('node:fs').mkdirSync(require('node:path').join(proj, 'src'), { recursive: true });
        require('node:fs').writeFileSync(require('node:path').join(proj, 'src', 'views.py'), SEVEN_RUNS);
      },
      oldString: `class C${k}:\n    def run(self):`, newString: `class C${k}:\n    def run(self, fast=False, *, x):`,
    });
    assert.equal(res.status, 0, res.stderr);
    if (expect) assert.match(res.stdout, new RegExp(expect));
    else assert.equal(res.stdout.trim(), '', `must stay silent, got: ${res.stdout}`);
  });
}

// Where a definition starts is its name's line: `// TODO: make this async\n
// fn load(` matches from the comment line, and counting from there picked
// the previous `load` of the file.
const TWO_LOADS = 'impl A {\n    // TODO: make this async\n    fn load(&self) {}\n}\n\nimpl B {\n    // TODO: make this async\n    fn load(&self) {}\n}\n';
const STUB_TWO_LOADS = STUB_SAME_FILE
  .replace("'Ambiguous symbol \\'fetch\\'", "'Ambiguous symbol \\'load\\'")
  .replace(/file_path: 'src\/app.py', type: 'method', node_id: 11, start_line: 2/, "file_path: 'src/lib.rs', type: 'method', node_id: 11, start_line: 3")
  .replace(/file_path: 'src\/app.py', type: 'method', node_id: 22, start_line: 7/, "file_path: 'src/lib.rs', type: 'method', node_id: 22, start_line: 8");
test('Q4: a match that starts on a comment line counts from its name\'s line', (t) => {
  const { res } = runPreEditHook(t, {
    relPath: 'src/lib.rs', stubExec: STUB_TWO_LOADS,
    setup: (proj) => {
      require('node:fs').mkdirSync(require('node:path').join(proj, 'src'), { recursive: true });
      require('node:fs').writeFileSync(require('node:path').join(proj, 'src', 'lib.rs'), TWO_LOADS);
    },
    oldString: 'impl B {\n    // TODO: make this async\n    fn load(&self) {}',
    newString: 'impl B {\n    // TODO: make this async\n    fn load(&self, n: usize) {}',
  });
  assert.equal(res.status, 0, res.stderr);
  assert.match(res.stdout, /caller_of_22/, `B.load starts at line 8; got: ${res.stdout}`);
});

test('Q4: no candidate at or before the edited line → silent, never a guess', (t) => {
  const stub = STUB_SAME_FILE.replace(/start_line: \d+ }/g, 'start_line: 50 }');
  const { res } = runPreEditHook(t, {
    relPath: 'src/app.py', setup: writeTwoGets, stubExec: stub,
    oldString: 'class B:\n    def fetch(self, k):', newString: 'class B:\n    def fetch(self, k, default=None):',
  });
  assert.equal(res.status, 0, res.stderr);
  assert.equal(res.stdout.trim(), '');
});

// ── D#560: edits in a linked worktree ───────────────────────────────────────
// The index stores the main checkout's layout. From `.claude/worktrees/<slug>`
// the hook asked for `impact X --file .claude/worktrees/<slug>/src/…`, which
// the CLI refuses as "File not found in index" (exit 1, candidates on stdout),
// and the hook went silent for every edit in a worktree subagent: 2026-10-16..23,
// 725 of the neutral projects' ~1,694 edits.

// Answers like the CLI: a `--file` the index does not hold is refused.
const STUB_INDEX_KEYS = `(bin, args) => {
  const f = args.includes('--file') ? args[args.indexOf('--file') + 1] : null;
  if (f !== null && f !== 'src/payments.js') {
    const e = new Error('exit 1');
    e.status = 1;
    e.stdout = JSON.stringify({ error: 'File not found in index', file: f, symbol: args[1],
      candidates: [{ file_path: 'src/payments.js', name: args[1], node_id: 7, start_line: 1, type: 'function' }] });
    throw e;
  }
  return JSON.stringify({ direct_callers: 2, total_callers: 3, affected_files: 2, risk: 'medium',
    callers: [{ name: 'checkout', file: 'src/checkout.js', depth: 1 }], test_callers: [] });
}`;

test('D#560: a changed signature in a linked worktree injects the impact summary', (t) => {
  const edit = {
    stubExec: STUB_INDEX_KEYS,
    oldString: 'function processPayment(order) {\n  return charge(order);',
    newString: 'function processPayment(order, currency) {\n  return charge(order, currency);',
  };
  const main = runPreEditHook(t, edit);
  assert.match(main.res.stdout, /code-graph:impact\] processPayment\(\)/, 'control: the main checkout');
  const { res } = runPreEditHook(t, { ...edit, worktree: 'wt' });
  assert.equal(res.status, 0, res.stderr);
  assert.match(res.stdout, /code-graph:impact\] processPayment\(\)[\s\S]*checkout \(src\/checkout\.js\)/,
    `got: ${res.stdout}`);
});

// The candidates' start lines are the main checkout's (the index describes its
// copy), so the edited line is counted there too. Counted in the worktree's
// copy, whose lines had moved, it named A.fetch's callers for an edit of
// B.fetch (review of D#560: before the fix the hook was silent here; a wrong
// answer is worse).
const TWO_GETS_SHIFTED = 'class A:\n    def fetch(self, k):\n        return k\nclass B:\n    def fetch(self, k):\n        return k\n';

test('D#560: a same-file same-name edit in a worktree is counted in the copy the index describes', (t) => {
  const { res } = runPreEditHook(t, {
    worktree: 'wt', relPath: 'src/app.py', stubExec: STUB_SAME_FILE,
    setup: (proj, workDir) => {
      writeTwoGets(proj);
      require('node:fs').mkdirSync(require('node:path').join(workDir, 'src'), { recursive: true });
      require('node:fs').writeFileSync(require('node:path').join(workDir, 'src', 'app.py'), TWO_GETS_SHIFTED);
    },
    oldString: 'class B:\n    def fetch(self, k):', newString: 'class B:\n    def fetch(self, k, default=None):',
  });
  assert.equal(res.status, 0, res.stderr);
  assert.match(res.stdout, /caller_of_22/, `B.fetch is at line 7 in the indexed copy (5 in the worktree's); got: ${res.stdout}`);
});

test('D#560: a same-file same-name edit whose text the main checkout lacks gets no guess', (t) => {
  const { res } = runPreEditHook(t, {
    worktree: 'wt', relPath: 'src/app.py', stubExec: STUB_SAME_FILE,
    setup: (proj, workDir) => writeTwoGets(workDir),
    oldString: 'class B:\n    def fetch(self, k):', newString: 'class B:\n    def fetch(self, k, default=None):',
  });
  assert.equal(res.status, 0, res.stderr);
  assert.equal(res.stdout.trim(), '', `got: ${res.stdout}`);
});

// The main checkout's copy exists but lacks the edited text (the worktree's
// copy has moved on): no line to count, so no guess. The definition sits
// past the hunk's first line so that a line counted without the text would
// land on a candidate (a definition at offset 0 counts to line 1, before both).
test('D#560: a same-file same-name edit whose text is not in the main checkout\'s copy gets no guess', (t) => {
  const { res } = runPreEditHook(t, {
    worktree: 'wt', relPath: 'src/app.py', stubExec: STUB_SAME_FILE,
    setup: (proj, workDir) => {
      writeTwoGets(proj);
      require('node:fs').mkdirSync(require('node:path').join(workDir, 'src'), { recursive: true });
      require('node:fs').writeFileSync(require('node:path').join(workDir, 'src', 'app.py'),
        TWO_GETS.replace(/return k\n$/, 'return k + 1\n'));
    },
    oldString: 'class B:\n    def fetch(self, k):\n        return k + 1',
    newString: 'class B:\n    def fetch(self, k, d=None):\n        return k + 1',
  });
  assert.equal(res.status, 0, res.stderr);
  assert.equal(res.stdout.trim(), '', `got: ${res.stdout}`);
});

// Unique in the worktree's copy, twice in the main checkout's (A's header was
// already changed in the worktree): the first occurrence there is A's, and
// counting from it named A.fetch's callers for an edit of B.fetch (review of
// 0.178.0). Neither occurrence can be told to be the edited one: no guess.
test('D#560: a same-file same-name edit whose text the main checkout holds twice gets no guess', (t) => {
  const { res } = runPreEditHook(t, {
    worktree: 'wt', relPath: 'src/app.py', stubExec: STUB_SAME_FILE,
    setup: (proj, workDir) => {
      writeTwoGets(proj);
      require('node:fs').mkdirSync(require('node:path').join(workDir, 'src'), { recursive: true });
      require('node:fs').writeFileSync(require('node:path').join(workDir, 'src', 'app.py'),
        TWO_GETS.replace('def fetch(self, k):', 'def fetch(self, k, d=None):'));
    },
    oldString: '    def fetch(self, k):\n        return k', newString: '    def fetch(self, key):\n        return key',
  });
  assert.equal(res.status, 0, res.stderr);
  assert.equal(res.stdout.trim(), '', `got: ${res.stdout}`);
});

// The rule is the worktree's only. In the main checkout a text the file holds
// twice is a `replace_all` Edit (Claude Code checks uniqueness only without
// it), which picks the first occurrence's definition as 0.177.0 did: one of
// the definitions the edit changes (review of 0.178.0).
test('Q4: a main-checkout edit whose text the file holds twice (replace_all) keeps its pick', (t) => {
  const { res } = runPreEditHook(t, {
    relPath: 'src/app.py', setup: writeTwoGets, stubExec: STUB_SAME_FILE,
    oldString: '    def fetch(self, k):\n        return k', newString: '    def fetch(self, key):\n        return key',
  });
  assert.equal(res.status, 0, res.stderr);
  assert.match(res.stdout, /caller_of_11/, `A.fetch, the first occurrence; got: ${res.stdout}`);
});

// ── Which definition the hunk changed: the Stop check's reader ─────────────
// tasks/specs/pre-edit-pick-by-reader.md, shapes S1-S9. The first pattern to
// match names the pick; where stop-impact.js reads the file's language, its
// reader decides which definition's header the edit changed. Measured on every
// Edit hunk in this machine's transcripts (4,118): the pick sent 35 of its 103
// impact queries for names that are no definition there (a Rust `match f(…)
// {`, a comment's "function already", a string), and missed 24 hunks whose
// definition header changed (a later definition, a `{` inside the parameters,
// an old header still inside the new one).

// Answers for any name, so the summary says which name was asked for.
const STUB_BY_NAME = `(bin, args) => JSON.stringify({ direct_callers: 1, total_callers: 1,
  affected_files: 1, risk: 'low', callers: [{ name: 'caller_of_' + args[1], file: 'src/use.x', depth: 1 }],
  test_callers: [] })`;

function pickOf(t, relPath, oldString, newString, extra = {}) {
  const run = runPreEditHook(t, { relPath, oldString, newString, stubExec: STUB_BY_NAME, ...extra });
  assert.equal(run.res.status, 0, run.res.stderr);
  const m = /caller_of_(\w+)/.exec(run.res.stdout);
  return { picked: m ? m[1] : null, ...run };
}

test('pick S1: a Rust call site `match f(…) {` is no definition: no summary', (t) => {
  const { picked } = pickOf(t, 'src/lib.rs',
    '    match parse_with_deadline(parser, source, timeout) {\n        Ok(t) => t,',
    '    match parse_with_deadline(parser, &parsed, timeout) {\n        Ok(t) => t,');
  assert.equal(picked, null);
});

test('pick S2: past a comment\'s "function already", the definition the edit changed', (t) => {
  const { picked } = pickOf(t, 'lib/db.mjs',
    '/**\n * a second call to recover a number this function already had\n */\nexport function dbUnusableRemedy(dbPath, err) {',
    '/**\n * a second call to recover a number this function already had\n */\nexport function dbUnusableRemedy(dbPath, err, { afterRepair = false } = {}) {');
  assert.equal(picked, 'dbUnusableRemedy');
});

test('pick S3: a `{` inside the parameters does not end the header', (t) => {
  const { picked } = pickOf(t, 'lib/steer.mjs',
    'export function writeLocalSteering(root, { slug, version }) {\n  return slug;',
    'export function writeLocalSteering(root, { slug, version, cwd = root }) {\n  return slug;');
  assert.equal(picked, 'writeLocalSteering');
});

test('pick S4: the later definition the edit changed, recorded for the Stop check', (t) => {
  const { picked, home } = pickOf(t, 'src/resolve.rs',
    'fn resolve_pending_calls(db: &Db) -> usize {\n    resolve_pending_calls_touching(db, &mut Vec::new())\n}\n\n' +
    'fn resolve_pending_calls_touching(db: &Db, touched: &mut Vec<u32>) -> usize {',
    'fn resolve_pending_calls(db: &Db) -> usize {\n    resolve_pending_calls_touching(db, &mut Vec::new(), &mut Vec::new())\n}\n\n' +
    'fn resolve_pending_calls_touching(db: &Db, touched: &mut Vec<u32>, seen: &mut Vec<u32>) -> usize {',
    { sessionId: 'sess-pick-s4' });
  assert.equal(picked, 'resolve_pending_calls_touching');
  const fs = require('node:fs');
  const path = require('node:path');
  const dir = path.join(home, 'tmp', 'code-graph-mcp');
  const log = fs.readdirSync(dir).find((f) => f.startsWith('.cg-edits-') && f.endsWith('-sess-pick-s4.jsonl'));
  assert.ok(log, `no edit log in ${dir}`);
  const symbols = fs.readFileSync(path.join(dir, log), 'utf8').split('\n').filter(Boolean)
    .map((l) => JSON.parse(l).symbol);
  assert.ok(symbols.includes('resolve_pending_calls_touching'), `logged: ${symbols}`);
});

test('pick S5: of two same-named definitions in the hunk, the changed one is placed', (t) => {
  const { res } = runPreEditHook(t, {
    relPath: 'src/app.py', setup: writeTwoGets, stubExec: STUB_SAME_FILE,
    oldString: 'class A:\n    def fetch(self, k):\n        return k\n\n\nclass B:\n    def fetch(self, k):',
    newString: 'class A:\n    def fetch(self, k):\n        return k\n\n\nclass B:\n    def fetch(self, k, default=None):',
  });
  assert.equal(res.status, 0, res.stderr);
  assert.match(res.stdout, /caller_of_22/, `B.fetch starts at line 7; got: ${res.stdout}`);
});

test('pick S6: a JS method whose parameters span lines keeps its summary', (t) => {
  // stop-impact.js reads one-line method headers only; the pick reads this one.
  const { picked } = pickOf(t, 'src/svc.js',
    '  async charge(order,\n    currency) {\n    return 1;',
    '  async charge(order,\n    currency, opts) {\n    return 1;');
  assert.equal(picked, 'charge');
});

test('pick S7: a language the reader does not read keeps the pick', (t) => {
  const { picked } = pickOf(t, 'scripts/deploy.sh',
    'function deploy_app() {\n  echo hi',
    'deploy_app() {\n  echo hi');
  assert.equal(picked, 'deploy_app');
});

test('pick S8: a header the reader cannot read keeps the pick, in a keyword language too', (t) => {
  const params = Array.from({ length: 80 }, (_, i) => `p${i}: u32`).join(', ');
  assert.ok(params.length > 600, 'longer than stop-impact.js MAX_SIGNATURE_CHARS');
  const { picked } = pickOf(t, 'src/big.rs',
    `fn big_one(${params}) -> u32 {\n    0`,
    `fn big_one(${params.replace('p0: u32', 'q0: u64')}) -> u32 {\n    0`);
  assert.equal(picked, 'big_one');
});

test('pick S9: a pick on a comment line with no changed definition: no summary', (t) => {
  const { picked } = pickOf(t, 'src/limits.js',
    '// a function wrapping head under\nconst limit = 1;',
    '// a function wrapping it under\nconst limit = 1;');
  assert.equal(picked, null);
});

// Pre-tag review of 0.179.0, H1: two definitions of one name with one old
// header were matched by count, which credited an edit of the first to the
// second (a real index: `impl Alpha { fn make() }` edited, Beta's callers).
test('pick: of two same-named definitions with one old header, the first one changed is placed', (t) => {
  const { res } = runPreEditHook(t, {
    relPath: 'src/app.py', setup: writeTwoGets, stubExec: STUB_SAME_FILE,
    oldString: TWO_GETS,
    newString: TWO_GETS.replace('    def fetch(self, k):', '    def fetch(self, k, default=None):'),
  });
  assert.equal(res.status, 0, res.stderr);
  assert.match(res.stdout, /caller_of_11/, `A.fetch starts at line 2; got: ${res.stdout}`);
});

test('pick: one of two same-named definitions with one old header removed: no guess', (t) => {
  const { res } = runPreEditHook(t, {
    relPath: 'src/app.py', setup: writeTwoGets, stubExec: STUB_SAME_FILE,
    oldString: TWO_GETS,
    newString: TWO_GETS.replace('    def fetch(self, k):\n        return k\n', ''),
  });
  assert.equal(res.status, 0, res.stderr);
  assert.doesNotMatch(res.stdout, /caller_of_/, 'which of the two went is not in the text');
});

// Review M1: a definition that is not the first thing on its line — the
// reader skips the line (`#`, `/*`) or reads its header from the line's start
// (`impl X {`) — kept 0.178.0's summary through the pick.
test('pick: a definition after an attribute, a closed comment or an `impl {` on its line keeps its summary', (t) => {
  const cases = [
    ['src/a.rs', '#[inline] pub fn widen(a: u8) -> u8 {\n    a', '#[inline] pub fn widen(a: u8, b: u8) -> u8 {\n    a', 'widen'],
    ['src/a.rs', '/* fast path */ pub fn quick(a: u8) {\n}', '/* fast path */ pub fn quick(a: u8, b: u8) {\n}', 'quick'],
    ['src/a.rs', 'impl Conv for Cfg { fn convert(&self, a: u8) -> u8 { a } }',
      'impl Conv for Cfg { fn convert(&self, a: u16) -> u16 { a } }', 'convert'],
    ['src/a.php', '#[Route("/x")] public function show($id) {\n}', '#[Route("/x")] public function show($id, $fmt) {\n}', 'show'],
    ['src/q.js', '/* istanbul ignore next */ function flushQueue(q) {\n}', '/* istanbul ignore next */ function flushQueue(q, n) {\n}', 'flushQueue'],
    ['src/n.ts', '/** @internal */ export function getNodeId(node: Node) {\n}', '/** @internal */ export function getNodeId(node: Node, at: number) {\n}', 'getNodeId'],
    ['src/f.c', '/* hot */ static int fold(int a) {\n}', '/* hot */ static int fold(int a, int b) {\n}', 'fold'],
  ];
  for (const [rel, before, after, name] of cases) {
    assert.equal(pickOf(t, rel, before, after).picked, name, `${rel}: ${before}`);
  }
});

// Review M2: a C++ statement `Type name(args);` reads as a definition. The
// edit is in `total`'s body; `buffer` is a variable, whatever the index holds.
test('pick: a C-family statement the reader takes for a definition is not named instead', (t) => {
  for (const [before, after] of [
    ['static int total(const C& c) {\n  std::vector<int> buffer(c.n);', 'static int total(const C& c) {\n  std::vector<int> buffer(c.n * 2);'],
    ['void Spin::step(int a) {\n  emit valueChanged(a + 1);', 'void Spin::step(int a) {\n  emit valueChanged(a + 2);'],
  ]) {
    const { picked, home } = pickOf(t, 'src/w.cpp', before, after, { sessionId: 'sess-cpp' });
    assert.equal(picked, null, before);
    const fs = require('node:fs');
    const path = require('node:path');
    const dir = path.join(home, 'tmp', 'code-graph-mcp');
    const log = fs.readdirSync(dir).find((f) => f.endsWith('-sess-cpp.jsonl'));
    const symbols = fs.readFileSync(path.join(dir, log), 'utf8').split('\n').filter(Boolean)
      .map((l) => JSON.parse(l).symbol).filter(Boolean);
    assert.deepEqual(symbols, [before.startsWith('static') ? 'total' : 'step'], 'the Stop check gets the pick only');
  }
});

test('pick: a changed definition of the picked name wins over an earlier changed one', (t) => {
  const { picked } = pickOf(t, 'src/app.py',
    'def fetch(self):\n    pass\n\ndef other(a):\n    pass\n\ndef fetch(self, k):\n    pass',
    'def fetch(self):\n    pass\n\ndef other(a, b):\n    pass\n\ndef fetch(self, k, d):\n    pass');
  assert.equal(picked, 'fetch');
});

test('pick: of several changed definitions, the first in the hunk', (t) => {
  const { picked } = pickOf(t, 'src/lib.rs',
    'fn keep_one() {\n}\nfn second_one(a: u8) {\n}\nfn third_one(a: u8) {\n}',
    'fn keep_one() {\n}\nfn second_one(a: u16) {\n}\nfn third_one(a: u16) {\n}');
  assert.equal(picked, 'second_one');
});

test('pick: a changed definition past the first 8 KB of the hunk is read', (t) => {
  const body = '    let x = 1;\n'.repeat(700);
  assert.ok(body.length > 8192);
  const { picked } = pickOf(t, 'src/lib.rs',
    `fn keep_one() {\n${body}}\nfn far_one(a: u8) {\n}`,
    `fn keep_one() {\n${body}}\nfn far_one(a: u16) {\n}`);
  assert.equal(picked, 'far_one');
});

test('pick: a pick inside a block comment that closes after it is on a comment', (t) => {
  const { picked } = pickOf(t, 'src/limits.js',
    '/* this function already caps it */\nconst limit = 1;',
    '/* this function always caps it */\nconst limit = 1;');
  assert.equal(picked, null);
});

test('pick: the comment test reads the pick\'s line in the old text', (t) => {
  // The new text has code where the old one had the comment line.
  const { picked } = pickOf(t, 'src/limits.js',
    '// a function wrapping head under\nconst limit = 1;',
    'const limit = 2;\n// a function wrapping it under');
  assert.equal(picked, null);
});

// E2E 2026-10-16 (plugin-user QA, P1): on flask `url_for`, `impact --json`
// reported 7 direct callers AND `ambiguous_callers_excluded: 32`; the CLI's text
// prints the ⚠ line, the hook printed "7 direct callers" alone — an undercount
// presented to the model as the blast radius. Same for a truncated traversal.
test('emit(subprocess): the summary carries the callers impact left out, not just its counts', (t) => {
  const stubExec = `(bin, args) => JSON.stringify({
    direct_callers: 7, total_callers: 8, affected_files: 3, risk: 'high',
    callers: [{ name: 'checkout', file: 'src/checkout.js', depth: 1 }],
    test_callers: [],
    ambiguous_callers_excluded: 32,
    callers_truncated: true,
    callers_truncated_note: 'Caller traversal hit the depth cap, so the caller counts … below …',
  })`;
  const { res } = runPreEditHook(t, { stubExec });
  assert.equal(res.status, 0, res.stderr);
  const ctx = JSON.parse(res.stdout.trim()).hookSpecificOutput.additionalContext;
  assert.match(ctx, /7 direct callers/);
  // Call edges, not callers: one caller can reach several same-named
  // definitions (round 3: flask `post` said 215 where 1 caller and 27 tests
  // were added).
  assert.match(ctx, /32 more call edge\(s\) matched only by name/, ctx);
  assert.match(ctx, /caller traversal stopped early, so these counts are a floor/, ctx);

  // Control: a payload without them adds neither line.
  const plain = runPreEditHook(t);
  const plainCtx = JSON.parse(plain.res.stdout.trim()).hookSpecificOutput.additionalContext;
  assert.doesNotMatch(plainCtx, /matched only by name|counts are a floor/);
});
