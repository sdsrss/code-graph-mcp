'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const {
  shouldHint,
  shouldBlock,
  extractPatterns,
  extractSearchPath,
  pickBlockPattern,
  buildHint,
  buildBlockReason,
  buildBlockReasonWithAnswer,
  buildNoHitsFyi,
  commandHash,
  isSilenced,
  isBlockDisabled,
  isAnswerDisabled,
} = require('./pre-grep-guide');

// ── Should fire: bare grep/rg/ag on indexed source tree ─────────────

test('shouldHint: grep -rn on src/', () => {
  assert.equal(shouldHint('grep -rn "fn fts5_search" src/storage/'), true);
});

test('shouldHint: rg on tests/', () => {
  assert.equal(shouldHint('rg "expand_acronym" tests/'), true);
});

test('shouldHint: grep -n on single file in src/', () => {
  assert.equal(shouldHint('grep -n "fn split_identifier" src/search/tokenizer.rs'), true);
});

test('shouldHint: grep -rn on claude-plugin/', () => {
  assert.equal(shouldHint('grep -rn "computeQuietHooks" claude-plugin/scripts/'), true);
});

test('shouldHint: grep with alternation against src/', () => {
  assert.equal(shouldHint('grep -rn "set_hook\\|panic_handler" src/main.rs src/lib.rs'), true);
});

test('shouldHint: grep with stderr redirect + head pipe (still a source search)', () => {
  // head/tail/sort pipes don't disqualify — the SEARCH operation is grep on src/
  assert.equal(shouldHint('grep -rn "fn fts5_search\\|MATCH" src/storage/ 2>&1 | head -10'), true);
});

test('shouldHint: ag on lib/', () => {
  assert.equal(shouldHint('ag "TODO" lib/'), true);
});

test('shouldHint: env-prefixed grep on src/', () => {
  assert.equal(shouldHint('env LANG=C grep -rn "Foo" src/'), true);
});

// ── Should NOT fire: pipe-grep (output filter, not search) ──────────

test('shouldHint: pipe-grep on cargo test output', () => {
  assert.equal(shouldHint('cargo test 2>&1 | grep "test result"'), false);
});

test('shouldHint: pipe-grep with -E flag', () => {
  assert.equal(shouldHint("cargo test --no-default-features 2>&1 | grep -E 'test result|FAILED'"), false);
});

test('shouldHint: pipe-rg', () => {
  assert.equal(shouldHint("cargo build 2>&1 | rg 'warning|error'"), false);
});

test('shouldHint: pipe-grep with src/ in pattern (still output filter)', () => {
  assert.equal(shouldHint("cargo build 2>&1 | grep 'src/main.rs'"), false);
});

// ── Should NOT fire: already using code-graph-mcp ───────────────────

test('shouldHint: code-graph-mcp grep itself', () => {
  assert.equal(shouldHint('code-graph-mcp grep "fn parse" src/'), false);
});

test('shouldHint: pipe through code-graph-mcp', () => {
  assert.equal(shouldHint('code-graph-mcp show foo | grep src/'), false);
});

// ── Should NOT fire: not source-tree paths ──────────────────────────

test('shouldHint: grep on Cargo.toml only', () => {
  assert.equal(shouldHint('grep "^version" Cargo.toml'), false);
});

test('shouldHint: grep -i docs on .gitignore', () => {
  assert.equal(shouldHint('grep -i docs .gitignore'), false);
});

test('shouldHint: grep on package.json', () => {
  assert.equal(shouldHint('grep "version" package.json'), false);
});

test('shouldHint: grep on a markdown changelog', () => {
  assert.equal(shouldHint('grep "v0.24" CHANGELOG.md'), false);
});

// ── Should NOT fire: not search tools ───────────────────────────────

test('shouldHint: ls src/', () => {
  assert.equal(shouldHint('ls src/storage/'), false);
});

test('shouldHint: cat src/main.rs', () => {
  assert.equal(shouldHint('cat src/main.rs'), false);
});

test('shouldHint: git log on src/', () => {
  assert.equal(shouldHint('git log --oneline -10 src/'), false);
});

