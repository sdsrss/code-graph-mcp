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
// The threshold sits above the linear number and three orders of magnitude below
// the quadratic one: anything in between means a bound was dropped.
test('SEC-04: bracket-free word-dense input matches in linear time', () => {
  for (const build of [
    (n) => 'a'.repeat(n),                        // one unbroken \w run
    (n) => 'foo_bar_baz_'.repeat(Math.ceil(n / 12)).slice(0, n), // underscore-dense
    (n) => 'public static void '.repeat(Math.ceil(n / 19)).slice(0, n), // feeds the \S+ pattern
  ]) {
    const input = build(400 * 1024);
    const t0 = process.hrtime.bigint();
    for (const pat of fnPatterns) input.match(pat);
    const ms = Number(process.hrtime.bigint() - t0) / 1e6;
    assert.ok(ms < 250, `400 KB of word-dense input took ${ms.toFixed(1)}ms across all patterns`);
  }
});

test('SEC-04: the scan window is bounded in the hook itself', () => {
  // Belt to the quantifier caps' braces, and the part that bounds a pattern a
  // future author adds without reading the note.
  assert.match(SOURCE, /oldStr\.length > 8192 \? oldStr\.slice\(0, 8192\)/);
  assert.match(SOURCE, /for \(const pat of fnPatterns\) \{\n\s*const m = scanned\.match\(pat\)/);
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

function runPreEditHook(t, { oldString = 'function processPayment(order) {', extraEnv = {} } = {}) {
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

  const preload = path.join(home, 'stub-preload.js');
  fs.writeFileSync(preload, `
    'use strict';
    const cp = require('child_process');
    cp.execFileSync = () => JSON.stringify({
      direct_callers: 2, total_callers: 3, affected_files: 2, risk: 'medium',
      callers: [{ name: 'checkout', file: 'src/checkout.js', depth: 1 }],
      test_callers: [],
    });
    const fb = require(${JSON.stringify(path.join(__dirname, 'find-binary.js'))});
    fb.findBinary = () => ${JSON.stringify(path.join(home, 'never-executed-binary'))};
  `);

  const editedFile = path.join(proj, 'src', 'payments.js');
  const res = spawnSync(process.execPath, ['--require', preload, path.join(__dirname, 'pre-edit-guide.js')], {
    cwd: proj,
    encoding: 'utf8',
    input: JSON.stringify({
      tool_name: 'Edit',
      tool_input: { file_path: editedFile, old_string: oldString, new_string: 'x' },
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
