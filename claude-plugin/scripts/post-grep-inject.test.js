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
// machine-global cgTmpDir, and every e2e command carries a `Date.now()` so no
// two runs ever collide: measured on the commit before this one, one run of this file left 10 flags
// behind (14 across the three hook test files), reclaimed only by pruneCgTmp's
// 24h sweep. `cleanupFixture` was supposed to cover that and did not — see the
// spelling bug documented there. Owning the directory fixes both the flags this
// file knows about and any a future test adds.
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