test('shouldHint: find on src/ (file path tool, not content search)', () => {
  // find is path-based, not pattern-based. Out of scope for this hook.
  assert.equal(shouldHint('find src/ -name "*.rs"'), false);
});

// ── Edge cases ──────────────────────────────────────────────────────

test('shouldHint: empty command', () => {
  assert.equal(shouldHint(''), false);
});

test('shouldHint: non-string input', () => {
  assert.equal(shouldHint(null), false);
  assert.equal(shouldHint(undefined), false);
  assert.equal(shouldHint(42), false);
});

test('shouldHint: oversize command (>1000 chars)', () => {
  assert.equal(shouldHint('grep -rn "x" src/ ' + 'y'.repeat(1100)), false);
});

// ── Hint content ────────────────────────────────────────────────────

test('buildHint: includes all four code-graph subcommands', () => {
  const out = buildHint();
  assert.match(out, /code-graph-mcp grep/);
  assert.match(out, /code-graph-mcp ast-search/);
  assert.match(out, /code-graph-mcp callgraph/);
  assert.match(out, /code-graph-mcp show/);
});

test('buildHint: stays under 700-byte budget (~175 tokens)', () => {
  const out = buildHint();
  assert.ok(out.length < 700, `hint length ${out.length} exceeds budget`);
});

test('buildHint: mentions repo-wide / LSP boundary', () => {
  assert.match(buildHint(), /Repo-wide index|LSP/);
});

// ── Cooldown hash ───────────────────────────────────────────────────

test('commandHash: deterministic + 12-char', () => {
  const h1 = commandHash('grep -rn "foo" src/');
  const h2 = commandHash('grep -rn "foo" src/');
  assert.equal(h1, h2);
  assert.equal(h1.length, 12);
});

test('commandHash: different commands → different hashes', () => {
  assert.notEqual(commandHash('grep -rn "foo" src/'), commandHash('grep -rn "bar" src/'));
});

// ── Kill switch ─────────────────────────────────────────────────────

test('isSilenced: default (no env) → not silenced (noisy)', () => {
  assert.equal(isSilenced({}), false);
});

test('isSilenced: CODE_GRAPH_QUIET_HOOKS=1 → silenced', () => {
  assert.equal(isSilenced({ CODE_GRAPH_QUIET_HOOKS: '1' }), true);
});

test('isSilenced: CODE_GRAPH_QUIET_HOOKS=0 → not silenced', () => {
  assert.equal(isSilenced({ CODE_GRAPH_QUIET_HOOKS: '0' }), false);
});

test('isSilenced: VERBOSE_HOOKS=1 alone → not silenced (noisy by default already)', () => {
  // pre-grep-guide is noisy-by-default; VERBOSE is irrelevant here.
  assert.equal(isSilenced({ CODE_GRAPH_VERBOSE_HOOKS: '1' }), false);
});

// ── Phase C: extended prefixes (real-world backend / DDD / web conventions) ──

// daagu pattern: `backend/app/services/...` — `app/` is preceded by `backend/`,
// which doesn't satisfy the `(?:^|\s|["'])` lookbehind in the old SRC_PATH.
// 7d audit found 5 of the worst missed sessions used exactly this layout.
test('shouldHint: grep -rn on backend/app/services/ (daagu)', () => {
  assert.equal(
    shouldHint('grep -rn "pct_chg|pct_change" backend/app/services/context_builder.py'),
    true
  );
});

test('shouldHint: grep -rn on backend/app/services/scheduler/', () => {
  assert.equal(
    shouldHint('grep -rn "TASK_ZOMBIE|zombie recovery|reason=age" backend/app/services/scheduler/'),
    true
  );
});

test('shouldHint: grep on services/ (no backend prefix)', () => {
  assert.equal(shouldHint('grep -rn "fetchUser" services/auth/'), true);
});

test('shouldHint: grep on models/ (Rails / Django)', () => {
  assert.equal(shouldHint('grep -rn "before_save" models/user.rb'), true);
});

