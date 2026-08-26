'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');

// Pre-edit-guide.js is a script with side effects (reads stdin, checks db).
// We test its PATTERNS directly without requiring the module.

// --- Function signature patterns (copied from pre-edit-guide.js) ---
const fnPatterns = [
  /(?:pub\s+)?(?:async\s+)?fn\s+(\w+)/,                        // Rust
  /(?:export\s+)?(?:async\s+)?function\s+(\w+)/,                // JS/TS
  /(?:const|let|var)\s+(\w+)\s*=\s*(?:async\s+)?(?:\([^)]*\)|_)\s*=>/, // JS arrow
  /(?:async\s+)?(\w+)\s*\([^)]*\)\s*\{/,                       // JS method / Go func
  /def\s+(\w+)/,                                                // Python/Ruby
  /func\s+(\w+)/,                                               // Go/Swift
  /(?:public|private|protected|static|override|virtual|abstract|internal)\s+\S+\s+(\w+)\s*\(/, // Java/C#/Kotlin
  /(?:public\s+)?function\s+(\w+)/,                             // PHP
];

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

test('pattern-sync: fnPatterns count matches source', () => {
  const fs = require('node:fs');
  const path = require('node:path');
  const source = fs.readFileSync(path.join(__dirname, 'pre-edit-guide.js'), 'utf8');
  // Count regex pattern lines in the fnPatterns array (lines containing // Language comment)
  const sourcePatternCount = (source.match(/\/\/\s*(Rust|JS|Python|Go|Java|C#|PHP|Ruby|Swift|Kotlin)/g) || []).length;
  assert.ok(fnPatterns.length === 8, `Expected 8 patterns, got ${fnPatterns.length}`);
  assert.ok(sourcePatternCount >= 7, `Source should have >= 7 language comments, found ${sourcePatternCount}`);
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
