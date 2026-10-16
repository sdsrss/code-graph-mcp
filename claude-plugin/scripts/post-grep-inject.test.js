'use strict';
// These tests spawn a real `node` stub through cg-answer, whose production
// timeout is 2 s. Under a loaded machine cold node startup can exceed it,
// which made the fanout-hint assertions fail about one run in seven while
// passing 12/12 in isolation. The timeout is a product decision for the hook,
// not for the test harness, so the test raises it rather than the product
// lowering its bar.
process.env._CG_ANSWER_TIMEOUT_MS = process.env._CG_ANSWER_TIMEOUT_MS || '30000';
const test = require('node:test');
const assert = require('node:assert/strict');
const { spawnSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

// TMPDIR is redirected into a private sandbox BEFORE `./tmp-dir` is required,
// because that module resolves `CG_TMP_DIR` from `os.tmpdir()` at require time —
// assigning after the require would be inert.
//
// The axis here is residue, not destruction. `recordInject` writes a
// `.code-graph-postinject-<cwdHash>-<commandHash>` flag into the ONE
// machine-global cgTmpDir: measured on the commit before this one, one run of
// this file left 10 flags behind (14 across the three hook test files),
// reclaimed only by pruneCgTmp's 24h sweep. `cleanupFixture` was supposed to
// cover that and did not — see the spelling bug documented there. Owning the
// directory fixes both the flags this file knows about and any a future test
// adds.
//
// This used to claim "every e2e command carries a `Date.now()` so no two runs
// ever collide". Pre-tag review judged that false and only the CHANGELOG copy
// of the sentence was corrected (audit 2026-08-29 ENG-03). What is true: most
// commands embed a per-run `uniq` (which is Date.now()-derived), two use fixed
// strings, and collisions do not matter either way — the sandbox above is
// per-run, so no flag from a previous run is visible to this one. The sandbox
// is the mechanism; the uniqueness was never the load-bearing part.
//
// All three names because node reads TMPDIR on POSIX but TMP/TEMP on Windows,
// where TMPDIR alone would leave this inert.
// Guarded by tests/hardening.rs `js_test_suite_leaves_the_shared_tmp_dir_intact`.
const TMP_SANDBOX = fs.mkdtempSync(path.join(os.tmpdir(), 'code-graph-postinject-tmp-'));
process.env.TMPDIR = TMP_SANDBOX;
process.env.TMP = TMP_SANDBOX;
process.env.TEMP = TMP_SANDBOX;
test.after(() => {
  try { fs.rmSync(TMP_SANDBOX, { recursive: true, force: true }); } catch { /* best effort */ }
});

const { cgTmpDir } = require('./tmp-dir');

// `CLAUDE_CONFIG_DIR` is dropped from THIS process before anything runs.
//
// Every sandbox below redirects HOME and then spawns with `{...process.env}`,
// which passes the variable straight through — and `claudeHome()` is
// `CLAUDE_CONFIG_DIR || homedir/.claude`, so the env var WINS over the
// redirected HOME. For a developer who exports it (the documented multi-profile
// setup) these tests wrote into their LIVE config: measured, a `9.9.9` plugin
// version landed in the real plugins cache. Deleting it here fixes every spawn
// site at once instead of 40 call sites, and `tests/hardening.rs`'s
// `js_test_files_neutralize_claude_config_dir` keeps new files from skipping it.
delete process.env.CLAUDE_CONFIG_DIR;

const {
  findFoldableGrepSegment,
  extractCallgraphSymbols,
  extractGrepOutput,
  grepFoundPattern,
  isSilenced,
  isInjectDisabled,
  buildInjectText,
  commandHash,
} = require('./post-grep-inject');

// ── grep-response gate ──────────────────────────────────────────────
// 2026-07-03 audit: 18/18 injects were 0 CONSUMED — they re-stated hits the model
// already had in its OWN grep output. PostToolUse hands the hook the command's
// actual output (tool_response); skip the inject when the grep already surfaced the
// symbol (redundant), inject only when it found nothing (cg's structural answer is
// then genuinely additive: "it's actually here / who calls it").

test('extractGrepOutput: reads top-level tool_output string (doc-stated shape; forward-compat)', () => {
  assert.equal(extractGrepOutput({ tool_output: 'src/a.rs:1 hit' }), 'src/a.rs:1 hit');
});

test('extractGrepOutput: reads tool_response.stdout (VERIFIED real CC runtime shape — Bash result obj)', () => {
  // CC v2.1.198 binary: hook input = {tool_response:{stdout,stderr,interrupted,...}}.
  // This is the load-bearing path the gate actually fires on in production.
  assert.equal(extractGrepOutput({ tool_response: { stdout: 'src/a.rs:1 hit' } }), 'src/a.rs:1 hit');
});

test('extractGrepOutput: defensive fallback — tool_response as a bare string', () => {
  assert.equal(extractGrepOutput({ tool_response: 'raw output' }), 'raw output');
});

test('extractGrepOutput: defensive fallback — tool_response.output field', () => {
  assert.equal(extractGrepOutput({ tool_response: { output: 'out text' } }), 'out text');
});

test('extractGrepOutput: absent output → null (unknown, caller injects — no regression)', () => {
  assert.equal(extractGrepOutput({}), null);
  assert.equal(extractGrepOutput({ tool_response: {} }), null);
  assert.equal(extractGrepOutput(null), null);
});

test('grepFoundPattern: output line containing the symbol → true (grep hit)', () => {
  assert.equal(grepFoundPattern('src/foo.rs:7  fn EmbeddingModel()', 'EmbeddingModel'), true);
});

test('grepFoundPattern: no line contains the symbol → false (grep found nothing)', () => {
  // e.g. `echo "===" && grep Sym f` where grep matched nothing — only the echo lands.
  assert.equal(grepFoundPattern('===\n', 'EmbeddingModel'), false);
});

test('grepFoundPattern: alternation — ANY alternand present → true', () => {
  assert.equal(grepFoundPattern('src/x.rs:3 created_at', 'markSuperseded|created_at'), true);
});

test('grepFoundPattern: null / empty output or pattern → false', () => {
  assert.equal(grepFoundPattern(null, 'Sym'), false);
  assert.equal(grepFoundPattern('', 'Sym'), false);
  assert.equal(grepFoundPattern('anything', ''), false);
  assert.equal(grepFoundPattern('anything', null), false);
});

test('grepFoundPattern: sibling echo mentions the symbol but grep MISSED → false (no hit-shaped line)', () => {
  // `echo "search for EmbeddingModel" && grep EmbeddingModel wrongpath/` where grep
  // found nothing → stdout is just the echo prose. Must NOT count as a hit, or the
  // additive grep-empty inject is unreachable for this common shape (review MEDIUM).
  assert.equal(grepFoundPattern('search for EmbeddingModel', 'EmbeddingModel'), false);
  assert.equal(grepFoundPattern('=== callers of EmbeddingModel ===', 'EmbeddingModel'), false);
});

test('grepFoundPattern: identifier matched as a WHOLE WORD, not a substring', () => {
  // `date` must not be swallowed by `update`/`validate` on a real hit line (review LOW#2).
  assert.equal(grepFoundPattern('src/x.rs:3  updated the row and validated it', 'TaskState|date'), false);
  // …but a genuine whole-word hit on a hit-shaped line still counts.
  assert.equal(grepFoundPattern('src/x.rs:3  const date = now()', 'TaskState|date'), true);
});

test('grepFoundPattern: bare path line (grep -l output) with the symbol → true', () => {
  assert.equal(grepFoundPattern('src/getVocabulary.rs', 'getVocabulary'), true);
});

test('grepFoundPattern: single-file `grep -n` linenum:content hit (no path prefix) → true', () => {
  // Real shape from a compound `grep -n Sym onefile.mjs` — the hit line is
  // `2:import { parseGitHubUrl }` with NO path token. Must still count as a hit.
  assert.equal(grepFoundPattern('2:import { parseGitHubUrl } from "../x.mjs";', 'parseGitHubUrl'), true);
});

test('grepFoundPattern: long colon-free line does NOT ReDoS (bounded prefix scan)', () => {
  // GREP_HIT_LINE's two `[^\s:]*` stars backtrack O(n²) on a long line carrying `/`.`
  // but no colon (~33s on 400KB pre-fix). The prefix cap must keep it O(1)/line.
  const huge = '/x.'.repeat(200000) + ' EmbeddingModel';  // ~600KB, has /. but no colon
  const t0 = process.hrtime.bigint();
  const r = grepFoundPattern(huge, 'EmbeddingModel');
  const ms = Number(process.hrtime.bigint() - t0) / 1e6;
  assert.ok(ms < 200, `grepFoundPattern took ${ms.toFixed(0)}ms on a 600KB line — ReDoS regressed`);
  // Not a grep-hit-shaped line (no colon in the prefix, too long for a bare path) → false.
  assert.equal(r, false);
});

test('grepFoundPattern: symbol present only in prose (no path token on the line) → false', () => {
  // Defends the hit-line requirement: a plain content line without a path:col prefix
  // (e.g. a `grep` on a single unnamed file, or non-grep sibling output) → inject
  // (safe over-inject) rather than a false-skip.
  assert.equal(grepFoundPattern('the EmbeddingModel struct is here', 'EmbeddingModel'), false);
});

// ── extractCallgraphSymbols ─────────────────────────────────────────
// Widen callgraph eligibility: an alternation / multi-symbol grep pattern
// used to fall to the redundant grep-echo because the WHOLE pattern wasn't a lone
// identifier. Extract the identifier tokens (callgraph self-filters non-symbols).

test('extractCallgraphSymbols: a lone identifier → [itself] (prior behavior)', () => {
  assert.deepEqual(extractCallgraphSymbols('markSuperseded'), ['markSuperseded']);
});

test('extractCallgraphSymbols: a lone SHORT identifier is preserved (no length filter on the fast path)', () => {
  // The <3-char length filter applies ONLY to multi-token extraction; a grep for
  // a lone 2-char symbol must still get its callgraph, exactly as before.
  assert.deepEqual(extractCallgraphSymbols('ok'), ['ok']);
});

test('extractCallgraphSymbols: alternation → each identifier in order', () => {
  assert.deepEqual(
    extractCallgraphSymbols('markSuperseded|created_at'),
    ['markSuperseded', 'created_at']);
});

test('extractCallgraphSymbols: strips regex escapes so `\\bdate` yields `date`, not `bdate`', () => {
  // The letter after \b/\d/\w is a regex metachar, not part of the symbol.
  assert.deepEqual(
    extractCallgraphSymbols('markSuperseded|\\bdate:|created_at'),
    ['markSuperseded', 'date', 'created_at']);
});

test('extractCallgraphSymbols: drops <3-char noise tokens in multi mode', () => {
  // `a|bb|ccc` → only `ccc` survives (a=1, bb=2 filtered).
  assert.deepEqual(extractCallgraphSymbols('a|bb|ccc'), ['ccc']);
});

test('extractCallgraphSymbols: dedups repeated tokens, order-preserving', () => {
  assert.deepEqual(extractCallgraphSymbols('foo|bar|foo'), ['foo', 'bar']);
});

test('extractCallgraphSymbols: caps attempts at 3', () => {
  assert.deepEqual(
    extractCallgraphSymbols('aaa|bbb|ccc|ddd|eee'),
    ['aaa', 'bbb', 'ccc']);
});

test('extractCallgraphSymbols: non-string / empty → []', () => {
  assert.deepEqual(extractCallgraphSymbols(null), []);
  assert.deepEqual(extractCallgraphSymbols(''), []);
  assert.deepEqual(extractCallgraphSymbols(undefined), []);
});

test('extractCallgraphSymbols: pattern with no identifier token → []', () => {
  assert.deepEqual(extractCallgraphSymbols('\\d+\\.\\d+'), []);
});

// ── Pure logic: findFoldableGrepSegment ─────────────────────────────
// Reuses splitTopLevelSegments + classifyBlock from pre-grep-guide. The FIRST
// segment whose head is grep AND whose classifyBlock is non-null is the foldable
// grep to answer. Leading-grep foldable commands were DENIED in PreToolUse and
// never ran → never reach PostToolUse, so no dedup is needed here.

test('findFoldableGrepSegment: compound `echo && grep "Sym" tests/` → the grep segment', () => {
  // classifyBlock requires a QUOTED, identifier-like pattern (the deny gate's
  // contract); `EmbeddingModel` stands for the spec's illustrative `Sym`.
  const seg = findFoldableGrepSegment('echo "x" && grep "EmbeddingModel" tests/');
  assert.ok(seg, 'expected a foldable grep segment');
  assert.equal(seg.segment, 'grep "EmbeddingModel" tests/');
  assert.equal(seg.block.mode, 'grep');
});

test('findFoldableGrepSegment: `git diff && grep "Sym" src/` → the grep segment', () => {
  const seg = findFoldableGrepSegment('git diff && grep "EmbeddingModel" src/');
  assert.ok(seg);
  assert.equal(seg.segment, 'grep "EmbeddingModel" src/');
});

test('findFoldableGrepSegment: `cargo test | grep FAIL` is an output filter → null', () => {
  // single pipe is NOT a split → head stays `cargo`, not a foldable grep.
  assert.equal(findFoldableGrepSegment('cargo test | grep FAIL'), null);
});

test('findFoldableGrepSegment: a leading non-compound grep is NOT folded here (PreToolUse denies it)', () => {
  // A bare leading foldable grep is handled by PreToolUse deny; if it somehow
  // reaches PostToolUse it still classifies, but the typical compound case is the
  // target. We DO answer a lone classifyBlock-positive segment when present.
  const seg = findFoldableGrepSegment('grep "EmbeddingModel" src/');
  assert.ok(seg, 'a classifyBlock-positive grep segment is foldable');
  assert.equal(seg.block.mode, 'grep');
});

test('findFoldableGrepSegment: non-foldable hint-tier grep (marker) → null', () => {
  // bare TODO marker passes shouldHint but classifyBlock is null → not foldable.
  assert.equal(findFoldableGrepSegment('echo hi && grep "TODO" src/'), null);
});

test('findFoldableGrepSegment: no grep anywhere → null', () => {
  assert.equal(findFoldableGrepSegment('cargo build && cargo test'), null);
});

test('findFoldableGrepSegment: for-loop body grep is isolated and folded', () => {
  const seg = findFoldableGrepSegment('for s in a b; do grep "EmbeddingModel" src/; done');
  assert.ok(seg, 'loop-body grep must be foldable');
  assert.match(seg.segment, /grep "EmbeddingModel" src\//);
});

test('findFoldableGrepSegment: empty / non-string → null', () => {
  assert.equal(findFoldableGrepSegment(''), null);
  assert.equal(findFoldableGrepSegment(null), null);
});

test('findFoldableGrepSegment: show-mode (decl anchor + context flag) classifies as show', () => {
  const seg = findFoldableGrepSegment('echo go && grep "fn handle_message" -A 5 src/');
  assert.ok(seg);
  assert.equal(seg.block.mode, 'show');
  assert.deepEqual(seg.block.symbols, ['handle_message']);
});

// ── buildInjectText ─────────────────────────────────────────────────

test('buildInjectText: carries a header + the answer text', () => {
  const out = buildInjectText({ text: 'src/foo.rs:7  fn x()', truncated: false }, 'grep');
  assert.match(out, /AST-aware view of your grep/);
  assert.match(out, /src\/foo\.rs:7/);
});

test('buildInjectText: truncation note appended when truncated', () => {
  const out = buildInjectText({ text: 'hit', truncated: true }, 'grep');
  assert.match(out, /truncated/);
});

test('buildInjectText: no truncation note when not truncated', () => {
  const out = buildInjectText({ text: 'hit', truncated: false }, 'grep');
  assert.doesNotMatch(out, /truncated/);
});

test('buildInjectText: callgraph mode uses the cross-file header (not the grep-echo header)', () => {
  const out = buildInjectText({ text: '  ← called by: alpha (src/b.rs)', truncated: false }, 'callgraph');
  assert.match(out, /Cross-file call graph/);
  assert.match(out, /grep can't show this/);
  assert.doesNotMatch(out, /AST-aware view of your grep/);
  assert.match(out, /← called by` = callers/);
});

test('buildInjectText: callgraph truncation note points at the callgraph command', () => {
  const out = buildInjectText({ text: 'tree', truncated: true }, 'callgraph');
  assert.match(out, /code-graph-mcp callgraph <symbol>/);
});

// ── opt-out / kill switch ───────────────────────────────────────────

test('isSilenced: CODE_GRAPH_QUIET_HOOKS=1 → silenced; default not', () => {
  assert.equal(isSilenced({ CODE_GRAPH_QUIET_HOOKS: '1' }), true);
  assert.equal(isSilenced({}), false);
});

test('isInjectDisabled: CODE_GRAPH_NO_INJECT=1 → disabled; default not', () => {
  assert.equal(isInjectDisabled({ CODE_GRAPH_NO_INJECT: '1' }), true);
  assert.equal(isInjectDisabled({ CODE_GRAPH_NO_INJECT: '0' }), false);
  assert.equal(isInjectDisabled({}), false);
});

// ── e2e: real spawn with stub binary (mirrors pre-grep-guide harness) ──
// PostToolUse-shaped stdin {tool_input:{command:"..."}}; assert on
// hookSpecificOutput.additionalContext.

function e2eFixture(stubBody) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'post-grep-e2e-'));
  fs.mkdirSync(path.join(dir, '.code-graph'), { recursive: true });
  fs.writeFileSync(path.join(dir, '.code-graph', 'index.db'), '');
  const stub = path.join(dir, 'cg-stub.js');
  fs.writeFileSync(stub, '#!/usr/bin/env node\n' + stubBody);
  fs.chmodSync(stub, 0o755);
  return { dir, stub };
}

function runHook(cmd, fixture, extraEnv = {}, cwdOverride, toolOutput) {
  const payload = { tool_input: { command: cmd } };
  // Drive the REAL CC runtime shape (verified against the v2.1.198 binary): the Bash
  // result reaches the hook as `tool_response.stdout`. Absent → unknown → the gate
  // injects (pre-gate behavior; no regression).
  if (toolOutput !== undefined) payload.tool_response = { stdout: toolOutput };
  return spawnSync(process.execPath, [path.join(__dirname, 'post-grep-inject.js')], {
    cwd: cwdOverride || fixture.dir,
    input: JSON.stringify(payload),
    encoding: 'utf8',
    env: {
      ...process.env,
      _CG_ANSWER_BINARY: fixture.stub,
      CODE_GRAPH_QUIET_HOOKS: '0',
      CODE_GRAPH_NO_INJECT: '0',
      ...extraEnv,
    },
  });
}

// ── Pre-ship review of the v0.144 grep-guard change ─────────────────────────
// PreToolUse stopped denying compound commands, which routes a measured 27% of
// former denies (23 of 85 in this repo's own log carried `tail:true`) onto this
// hook. Two consequences were found before release: the redundancy skip made
// that whole class invisible to the funnel, and the flag/glob fidelity fix had
// been applied only to the deny path — so the newly-routed traffic ran the
// UNFIXED answer.

// Round 4: repair #2 of round 3 — filing the TRANSLATED pattern so the deny and
// the skip agree — was pinned by nothing; reverting it to `rawPattern` left the
// whole suite green. The funnel compares those strings for equality, so a BRE
// pattern is the witness.
test('e2e: the skip files the same pattern spelling the deny would have', (t) => {
  const uniq = `InjAlign${Date.now()}`;
  const fixture = e2eFixture(`process.stdout.write('cg out\\n');`);
  const cmd = `grep -rn "${uniq}\\|other_symbol" src/ && echo done`;
  try {
    runHook(cmd, fixture, {}, undefined, `src/foo.rs:12  ${uniq}`);
    const rec = JSON.parse(fs.readFileSync(
      path.join(fixture.dir, '.code-graph', 'recommendations.jsonl'), 'utf8').trim().split('\n').pop());
    assert.equal(rec.pattern, `${uniq}|other_symbol`,
      'the BRE alternation must be filed unescaped, as every sibling record files it');
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: the redundancy skip is RECORDED, not silent', (t) => {
  const uniq = `InjSkip${Date.now()}`;
  const fixture = e2eFixture(`process.stdout.write('cg out\\n');`);
  const cmd = `grep -rn "${uniq}" src/`;
  try {
    // The model's own grep already surfaced the symbol → the inject is redundant.
    const res = runHook(cmd, fixture, {}, undefined, `src/foo.rs:12  fn ${uniq}()`);
    assert.equal(res.status, 0);
    assert.equal(res.stdout.trim(), '', 'a redundant inject must still not be emitted');
    const rec = JSON.parse(fs.readFileSync(
      path.join(fixture.dir, '.code-graph', 'recommendations.jsonl'), 'utf8').trim().split('\n').pop());
    assert.equal(rec.reason, 'grep-hit-redundant');
    assert.equal(rec.pattern, uniq);
    // The load-bearing half, and what round 2 caught round 1 getting wrong. The
    // Rust aggregator (src/cli/usage.rs) clears the funnel's `armed` flag on
    // EVERY line but only scores hooks it knows, so an invented hook name eats
    // the arm set by the preceding answered deny without scoring it — the first
    // version of this record used `hook:'grep-inject'`/`action:'skip'` and took
    // `fallthrough_rate` from 1.0 to 0.0 on a crafted log. `action` must also be
    // one the aggregator excludes from the recommendation total, or every skip
    // inflates `tool_calls_per_rec` across releases.
    assert.equal(rec.hook, 'grep', 'must be a hook the aggregator scores');
    assert.equal(rec.action, 'observe', 'must be an action the aggregator excludes from total');
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: the inject honours the grep\'s flags and glob, like the deny path', (t) => {
  const uniq = `InjFlag${Date.now()}`;
  const fixture = e2eFixture(
    `process.stdout.write('ARGV[' + process.argv.slice(2).join(' ') + ']\\nsrc/foo.rs\\n');`);
  // A miss, so the inject is additive and actually fires.
  const cmd = `grep -rln "${uniq}" tests/ && echo done`;
  // The shell expands `tests/*.mjs` one level deep; `-g '*.mjs'` matches
  // every depth, so a glob path is no longer answered at all (D#133).
  const glob = `grep -rln "${uniq}G" tests/*.mjs && echo done`;
  try {
    const res = runHook(cmd, fixture, {}, undefined, '');
    const ctx = JSON.parse(res.stdout).hookSpecificOutput.additionalContext;
    assert.match(ctx, new RegExp(`ARGV\\[grep -l ${uniq} tests/\\]`),
      `the inject dropped -l while the deny path kept it: ${ctx}`);
    assert.equal(runHook(glob, fixture, {}, undefined, '').stdout.trim(), '', 'a glob path is not answered');
  } finally {
    cleanupFixture(fixture, cmd);
    cleanupFixture(fixture, glob);
  }
});

test('e2e: the inject\'s show-mode fallback to grep also carries the flags', (t) => {
  const uniq = `InjShowFb${Date.now()}`;
  const fixture = e2eFixture(
    `if (process.argv[2] !== 'grep') process.exit(1);\n` +
    `process.stdout.write('ARGV[' + process.argv.slice(2).join(' ') + ']\\nsrc/foo.rs\\n');`);
  const cmd = `echo x && grep -iA3 "fn ${uniq}" src/`;
  try {
    const ctx = JSON.parse(runHook(cmd, fixture, {}, undefined, '').stdout)
      .hookSpecificOutput.additionalContext;
    assert.match(ctx, /ARGV\[grep -i /, `the fallback dropped -i: ${ctx}`);
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: the inject\'s -F guard is not fooled by `-F` in a VALUE position', (t) => {
  const uniq = `InjFVal${Date.now()}`;
  const fixture = e2eFixture(
    `if (process.argv[2] !== 'grep') process.exit(1);\n` +
    `process.stdout.write('ARGV[' + process.argv.slice(2).join(' ') + ']\\nsrc/foo.rs\\n');`);
  const cmd = `echo x && grep -rn --include -F "${uniq}\\|other_symbol" src/`;
  fs.mkdirSync(path.join(fixture.dir, 'src'));
  try {
    const ctx = JSON.parse(runHook(cmd, fixture, {}, undefined, '').stdout)
      .hookSpecificOutput.additionalContext;
    assert.match(ctx, new RegExp(`ARGV\\[grep -g -F ${uniq}\\|other_symbol src/\\]`),
      `-F here is --include's value; the BRE unescape must still run: ${ctx}`);
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: the inject honours -F — flag forwarded, pattern left literal', (t) => {
  const uniq = `InjLit${Date.now()}`;
  const fixture = e2eFixture(
    `if (process.argv[2] !== 'grep') process.exit(1);\n` +
    `process.stdout.write('ARGV[' + process.argv.slice(2).join(' ') + ']\\nsrc/foo.rs\\n');`);
  const cmd = `echo x && grep -rF "${uniq}\\|other_symbol" src/`;
  try {
    const ctx = JSON.parse(runHook(cmd, fixture, {}, undefined, '').stdout)
      .hookSpecificOutput.additionalContext;
    assert.match(ctx, new RegExp(`ARGV\\[grep -F ${uniq}\\\\\\|other_symbol src/\\]`),
      `the inject unescaped a literal pattern or dropped -F: ${ctx}`);
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

// The command-hash tail of an inject cooldown flag. The full name is
// `.code-graph-postinject-<cwdHash>-<commandHash>` (post-grep-inject.js
// `flagPath`), and matching the TAIL alone is what makes this independent of the
// cwd: the hook keys the flag on the RESOLVED project root, which need not equal
// the fixture path byte for byte.
function cooldownFlagTail(cmd) {
  return `-${commandHash(cmd)}`;
}

function cleanupFixture(fixture, cmd) {
  fs.rmSync(fixture.dir, { recursive: true, force: true });
  // This deleted NOTHING from the day the flag became project-scoped: it spelled
  // `.code-graph-postinject-<commandHash>` while production writes
  // `.code-graph-postinject-<cwdHash>-<commandHash>`, and the try/catch swallowed
  // the ENOENT, so nothing ever reported the miss. pre-grep-guide.test.js hit the
  // identical bug and fixed it; this sibling was left on the old spelling — the
  // sweep-every-sibling rule exists for exactly this. The guard test below keeps
  // it honest by asserting the flag EXISTS before cleanup runs.
  const tail = cooldownFlagTail(cmd);
  let entries;
  try { entries = fs.readdirSync(cgTmpDir()); } catch { return; }
  for (const name of entries) {
    if (!name.startsWith('.code-graph-postinject-') || !name.endsWith(tail)) continue;
    try { fs.unlinkSync(path.join(cgTmpDir(), name)); } catch { /* raced */ }
  }
}

test('e2e: cleanupFixture actually removes the inject cooldown flag the hook wrote', () => {
  // A negative control for the test harness itself, mirroring pre-grep-guide's.
  // A cleanup miss is indistinguishable from "already gone" through unlink +
  // catch, so the only way to know the helper works is to assert the flag is
  // there first — otherwise this passes vacuously against a hook that wrote
  // nothing at all.
  const uniq = `PostClean${Date.now()}`;
  const fixture = e2eFixture(`process.stdout.write('tests/foo.rs:7  hit\\n');`);
  const cmd = `echo "x" && grep "${uniq}" tests/`;
  const tail = cooldownFlagTail(cmd);
  const flags = () => fs.readdirSync(cgTmpDir())
    .filter((f) => f.startsWith('.code-graph-postinject-') && f.endsWith(tail));
  try {
    runHook(cmd, fixture);
    assert.deepEqual(flags().length, 1, 'the hook marks exactly one inject flag for this command');
  } finally {
    cleanupFixture(fixture, cmd);
  }
  assert.deepEqual(flags(), [], 'cleanupFixture leaves no inject flag behind');
});

test('e2e: `echo "x" && grep Sym tests/` → injects additionalContext with the stub hits + records inject', () => {
  const uniq = `PostHit${Date.now()}`;
  const fixture = e2eFixture(
    `process.stdout.write('tests/foo.rs:7  fn ' + process.argv[3] + '()\\n');`);
  const cmd = `echo "x" && grep "${uniq}" tests/`;
  try {
    const res = runHook(cmd, fixture);
    assert.equal(res.status, 0);
    const out = JSON.parse(res.stdout);
    assert.equal(out.hookSpecificOutput.hookEventName, 'PostToolUse');
    assert.equal(out.hookSpecificOutput.permissionDecision, undefined,
      'PostToolUse inject must be permission-neutral (no permissionDecision)');
    assert.match(out.hookSpecificOutput.additionalContext, new RegExp(uniq));
    assert.match(out.hookSpecificOutput.additionalContext, /tests\/foo\.rs:7/);
    const recs = fs.readFileSync(
      path.join(fixture.dir, '.code-graph', 'recommendations.jsonl'), 'utf8');
    const rec = JSON.parse(recs.trim().split('\n').pop());
    assert.equal(rec.action, 'inject');
    assert.equal(rec.answered, true);
    assert.equal(rec.hook, 'grep');
    assert.equal(rec.pattern, uniq);
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: `git diff && grep Sym src/` → inject', () => {
  const uniq = `GitDiffHit${Date.now()}`;
  const fixture = e2eFixture(
    `process.stdout.write('src/foo.rs:9  fn ' + process.argv[3] + '()\\n');`);
  const cmd = `git diff && grep "${uniq}" src/`;
  try {
    const res = runHook(cmd, fixture);
    assert.equal(res.status, 0);
    const out = JSON.parse(res.stdout);
    assert.match(out.hookSpecificOutput.additionalContext, new RegExp(uniq));
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: `cargo test | grep FAIL` → no inject (output filter)', () => {
  const fixture = e2eFixture(`process.stdout.write('should not run\\n');`);
  const cmd = `cargo test | grep FAIL`;
  try {
    const res = runHook(cmd, fixture);
    assert.equal(res.status, 0);
    assert.equal(res.stdout.trim(), '', 'an output-filter pipe must not inject');
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: stub reports no hits → silent (no inject) but RECORDS the skip', () => {
  const uniq = `PostMiss${Date.now()}`;
  const fixture = e2eFixture(
    `process.stdout.write('[code-graph] No matches\\n');`);
  const cmd = `echo go && grep "${uniq}" src/`;
  try {
    const res = runHook(cmd, fixture);
    assert.equal(res.status, 0);
    assert.equal(res.stdout.trim(), '', 'no-hits must inject nothing');
    // Dark-vs-empty disclosure (roadmap 2026-07-18 §1.6): the skip must land in
    // recommendations.jsonl so the funnel can tell "ran, nothing to say" from
    // "hook never fired". answered:false keeps it out of funnel arming.
    const recs = fs.readFileSync(
      path.join(fixture.dir, '.code-graph', 'recommendations.jsonl'), 'utf8');
    const rec = JSON.parse(recs.trim().split('\n').pop());
    assert.equal(rec.action, 'inject');
    assert.equal(rec.answered, false);
    assert.equal(rec.hook, 'grep');
    assert.equal(rec.fallthrough, 'no-hits');
    assert.equal(rec.reason, 'no-hits');
    assert.equal(rec.pattern, uniq);
    // D#147: the skip must name the mode that burned the attempt. Without it
    // every skip aggregates under one unattributable bucket and the funnel can
    // see THAT injects fail but not WHICH mode is failing. Here callgraph found
    // no edges and the run fell through to the grep echo, which also missed.
    assert.equal(rec.mode, 'grep', 'skip rec must record the mode that was tried');
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: cg run fails → `unavailable` skip records mode too', () => {
  // The second skip shape: a runtime failure, not "ran and found nothing".
  // (`no-binary` is a third, reached only when findBinary() cannot LOCATE a
  // binary — an unset _CG_ANSWER_BINARY, which a dev box with cg installed
  // cannot force; pointing at a nonexistent path spawns and fails → unavailable.)
  // It must be attributable exactly like the no-hits skip: unattributable rows
  // are what made "13 of 74" unreadable in the 2026-08-19 telemetry.
  const uniq = `PostUnavail${Date.now()}`;
  const fixture = e2eFixture(`process.stdout.write('unused\\n');`);
  const cmd = `echo go && grep "${uniq}" src/`;
  try {
    const res = runHook(cmd, fixture, {
      _CG_ANSWER_BINARY: path.join(fixture.dir, 'does-not-exist-cg'),
    });
    assert.equal(res.status, 0);
    assert.equal(res.stdout.trim(), '', 'an unrunnable binary must inject nothing');
    const recs = fs.readFileSync(
      path.join(fixture.dir, '.code-graph', 'recommendations.jsonl'), 'utf8');
    const rec = JSON.parse(recs.trim().split('\n').pop());
    assert.equal(rec.answered, false);
    assert.equal(rec.reason, 'unavailable');
    assert.equal(rec.mode, 'grep', 'a failed run is still charged to the mode it tried');
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: CODE_GRAPH_NO_INJECT=1 silences the hook', () => {
  const uniq = `PostOptout${Date.now()}`;
  const fixture = e2eFixture(`process.stdout.write('src/foo.rs:7  hit\\n');`);
  const cmd = `echo go && grep "${uniq}" src/`;
  try {
    const res = runHook(cmd, fixture, { CODE_GRAPH_NO_INJECT: '1' });
    assert.equal(res.status, 0);
    assert.equal(res.stdout.trim(), '', 'opt-out must silence the inject');
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: per-command cooldown — verbatim re-run within window injects only once', () => {
  const uniq = `PostCool${Date.now()}`;
  const fixture = e2eFixture(`process.stdout.write('src/foo.rs:7  hit\\n');`);
  const cmd = `echo go && grep "${uniq}" src/`;
  try {
    const r1 = runHook(cmd, fixture);
    assert.notEqual(r1.stdout.trim(), '', 'first run injects');
    const r2 = runHook(cmd, fixture);
    assert.equal(r2.stdout.trim(), '', 'second run within cooldown is silent');
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: alternation grep `Alpha|Beta` → callgraph mode when a symbol has edges', () => {
  // The whole pattern is not a lone identifier, but the FIRST alternand resolves
  // to a symbol with cross-file edges → callgraph payload, not the grep echo.
  const uniq = `AltCg${Date.now()}`;
  const fixture = e2eFixture(
    // stub: argv = [node, stub, subcmd, sym/pattern, ...]. callgraph → edge-bearing
    // tree; anything else (grep) → a plain hit line.
    `const sub = process.argv[2], arg = process.argv[3];\n` +
    `if (sub === 'callgraph') { process.stdout.write(arg + '\\n  \\u2190 called by: someCaller (src/x.rs:3)\\n'); process.exit(0); }\n` +
    `process.stdout.write('src/foo.rs:7  fn ' + arg + '()\\n');`);
  const cmd = `echo "x" && grep "${uniq}|OtherSym" src/`;
  try {
    const res = runHook(cmd, fixture);
    assert.equal(res.status, 0);
    const out = JSON.parse(res.stdout);
    assert.match(out.hookSpecificOutput.additionalContext, /Cross-file call graph/,
      'a resolving alternand must produce the callgraph payload, not the grep echo');
    assert.match(out.hookSpecificOutput.additionalContext, /called by: someCaller/);
    const recs = fs.readFileSync(
      path.join(fixture.dir, '.code-graph', 'recommendations.jsonl'), 'utf8');
    const rec = JSON.parse(recs.trim().split('\n').pop());
    assert.equal(rec.action, 'inject');
    assert.equal(rec.mode, 'callgraph', 'inject rec must record mode:callgraph');
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('F1: a grep into an indexed package dir is injected once the index lists it as a source root', () => {
  const uniq = `PkgRoot${Date.now()}`;
  const fixture = e2eFixture(
    `const sub = process.argv[2], arg = process.argv[3];\n` +
    `if (sub === 'callgraph') { process.stdout.write(arg + '\\n  \\u2190 called by: someCaller (networkx/x.py:3)\\n'); process.exit(0); }\n` +
    `process.stdout.write('networkx/foo.py:7  def ' + arg + '()\\n');`);
  const cmd = `echo "x" && grep "${uniq}" networkx/`;
  try {
    assert.equal(runHook(cmd, fixture).stdout, '', 'no source-roots.json: not a source path, as before');
    fs.writeFileSync(path.join(fixture.dir, '.code-graph', 'source-roots.json'),
      JSON.stringify({ version: 1, roots: ['networkx'] }));
    const out = JSON.parse(runHook(cmd, fixture).stdout);
    assert.match(out.hookSpecificOutput.additionalContext, /called by: someCaller/);
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('F1b: grep -r into a package dir holding an ignored __pycache__ still gets the grep echo', () => {
  const uniq = `PycEcho${Date.now()}`;
  const fixture = e2eFixture(
    `const sub = process.argv[2];\n` +
    `if (sub === 'callgraph') { process.exit(1); }\n` +
    `process.stdout.write('networkx/graph.py:1  ${uniq} = 1\\n');`);
  const git = (...a) => spawnSync('git', ['-c', 'user.email=t@t', '-c', 'user.name=t', ...a],
    { cwd: fixture.dir, encoding: 'utf8' });
  const cmd = `echo "x" && grep -rn "${uniq}" networkx/`;
  try {
    fs.mkdirSync(path.join(fixture.dir, 'networkx', '__pycache__'), { recursive: true });
    fs.writeFileSync(path.join(fixture.dir, 'networkx', 'graph.py'), `${uniq} = 1\n`);
    fs.writeFileSync(path.join(fixture.dir, 'networkx', '__pycache__', 'graph.cpython-312.pyc'),
      Buffer.from([0x6f, 0x0d, 0x0d, 0x0a, 0, 0, 0xff]));
    fs.writeFileSync(path.join(fixture.dir, '.gitignore'), '.code-graph/\n__pycache__/\ncg-stub.js\n');
    fs.writeFileSync(path.join(fixture.dir, '.code-graph', 'source-roots.json'),
      JSON.stringify({ version: 1, roots: ['networkx'] }));
    for (const a of [['init', '-q', '.'], ['add', '-A'], ['commit', '-qm', 'init']]) assert.equal(git(...a).status, 0);
    const out = JSON.parse(runHook(cmd, fixture).stdout);
    assert.match(out.hookSpecificOutput.additionalContext, /AST-aware view of your grep/);
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('D5: while the startup index is being written, no call graph — the grep echo still answers', () => {
  // Call edges from a partial index are a subset presented as the whole graph;
  // the grep echo comes from the files themselves and stays correct.
  const uniq = `AltBuild${Date.now()}`;
  const fixture = e2eFixture(
    `const sub = process.argv[2], arg = process.argv[3];\n` +
    `if (sub === 'callgraph') { process.stdout.write(arg + '\\n  \\u2190 called by: someCaller (src/x.rs:3)\\n'); process.exit(0); }\n` +
    `process.stdout.write('src/foo.rs:7  fn ' + arg + '()\\n');`);
  fs.writeFileSync(path.join(fixture.dir, '.code-graph', 'indexing-status.json'),
    JSON.stringify({ s: 'finalizing', d: 50, t: 50 }));
  const cmd = `echo "x" && grep "${uniq}|OtherSym" src/`;
  try {
    const out = JSON.parse(runHook(cmd, fixture).stdout);
    assert.doesNotMatch(out.hookSpecificOutput.additionalContext, /Cross-file call graph|called by/);
    assert.match(out.hookSpecificOutput.additionalContext, /AST-aware view of your grep/);
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: alternation grep, no symbol has edges → falls back to grep echo (grep mode)', () => {
  // callgraph returns exit 1 (no node) for every alternand → the grep-echo path
  // still delivers, mode:grep. Guards that widening never LOSES the echo fallback.
  const uniq = `AltEcho${Date.now()}`;
  const fixture = e2eFixture(
    `const sub = process.argv[2], arg = process.argv[3];\n` +
    `if (sub === 'callgraph') { process.exit(1); }\n` +
    `process.stdout.write('src/foo.rs:7  fn matched()\\n');`);
  const cmd = `echo "x" && grep "${uniq}|OtherSym" src/`;
  try {
    const res = runHook(cmd, fixture);
    assert.equal(res.status, 0);
    const out = JSON.parse(res.stdout);
    assert.match(out.hookSpecificOutput.additionalContext, /AST-aware view of your grep/);
    const recs = fs.readFileSync(
      path.join(fixture.dir, '.code-graph', 'recommendations.jsonl'), 'utf8');
    const rec = JSON.parse(recs.trim().split('\n').pop());
    assert.equal(rec.mode, 'grep');
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: grep-response gate — grep ALREADY showed the symbol → skip inject (redundant)', () => {
  // The model's own grep output contains the symbol → inject would re-state hits it
  // already has (the 18/18-CONSUMED=0 case). Even though the stub WOULD answer, the
  // gate suppresses the redundant inject.
  const uniq = `GateHit${Date.now()}`;
  const fixture = e2eFixture(`process.stdout.write('src/foo.rs:7  fn ' + process.argv[3] + '()\\n');`);
  const cmd = `echo "x" && grep "${uniq}" src/`;
  const grepOutput = `src/real.rs:42  fn ${uniq}() {  // the model's own grep already found it`;
  try {
    const res = runHook(cmd, fixture, {}, undefined, grepOutput);
    assert.equal(res.status, 0);
    assert.equal(res.stdout.trim(), '', 'a grep that already surfaced the symbol must NOT trigger a redundant inject');
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: grep-response gate — grep found NOTHING → inject (cg answer is additive)', () => {
  // The grep produced no hit for the symbol (dialect/scope miss) → cg's structural
  // answer is genuinely new info → inject fires.
  const uniq = `GateMiss${Date.now()}`;
  const fixture = e2eFixture(`process.stdout.write('src/foo.rs:7  fn ' + process.argv[3] + '()\\n');`);
  const cmd = `echo "===" && grep "${uniq}" src/`;
  const grepOutput = `===\n`; // only the echo landed; grep matched nothing
  try {
    const res = runHook(cmd, fixture, {}, undefined, grepOutput);
    assert.equal(res.status, 0);
    const out = JSON.parse(res.stdout);
    assert.match(out.hookSpecificOutput.additionalContext, new RegExp(uniq),
      'a grep that found nothing must still get the additive cg answer');
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: grep-response gate — absent output field → inject (no regression on unknown)', () => {
  // No tool_response (older CC, or unreadable) → the gate can't confirm redundancy →
  // it injects, exactly as before the gate existed.
  const uniq = `GateUnknown${Date.now()}`;
  const fixture = e2eFixture(`process.stdout.write('src/foo.rs:7  fn ' + process.argv[3] + '()\\n');`);
  const cmd = `echo "x" && grep "${uniq}" src/`;
  try {
    const res = runHook(cmd, fixture); // no toolOutput arg
    assert.equal(res.status, 0);
    const out = JSON.parse(res.stdout);
    assert.match(out.hookSpecificOutput.additionalContext, new RegExp(uniq));
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: no index up to $HOME → silent exit 0', () => {
  // A cwd with no .code-graph anywhere up the tree resolves to null root → exit.
  const bare = fs.mkdtempSync(path.join(os.tmpdir(), 'post-grep-noidx-'));
  const stub = path.join(bare, 'cg-stub.js');
  fs.writeFileSync(stub, '#!/usr/bin/env node\nprocess.stdout.write("hit\\n");');
  fs.chmodSync(stub, 0o755);
  const cmd = `echo go && grep "FooBar" src/`;
  try {
    const res = spawnSync(process.execPath, [path.join(__dirname, 'post-grep-inject.js')], {
      cwd: bare,
      input: JSON.stringify({ tool_input: { command: cmd } }),
      encoding: 'utf8',
      env: { ...process.env, _CG_ANSWER_BINARY: stub, HOME: bare, USERPROFILE: bare, CODE_GRAPH_QUIET_HOOKS: '0' },
    });
    assert.equal(res.status, 0);
    assert.equal(res.stdout.trim(), '');
  } finally {
    fs.rmSync(bare, { recursive: true, force: true });
  }
});

test('the two grep hooks keep separate cooldown flags after sharing one implementation', () => {
  // ARC-06 (audit 2026-08-29): the cooldown quartet lived twice, near
  // byte-identically, in pre-grep-guide.js and post-grep-inject.js. It is one
  // implementation now (`makeCooldown` in tmp-dir.js), and the ONE thing that
  // must not be shared is the flag namespace: a PreToolUse deny and a
  // PostToolUse inject for different commands must not suppress each other.
  // A factory makes that a one-character mistake, so it gets a test rather than
  // a comment.
  const pre = require('./pre-grep-guide');
  const post = require('./post-grep-inject');
  const cmd = `grep -rn "ArcSix${Date.now()}" src/`;
  const cwd = '/repo/arc06';

  post.markCooldown(cmd, cwd);
  assert.equal(post.isOnCooldown(cmd, Date.now(), 60000, cwd), true,
    'the hook that wrote the flag must see it — otherwise the rest is vacuous');
  assert.equal(pre.isOnCooldown(cmd, Date.now(), 60000, cwd), false,
    'a PostToolUse inject must not put the PreToolUse guard on cooldown');

  pre.markCooldown(cmd, cwd);
  assert.equal(pre.isOnCooldown(cmd, Date.now(), 60000, cwd), true);

  // Both flags exist side by side, distinguished only by prefix.
  const flags = fs.readdirSync(cgTmpDir())
    .filter(f => f.includes(post.commandHash(cmd)));
  assert.equal(flags.length, 2, `expected one flag per hook, got ${flags.join(', ')}`);
  assert.equal(flags.filter(f => f.startsWith('.code-graph-bash-')).length, 1);
  assert.equal(flags.filter(f => f.startsWith('.code-graph-postinject-')).length, 1);

  // Both hooks derive the same command digest — they always did, and the shared
  // helper is what keeps that true rather than a coincidence of two copies.
  assert.equal(pre.commandHash(cmd), post.commandHash(cmd));
});

test('e2e: a spent hook budget records fallthrough_reason:budget, not a bare unavailable', () => {
  // The skip shape neither of the two above covers: the hook was HEALTHY and
  // ran out of time. This one records as `unavailable` — the same word a wedged
  // or broken binary produces — so without a cause the funnel cannot see
  // NEW-08's failure mode (a fine hook that deterministically answers nothing
  // once uptime eats the reserve) at all.
  //
  // Costs ~4.6 s by construction and cannot be made cheaper: exhausting a 5 s
  // budget takes 5 s of real time. Driven the way the budget is really spent
  // (JS-03's serial runs) rather than by poking the deadline — a stub that never
  // returns makes the first runner burn the budget, and every later one in the
  // callgraph → show → grep chain then finds it gone.
  //
  // The per-call timeout is deliberately ABOVE the 5 s hook budget. At 2500 ms
  // this test failed for the wrong reason: two calls consumed 2500 + ~2050 ms,
  // the second was killed by its own timeout at the exact instant the budget
  // ran out, and no third call ever started — so the run reported `unavailable`
  // with no cause and the assertion below would have been measuring a real
  // timeout, not an exhausted budget. Clamping the FIRST call to whatever is
  // left makes the SECOND deterministically hit `remainingMs() === null`.
  const uniq = `PostBudget${Date.now()}`;
  const fixture = e2eFixture(`setTimeout(() => {}, 60000);`);
  const cmd = `echo go && grep "${uniq}" src/`;
  try {
    const res = runHook(cmd, fixture, { _CG_ANSWER_TIMEOUT_MS: '9000' });
    assert.equal(res.status, 0, 'a starved hook still exits 0 — it degrades, it does not fail');
    assert.equal(res.stdout.trim(), '', 'nothing was answered, so nothing may be injected');
    const recs = fs.readFileSync(
      path.join(fixture.dir, '.code-graph', 'recommendations.jsonl'), 'utf8');
    const rec = JSON.parse(recs.trim().split('\n').pop());
    assert.equal(rec.action, 'inject');
    assert.equal(rec.answered, false);
    assert.equal(rec.reason, 'unavailable',
      'the enum src/cli/usage.rs:448 scores must keep its value');
    assert.equal(rec.fallthrough_reason, 'budget',
      'a starved hook must be distinguishable from a broken binary');
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

// ── D#73: bare source dir on the inject path ────────────────────────────────
// Commands newly recognized by D#73 reach this hook too (a bare dir with a
// `| head` tail is never rewritten, so it lands here). Pre-ship review round 1
// found the inject answering the wrong question on text-level matches; the
// recognition is now the rewrite grammar's, and a subdir shell is guarded.

test('e2e D#73: a bare dir with a tail gets the inject, scoped to that dir', (t) => {
  const uniq = `InjBare${Date.now()}`;
  const fixture = e2eFixture(
    `process.stdout.write('ARGV[' + process.argv.slice(2).join(' ') + ']\\nsrc/foo.rs\\n');`);
  const cmd = `grep -rn "${uniq}" src | head -3`;
  try {
    fs.mkdirSync(path.join(fixture.dir, 'src'), { recursive: true });
    const res = runHook(cmd, fixture, {}, undefined, '');
    const ctx = JSON.parse(res.stdout).hookSpecificOutput.additionalContext;
    assert.match(ctx, new RegExp(`ARGV\\[grep ${uniq} src\\]`), ctx);
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e D#73: a bare dir from a subdir shell gets no inject', (t) => {
  const uniq = `InjBareSub${Date.now()}`;
  const fixture = e2eFixture(`process.stdout.write('ARGV[never]\\n');`);
  const cmd = `grep -rn "${uniq}" src | head -3`;
  try {
    fs.mkdirSync(path.join(fixture.dir, 'src'), { recursive: true });
    fs.mkdirSync(path.join(fixture.dir, 'xtask', 'src'), { recursive: true });
    const res = runHook(cmd, fixture, {}, path.join(fixture.dir, 'xtask'), '');
    assert.equal(res.status, 0, res.stderr);
    assert.equal(res.stdout, '', `inject answered the root's src for a subdir grep: ${res.stdout}`);
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

for (const cmd of [
  'grep -n tasks "task_queue.py"',                   // M1: the bare word is the pattern
  'grep -rn "widget_count" $(ls -d src tests) | wc -l', // M2
  'ag "widget_count" --ignore tests . | wc -l',       // M3
]) {
  test(`e2e D#73: no inject for a non-path bare word: ${cmd}`, (t) => {
    const fixture = e2eFixture(`process.stdout.write('ARGV[never]\\n');`);
    try {
      for (const d of ['src', 'tests', 'tasks']) fs.mkdirSync(path.join(fixture.dir, d), { recursive: true });
      const res = runHook(cmd, fixture, {}, undefined, '');
      assert.equal(res.status, 0, res.stderr);
      assert.equal(res.stdout, '', `unexpected inject: ${res.stdout}`);
    } finally {
      cleanupFixture(fixture, cmd);
    }
  });
}

// Round 2 B3 / round 3: anything before the grep in the same command can move
// the shell (`cd`, and 19 other forms round 3 reproduced: `builtin cd`, `if cd`,
// `{ cd …; }`, `\\cd`, `popd`, `eval`, a function …). A denylist of those never
// closes, so a bare dir is answered only when the grep is the FIRST segment.
for (const prefix of [
  'cd xtask && ', 'builtin cd xtask && ', 'if cd xtask; then ', '{ cd xtask; ',
  '\\cd xtask && ', 'popd >/dev/null; ', 'eval "cd xtask"; ', 'echo start; ',
]) {
  test(`e2e D#73: no bare-dir inject when a segment precedes the grep: ${prefix}`, (t) => {
    const uniq = `InjBarePre${Date.now()}${Math.floor(Math.random() * 1e6)}`;
    const fixture = e2eFixture(`process.stdout.write('ARGV[never]\\n');`);
    const cmd = `${prefix}grep -rn "${uniq}" src`;
    try {
      fs.mkdirSync(path.join(fixture.dir, 'src'), { recursive: true });
      fs.mkdirSync(path.join(fixture.dir, 'xtask', 'src'), { recursive: true });
      const res = runHook(cmd, fixture, {}, undefined, '');
      assert.equal(res.status, 0, res.stderr);
      assert.equal(res.stdout, '', `inject answered the root's src: ${res.stdout}`);
    } finally {
      cleanupFixture(fixture, cmd);
    }
  });
}

// Round 2 B1, end to end: the inject must search the grammar's operand.
test('e2e D#73: a path-shaped pattern is not taken for the inject scope', (t) => {
  const uniq = `InjPatPath${Date.now()}`;
  const fixture = e2eFixture(
    `process.stdout.write('ARGV[' + process.argv.slice(2).join(' ') + ']\\n');`);
  const cmd = `grep -rn "./src/${uniq}" tests | head`;
  try {
    fs.mkdirSync(path.join(fixture.dir, 'tests'), { recursive: true });
    const res = runHook(cmd, fixture, {}, undefined, '');
    assert.equal(res.status, 0, res.stderr);
    if (res.stdout) {
      const ctx = JSON.parse(res.stdout).hookSpecificOutput.additionalContext;
      assert.doesNotMatch(ctx, new RegExp(`ARGV\\[[^\\]]* \\./src/${uniq}\\]`), `pattern used as scope: ${ctx}`);
      assert.match(ctx, /ARGV\[[^\]]* tests\]/, ctx);
    }
  } finally {
    cleanupFixture(fixture, cmd);
  }
});


// ── D#76: the inject searches where the grep searched ─────────────────────
// The answer runs from the root with a root-relative path. A segment after a
// `cd`, or a subdirectory shell's operand the rebase left alone, searched
// somewhere else.
function injectArgs(res) {
  assert.equal(res.status, 0, res.stderr);
  if (!res.stdout) return null;
  const m = JSON.parse(res.stdout).hookSpecificOutput.additionalContext.match(/ARGV\[([^\]]*)\]/);
  return m ? m[1] : 'no-argv';
}
const ARGV_ECHO = `process.stdout.write('ARGV[' + process.argv.slice(2).join(' ') + ']\\n');`;

function withTwoSrcDirs(fn) {
  const fixture = e2eFixture(ARGV_ECHO);
  fs.mkdirSync(path.join(fixture.dir, 'src'), { recursive: true });
  fs.mkdirSync(path.join(fixture.dir, 'xtask', 'src'), { recursive: true });
  return fixture;
}

test('e2e D#76: `cd xtask && grep … src/` is not answered with the root\'s src', () => {
  const uniq = `InjCd${Date.now()}`;
  const fixture = withTwoSrcDirs();
  const cmd = `cd xtask && grep -rn "${uniq}" src/ | head -3`;
  try {
    assert.equal(injectArgs(runHook(cmd, fixture, {}, undefined, '')), null);
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e D#76: a subdir shell\'s `src/` the rebase left alone gets no inject', () => {
  const uniq = `InjSubSlash${Date.now()}`;
  const fixture = withTwoSrcDirs();
  const cmd = `echo x; grep -rn "${uniq}" src/ | head -3`;
  try {
    assert.equal(injectArgs(runHook(cmd, fixture, {}, path.join(fixture.dir, 'xtask'), '')), null);
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e D#76: `cd <root> && grep … src/` from a subdir is the root\'s src', () => {
  const uniq = `InjCdRoot${Date.now()}`;
  const fixture = withTwoSrcDirs();
  const real = fs.realpathSync(fixture.dir);
  const cmd = `cd ${real} && grep -rn "${uniq}" src/ | head -3`;
  try {
    assert.equal(injectArgs(runHook(cmd, fixture, {}, path.join(real, 'xtask'), '')), `grep ${uniq} src/`);
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e D#76: an absolute operand after a cd is still that path', () => {
  const uniq = `InjCdAbs${Date.now()}`;
  const fixture = withTwoSrcDirs();
  const real = fs.realpathSync(fixture.dir);
  const cmd = `cd xtask && grep -rn "${uniq}" ${real}/src/ | head -3`;
  try {
    assert.equal(injectArgs(runHook(cmd, fixture, {}, undefined, '')), `grep ${uniq} src/`);
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e D#76: cwd-neutral segments before the grep keep the inject', () => {
  const uniq = `InjNeutral${Date.now()}`;
  const fixture = withTwoSrcDirs();
  const cmd = `echo "== a"; git status; grep -rn "${uniq}" src/ | head -3`;
  try {
    assert.equal(injectArgs(runHook(cmd, fixture, {}, undefined, '')), `grep ${uniq} src/`);
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

// D#66 — a grep line inside a heredoc body never ran.
test('e2e D#66: a grep inside a heredoc body gets no inject', () => {
  const uniq = `InjHeredoc${Date.now()}`;
  const fixture = withTwoSrcDirs();
  const cmd = `python3 - <<'PY'\nprint("x")\ngrep -rn "${uniq}" src/\nPY\necho done`;
  try {
    assert.equal(injectArgs(runHook(cmd, fixture, {}, undefined, '')), null);
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e D#66: a grep after a heredoc is found, whatever the body quotes', () => {
  const uniq = `InjAfterDoc${Date.now()}`;
  const fixture = withTwoSrcDirs();
  const cmd = `python3 - <<'PY'\nprint('it\\'s')\nPY\ngrep -rn "${uniq}" src/ | head -3`;
  try {
    assert.equal(injectArgs(runHook(cmd, fixture, {}, undefined, '')), `grep ${uniq} src/`);
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

// D#125 #1: the root strip rewrote a pattern holding the project root, and the
// inject answered the stripped pattern.
test('e2e: a pattern holding the project root is not answered', (t) => {
  const uniq = `InjRootPat${Date.now()}`;
  const fixture = e2eFixture(`process.stdout.write('src/foo.rs:1  hit\\n');`);
  const root = fs.realpathSync(fixture.dir);
  const cmd = `echo x && grep -rn "${root}/${uniq}" src/`;
  const control = `echo x && grep -rn "${uniq}" "${root}/src/"`;
  try {
    const res = runHook(cmd, fixture, {}, root, '');
    assert.equal(res.status, 0, res.stderr);
    assert.equal(res.stdout.trim(), '', `must not inject: ${res.stdout}`);
    // Control: a quoted path under the root is still answered.
    const ok = runHook(control, fixture, {}, root, '');
    assert.match(JSON.parse(ok.stdout).hookSpecificOutput.additionalContext, /src\/foo\.rs/);
  } finally {
    cleanupFixture(fixture, cmd);
    cleanupFixture(fixture, control);
  }
});

// Pre-release review of D#125, F1 and F4.
test('e2e: the inject skips a -e pattern holding the root', (t) => {
  const uniq = `InjRootE${Date.now()}`;
  const fixture = e2eFixture(
    `if (process.argv[2] === 'callgraph') process.exit(1);\n` +
    `process.stdout.write('src/foo.rs:1  ARGV[' + process.argv.slice(2).join(' ') + ']\\n');`);
  const root = fs.realpathSync(fixture.dir);
  const cmd = `grep -rn -e "${root}/def ${uniq}" -e "BarBaz" src/; echo x`;
  try {
    const res = runHook(cmd, fixture, {}, root, '');
    assert.equal(res.stdout.trim(), '', `must not inject: ${res.stdout}`);
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: the inject scopes show to the grep path and skips a non-recursive dir grep', (t) => {
  const uniq = `InjScope${Date.now()}`;
  const fixture = e2eFixture(
    `if (process.argv[2] === 'callgraph') process.exit(1);\n` +
    `if (process.argv[2] === 'show') { process.stdout.write('fn ' + process.argv[3] + '  src/a/x.rs:1-3  ()\\n  body\\n'); process.exit(0); }\n` +
    `process.stdout.write('ARGV[' + process.argv.slice(2).join(' ') + ']\\nsrc/foo.rs\\n');`);
  fs.mkdirSync(path.join(fixture.dir, 'lib'));
  fs.mkdirSync(path.join(fixture.dir, 'src'));
  const scoped = `grep -rn -A3 "fn ${uniq}\\b" lib/ && echo done`;
  const flat = `grep -n "${uniq}Bar" src/ ; echo done`;
  try {
    const a = runHook(scoped, fixture, {}, undefined, '').stdout.trim();
    if (a) assert.doesNotMatch(JSON.parse(a).hookSpecificOutput.additionalContext, /src\/a\/x\.rs/,
      'a definition outside the grep path');
    const b = runHook(flat, fixture, {}, undefined, '');
    assert.equal(b.stdout.trim(), '', `a grep without -r searched no directory: ${b.stdout}`);
  } finally {
    cleanupFixture(fixture, scoped);
    cleanupFixture(fixture, flat);
  }
});

// Review of D#125, round 2: F1 (a root-holding pattern spelled like a path was
// answered) and F2 (the inject printed show output from outside the grep path).
test('e2e: the inject answers neither a root-holding pattern nor show output outside the path', (t) => {
  const uniq = `InjR2${Date.now()}`;
  const fixture = e2eFixture(
    `if (process.argv[2] === 'callgraph') process.exit(1);\n` +
    `if (process.argv[2] === 'show') { for (const f of ['lib/d.rs', 'src/a.rs']) process.stdout.write('fn ' + process.argv[3] + '  ' + f + ':1-3  ()\\n  body\\n'); process.exit(0); }\n` +
    `process.stdout.write('src/foo_mod/m.rs:1  ARGV[' + process.argv.slice(2).join(' ') + ']\\n');`);
  fs.mkdirSync(path.join(fixture.dir, 'lib'));
  fs.mkdirSync(path.join(fixture.dir, 'src', 'foo_mod'), { recursive: true });
  const root = fs.realpathSync(fixture.dir);
  const rootPat = `echo x; grep -rn "${root}/src/foo_mod" lib/`;
  const scoped = `grep -rn -A3 "fn ${uniq}\\b" src/a.rs; echo done`;
  try {
    assert.equal(runHook(rootPat, fixture, {}, root, '').stdout.trim(), '', 'root-holding pattern');
    const out = runHook(scoped, fixture, {}, root, '').stdout.trim();
    if (out) assert.doesNotMatch(JSON.parse(out).hookSpecificOutput.additionalContext, /lib\/d\.rs/);
  } finally {
    cleanupFixture(fixture, rootPat);
    cleanupFixture(fixture, scoped);
  }
});

// ── D#133: leftovers of the 0.160.0/0.161.0 reviews ─────────────────
// The inject had no grammar: any flag it did not know was dropped, so an
// inverted, non-recursive or differently-filtered grep got an answer for
// another search. It now answers only a grep clause the rewrite grammar reads
// (the same allowlist per verb), with the same pattern and path.
function d133Stub() {
  return `if (process.argv[2] === 'callgraph') process.exit(1);\n` +
    `if (process.argv[2] === 'show') { process.stdout.write('fn ' + process.argv[3] + '  src/a.rs:1-3  ()\\n  SHOWBODY\\n'); process.exit(0); }\n` +
    `process.stdout.write('src/a.rs:1  ARGV[' + process.argv.slice(2).join(' ') + ']\\n');`;
}

test('e2e D#133: the inject declines a grep whose flags or path it cannot reproduce', () => {
  const uniq = `InjD133${Date.now()}`;
  const fixture = e2eFixture(d133Stub());
  fs.mkdirSync(path.join(fixture.dir, 'src', 'sub'), { recursive: true });
  fs.writeFileSync(path.join(fixture.dir, 'src', 'a.js'), 'x\n');
  const declined = [
    // #9: the shell strips the quotes; grep reads -r -v.
    `echo start; grep "-rv" "${uniq}A" src/sub/`,
    `echo start; grep -rn "${uniq}B" src/ "-v"`,
    // Non-recursive: a glob the shell expands one level deep, ag -n, a depth cap.
    `echo x; grep -n "${uniq}C" src/*.js`,
    `echo x; grep -rn "${uniq}D" src/*.js`,
    `echo x; ag -n "${uniq}E" src/`,
    `echo x; rg --max-depth 1 "${uniq}F" src/`,
    `echo x; grep -rn -d skip "${uniq}G" src/`,
    // An abbreviated long flag GNU grep accepts (--invert-match, --include).
    `echo x; grep -rn --inv "${uniq}H" src/`,
    `echo x; grep -rn --inc='*.rs' "${uniq}I" src/`,
    // #7: GNU grep matches --include against the base name.
    `echo x; grep -rn --include='src/*.js' "${uniq}J" src/`,
    // #5: -E / -P dialect.
    `echo x; grep -rnE "${uniq}K\\d" src/`,
    `echo x; grep -rnP "\\<${uniq}L" src/`,
    // M19: an unquoted expansion joined to the quoted pattern.
    `echo x; grep -rn "${uniq}M"$SUFFIX src/`,
    // #10: the grep never ran.
    `exit 0; grep -rn "${uniq}N" src/`,
    `set -e; test -f nope.txt; grep -rn "${uniq}O" src/`,
  ];
  const accepted = [
    `echo x; grep -rn "${uniq}P" src/`,
    `echo x; grep -rnE "${uniq}Q|other_symbol" src/`,
    `echo x; grep -rn --include='*.js' "${uniq}R" src/`,
    `set -x; grep -rn "${uniq}S" src/`,
  ];
  try {
    for (const cmd of declined) {
      const res = runHook(cmd, fixture, {}, undefined, '');
      assert.equal(res.status, 0, res.stderr);
      assert.equal(res.stdout.trim(), '', `must not inject: ${cmd}\n${res.stdout}`);
    }
    for (const cmd of accepted) {
      const out = runHook(cmd, fixture, {}, undefined, '').stdout.trim();
      assert.ok(out.startsWith('{'), `control must inject: ${cmd}`);
    }
  } finally {
    for (const cmd of [...declined, ...accepted]) cleanupFixture(fixture, cmd);
  }
});

// 0.161.0 Not covered: `show` answers exact names. The inject's show answer
// now needs a pattern naming whole definitions; otherwise it answers the grep.
test('e2e D#133: the inject answers a prefix declaration grep with grep, not show', () => {
  const uniq = `InjShowExact${Date.now()}`;
  const fixture = e2eFixture(d133Stub());
  fs.mkdirSync(path.join(fixture.dir, 'src'));
  const prefix = `echo x; grep -rn -A3 "fn ${uniq}" src/`;
  const exact = `echo x; grep -rn -A3 "fn ${uniq}B\\b" src/`;
  try {
    const a = JSON.parse(runHook(prefix, fixture, {}, undefined, '').stdout).hookSpecificOutput.additionalContext;
    assert.doesNotMatch(a, /SHOWBODY/, `fn X also matches fn Xbar: ${a}`);
    assert.match(a, /ARGV\[grep /);
    const b = JSON.parse(runHook(exact, fixture, {}, undefined, '').stdout).hookSpecificOutput.additionalContext;
    assert.match(b, /SHOWBODY/, `a bounded name is answered by show: ${b}`);
  } finally {
    cleanupFixture(fixture, prefix);
    cleanupFixture(fixture, exact);
  }
});

// M22 of the 0.160.0 review stayed green: post-grep-inject not passing `seps`
// to segmentCwd. `true || cd .. && grep` runs the grep without the cd.
test('e2e D#133: a cd after || is not followed end to end (M22)', () => {
  const uniq = `InjSeps${Date.now()}`;
  const fixture = e2eFixture(d133Stub());
  fs.mkdirSync(path.join(fixture.dir, 'src'));
  fs.mkdirSync(path.join(fixture.dir, 'docs'));
  const docs = path.join(fs.realpathSync(fixture.dir), 'docs');
  const orCd = `true || cd .. && grep -rn "${uniq}A" src/`;
  const andCd = `cd .. && grep -rn "${uniq}B" src/`;
  try {
    assert.equal(runHook(orCd, fixture, {}, docs, '').stdout.trim(), '',
      'the grep ran in docs/, where src/ does not exist');
    assert.ok(runHook(andCd, fixture, {}, docs, '').stdout.trim().startsWith('{'),
      'control: a cd that must have run is followed');
  } finally {
    cleanupFixture(fixture, orCd);
    cleanupFixture(fixture, andCd);
  }
});

// Mutations of the D#133 gate that stayed green: the call graph is injected
// before any grep answer, so a declined grep must be refused before it too; and
// the gate's pattern and path must be the ones the answer uses.
test('e2e D#133: the grammar gate covers the call graph, the pattern and the path', () => {
  const uniq = `InjGate${Date.now()}`;
  const fixture = e2eFixture(
    `if (process.argv[2] === 'callgraph') { process.stdout.write(process.argv[3] + '\\n  ← called by: caller (src/b.rs)\\n'); process.exit(0); }\n` +
    `process.stdout.write('src/a.rs:1  ARGV[' + process.argv.slice(2).join(' ') + ']\\n');`);
  fs.mkdirSync(path.join(fixture.dir, 'src', 'sub'), { recursive: true });
  fs.mkdirSync(path.join(fixture.dir, 'src', 'x_y'), { recursive: true });
  fs.mkdirSync(path.join(fixture.dir, 'tmp'));
  const declined = [
    // grep reads -r -v; a call graph of the symbol is no view of this grep
    `echo start; grep -rn "${uniq}A" src/ "-v"`,
    // the unquoted word is the pattern; the quoted path looks like one
    `echo x; grep -rn ${uniq}_b "src/x_y/"`,
    // the quoted pattern looks like a path; the grep searched tmp/
    `echo x; grep -rn "src/${uniq}_c" tmp/`,
  ];
  const control = `echo start; grep -rn "${uniq}D" src/`;
  try {
    for (const cmd of declined) {
      assert.equal(runHook(cmd, fixture, {}, undefined, '').stdout.trim(), '', `must not inject: ${cmd}`);
    }
    assert.match(JSON.parse(runHook(control, fixture, {}, undefined, '').stdout).hookSpecificOutput.additionalContext,
      /Cross-file call graph/, 'control');
  } finally {
    for (const cmd of [...declined, control]) cleanupFixture(fixture, cmd);
  }
});

// D#133 #8 on the PostToolUse side: an untracked hidden file under the path is
// read by grep -r and not by the answer.
test('e2e D#133: the inject declines when the grep reads files the answer does not', () => {
  const uniq = `InjFiles${Date.now()}`;
  const fixture = e2eFixture(d133Stub());
  const git = (...a) => {
    const r = spawnSync('git', ['-c', 'user.email=t@t', '-c', 'user.name=t', ...a], { cwd: fixture.dir, encoding: 'utf8' });
    assert.equal(r.status, 0, r.stderr);
  };
  git('init', '-q', '.');
  fs.mkdirSync(path.join(fixture.dir, 'src'));
  fs.writeFileSync(path.join(fixture.dir, 'src', 'a.rs'), 'x\n');
  fs.writeFileSync(path.join(fixture.dir, '.gitignore'), '.code-graph/\n');
  git('add', '-A');
  git('commit', '-qm', 'init');
  const control = `echo x; grep -rn "${uniq}A" src/`;
  const cmd = `echo x; grep -rn "${uniq}B" src/`;
  try {
    assert.ok(runHook(control, fixture, {}, undefined, '').stdout.trim().startsWith('{'), 'control');
    fs.writeFileSync(path.join(fixture.dir, 'src', '.local.rs'), 'x\n');
    assert.equal(runHook(cmd, fixture, {}, undefined, '').stdout.trim(), '', 'an untracked hidden file');
  } finally {
    cleanupFixture(fixture, cmd);
    cleanupFixture(fixture, control);
  }
});

// ── D#133 review repairs on the PostToolUse side ─────────────────────
function gitFixtureInject(stubBody) {
  const fixture = e2eFixture(stubBody);
  const git = (...a) => {
    const r = spawnSync('git', ['-c', 'user.email=t@t', '-c', 'user.name=t', ...a], { cwd: fixture.dir, encoding: 'utf8' });
    assert.equal(r.status, 0, r.stderr);
  };
  git('init', '-q', '.');
  fs.mkdirSync(path.join(fixture.dir, 'src'));
  fs.writeFileSync(path.join(fixture.dir, 'src', 'a.rs'), 'x\n');
  fs.writeFileSync(path.join(fixture.dir, '.gitignore'), '.code-graph/\nsrc/gen/\n');
  git('add', '-A');
  git('commit', '-qm', 'init');
  return { fixture, git };
}

// L-6 R29: the inject passed its own verb to the file check; a check run for
// another verb (git grep reads no untracked file, ignored or not) let grep -r's
// ignored file through.
test('e2e review L-6 R29: the inject checks the files of the grep\'s own verb', () => {
  const uniq = `InjVerb${Date.now()}`;
  const { fixture } = gitFixtureInject(d133Stub());
  const control = `echo x; grep -rn "${uniq}A" src/`;
  const cmd = `echo x; grep -rn "${uniq}B" src/`;
  try {
    assert.ok(runHook(control, fixture, {}, undefined, '').stdout.trim().startsWith('{'), 'control');
    fs.mkdirSync(path.join(fixture.dir, 'src', 'gen'));
    fs.writeFileSync(path.join(fixture.dir, 'src', 'gen', 'g.rs'), 'x\n');
    assert.equal(runHook(cmd, fixture, {}, undefined, '').stdout.trim(), '', 'an untracked ignored file grep -r reads');
  } finally {
    cleanupFixture(fixture, cmd);
    cleanupFixture(fixture, control);
  }
});

// Review H-1 and H-2 on the inject: a `.ignore` hides files from cg's walk, and
// a show answer reads the index, which skips vendor/.
test('e2e review H-1/H-2: the inject declines on a ripgrep-only ignore file and on a file the index skips', () => {
  const uniq = `InjIdx${Date.now()}`;
  const cmds = [];
  try {
    {
      const { fixture } = gitFixtureInject(d133Stub());
      const control = `echo x; grep -rn -A2 "fn ${uniq}A\\b" src/`;
      const cmd = `echo x; grep -rn -A2 "fn ${uniq}B\\b" src/`;
      cmds.push([fixture, control], [fixture, cmd]);
      assert.match(JSON.parse(runHook(control, fixture, {}, undefined, '').stdout).hookSpecificOutput.additionalContext,
        /SHOWBODY/, 'control answers with show');
      fs.mkdirSync(path.join(fixture.dir, 'src', 'vendor'));
      fs.writeFileSync(path.join(fixture.dir, 'src', 'vendor', 'b.rs'), `fn ${uniq}B() {}\n`);
      assert.equal(runHook(cmd, fixture, {}, undefined, '').stdout.trim(), '', 'src/vendor/ is not indexed');
    }
    {
      const { fixture } = gitFixtureInject(d133Stub());
      const control = `echo x; grep -rn "${uniq}C" src/`;
      const cmd = `echo x; grep -rn "${uniq}D" src/`;
      cmds.push([fixture, control], [fixture, cmd]);
      assert.ok(runHook(control, fixture, {}, undefined, '').stdout.trim().startsWith('{'), 'control');
      fs.writeFileSync(path.join(fixture.dir, 'src', '.ignore'), 'u.rs\n');
      fs.writeFileSync(path.join(fixture.dir, 'src', 'u.rs'), 'x\n');
      assert.equal(runHook(cmd, fixture, {}, undefined, '').stdout.trim(), '', 'src/.ignore');
    }
  } finally {
    for (const [fixture, cmd] of cmds) cleanupFixture(fixture, cmd);
  }
});

// ── B11 (2026-09-29 usage evaluation): a symbol defined outside the grep's path ──
// A grep scoped to one file found nothing, and the hook injected 4 KB of the
// call tree of a same-named symbol defined elsewhere (`indexed_project` in
// src/indexer/resync.rs for a grep in tests/cli_e2e.rs). Only its location
// was useful: that is now all it gets, as one pointer line.
{
  const { definitionFile, searchCovers, buildPointerText } = require('./post-grep-inject');

  test('definitionFile: the file on the callgraph root line, or null', () => {
    assert.equal(definitionFile('indexed_project (src/indexer/resync.rs)\n  ← called by: x (a.rs)'), 'src/indexer/resync.rs');
    assert.equal(definitionFile('indexed_project\n  ← called by: x (a.rs)'), null, 'no file on the root line');
    assert.equal(definitionFile(''), null);
    assert.equal(definitionFile(undefined), null);
  });

  test('searchCovers: a path covers itself and what is under it; a glob counts as its directory', () => {
    const cases = [
      [null, 'src/a.rs', true], ['', 'src/a.rs', true], ['.', 'src/a.rs', true], ['./', 'src/a.rs', true],
      ['src', 'src/a.rs', true], ['src/', 'src/deep/a.rs', true], ['./src', 'src/a.rs', true],
      ['src/a.rs', 'src/a.rs', true], ['tests/cli_e2e.rs', 'src/indexer/resync.rs', false],
      ['tests', 'src/a.rs', false], ['src/a', 'src/ab.rs', false],
      ['tests/*.rs', 'tests/x.rs', true], ['tests/*.rs', 'src/x.rs', false], ['*.py', 'src/x.py', true],
      ['src/**/x.rs', 'src/deep/x.rs', true],
      ['tests', null, true],
    ];
    for (const [p, f, want] of cases) assert.equal(searchCovers(p, f), want, `${p} covers ${f}`);
  });

  // The pointer used to end in `Its callers and callees: code-graph-mcp callgraph X`.
  // From 0.164.0 to 2026-10-16 no session ran it (0 of 45); the next step was a
  // read of the definition. So the line now carries the definition's lines and
  // signature when `show` names exactly one, and no command either way
  // (tasks/specs/outside-path-definition-answer.md).
  test('buildPointerText: the definition\'s lines and signature, never a command', () => {
    const def = { status: 'hits', start: 10, end: 20, label: 'function indexed_project(root: &Path)' };
    const t = buildPointerText('indexed_project', 'src/indexer/resync.rs', 'tests/cli_e2e.rs', def);
    assert.equal(t, '[code-graph] indexed_project is defined in src/indexer/resync.rs:10-20, outside the path ' +
      'your grep searched (tests/cli_e2e.rs): function indexed_project(root: &Path)');
    for (const other of [undefined, { status: 'several' }, { status: 'no-hits' }, { status: 'unavailable' }]) {
      assert.equal(buildPointerText('indexed_project', 'src/indexer/resync.rs', 'tests/cli_e2e.rs', other),
        '[code-graph] indexed_project is defined in src/indexer/resync.rs, outside the path your grep searched (tests/cli_e2e.rs).',
        JSON.stringify(other));
    }
    assert.match(buildPointerText('f', "src/a b'.rs", 'x'), /'src\/a b'\\''\.rs'/);
  });

  // `show` answers for the callgraph root's file only when SHOW is set; the
  // argv must be exactly the one runDefinitionAnswer sends.
  const STUB_ROOTED = (file, show = false) =>
    `const sub = process.argv[2], arg = process.argv[3], a = process.argv.slice(2);\n` +
    `if (sub === 'callgraph') { process.stdout.write(arg + ' (${file})\\n  \\u2190 called by: someCaller (src/x.rs:3)\\n'); process.exit(0); }\n` +
    (show
      ? `if (sub === 'show' && a.length === 6 && a[2] === '--file' && a[3] === '${file}' && a[4] === '--json' && a[5] === '--compact') {\n` +
        `  process.stdout.write(JSON.stringify([{ file_path: '${file}', name: arg, type: 'function', signature: '(root: &Path)', start_line: 41, end_line: 77 }]));\n` +
        `  process.exit(0);\n}\n`
      : '') +
    `process.exit(1);`;

  test('e2e B11: a symbol defined outside the grep path gets its definition, not its call graph', () => {
    const uniq = `Outside${Date.now()}`;
    const fixture = e2eFixture(STUB_ROOTED('src/indexer/resync.rs', true));
    const cmd = `echo "x" && grep "${uniq}" tests/cli_e2e.rs`;
    try {
      const res = runHook(cmd, fixture, {}, undefined, '');
      assert.equal(res.status, 0, res.stderr);
      const ctx = JSON.parse(res.stdout).hookSpecificOutput.additionalContext;
      assert.equal(ctx, `[code-graph] ${uniq} is defined in src/indexer/resync.rs:41-77, outside the path your grep ` +
        `searched (tests/cli_e2e.rs): function ${uniq}(root: &Path)`);
      assert.doesNotMatch(ctx, /called by|code-graph-mcp /);
      const recs = fs.readFileSync(path.join(fixture.dir, '.code-graph', 'recommendations.jsonl'), 'utf8');
      const rec = JSON.parse(recs.trim().split('\n').pop());
      assert.equal(rec.mode, 'callgraph');
      assert.equal(rec.scope, 'outside');
    } finally {
      cleanupFixture(fixture, cmd);
    }
  });

  test('e2e B11: when show names no single definition, the location alone', () => {
    const uniq = `OutsideNoShow${Date.now()}`;
    const fixture = e2eFixture(STUB_ROOTED('src/indexer/resync.rs'));
    const cmd = `echo "x" && grep "${uniq}" tests/cli_e2e.rs`;
    try {
      const res = runHook(cmd, fixture, {}, undefined, '');
      assert.equal(res.status, 0, res.stderr);
      const ctx = JSON.parse(res.stdout).hookSpecificOutput.additionalContext;
      assert.equal(ctx, `[code-graph] ${uniq} is defined in src/indexer/resync.rs, outside the path your grep searched (tests/cli_e2e.rs).`);
      const recs = fs.readFileSync(path.join(fixture.dir, '.code-graph', 'recommendations.jsonl'), 'utf8');
      const rec = JSON.parse(recs.trim().split('\n').pop());
      assert.equal(rec.mode, 'callgraph');
      assert.equal(rec.scope, 'outside');
    } finally {
      cleanupFixture(fixture, cmd);
    }
  });

  test('e2e B11: a symbol defined inside the grep path keeps its call graph', () => {
    const uniq = `Inside${Date.now()}`;
    const fixture = e2eFixture(STUB_ROOTED('src/lib.rs'));
    const cmd = `echo "x" && grep "${uniq}" src/`;
    try {
      const res = runHook(cmd, fixture, {}, undefined, '');
      assert.equal(res.status, 0, res.stderr);
      const ctx = JSON.parse(res.stdout).hookSpecificOutput.additionalContext;
      assert.match(ctx, /Cross-file call graph/);
      assert.match(ctx, /called by: someCaller/);
      const recs = fs.readFileSync(path.join(fixture.dir, '.code-graph', 'recommendations.jsonl'), 'utf8');
      assert.equal(JSON.parse(recs.trim().split('\n').pop()).scope, undefined);
    } finally {
      cleanupFixture(fixture, cmd);
    }
  });
}