test('shouldHint: grep on controllers/ (Rails / ASP.NET)', () => {
  assert.equal(shouldHint('grep -rn "def index" controllers/UsersController.rb'), true);
});

test('shouldHint: grep on domain/ (DDD architecture)', () => {
  assert.equal(shouldHint('grep -rn "Aggregate" domain/orders/'), true);
});

test('shouldHint: grep on handlers/ (web server)', () => {
  assert.equal(shouldHint('grep -rn "func New" handlers/api/'), true);
});

test('shouldHint: grep on migrations/ (db schema)', () => {
  assert.equal(shouldHint('grep -rn "add_column" migrations/'), true);
});

test('shouldHint: grep on features/ (modular monolith)', () => {
  assert.equal(shouldHint('grep -rn "useFeature" features/billing/'), true);
});

test('shouldHint: grep on api/ + frontend/', () => {
  assert.equal(shouldHint('grep -rn "POST" api/v1/'), true);
  assert.equal(shouldHint('grep -rn "import React" frontend/'), true);
});

// Precision guards — these MUST still NOT fire after the expansion.

test('shouldHint: grep on web.config (config file ext keeps suppression)', () => {
  assert.equal(shouldHint('grep "<connectionStrings" web.config'), false);
});

test('shouldHint: grep on node_modules/ (NOT in src list)', () => {
  assert.equal(shouldHint('grep -rn "deprecated" node_modules/some-pkg/'), false);
});

test('shouldHint: grep on docs/ (docs trees stay out)', () => {
  // We deliberately did NOT add `docs` to the prefix list — docs are typically
  // markdown and the existing CONFIG_TARGET_ONLY already filters `.md`-only
  // greps. A bare `grep "X" docs/foo.md` would be CONFIG_TARGET_ONLY-suppressed.
  assert.equal(shouldHint('grep "v0.24" docs/CHANGELOG.md'), false);
});

// ── Regression cases from real session telemetry (2026-05-11) ───────

test('regression: grep -n "Error\\|anyhow" src/main.rs (sess 5052e2a1)', () => {
  assert.equal(shouldHint('grep -n "Error\\|anyhow\\|context" src/main.rs'), true);
});

test('regression: grep -rn "fn fts5_search" src/storage/ (sess 25fa8050)', () => {
  assert.equal(shouldHint('grep -rn "fn fts5_search\\|MATCH\\|fts.*tokenize" src/storage/'), true);
});

test('regression: grep multi-extension MEMORY.md tag search (sess 5052e2a1)', () => {
  // This one targets MEMORY.md files — should NOT fire because the --include flags
  // are for non-source extensions and there's no `src/` etc. in the args.
  assert.equal(shouldHint("grep -rn 'callgraph, impact' --include='*.md'"), false);
});

test('regression: cargo test pipe filter NOT fires (sess 45691293)', () => {
  assert.equal(shouldHint('cargo test --no-default-features 2>&1 | grep -E "test result|FAILED|error\\[" | tail -15'), false);
});

test('regression: grep -m1 "^version" Cargo.toml NOT fires', () => {
  assert.equal(shouldHint('grep -m1 "^version" Cargo.toml'), false);
});

// ════════════════════════════════════════════════════════════════════
// v0.32.0 — Block tier (shouldBlock, buildBlockReason, isBlockDisabled)
// ════════════════════════════════════════════════════════════════════

// ── shouldBlock: SHOULD block — identifier-shaped symbol scan ───────

test('shouldBlock: CamelCase identifier on src/', () => {
  assert.equal(shouldBlock('grep -rn "EmbeddingModel" src/'), true);
});

test('shouldBlock: snake_case identifier on src/', () => {
  assert.equal(shouldBlock('grep -rn "fts5_search" src/storage/'), true);
});

test('shouldBlock: fn declaration anchor on src/', () => {
  assert.equal(shouldBlock('grep -rn "fn fts5_search" src/storage/'), true);
});

test('shouldBlock: alternation with identifiers on src/', () => {
  assert.equal(shouldBlock('grep -rn "fn fts5_search\\|MATCH" src/storage/'), true);
});

test('shouldBlock: class declaration on src/', () => {
  assert.equal(shouldBlock('grep -rn "class UserService" src/'), true);
});

test('shouldBlock: def declaration on backend/app/', () => {
  assert.equal(shouldBlock('grep -rn "def fetch_user" backend/app/services/'), true);
});

test('shouldBlock: rg with CamelCase on lib/', () => {
  assert.equal(shouldBlock('rg "AuthHandler" lib/'), true);
});

// ── shouldBlock: should NOT block (downgrade to hint) — precision flags ─

test('shouldBlock: grep -l (files-with-matches) → hint only', () => {
  assert.equal(shouldBlock('grep -rl "EmbeddingModel" src/'), false);
});

test('shouldBlock: --include=*.rs → user already filtering, hint only', () => {
  assert.equal(shouldBlock('grep -rn --include="*.rs" "EmbeddingModel" src/'), false);
});

test('shouldBlock: --exclude-dir=tests → hint only', () => {
  assert.equal(shouldBlock('grep -rn --exclude=tests "EmbeddingModel" src/'), false);
});

test('shouldBlock: -A 3 context flag → hint only', () => {
  assert.equal(shouldBlock('grep -rn -A 3 "EmbeddingModel" src/'), false);
});

test('shouldBlock: -B 2 context flag → hint only', () => {
  assert.equal(shouldBlock('grep -rn -B 2 "EmbeddingModel" src/'), false);
});

test('shouldBlock: -C 5 context flag → hint only', () => {
  assert.equal(shouldBlock('grep -rn -C 5 "EmbeddingModel" src/'), false);
});

// ── shouldBlock: should NOT block — marker-only patterns ────────────

test('shouldBlock: bare TODO marker → hint only (no cg equivalent)', () => {
  assert.equal(shouldBlock('grep -rn "TODO" src/'), false);
});

test('shouldBlock: bare FIXME marker → hint only', () => {
  assert.equal(shouldBlock('grep -rn "FIXME" src/'), false);
});

test('shouldBlock: bare XXX marker → hint only', () => {
  assert.equal(shouldBlock('grep -rn "XXX" src/'), false);
});

test('shouldBlock: bare HACK marker → hint only', () => {
  assert.equal(shouldBlock('grep -rn "HACK" src/'), false);
});

// ── shouldBlock: should NOT block — non-identifier text ─────────────

test('shouldBlock: short lowercase word "foo" → hint only', () => {
  // No CamelCase, no _, no declaration anchor → not symbol-shaped
  assert.equal(shouldBlock('grep -rn "foo" src/'), false);
});

test('shouldBlock: short alphanumeric "v1" → hint only', () => {
  assert.equal(shouldBlock('grep -rn "v1" src/'), false);
});

// ── shouldBlock: should NOT block — inherits shouldHint=false ──────

test('shouldBlock: pipe-grep → false (already shouldHint=false)', () => {
  assert.equal(shouldBlock('cargo test 2>&1 | grep "EmbeddingModel"'), false);
});

test('shouldBlock: code-graph-mcp already used → false', () => {
  assert.equal(shouldBlock('code-graph-mcp grep "EmbeddingModel" src/'), false);
});

test('shouldBlock: empty / non-string → false', () => {
  assert.equal(shouldBlock(''), false);
  assert.equal(shouldBlock(null), false);
});

test('shouldBlock: grep on Cargo.toml only → false', () => {
  assert.equal(shouldBlock('grep "EmbeddingModel" Cargo.toml'), false);
});

// ── buildBlockReason content ────────────────────────────────────────

test('buildBlockReason: includes "denied"', () => {
  assert.match(buildBlockReason(), /denied/i);
});

test('buildBlockReason: lists cg grep + ast-search + callgraph', () => {
  const out = buildBlockReason();
  assert.match(out, /code-graph-mcp grep/);
  assert.match(out, /code-graph-mcp ast-search/);
  assert.match(out, /code-graph-mcp callgraph/);
});

test('buildBlockReason: documents the escape hatch env var', () => {
  assert.match(buildBlockReason(), /CODE_GRAPH_NO_BLOCK_GREP=1/);
});

test('buildBlockReason: under 700-byte budget (single CC message)', () => {
  const out = buildBlockReason();
  assert.ok(out.length < 700, `reason length ${out.length} exceeds budget`);
});

// ── isBlockDisabled escape hatch ────────────────────────────────────

test('isBlockDisabled: default (no env) → false (block enabled)', () => {
  assert.equal(isBlockDisabled({}), false);
});

test('isBlockDisabled: CODE_GRAPH_NO_BLOCK_GREP=1 → true', () => {
  assert.equal(isBlockDisabled({ CODE_GRAPH_NO_BLOCK_GREP: '1' }), true);
});

test('isBlockDisabled: CODE_GRAPH_NO_BLOCK_GREP=0 → false', () => {
  assert.equal(isBlockDisabled({ CODE_GRAPH_NO_BLOCK_GREP: '0' }), false);
});

test('isBlockDisabled: independent of CODE_GRAPH_QUIET_HOOKS', () => {
  // QUIET_HOOKS=1 silences entirely (no block, no hint).
  // NO_BLOCK_GREP=1 downgrades block to hint only.
  // The two flags must be orthogonal — neither implies the other.
  assert.equal(isBlockDisabled({ CODE_GRAPH_QUIET_HOOKS: '1' }), false);
  assert.equal(isSilenced({ CODE_GRAPH_NO_BLOCK_GREP: '1' }), false);
});

// ════════════════════════════════════════════════════════════════════
// v0.32.1 — extractPatterns + I1/I4 false-positive regressions
// ════════════════════════════════════════════════════════════════════

// ── extractPatterns: pulls quoted args from grep/rg/ag commands ──────

test('extractPatterns: single double-quoted pattern', () => {
  assert.deepEqual(extractPatterns('grep -rn "EmbeddingModel" src/'), ['EmbeddingModel']);
});

test('extractPatterns: single-quoted pattern', () => {
  assert.deepEqual(extractPatterns("grep -rn 'fts5_search' src/"), ['fts5_search']);
});

test('extractPatterns: env-prefixed verb', () => {
  assert.deepEqual(extractPatterns('env LANG=C grep -rn "Foo" src/'), ['Foo']);
});

test('extractPatterns: multiple -e patterns', () => {
  // Multi-pattern grep: both quoted args should be returned.
  const got = extractPatterns('grep -rn -e "first" -e "second" src/');
  assert.deepEqual(got, ['first', 'second']);
});

test('extractPatterns: pattern with alternation', () => {
  assert.deepEqual(
    extractPatterns('grep -rn "fn fts5_search\\|MATCH" src/storage/'),
    ['fn fts5_search\\|MATCH']
  );
});

test('extractPatterns: no quotes at all → empty array', () => {
  // Unquoted pattern (`grep foo src/`) — we deliberately don't try to parse
  // shell tokenization; shouldBlock falls back to hint in this case.
  assert.deepEqual(extractPatterns('grep -rn foo src/'), []);
});

test('extractPatterns: empty / non-string → empty array', () => {
  assert.deepEqual(extractPatterns(''), []);
  assert.deepEqual(extractPatterns(null), []);
  assert.deepEqual(extractPatterns(undefined), []);
});

test('extractPatterns: rg / ag head also stripped', () => {
  assert.deepEqual(extractPatterns('rg "Foo" lib/'), ['Foo']);
  assert.deepEqual(extractPatterns('ag "Bar" src/'), ['Bar']);
});

// ── I1 regression: identifier-shaped PATHS no longer trigger block ──

test('I1: grep -rn "abc" src/EmbeddingModel.rs → HINT (path has CamelCase, pattern doesn\'t)', () => {
  // CamelCase is in the FILENAME, not the pattern. v0.32.0 false-blocked
  // this. Pattern "abc" has no identifier shape → must downgrade to hint.
  assert.equal(shouldBlock('grep -rn "abc" src/EmbeddingModel.rs'), false);
});

test('I1: grep -rn "x" src/some_module/file.rs → HINT (path has snake_case)', () => {
  assert.equal(shouldBlock('grep -rn "x" src/some_module/file.rs'), false);
});

test('I1: grep -rn "the quick brown fox" src/EmbeddingModel.rs → HINT (English prose pattern)', () => {
  assert.equal(shouldBlock('grep -rn "the quick brown fox" src/EmbeddingModel.rs'), false);
});

test('I1: unquoted pattern grep -rn foo src/ → HINT (conservative fallback)', () => {
  // Without quotes we can't safely identify the pattern arg via shell rules
  // alone. Conservative: hint only.
  assert.equal(shouldBlock('grep -rn foo src/'), false);
});

test('I1: identifier pattern still blocks even with non-identifier path', () => {
  // Sanity check the inverse — block tier shouldn't get over-relaxed.
  // Path is plain `src/` but pattern is CamelCase → still block.
  assert.equal(shouldBlock('grep -rn "EmbeddingModel" src/'), true);
});

// ── I4 regression: declaration-anchor + `type` keyword fixes ─────────

test('I4: grep -rn "# type checking" src/ → HINT (comment scan, "type" not a decl keyword anymore)', () => {
  assert.equal(shouldBlock('grep -rn "# type checking" src/'), false);
});

test('I4: grep -rn "some type X" src/ → HINT (type not at pattern start, no longer over-matches)', () => {
  assert.equal(shouldBlock('grep -rn "some type X" src/'), false);
});

test('I4: grep -rn "the def keyword" src/ → HINT (def not at pattern start)', () => {
  // "the def keyword" had `\bdef\s+\w` match `def k` previously.
  // ^\s*(?:fn|def|...) anchor stops this.
  assert.equal(shouldBlock('grep -rn "the def keyword" src/'), false);
});

test('I4: grep -rn "def calc_total" src/ → BLOCK (def at start + snake_case)', () => {
  // Real declaration search — still blocks correctly.
  assert.equal(shouldBlock('grep -rn "def calc_total" src/'), true);
});

test('I4: grep -rn "fn render" src/ → BLOCK (decl anchor at start)', () => {
  assert.equal(shouldBlock('grep -rn "fn render" src/'), true);
});

// ── v0.47.0 deny-with-answer: extractSearchPath / pickBlockPattern ──

test('extractSearchPath: dir path after pattern', () => {
  assert.equal(extractSearchPath('grep -rn "fts5_search" src/storage/'), 'src/storage/');
});

test('extractSearchPath: single file in src/', () => {
  assert.equal(
    extractSearchPath('grep -n "split_identifier" src/search/tokenizer.rs'),
    'src/search/tokenizer.rs');
});

test('extractSearchPath: first of multiple paths wins', () => {
  assert.equal(extractSearchPath('grep -rn "set_hook" src/main.rs src/lib.rs'), 'src/main.rs');
});

test('extractSearchPath: quoted path is unwrapped', () => {
  assert.equal(extractSearchPath('grep -rn "Foo" "claude-plugin/scripts/"'), 'claude-plugin/scripts/');
});

test('extractSearchPath: flags and redirects are skipped', () => {
  assert.equal(extractSearchPath('grep -rn "Foo" src/ 2>&1'), 'src/');
});

test('extractSearchPath: ./-prefixed path is accepted', () => {
  assert.equal(extractSearchPath('grep -rn "Foo" ./src/parser/'), './src/parser/');
});

test('extractSearchPath: path traversal is rejected', () => {
  assert.equal(extractSearchPath('grep -rn "Foo" src/../../etc/'), undefined);
});

test('extractSearchPath: no source path → undefined', () => {
  assert.equal(extractSearchPath('grep -rn "Foo"'), undefined);
});

test('pickBlockPattern: returns the identifier-like pattern', () => {
  assert.equal(pickBlockPattern('grep -rn "EmbeddingModel" src/'), 'EmbeddingModel');
});

test('pickBlockPattern: skips non-identifier, picks identifier from -e args', () => {
  assert.equal(
    pickBlockPattern('grep -rn -e "some words" -e "fts5_search" src/'),
    'fts5_search');
});

test('pickBlockPattern: no identifier-like pattern → undefined', () => {
  assert.equal(pickBlockPattern('grep -rn "no ident here" src/'), undefined);
});

// ── v0.47.0 deny-with-answer: message builders + env gate ───────────

test('buildBlockReasonWithAnswer: embeds results, command, and escape hatch', () => {
  const reason = buildBlockReasonWithAnswer('fts5_search', 'src/storage/', {
    status: 'hits', text: 'src/storage/db.rs:42  fn fts5_search()', truncated: false,
  });
  assert.match(reason, /already ran/);
  assert.match(reason, /code-graph-mcp grep "fts5_search" src\/storage\//);
  assert.match(reason, /src\/storage\/db\.rs:42/);
  assert.match(reason, /CODE_GRAPH_NO_BLOCK_GREP=1/);
  assert.doesNotMatch(reason, /truncated/);
});

test('buildBlockReasonWithAnswer: no searchPath → command has no path arg', () => {
  const reason = buildBlockReasonWithAnswer('fts5_search', undefined, {
    status: 'hits', text: 'hit', truncated: false,
  });
  assert.match(reason, /code-graph-mcp grep "fts5_search"\n/);
});

test('buildBlockReasonWithAnswer: truncated flag adds marker', () => {
  const reason = buildBlockReasonWithAnswer('fts5_search', 'src/', {
    status: 'hits', text: 'hit', truncated: true,
  });
  assert.match(reason, /truncated/);
});

test('buildNoHitsFyi: names the pattern and says raw grep proceeds', () => {
  const fyi = buildNoHitsFyi('GhostSymbol');
  assert.match(fyi, /GhostSymbol/);
  assert.match(fyi, /[Nn]o matches/);
});

test('isAnswerDisabled: only env=1 disables', () => {
  assert.equal(isAnswerDisabled({ CODE_GRAPH_NO_ANSWER_IN_DENY: '1' }), true);
  assert.equal(isAnswerDisabled({ CODE_GRAPH_NO_ANSWER_IN_DENY: '0' }), false);
  assert.equal(isAnswerDisabled({}), false);
});

// ── v0.47.0 deny-with-answer: stdin-spawn e2e with stub binary ──────

const { spawnSync: spawnHook } = require('child_process');
const fsE2e = require('fs');
const osE2e = require('os');
const pathE2e = require('path');
const { cgTmpDir } = require('./tmp-dir');

function e2eFixture(stubBody) {
  const dir = fsE2e.mkdtempSync(pathE2e.join(osE2e.tmpdir(), 'pre-grep-e2e-'));
  fsE2e.mkdirSync(pathE2e.join(dir, '.code-graph'), { recursive: true });
  fsE2e.writeFileSync(pathE2e.join(dir, '.code-graph', 'index.db'), '');
  const stub = pathE2e.join(dir, 'cg-stub.js');
  fsE2e.writeFileSync(stub, '#!/usr/bin/env node\n' + stubBody);
  fsE2e.chmodSync(stub, 0o755);
  return { dir, stub };
}

function runHook(cmd, fixture) {
  const res = spawnHook(process.execPath, [pathE2e.join(__dirname, 'pre-grep-guide.js')], {
    cwd: fixture.dir,
    input: JSON.stringify({ tool_input: { command: cmd } }),
    encoding: 'utf8',
    env: {
      ...process.env,
      _CG_ANSWER_BINARY: fixture.stub,
      CODE_GRAPH_QUIET_HOOKS: '0',
      CODE_GRAPH_NO_BLOCK_GREP: '0',
      CODE_GRAPH_NO_ANSWER_IN_DENY: '0',
    },
  });
  return res;
}

function cleanupFixture(fixture, cmd) {
  fsE2e.rmSync(fixture.dir, { recursive: true, force: true });
  // cooldown flag for this command lives in cgTmpDir — remove so reruns stay deterministic
  try {
    fsE2e.unlinkSync(pathE2e.join(cgTmpDir(), `.code-graph-bash-${commandHash(cmd)}`));
  } catch { /* ok */ }
}

test('e2e: denied grep with stub hits → deny JSON embeds the answer + records answered:true', () => {
  const uniq = `StubHit${Date.now()}`;
  const fixture = e2eFixture(
    `process.stdout.write('src/foo.rs:7  fn ' + process.argv[3] + '()\\n');`);
  const cmd = `grep -rn "${uniq}" src/`;
  try {
    const res = runHook(cmd, fixture);
    assert.equal(res.status, 0);
    const out = JSON.parse(res.stdout);
    assert.equal(out.hookSpecificOutput.permissionDecision, 'deny');
    assert.match(out.hookSpecificOutput.permissionDecisionReason, /src\/foo\.rs:7/);
    assert.match(out.hookSpecificOutput.permissionDecisionReason, new RegExp(uniq));
    const recs = fsE2e.readFileSync(
      pathE2e.join(fixture.dir, '.code-graph', 'recommendations.jsonl'), 'utf8');
    const rec = JSON.parse(recs.trim().split('\n').pop());
    assert.equal(rec.action, 'deny');
    assert.equal(rec.answered, true);
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: stub reports no matches → grep allowed with FYI + records fallthrough', () => {
  const uniq = `StubMiss${Date.now()}`;
  const fixture = e2eFixture(
    `process.stdout.write('[code-graph] No matches for: ' + process.argv[3] + '\\n');`);
  const cmd = `grep -rn "${uniq}" src/`;
  try {
    const res = runHook(cmd, fixture);
    assert.equal(res.status, 0);
    // No deny JSON — plain FYI text means the grep proceeds
    assert.throws(() => JSON.parse(res.stdout));
    assert.match(res.stdout, /[Nn]o matches/);
    const recs = fsE2e.readFileSync(
      pathE2e.join(fixture.dir, '.code-graph', 'recommendations.jsonl'), 'utf8');
    const rec = JSON.parse(recs.trim().split('\n').pop());
    assert.equal(rec.action, 'hint');
    assert.equal(rec.fallthrough, 'no-hits');
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: stub fails → static deny (v0.46 fallback) + records answered:false', () => {
  const uniq = `StubBoom${Date.now()}`;
  const fixture = e2eFixture(`process.exit(3);`);
  const cmd = `grep -rn "${uniq}" src/`;
  try {
    const res = runHook(cmd, fixture);
    assert.equal(res.status, 0);
    const out = JSON.parse(res.stdout);
    assert.equal(out.hookSpecificOutput.permissionDecision, 'deny');
    // static reason, no embedded results
    assert.match(out.hookSpecificOutput.permissionDecisionReason, /denied by code-graph hook/);
    const rec = JSON.parse(fsE2e.readFileSync(
      pathE2e.join(fixture.dir, '.code-graph', 'recommendations.jsonl'), 'utf8').trim());
    assert.equal(rec.action, 'deny');
    assert.equal(rec.answered, false);
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: CODE_GRAPH_NO_ANSWER_IN_DENY=1 → static deny even when stub would hit', () => {
  const uniq = `StubOptout${Date.now()}`;
  const fixture = e2eFixture(`process.stdout.write('src/foo.rs:7  hit\\n');`);
  const cmd = `grep -rn "${uniq}" src/`;
  try {
    const res = spawnHook(process.execPath, [pathE2e.join(__dirname, 'pre-grep-guide.js')], {
      cwd: fixture.dir,
      input: JSON.stringify({ tool_input: { command: cmd } }),
      encoding: 'utf8',
      env: {
        ...process.env,
        _CG_ANSWER_BINARY: fixture.stub,
        CODE_GRAPH_QUIET_HOOKS: '0',
        CODE_GRAPH_NO_BLOCK_GREP: '0',
        CODE_GRAPH_NO_ANSWER_IN_DENY: '1',
      },
    });
    const out = JSON.parse(res.stdout);
    assert.equal(out.hookSpecificOutput.permissionDecision, 'deny');
    assert.doesNotMatch(out.hookSpecificOutput.permissionDecisionReason, /src\/foo\.rs:7/);
  } finally {
    cleanupFixture(fixture, cmd);
  }
});
