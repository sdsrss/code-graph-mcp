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
const {
  shouldHint,
  shouldBlock,
  classifyBlock,
  splitTopLevelSegments,
  firstShellClause,
  countNamedPaths,
  extractDeclSymbols,
  translateBreToRg,
  buildShowDenyReason,
  extractSedReadTargets,
  extractUnansweredTail,
  extractPatterns,
  extractSearchPath,
  normalizeCommandPaths,
  resolveProjectRoot,
  rebaseRelativePaths,
  commandHasBypass,
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

// ── git grep coverage (v0.71): `git grep` is raw BRE search on the tracked
//    source tree — same foldable intent as `grep`, but its command HEAD is
//    `git`, so it leaked past GREP_HEAD until v0.71. cg grep is a superset
//    (tracked AND gitignored), so folding `git grep` into it is sound. The verb
//    set is shared across GREP_HEAD / VERB_STRIP / PIPE_INTO_GREP — these lock
//    each parse site that touches the verb.

test('git grep: shouldHint fires on `git grep` against src/', () => {
  assert.equal(shouldHint('git grep -n "fts5_search" src/storage/'), true);
});

test('git grep: shouldHint fires with the `--` pathspec separator', () => {
  assert.equal(shouldHint('git grep "FooBar" -- src/lib.rs'), true);
});

test('git grep: identifier search is a deny (block tier, same as grep)', () => {
  assert.equal(shouldBlock('git grep "FooBar" src/'), true);
});

test('git grep: context flag + decl anchor → show mode', () => {
  assert.deepEqual(
    classifyBlock('git grep "fn handle_message" -A 5 src/'),
    { mode: 'show', symbols: ['handle_message'] });
});

test('git grep: multi-file named search downgrades to hint (v0.70 parity)', () => {
  // inline answer scopes to ONE path; ≥2 named files → hint so the full grep runs.
  assert.equal(classifyBlock('git grep "FooBar" src/a.rs src/b.rs'), null);
});

test('git grep: BRE alternation is translated to rust-regex dialect', () => {
  // git grep speaks BRE like plain grep → an escaped \| must unescape for cg grep.
  assert.equal(translateBreToRg('git grep "a\\|b" src/', 'a\\|b'), 'a|b');
});

test('git grep: `| git grep` is an output-filter pipe (no fire)', () => {
  assert.equal(shouldHint('grep -rn "Foo" src/ | git grep "Bar"'), false);
});

test('git grep: rebaseRelativePaths rebases the real subdir path, not the `grep` word', () => {
  // shell sits in backend/; `app` is subdir-relative → rebased. `grep` is the
  // git subcommand and is existence-gated so it never masquerades as a path.
  const exists = (p) => p.endsWith('/root/backend/app');
  const out = rebaseRelativePaths('git grep "Foo" app', 'backend', '/root', exists);
  assert.match(out, /git grep "Foo" backend\/app/);
});

// v0.71 — git grep at a scope the working-tree cg answer can't honor (staged
// index / another revision) must NOT deny: folding it would substitute
// current-tree hits for a different revision. The hook stays out entirely.
test('git grep: --cached (staged index) is not denied — cg cannot honor that scope', () => {
  assert.equal(shouldHint('git grep --cached "FooBar" src/'), false);
  assert.equal(shouldBlock('git grep --cached "FooBar" src/'), false);
});

test('git grep: a treeish ref before `--` (another revision) is not denied', () => {
  assert.equal(shouldHint('git grep "FooBar" HEAD~3 -- src/'), false);
  assert.equal(shouldBlock('git grep "cascade_failure" main -- src/'), false);
});

test('git grep: a bare `-- path` (no ref, working-tree scope) STILL denies', () => {
  // guard: the revision-scope exclusion must not over-catch a plain pathspec sep.
  assert.equal(shouldBlock('git grep "FooBar" -- src/lib.rs'), true);
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

// ── Floor (v0.69 hardening): non-foldable greps must NEVER deny/hint ──
// cg has no structural answer for these → a deny is friction-without-value that teaches
// CODE_GRAPH_NO_BLOCK_GREP bypass. 2026-06-23 reach audit: foldability (~24%) ≈
// interception (24%), so the floor (precision) is the lever — not reach expansion.

test('floor: grep on an external / non-indexed dir (/tmp clone) never fires', () => {
  assert.equal(shouldHint('grep -rn "FooBar" /tmp/openwolf-analysis'), false);
  assert.equal(shouldBlock('grep -rn "FooBar" /tmp/openwolf-analysis'), false);
});

test('floor: external path with an embedded src/ segment never fires', () => {
  // SRC_PATH only matches a prefix at ^|\s|quote — `/tmp/clone/src/` is not a project path.
  assert.equal(shouldHint('grep -rn "FooBar" /tmp/clone/src/'), false);
});

test('floor: a non-source data file (.log) under src/ never fires', () => {
  assert.equal(shouldHint('grep "ErrorHandler" src/fixtures/app.log'), false);
  assert.equal(shouldBlock('grep "ErrorHandler" src/fixtures/app.log'), false);
});

test('floor: ini/conf/xml/csv data files under src/ never fire', () => {
  assert.equal(shouldHint('grep "FooBar" src/config.ini'), false);
  assert.equal(shouldHint('grep "FooBar" src/app.conf'), false);
  assert.equal(shouldHint('grep "FooBar" src/data.xml'), false);
  assert.equal(shouldHint('grep "FooBar" src/rows.csv'), false);
});

test('floor: multiple config files under a src prefix all peel off → skip', () => {
  // global strip (v0.69): pre-fix only the first .json peeled, the 2nd false-matched SRC_PATH.
  assert.equal(shouldHint('grep "FooBar" src/a.json src/b.json'), false);
});

test('floor: mixed target (data file + real source file) STILL fires (no foldable miss)', () => {
  assert.equal(shouldHint('grep -rn "FooBar" src/app.log src/handler.rs'), true);
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

test('shouldBlock: grep -l (files-with-matches) → deny, grep answer covers file lists (v0.49)', () => {
  assert.equal(shouldBlock('grep -rl "EmbeddingModel" src/'), true);
  assert.deepEqual(classifyBlock('grep -rl "EmbeddingModel" src/'), { mode: 'grep' });
});

test('shouldBlock: --include=*.rs → deny, path-scoped grep answer covers it (v0.49)', () => {
  assert.equal(shouldBlock('grep -rn --include="*.rs" "EmbeddingModel" src/'), true);
});

test('shouldBlock: --exclude=tests → hint only (answer cannot honor exclusion)', () => {
  assert.equal(shouldBlock('grep -rn --exclude=tests "EmbeddingModel" src/'), false);
});

// ── B (v0.70): deny only when the inline answer covers the FULL scope ──
// The deny scopes to ONE path (extractSearchPath = first src-prefixed token). A grep naming
// ≥2 file paths would get a first-path-only answer (the rest silently dropped) — an incomplete
// substitute that rationally teaches CODE_GRAPH_NO_BLOCK_GREP bypass. Downgrade those to HINT;
// single file / directory greps (which the answer fully covers) still deny.

test('B: ≥2 named files downgrade deny→hint (deny would drop all but the first)', () => {
  const cmd = 'grep -n "CLAUDE_MEM_DIR" scripts/setup.sh hook-shared.mjs';
  assert.equal(shouldHint(cmd), true);     // still nudges
  assert.equal(shouldBlock(cmd), false);   // but does NOT deny (answer can't cover hook-shared.mjs)
  assert.equal(classifyBlock(cmd), null);
});

test('B: two source files also downgrade (deny would cover only the first)', () => {
  assert.equal(shouldBlock('grep -rn "set_hook" src/main.rs src/lib.rs'), false);
  assert.equal(shouldHint('grep -rn "set_hook" src/main.rs src/lib.rs'), true);
});

test('B: single file still DENIES (inline answer fully covers it)', () => {
  assert.deepEqual(classifyBlock('grep -n "handleMessage" src/server.mjs'), { mode: 'grep' });
});

test('B: single directory (recursive) still DENIES (cg grep covers the whole dir)', () => {
  assert.equal(shouldBlock('grep -rn "EmbeddingModel" src/'), true);
});

test('B: --include on a single dir still DENIES (one path, fully scoped)', () => {
  assert.equal(shouldBlock('grep -rn --include="*.rs" "EmbeddingModel" src/'), true);
});

test('countNamedPaths: counts paths, excludes flags and the quoted pattern', () => {
  assert.equal(countNamedPaths('grep -n "Foo" src/a.rs src/b.rs', ['Foo']), 2);
  assert.equal(countNamedPaths('grep -rn "Foo" src/', ['Foo']), 1);
  // a path-shaped pattern is the pattern, not a second path token
  assert.equal(countNamedPaths('grep "config.json" src/app.rs', ['config.json']), 1);
  // a path in a compound tail (sed/pipe target) is NOT a 2nd grep target → stays 1 (deny)
  assert.equal(countNamedPaths("grep -n \"Foo\" src/foo.rs | head; sed -n '1,5p' src/bar.rs", ['Foo']), 1);
});

test('shouldBlock: -L / -v inverted intents → hint only', () => {
  assert.equal(shouldBlock('grep -rL "EmbeddingModel" src/'), false);
  assert.equal(shouldBlock('grep -rnv "EmbeddingModel" src/'), false);
});

test('shouldBlock: -A 3 with bare identifier → hint only (cannot honor ±N lines)', () => {
  assert.equal(shouldBlock('grep -rn -A 3 "EmbeddingModel" src/'), false);
});

test('shouldBlock: -B 2 with bare identifier → hint only', () => {
  assert.equal(shouldBlock('grep -rn -B 2 "EmbeddingModel" src/'), false);
});

test('shouldBlock: -C 5 with bare identifier → hint only', () => {
  assert.equal(shouldBlock('grep -rn -C 5 "EmbeddingModel" src/'), false);
});

// ── translateBreToRg (v0.49) — BRE→rust-regex dialect bridge ─────────

test('translateBreToRg: plain grep BRE alternation unescaped', () => {
  assert.equal(
    translateBreToRg('grep -rn "UnifiedPickerEngine\\|engine.run" src/', 'UnifiedPickerEngine\\|engine.run'),
    'UnifiedPickerEngine|engine.run');
});

test('translateBreToRg: rg patterns untouched (already extended)', () => {
  assert.equal(translateBreToRg('rg "a\\|b" src/', 'a\\|b'), 'a\\|b');
});

test('translateBreToRg: grep -E untouched', () => {
  assert.equal(translateBreToRg('grep -rnE "a\\|b" src/', 'a\\|b'), 'a\\|b');
});

test('translateBreToRg: unescapes groups/braces/quantifiers for plain grep', () => {
  assert.equal(translateBreToRg('grep "fn \\(x\\)\\+" src/', 'fn \\(x\\)\\+'), 'fn (x)+');
});

// ── classifyBlock: show mode (v0.49) — the daagu 22/128 function-body reads ──

test('classifyBlock: declaration anchor + -A → show mode with symbols', () => {
  assert.deepEqual(
    classifyBlock('rg -n "def cascade_failure|def reset_task" -A 25 backend/app/'),
    { mode: 'show', symbols: ['cascade_failure', 'reset_task'] });
});

test('classifyBlock: multi-decl alternation caps at 3 symbols', () => {
  const c = classifyBlock('rg -n "def a_one|def b_two|class CThree|fn d_four" -A 10 src/');
  assert.equal(c.mode, 'show');
  assert.deepEqual(c.symbols, ['a_one', 'b_two', 'CThree']);
});

test('classifyBlock: declaration anchor WITHOUT context flag → plain grep deny', () => {
  assert.deepEqual(classifyBlock('grep -rn "def fetch_user" backend/app/services/'),
    { mode: 'grep' });
});

test('extractDeclSymbols: dedupes and spans fn/def/class/struct anchors', () => {
  assert.deepEqual(
    extractDeclSymbols(['fn alpha_one', 'struct BetaTwo', 'fn alpha_one']),
    ['alpha_one', 'BetaTwo']);
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

test('buildBlockReason: NEVER documents the escape hatch (v0.49 — the "THIS command only" scoping was adopted as a permanent prefix in 8s on 2026-06-12)', () => {
  assert.doesNotMatch(buildBlockReason(), /CODE_GRAPH_NO_BLOCK_GREP/);
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

// ── v0.47.1 abs-path matcher fix: normalizeCommandPaths ─────────────
// CC harness steers Bash toward ABSOLUTE paths (cd in compound commands
// triggers permission prompts), so `grep -rn "X" /abs/root/backend/...` is
// the dominant real-world shape. SRC_PATH's lookbehind (^|\s|quote) never
// matched it: daagu 2026-06-11 replay — 42/42 head-greps absolute, 1 hint /
// 0 block as-is vs 30 hint / 16 block after cwd-strip.

test('normalizeCommandPaths: strips cwd prefix from path args', () => {
  assert.equal(
    normalizeCommandPaths('grep -rn "X" /proj/root/src/storage/', '/proj/root'),
    'grep -rn "X" src/storage/');
});

test('normalizeCommandPaths: strips every occurrence', () => {
  assert.equal(
    normalizeCommandPaths('grep -rn "X" /proj/root/src/a.rs /proj/root/tests/', '/proj/root'),
    'grep -rn "X" src/a.rs tests/');
});

test('normalizeCommandPaths: strips inside quotes', () => {
  assert.equal(
    normalizeCommandPaths('grep -rn "X" "/proj/root/backend/app/"', '/proj/root'),
    'grep -rn "X" "backend/app/"');
});

test('normalizeCommandPaths: leaves foreign absolute paths alone', () => {
  assert.equal(
    normalizeCommandPaths('grep -rn "X" /other/place/src/', '/proj/root'),
    'grep -rn "X" /other/place/src/');
});

test('normalizeCommandPaths: no-op when cwd absent / falsy inputs', () => {
  assert.equal(normalizeCommandPaths('grep -rn "X" src/', '/proj/root'), 'grep -rn "X" src/');
  assert.equal(normalizeCommandPaths('', '/proj/root'), '');
  assert.equal(normalizeCommandPaths('grep "X" src/', ''), 'grep "X" src/');
});

// Real daagu transcript commands (2026-06-11 session 23f149f0…), the exact
// shape that was invisible to v0.47.0. Replay must fire post-normalization.
const DAAGU = '/mnt/data_ssd/dev/projects/daagu';

test('replay: real abs-path symbol grep → BLOCK after normalization', () => {
  const cmd = `grep -n "_parse_finish_reason\\|_last_finish_reason\\|class OpenRouterProvider" ${DAAGU}/backend/app/services/llm_engine/openrouter.py`;
  assert.equal(shouldHint(cmd), false);                       // documents the v0.47.0 blindspot
  const norm = normalizeCommandPaths(cmd, DAAGU);
  assert.equal(shouldHint(norm), true);
  assert.equal(shouldBlock(norm), true);
});

test('replay: real abs-path -rln grep → DENY after normalization (v0.49: file lists answerable)', () => {
  const cmd = `grep -rln "load_active_config_standalone" ${DAAGU}/backend/tests/ | head -5`;
  const norm = normalizeCommandPaths(cmd, DAAGU);
  assert.equal(shouldHint(norm), true);
  assert.deepEqual(classifyBlock(norm), { mode: 'grep' });    // grep answer lists files per hit
});

test('replay: abs-path config-only grep stays silent after normalization', () => {
  const cmd = `grep -n '"typecheck"\\|"type-check"\\|vue-tsc' ${DAAGU}/frontend/package.json`;
  assert.equal(shouldHint(normalizeCommandPaths(cmd, DAAGU)), false);
});

test('replay: extractSearchPath gets relative path from normalized abs command', () => {
  const cmd = `grep -rn "config_version" ${DAAGU}/backend/app/services/stock_picker/data_providers.py 2>/dev/null | head -5`;
  assert.equal(
    extractSearchPath(normalizeCommandPaths(cmd, DAAGU)),
    'backend/app/services/stock_picker/data_providers.py');
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

// ════════════════════════════════════════════════════════════════════
// v0.96 — grep-clause scoping: classification must see ONLY the grep's own
// args, never a path/flag/pattern in a non-grep compound tail. Regression for
// the 2026-07-13 mis-answer: `grep "VERSION" skills/moa/scripts/moa.py | head;
// …; python3 … scripts/bump-version.sh` denied with a cg answer for
// scripts/bump-version.sh — a file the user never grepped.
// ════════════════════════════════════════════════════════════════════

test('firstShellClause: truncates at the first top-level separator', () => {
  assert.equal(firstShellClause('grep -n "X" src/a.rs; wc scripts/b.sh'), 'grep -n "X" src/a.rs');
  assert.equal(firstShellClause('grep -n "X" src/a.rs | head'), 'grep -n "X" src/a.rs ');
  assert.equal(firstShellClause('grep -n "X" src/a.rs && cargo test'), 'grep -n "X" src/a.rs ');
});

test('firstShellClause: a separator inside quotes is literal (not a cut point)', () => {
  assert.equal(firstShellClause('grep -n "a;b|c" src/'), 'grep -n "a;b|c" src/');
  assert.equal(firstShellClause("grep -n 'a && b' src/"), "grep -n 'a && b' src/");
});

test('firstShellClause: escaped quote inside double quotes does not close (lesson #9656)', () => {
  // `\"` must not terminate the quote, so the `;` after it stays inside the string.
  assert.equal(firstShellClause('grep -n "a\\";b" src/'), 'grep -n "a\\";b" src/');
});

test('firstShellClause: no separator / empty / non-string', () => {
  assert.equal(firstShellClause('grep -n "X" src/'), 'grep -n "X" src/');
  assert.equal(firstShellClause(''), '');
  assert.equal(firstShellClause(null), null);
});

test('firstShellClause: redirects are NOT clause boundaries (grep path follows them)', () => {
  // A redirect (`>` `<`, incl. `2>&1` and process substitution) keeps the grep's
  // path args after it — truncating there would silently blind the hook.
  assert.equal(firstShellClause('grep -rn "FooBar" 2>&1 src/'), 'grep -rn "FooBar" 2>&1 src/');
  assert.equal(firstShellClause('grep -f <(cat pats) src/storage/'), 'grep -f <(cat pats) src/storage/');
  assert.equal(firstShellClause('grep -rn "X" src/ >/tmp/out'), 'grep -rn "X" src/ >/tmp/out');
});

test('firstShellClause: a single background & is not a boundary; && is', () => {
  assert.equal(firstShellClause('grep -rn "X" src/ &'), 'grep -rn "X" src/ &');
  assert.equal(firstShellClause('grep -rn "X" src/a.rs && wc src/b.rs'), 'grep -rn "X" src/a.rs ');
});

test('v0.96 REGRESSION guard: redirect-before-path grep still fires + scopes to its path', () => {
  // `grep -rn "FooBar" 2>&1 src/` — path sits AFTER the redirect. Must not go dark.
  assert.equal(shouldHint('grep -rn "FooBar" 2>&1 src/'), true);
  assert.equal(extractSearchPath('grep -rn "FooBar" 2>&1 src/'), 'src/');
  assert.deepEqual(classifyBlock('grep -rn "FooBar" 2>&1 src/'), { mode: 'grep' });
});

test('v0.96: extractUnansweredTail handles an escaped quote inside the grep pattern (lesson #9656)', () => {
  // `\"` must not close the quote, so the in-pattern `;` is literal and the real
  // `; sed …` tail (not a fragment of the pattern) is what gets flagged.
  assert.equal(
    extractUnansweredTail('grep -n "a\\";b" src/foo.rs; sed -n 1,5p src/foo.rs'),
    'sed -n 1,5p src/foo.rs');
});

test('v0.96 BUG: grep target NOT allowlisted + tail path IS → hook stays silent (no wrong answer)', () => {
  // The grep searches skills/moa/scripts/moa.py; the ONLY allowlisted path
  // (scripts/bump-version.sh) is in a non-grep tail. Pre-fix the SRC_PATH gate
  // scanned the whole command and fired, then answered the tail's file.
  const cmd = 'grep -n "VERSION\\|version" skills/moa/scripts/moa.py | head -5; echo ---; wc -l scripts/bump-version.sh';
  // With skills/ now allowlisted this specific grep IS a source search — but the
  // answer must scope to the grep's OWN file, never the tail's.
  assert.equal(extractSearchPath(cmd), 'skills/moa/scripts/moa.py');
  assert.notEqual(extractSearchPath(cmd), 'scripts/bump-version.sh');
});

test('v0.96 BUG: unrecognized grep target + tail src path → shouldHint false (grep clause has no src path)', () => {
  // docs/ is deliberately NOT allowlisted; a scripts/ path in the tail must not
  // make the hook fire on a docs/ grep.
  const cmd = 'grep -n "someThing" docs/notes.txt; wc -l scripts/build.sh';
  assert.equal(shouldHint(cmd), false);
  assert.equal(extractSearchPath(cmd), undefined);
});

test('v0.96: legit single-file grep with a compound tail scopes to the GREP file, not the tail file', () => {
  // Both paths are allowlisted src files — the answer must claim the grep's file.
  const cmd = 'grep -n "EmbeddingModel" src/a.rs; cat src/b.rs';
  assert.equal(shouldHint(cmd), true);
  assert.deepEqual(classifyBlock(cmd), { mode: 'grep' });
  assert.equal(extractSearchPath(cmd), 'src/a.rs');   // NOT src/b.rs
});

test('v0.96: a tail grep flag (-v) must not disqualify the answerable head grep', () => {
  // `-v` (invert) is UNANSWERABLE, but only in the TAIL — the head grep is answerable.
  const cmd = 'grep -n "EmbeddingModel" src/a.rs; grep -v "X" src/b.rs';
  assert.deepEqual(classifyBlock(cmd), { mode: 'grep' });
});

test('v0.96: skills/ is now an allowlisted source prefix (plugin/agent monorepos)', () => {
  assert.equal(shouldHint('grep -rn "MoaMember" skills/moa/scripts/'), true);
  assert.equal(extractSearchPath('grep -n "fn run" skills/moa/foo.py'), 'skills/moa/foo.py');
});

test('v0.96: extractPatterns ignores a quoted string in a compound tail', () => {
  // The tail's "TailPattern" is not a grep pattern → must not be screened.
  assert.deepEqual(extractPatterns('grep -n "HeadPattern" src/a.rs; echo "TailPattern"'), ['HeadPattern']);
});

// ── v0.47.0 deny-with-answer: message builders + env gate ───────────

test('buildBlockReasonWithAnswer: embeds results and command', () => {
  const reason = buildBlockReasonWithAnswer('fts5_search', 'src/storage/', {
    status: 'hits', text: 'src/storage/db.rs:42  fn fts5_search()', truncated: false,
  });
  assert.match(reason, /already ran/);
  assert.match(reason, /code-graph-mcp grep "fts5_search" src\/storage\//);
  assert.match(reason, /src\/storage\/db\.rs:42/);
  assert.doesNotMatch(reason, /truncated/);
});

test('buildBlockReasonWithAnswer: NEVER advertises the bypass (v0.48 — one deny taught a 14-grep permanent prefix)', () => {
  const reason = buildBlockReasonWithAnswer('fts5_search', 'src/storage/', {
    status: 'hits', text: 'hit', truncated: false,
  });
  assert.doesNotMatch(reason, /CODE_GRAPH_NO_BLOCK_GREP/);
});

test('buildBlockReasonWithAnswer: no salience restatement — answer is already in context (v0.63 removed)', () => {
  // A forced "name the hit you will act on" line was trialed and removed: the
  // answer is delivered, so restatement is performative friction. Keep the deny
  // copy to delivery + the plain "use directly" nudge only.
  const reason = buildBlockReasonWithAnswer('fts5_search', 'src/storage/', {
    status: 'hits', text: 'hit', truncated: false,
  });
  assert.doesNotMatch(reason, /name the hit you will act on/i);
  assert.match(reason, /use these results directly/i);
});

test('buildShowDenyReason: no salience restatement (v0.63 removed)', () => {
  const reason = buildShowDenyReason({ status: 'hits', text: 'fn body', truncated: false });
  assert.doesNotMatch(reason, /name which definition above you will change/i);
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

// ── v0.50 compound-command tail: deny answers the grep, NOT the rest ─

test('extractUnansweredTail: `; sed` tail after piped grep (2026-06-13 real deny shape)', () => {
  assert.equal(
    extractUnansweredTail(
      'grep -n "mem_update\\|registerTool" tests/server.test.mjs | head -20; sed -n \'1,60p\' tests/server.test.mjs'),
    "sed -n '1,60p' tests/server.test.mjs");
});

test('extractUnansweredTail: && tail is unanswered (would have run on grep success)', () => {
  assert.equal(
    extractUnansweredTail('grep -rn "fts5_search" src/ && wc -l src/storage/db.rs'),
    'wc -l src/storage/db.rs');
});

test('extractUnansweredTail: quoted separators are pattern text, not a tail', () => {
  assert.equal(extractUnansweredTail('grep -rn "a;b" src/'), null);
  assert.equal(extractUnansweredTail("grep -rn 'a && b' src/"), null);
});

test('extractUnansweredTail: pipes and redirects are the same pipeline, not a tail', () => {
  assert.equal(extractUnansweredTail('grep -rn "Foo" src/ 2>&1 | head -10'), null);
});

test('extractUnansweredTail: || branch would NOT have run on hits — no tail', () => {
  assert.equal(extractUnansweredTail('grep -rn "Foo" src/ || echo none'), null);
});

test('extractUnansweredTail: trailing separator with nothing after → no tail', () => {
  assert.equal(extractUnansweredTail('grep -rn "Foo" src/;'), null);
});

test('buildBlockReasonWithAnswer: compound tail → note says the rest did NOT run', () => {
  const reason = buildBlockReasonWithAnswer('fts5_search', 'src/', {
    status: 'hits', text: 'hit', truncated: false,
  }, "sed -n '1,60p' tests/server.test.mjs");
  assert.match(reason, /did NOT run/);
  assert.match(reason, /sed -n '1,60p' tests\/server\.test\.mjs/);
});

test('buildBlockReasonWithAnswer: no tail → no compound note', () => {
  const reason = buildBlockReasonWithAnswer('fts5_search', 'src/', {
    status: 'hits', text: 'hit', truncated: false,
  });
  assert.doesNotMatch(reason, /did NOT run/);
});

test('buildShowDenyReason: compound tail → note says the rest did NOT run', () => {
  const reason = buildShowDenyReason(
    { status: 'hits', text: 'fn body', truncated: false },
    'cargo test -q');
  assert.match(reason, /did NOT run/);
  assert.match(reason, /cargo test -q/);
});

test('buildShowDenyReason: no tail → no compound note', () => {
  const reason = buildShowDenyReason({ status: 'hits', text: 'fn body', truncated: false });
  assert.doesNotMatch(reason, /did NOT run/);
});

test('buildBlockReason: compound tail → static deny also flags the unanswered tail', () => {
  const reason = buildBlockReason("sed -n '1,60p' tests/server.test.mjs");
  assert.match(reason, /did NOT run/);
  assert.match(reason, /sed -n '1,60p' tests\/server\.test\.mjs/);
});

test('buildBlockReason: no tail → unchanged static deny', () => {
  const reason = buildBlockReason();
  assert.match(reason, /denied by code-graph hook/);
  assert.doesNotMatch(reason, /did NOT run/);
});

test('deny copy: compound tail flagged on the FIRST line of all three builders', () => {
  // The re-issue NOTE sits at the END of a long deny message; Claude Code's
  // transcript view truncates long tool errors, so a human reading the folded
  // view saw "answered" with no clue a tail was dropped (2026-07-18 field
  // misdiagnosis: two compound denies read as product bugs). The model sees the
  // full reason either way — the head-line marker is for the truncated view.
  const tail = "sed -n '100,150p' lib/x.mjs";
  const answered = buildBlockReasonWithAnswer('fts5_search', 'src/', {
    status: 'hits', text: 'hit', truncated: false,
  }, tail);
  assert.match(answered.split('\n')[0], /compound tail NOT run — see NOTE at end/);
  const show = buildShowDenyReason({ status: 'hits', text: 'fn body', truncated: false }, tail);
  assert.match(show.split('\n')[0], /compound tail NOT run — see NOTE at end/);
  const staticDeny = buildBlockReason(tail);
  assert.match(staticDeny.split('\n')[0], /compound tail NOT run — see NOTE at end/);
});

test('deny copy: no tail → no head-line marker in any builder', () => {
  const answered = buildBlockReasonWithAnswer('fts5_search', 'src/', {
    status: 'hits', text: 'hit', truncated: false,
  });
  assert.doesNotMatch(answered, /compound tail NOT run/);
  const show = buildShowDenyReason({ status: 'hits', text: 'fn body', truncated: false });
  assert.doesNotMatch(show, /compound tail NOT run/);
  const staticDeny = buildBlockReason();
  assert.doesNotMatch(staticDeny, /compound tail NOT run/);
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

function runHook(cmd, fixture, cwdOverride) {
  const res = spawnHook(process.execPath, [pathE2e.join(__dirname, 'pre-grep-guide.js')], {
    cwd: cwdOverride || fixture.dir,
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

// The command-hash tail of a cooldown flag. The flag's full name is
// `.code-graph-bash-<cwdHash>-<commandHash>` (pre-grep-guide.js `flagPath`), and
// this helper deliberately matches on the TAIL alone: the hook keys the flag on
// the RESOLVED project root, which need not equal the fixture path byte for
// byte, and matching the prefix would re-create the bug below.
function cooldownFlagTail(cmd) {
  return `-${commandHash(cmd)}`;
}

function cleanupFixture(fixture, cmd) {
  fsE2e.rmSync(fixture.dir, { recursive: true, force: true });
  // Cooldown flags for this command live in cgTmpDir — remove so reruns stay
  // deterministic. This deleted NOTHING from the day the flag became
  // project-scoped: it spelled the name `.code-graph-bash-<commandHash>` while
  // production writes `.code-graph-bash-<cwdHash>-<commandHash>`, and the
  // try/catch swallowed the ENOENT, so the "reruns stay deterministic" the
  // comment promised was never bought and every e2e run left a flag behind
  // (reaped only by pruneCgTmp's 24h sweep). Matching the tail is what makes
  // this independent of both the cwd and the prefix spelling; the guard test
  // `cleanupFixture actually removes the cooldown flag` keeps it honest.
  const tail = cooldownFlagTail(cmd);
  let entries;
  try { entries = fsE2e.readdirSync(cgTmpDir()); } catch { return; }
  for (const name of entries) {
    if (!name.endsWith(tail)) continue;
    try { fsE2e.unlinkSync(pathE2e.join(cgTmpDir(), name)); } catch { /* raced */ }
  }
}

test('e2e: cleanupFixture actually removes the cooldown flag the hook wrote', () => {
  // A negative control for the test harness itself. The previous cleanup
  // matched a name production had stopped writing, and because a miss is
  // indistinguishable from "already gone" through unlinkSync + catch, nothing
  // ever reported it. This asserts the flag EXISTS first, so the check cannot
  // pass vacuously against a hook that wrote no flag at all.
  const uniq = `StubClean${Date.now()}`;
  const fixture = e2eFixture(`process.stdout.write('src/foo.rs:7  hit\\n');`);
  const cmd = `grep -rn "${uniq}" src/`;
  const tail = cooldownFlagTail(cmd);
  const flags = () => fsE2e.readdirSync(cgTmpDir()).filter(f => f.endsWith(tail));
  try {
    runHook(cmd, fixture);
    assert.equal(flags().length, 1, 'the hook marks exactly one cooldown flag for this command');
  } finally {
    cleanupFixture(fixture, cmd);
  }
  assert.deepEqual(flags(), [], 'cleanupFixture leaves no cooldown flag behind');
});

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
    // An answered deny carries no failure reason — the field is reserved for
    // the not-answered fallback so 'no-binary' vs 'unavailable' stays legible.
    assert.equal(rec.reason, undefined);
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: `git grep` identifier on src/ → deny with the embedded answer', () => {
  const uniq = `GitHit${Date.now()}`;
  const fixture = e2eFixture(
    `process.stdout.write('src/foo.rs:9  fn ' + process.argv[3] + '()\\n');`);
  const cmd = `git grep -n "${uniq}" src/`;
  try {
    const res = runHook(cmd, fixture);
    assert.equal(res.status, 0);
    const out = JSON.parse(res.stdout);
    assert.equal(out.hookSpecificOutput.permissionDecision, 'deny');
    assert.match(out.hookSpecificOutput.permissionDecisionReason, new RegExp(uniq));
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: denied grep records the denied pattern (fingerprint for verbatim re-grep detection)', () => {
  // The Rust funnel (aggregate_recommendations_jsonl) scores a follow-up search
  // carrying the SAME pattern as the armed answered deny as fall-through, not a
  // sustained drill-down. That needs the pattern on the deny event.
  const uniq = `StubPat${Date.now()}`;
  const fixture = e2eFixture(`process.stdout.write('src/foo.rs:7  hit\\n');`);
  const cmd = `grep -rn "${uniq}" src/`;
  try {
    const res = runHook(cmd, fixture);
    assert.equal(res.status, 0);
    const rec = JSON.parse(fsE2e.readFileSync(
      pathE2e.join(fixture.dir, '.code-graph', 'recommendations.jsonl'), 'utf8').trim().split('\n').pop());
    assert.equal(rec.action, 'deny');
    assert.equal(rec.pattern, uniq, 'deny event carries the denied pattern as a fingerprint');
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: re-grep within cooldown → observe carries the same pattern (answer-ignored fingerprint)', () => {
  // First grep denies + marks the cooldown; the verbatim re-grep within the
  // window runs silently as an observe. It must carry the same pattern so the
  // funnel can tell "ignored the inline answer" from "drilled into something new".
  const uniq = `StubCool${Date.now()}`;
  const fixture = e2eFixture(`process.stdout.write('src/foo.rs:7  hit\\n');`);
  const cmd = `grep -rn "${uniq}" src/`;
  try {
    runHook(cmd, fixture);              // 1st → deny + markCooldown
    const res2 = runHook(cmd, fixture); // 2nd within window → observe
    assert.equal(res2.status, 0);
    const last = JSON.parse(fsE2e.readFileSync(
      pathE2e.join(fixture.dir, '.code-graph', 'recommendations.jsonl'), 'utf8').trim().split('\n').pop());
    assert.equal(last.action, 'observe');
    assert.equal(last.pattern, uniq, 'cooldown observe carries the re-grepped pattern');
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

test('e2e: stub ran-and-failed → grep ALLOWED (no static deny) + records fallthrough:unavailable', () => {
  // v0.92 — a binary that ran but failed (exit 3) can't answer, so a static deny
  // would hand the model nothing (pure friction that teaches the bypass). ALLOW
  // the raw grep instead; the funnel still distinguishes this from no-hits /
  // no-binary via the `fallthrough` field on the recorded hint event.
  const uniq = `StubBoom${Date.now()}`;
  const fixture = e2eFixture(`process.exit(3);`);
  const cmd = `grep -rn "${uniq}" src/`;
  try {
    const res = runHook(cmd, fixture);
    assert.equal(res.status, 0);
    // No deny JSON — plain FYI text means the grep proceeds.
    assert.throws(() => JSON.parse(res.stdout));
    assert.match(res.stdout, /unavailable \(ran but failed\)/);
    const rec = JSON.parse(fsE2e.readFileSync(
      pathE2e.join(fixture.dir, '.code-graph', 'recommendations.jsonl'), 'utf8').trim());
    assert.equal(rec.action, 'hint');
    // Runtime-fail must stay distinguishable from a missing-binary fallthrough.
    assert.equal(rec.fallthrough, 'unavailable');
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: ABS-path grep under fixture root → deny fires, CLI argv gets relative path', () => {
  const uniq = `StubAbs${Date.now()}`;
  const fixture = e2eFixture(
    `process.stdout.write('args=' + JSON.stringify(process.argv.slice(2)) + '\\n');`);
  // fs.realpathSync: on macOS/Linux tmpdir may be a symlink; the hook sees the
  // resolved cwd, so build the command from the same resolved form.
  const realDir = fsE2e.realpathSync(fixture.dir);
  const cmd = `grep -rn "${uniq}" ${realDir}/src/storage/`;
  try {
    const res = runHook(cmd, fixture);
    assert.equal(res.status, 0);
    const out = JSON.parse(res.stdout);
    assert.equal(out.hookSpecificOutput.permissionDecision, 'deny');
    assert.match(out.hookSpecificOutput.permissionDecisionReason,
      /args=\["grep","StubAbs\d+","src\/storage\/"\]/);
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

test('e2e: compound `grep …; sed` → deny answers grep AND flags the unanswered sed tail', () => {
  const uniq = `StubTail${Date.now()}`;
  const fixture = e2eFixture(
    `process.stdout.write('src/foo.rs:7  fn ' + process.argv[3] + '()\\n');`);
  const cmd = `grep -n "${uniq}" src/foo.rs | head -20; sed -n '100,160p' src/foo.rs`;
  try {
    fsE2e.mkdirSync(pathE2e.join(fixture.dir, 'src'), { recursive: true });
    fsE2e.writeFileSync(pathE2e.join(fixture.dir, 'src', 'foo.rs'), 'fn x() {}\n');
    const res = runHook(cmd, fixture);
    assert.equal(res.status, 0);
    const out = JSON.parse(res.stdout);
    assert.equal(out.hookSpecificOutput.permissionDecision, 'deny');
    const reason = out.hookSpecificOutput.permissionDecisionReason;
    assert.match(reason, /src\/foo\.rs:7/);            // grep half answered
    assert.match(reason, /did NOT run/);               // tail flagged honestly
    assert.match(reason, /sed -n '100,160p' src\/foo\.rs/); // verbatim re-issue line
    // funnel: the deny record marks that a tail note was carried
    const recs = fsE2e.readFileSync(
      pathE2e.join(fixture.dir, '.code-graph', 'recommendations.jsonl'), 'utf8');
    const rec = JSON.parse(recs.trim().split('\n').pop());
    assert.equal(rec.action, 'deny');
    assert.equal(rec.tail, true);
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: compound cmd + answer failure → grep ALLOWED, whole command runs intact (no half-run tail drop)', () => {
  // v0.92 — the marquee fix: when cg can't answer, a static deny used to block
  // the grep AND drop the `&& cargo test` tail, leaving the model with nothing +
  // a re-issue chore (ubuntu-sec: the `grep "def render" …; python3 …` case).
  // Now the whole compound command is ALLOWED to run intact — no deny, no tail
  // note, no half-run. Recorded as a hint/fallthrough so the funnel still sees it.
  const uniq = `StubTailBoom${Date.now()}`;
  const fixture = e2eFixture(`process.exit(3);`);
  const cmd = `grep -n "${uniq}" src/foo.rs && cargo test -q`;
  try {
    fsE2e.mkdirSync(pathE2e.join(fixture.dir, 'src'), { recursive: true });
    fsE2e.writeFileSync(pathE2e.join(fixture.dir, 'src', 'foo.rs'), 'fn x() {}\n');
    const res = runHook(cmd, fixture);
    assert.equal(res.status, 0);
    // No deny JSON — the whole compound command proceeds, tail included.
    assert.throws(() => JSON.parse(res.stdout));
    assert.match(res.stdout, /unavailable \(ran but failed\)/);
    const rec = JSON.parse(fsE2e.readFileSync(
      pathE2e.join(fixture.dir, '.code-graph', 'recommendations.jsonl'), 'utf8').trim());
    assert.equal(rec.action, 'hint');
    assert.equal(rec.fallthrough, 'unavailable');
    assert.equal(rec.tail, undefined);   // nothing dropped → no tail flag
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: simple (non-compound) denied grep → no tail field in the deny record', () => {
  const uniq = `StubNoTail${Date.now()}`;
  const fixture = e2eFixture(
    `process.stdout.write('src/foo.rs:7  fn ' + process.argv[3] + '()\\n');`);
  const cmd = `grep -rn "${uniq}" src/`;
  try {
    const res = runHook(cmd, fixture);
    const out = JSON.parse(res.stdout);
    assert.equal(out.hookSpecificOutput.permissionDecision, 'deny');
    const rec = JSON.parse(fsE2e.readFileSync(
      pathE2e.join(fixture.dir, '.code-graph', 'recommendations.jsonl'), 'utf8').trim());
    assert.equal(rec.action, 'deny');
    assert.equal('tail' in rec, false);
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: missing binary → grep ALLOWED (no static deny) records fallthrough:no-binary (distinct from no-hits & unavailable)', () => {
  // v0.92 — when the binary can't be found the hook can't answer AND denying
  // would block the user's only search tool, so it ALLOWS the raw grep. The
  // `fallthrough` field still keeps a missing-binary case distinguishable in the
  // funnel from no-hits / runtime-unavailable. We can't make findBinary() return
  // null in-repo (dev target/release is always there), so run the child with a
  // `--require` shim that forces it null — and DON'T set _CG_ANSWER_BINARY (it
  // would short-circuit before findBinary()).
  const uniq = `StubGone${Date.now()}`;
  const fixture = e2eFixture(`process.stdout.write('unused\\n');`);
  const shim = pathE2e.join(fixture.dir, 'no-binary-shim.js');
  fsE2e.writeFileSync(shim, `
const Module = require('module');
const orig = Module.prototype.require;
Module.prototype.require = function (id) {
  const m = orig.apply(this, arguments);
  if (id === './find-binary') {
    return new Proxy(m, { get(t, p) { return p === 'findBinary' ? () => null : t[p]; } });
  }
  return m;
};
`);
  const cmd = `grep -rn "${uniq}" src/`;
  try {
    const res = spawnHook(process.execPath, [pathE2e.join(__dirname, 'pre-grep-guide.js')], {
      cwd: fixture.dir,
      input: JSON.stringify({ tool_input: { command: cmd } }),
      encoding: 'utf8',
      env: {
        ...process.env,
        // _CG_ANSWER_BINARY intentionally UNSET so the shimmed findBinary() runs.
        _CG_ANSWER_BINARY: '',
        NODE_OPTIONS: `--require ${shim}`,
        CODE_GRAPH_QUIET_HOOKS: '0',
        CODE_GRAPH_NO_BLOCK_GREP: '0',
        CODE_GRAPH_NO_ANSWER_IN_DENY: '0',
      },
    });
    assert.equal(res.status, 0);
    // No deny JSON — the raw grep proceeds; FYI names the missing-binary cause.
    assert.throws(() => JSON.parse(res.stdout));
    assert.match(res.stdout, /unavailable \(binary not found\)/);
    const rec = JSON.parse(fsE2e.readFileSync(
      pathE2e.join(fixture.dir, '.code-graph', 'recommendations.jsonl'), 'utf8').trim());
    assert.equal(rec.action, 'hint');
    assert.equal(rec.fallthrough, 'no-binary',
      'a missing-binary fallthrough must be distinguishable from an unavailable (runtime-fail) one');
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

// ── v0.48 subdir-cwd dark fix: resolveProjectRoot / rebaseRelativePaths ──
// daagu 2026-06-11: the persistent shell `cd backend/` darkened 38/40
// head-greps for the rest of the night — gate 5 checked process.cwd() only.

const { sanitizeSearchPath } = require('./cg-answer');

test('resolveProjectRoot: index at start dir', () => {
  const base = fsE2e.mkdtempSync(pathE2e.join(osE2e.tmpdir(), 'cg-root-'));
  try {
    fsE2e.mkdirSync(pathE2e.join(base, 'proj', '.code-graph'), { recursive: true });
    fsE2e.writeFileSync(pathE2e.join(base, 'proj', '.code-graph', 'index.db'), '');
    assert.equal(
      resolveProjectRoot(pathE2e.join(base, 'proj'), { home: base }),
      pathE2e.join(base, 'proj'));
  } finally { fsE2e.rmSync(base, { recursive: true, force: true }); }
});

test('resolveProjectRoot: walks up from nested subdir to the indexed root', () => {
  const base = fsE2e.mkdtempSync(pathE2e.join(osE2e.tmpdir(), 'cg-root-'));
  try {
    const proj = pathE2e.join(base, 'proj');
    fsE2e.mkdirSync(pathE2e.join(proj, '.code-graph'), { recursive: true });
    fsE2e.writeFileSync(pathE2e.join(proj, '.code-graph', 'index.db'), '');
    const deep = pathE2e.join(proj, 'backend', 'app', 'services');
    fsE2e.mkdirSync(deep, { recursive: true });
    assert.equal(resolveProjectRoot(deep, { home: base }), proj);
  } finally { fsE2e.rmSync(base, { recursive: true, force: true }); }
});

test('resolveProjectRoot: no index up to $HOME → null (home\'s own index never adopted)', () => {
  const base = fsE2e.mkdtempSync(pathE2e.join(osE2e.tmpdir(), 'cg-root-'));
  try {
    const deep = pathE2e.join(base, 'somewhere', 'deep');
    fsE2e.mkdirSync(deep, { recursive: true });
    assert.equal(resolveProjectRoot(deep, { home: base }), null);
    // A stray `~/.code-graph` (home dir indexed once by accident) must NOT leak
    // into every un-indexed dir under home — the stray-detection walk already
    // treats an index at home as an unrelated outer project; the ancestor walk
    // must agree. Resolving from home ITSELF still honors its index (own-index
    // rule), tested below.
    fsE2e.mkdirSync(pathE2e.join(base, '.code-graph'), { recursive: true });
    fsE2e.writeFileSync(pathE2e.join(base, '.code-graph', 'index.db'), '');
    assert.equal(resolveProjectRoot(deep, { home: base }), null);
    assert.equal(resolveProjectRoot(base, { home: base }), base);
  } finally { fsE2e.rmSync(base, { recursive: true, force: true }); }
});

test('resolveProjectRoot: skips a STRAY nested subdir index, prefers the .git root', () => {
  // monorepo (daagu shape): root has .git + index; a subdir carries a stray
  // index relic but no .git. Resolving from the subdir must climb to the root,
  // not pin the stray nested index (the statusline "oscillation" root cause).
  const base = fsE2e.mkdtempSync(pathE2e.join(osE2e.tmpdir(), 'cg-root-'));
  try {
    const proj = pathE2e.join(base, 'proj');
    fsE2e.mkdirSync(pathE2e.join(proj, '.git'), { recursive: true });
    fsE2e.mkdirSync(pathE2e.join(proj, '.code-graph'), { recursive: true });
    fsE2e.writeFileSync(pathE2e.join(proj, '.code-graph', 'index.db'), '');
    const sub = pathE2e.join(proj, 'backend');
    fsE2e.mkdirSync(pathE2e.join(sub, '.code-graph'), { recursive: true });
    fsE2e.writeFileSync(pathE2e.join(sub, '.code-graph', 'index.db'), '');
    assert.equal(resolveProjectRoot(sub, { home: base }), proj);
  } finally { fsE2e.rmSync(base, { recursive: true, force: true }); }
});

test('resolveProjectRoot: a nested index with its OWN .git (submodule) still wins', () => {
  const base = fsE2e.mkdtempSync(pathE2e.join(osE2e.tmpdir(), 'cg-root-'));
  try {
    const proj = pathE2e.join(base, 'proj');
    fsE2e.mkdirSync(pathE2e.join(proj, '.git'), { recursive: true });
    fsE2e.mkdirSync(pathE2e.join(proj, '.code-graph'), { recursive: true });
    fsE2e.writeFileSync(pathE2e.join(proj, '.code-graph', 'index.db'), '');
    const sub = pathE2e.join(proj, 'vendored');
    fsE2e.mkdirSync(pathE2e.join(sub, '.git'), { recursive: true });
    fsE2e.mkdirSync(pathE2e.join(sub, '.code-graph'), { recursive: true });
    fsE2e.writeFileSync(pathE2e.join(sub, '.code-graph', 'index.db'), '');
    assert.equal(resolveProjectRoot(sub, { home: base }), sub);
  } finally { fsE2e.rmSync(base, { recursive: true, force: true }); }
});

test('resolveProjectRoot: start with its OWN .git but no index → null (boundary, no escape)', () => {
  const base = fsE2e.mkdtempSync(pathE2e.join(osE2e.tmpdir(), 'cg-root-'));
  try {
    const proj = pathE2e.join(base, 'proj'); // indexed parent
    fsE2e.mkdirSync(pathE2e.join(proj, '.code-graph'), { recursive: true });
    fsE2e.writeFileSync(pathE2e.join(proj, '.code-graph', 'index.db'), '');
    const sub = pathE2e.join(proj, 'sub'); // own .git, no index
    fsE2e.mkdirSync(pathE2e.join(sub, '.git'), { recursive: true });
    assert.equal(resolveProjectRoot(sub, { home: base }), null);
  } finally { fsE2e.rmSync(base, { recursive: true, force: true }); }
});

test('resolveProjectRoot: non-git monorepo — stray subdir index resolves to indexed ancestor', () => {
  const base = fsE2e.mkdtempSync(pathE2e.join(osE2e.tmpdir(), 'cg-root-'));
  try {
    const root = pathE2e.join(base, 'mono'); // indexed, NO .git
    fsE2e.mkdirSync(pathE2e.join(root, '.code-graph'), { recursive: true });
    fsE2e.writeFileSync(pathE2e.join(root, '.code-graph', 'index.db'), '');
    const sub = pathE2e.join(root, 'backend'); // stray index, no .git
    fsE2e.mkdirSync(pathE2e.join(sub, '.code-graph'), { recursive: true });
    fsE2e.writeFileSync(pathE2e.join(sub, '.code-graph', 'index.db'), '');
    assert.equal(resolveProjectRoot(sub, { home: base }), root);
  } finally { fsE2e.rmSync(base, { recursive: true, force: true }); }
});

// ── linked git worktrees (`.git` FILE with gitdir: …/.git/worktrees/<name>) ──
// Claude Code's EnterWorktree puts branch checkouts under
// <main>/.claude/worktrees/<slug>; the worktree has no index of its own, so the
// pre-fix hard `.git` boundary left the statusline + every hook dark there
// (ubuntu-sec feat/m11-env-precheck, 2026-07-18) while SUBDIRS of the worktree
// inconsistently escaped to the main checkout via the ancestor walk.

function mkWorktree(base, mainName, wtName) {
  // main checkout with .git dir + index; linked worktree with .git FILE
  const main = pathE2e.join(base, mainName);
  fsE2e.mkdirSync(pathE2e.join(main, '.git', 'worktrees', wtName), { recursive: true });
  fsE2e.mkdirSync(pathE2e.join(main, '.code-graph'), { recursive: true });
  fsE2e.writeFileSync(pathE2e.join(main, '.code-graph', 'index.db'), '');
  const wt = pathE2e.join(base, 'wt', wtName);
  fsE2e.mkdirSync(wt, { recursive: true });
  fsE2e.writeFileSync(pathE2e.join(wt, '.git'),
    `gitdir: ${pathE2e.join(main, '.git', 'worktrees', wtName)}\n`);
  return { main, wt };
}

test('resolveProjectRoot: worktree root resolves to the main checkout index', () => {
  const base = fsE2e.mkdtempSync(pathE2e.join(osE2e.tmpdir(), 'cg-root-'));
  try {
    const { main, wt } = mkWorktree(base, 'proj', 'feat-x');
    assert.equal(resolveProjectRoot(wt, { home: base }), main);
  } finally { fsE2e.rmSync(base, { recursive: true, force: true }); }
});

test('resolveProjectRoot: worktree SUBDIR resolves to the main checkout (consistent with root)', () => {
  const base = fsE2e.mkdtempSync(pathE2e.join(osE2e.tmpdir(), 'cg-root-'));
  try {
    const { main, wt } = mkWorktree(base, 'proj', 'feat-x');
    const sub = pathE2e.join(wt, 'src', 'deep');
    fsE2e.mkdirSync(sub, { recursive: true });
    assert.equal(resolveProjectRoot(sub, { home: base }), main);
  } finally { fsE2e.rmSync(base, { recursive: true, force: true }); }
});

test('resolveProjectRoot: worktree with its OWN index wins over the main checkout', () => {
  const base = fsE2e.mkdtempSync(pathE2e.join(osE2e.tmpdir(), 'cg-root-'));
  try {
    const { wt } = mkWorktree(base, 'proj', 'feat-x');
    fsE2e.mkdirSync(pathE2e.join(wt, '.code-graph'), { recursive: true });
    fsE2e.writeFileSync(pathE2e.join(wt, '.code-graph', 'index.db'), '');
    assert.equal(resolveProjectRoot(wt, { home: base }), wt);
  } finally { fsE2e.rmSync(base, { recursive: true, force: true }); }
});

test('resolveProjectRoot: worktree of an UNINDEXED main checkout → null', () => {
  const base = fsE2e.mkdtempSync(pathE2e.join(osE2e.tmpdir(), 'cg-root-'));
  try {
    const { main, wt } = mkWorktree(base, 'proj', 'feat-x');
    fsE2e.rmSync(pathE2e.join(main, '.code-graph'), { recursive: true });
    assert.equal(resolveProjectRoot(wt, { home: base }), null);
  } finally { fsE2e.rmSync(base, { recursive: true, force: true }); }
});

test('resolveProjectRoot: submodule `.git` FILE (gitdir: …/.git/modules/…) stays a hard boundary', () => {
  const base = fsE2e.mkdtempSync(pathE2e.join(osE2e.tmpdir(), 'cg-root-'));
  try {
    const proj = pathE2e.join(base, 'proj'); // indexed parent repo
    fsE2e.mkdirSync(pathE2e.join(proj, '.git', 'modules', 'lib'), { recursive: true });
    fsE2e.mkdirSync(pathE2e.join(proj, '.code-graph'), { recursive: true });
    fsE2e.writeFileSync(pathE2e.join(proj, '.code-graph', 'index.db'), '');
    const sub = pathE2e.join(proj, 'lib'); // submodule checkout, no index
    fsE2e.mkdirSync(sub, { recursive: true });
    fsE2e.writeFileSync(pathE2e.join(sub, '.git'),
      `gitdir: ${pathE2e.join(proj, '.git', 'modules', 'lib')}\n`);
    assert.equal(resolveProjectRoot(sub, { home: base }), null);
  } finally { fsE2e.rmSync(base, { recursive: true, force: true }); }
});

test('resolveProjectRoot: SUBDIR of an unindexed nested repo never escapes to the outer index', () => {
  // Sibling of the "start with its OWN .git but no index → null" boundary rule:
  // the ancestor walk must stop AT the nested repo's .git, not sail through it
  // into the outer project's index (pre-fix it did, making root-vs-subdir
  // behavior contradictory inside the same nested repo).
  const base = fsE2e.mkdtempSync(pathE2e.join(osE2e.tmpdir(), 'cg-root-'));
  try {
    const proj = pathE2e.join(base, 'proj'); // indexed outer
    fsE2e.mkdirSync(pathE2e.join(proj, '.code-graph'), { recursive: true });
    fsE2e.writeFileSync(pathE2e.join(proj, '.code-graph', 'index.db'), '');
    const inner = pathE2e.join(proj, 'vendored'); // own .git, no index
    fsE2e.mkdirSync(pathE2e.join(inner, '.git'), { recursive: true });
    const deep = pathE2e.join(inner, 'src');
    fsE2e.mkdirSync(deep, { recursive: true });
    assert.equal(resolveProjectRoot(deep, { home: base }), null);
  } finally { fsE2e.rmSync(base, { recursive: true, force: true }); }
});

test('resolveProjectRoot: indexed sub-project inside an unindexed repo still resolves from below', () => {
  // Guard for the legit shape the boundary fix must NOT break: repo root has
  // .git but was never indexed; only packages/foo was. Resolving from
  // packages/foo/src must find packages/foo (nearest indexed ancestor INSIDE
  // the boundary), not bail at the unindexed .git root.
  const base = fsE2e.mkdtempSync(pathE2e.join(osE2e.tmpdir(), 'cg-root-'));
  try {
    const repo = pathE2e.join(base, 'repo');
    fsE2e.mkdirSync(pathE2e.join(repo, '.git'), { recursive: true });
    const pkg = pathE2e.join(repo, 'packages', 'foo');
    fsE2e.mkdirSync(pathE2e.join(pkg, '.code-graph'), { recursive: true });
    fsE2e.writeFileSync(pathE2e.join(pkg, '.code-graph', 'index.db'), '');
    const deep = pathE2e.join(pkg, 'src');
    fsE2e.mkdirSync(deep, { recursive: true });
    assert.equal(resolveProjectRoot(deep, { home: base }), pkg);
  } finally { fsE2e.rmSync(base, { recursive: true, force: true }); }
});

test('rebaseRelativePaths: daagu shape — bare `app` from backend/ cwd', () => {
  const exists = (p) => p.endsWith(pathE2e.join('backend', 'app'));
  const cmd = 'grep -rn "rr_source\\|max_retries" app --include=*.py';
  const rebased = rebaseRelativePaths(cmd, 'backend', '/proj', exists);
  assert.equal(rebased, 'grep -rn "rr_source\\|max_retries" backend/app --include=*.py');
  assert.equal(shouldHint(rebased), true);
  assert.equal(extractSearchPath(rebased), 'backend/app');
});

test('rebaseRelativePaths: deep relPrefix, multiple file args', () => {
  const exists = (p) => p.endsWith('.py');
  const rel = 'backend/app/services/scheduler/tasks';
  const cmd = 'grep -n "except Exception" asr_preload.py xuanlun_pro_scan.py';
  const rebased = rebaseRelativePaths(cmd, rel, '/proj', exists);
  assert.match(rebased, /backend\/app\/services\/scheduler\/tasks\/asr_preload\.py/);
  assert.match(rebased, /backend\/app\/services\/scheduler\/tasks\/xuanlun_pro_scan\.py/);
  assert.equal(shouldHint(rebased), true);
});

test('rebaseRelativePaths: quoted patterns are never rebased even if a same-named path exists', () => {
  const exists = () => true; // adversarial: everything "exists"
  const cmd = 'grep -rn "retry" app';
  const rebased = rebaseRelativePaths(cmd, 'backend', '/proj', exists);
  assert.equal(rebased, 'grep -rn "retry" backend/app');
});

test('rebaseRelativePaths: flags, absolute, traversal, operators untouched', () => {
  const exists = () => true;
  const cmd = 'grep -rn "X" /etc/hosts ../up --include=*.py 2>/dev/null';
  assert.equal(rebaseRelativePaths(cmd, 'backend', '/proj', exists), cmd);
});

test('rebaseRelativePaths: non-source relPrefix (docs/) → unchanged', () => {
  const exists = () => true;
  const cmd = 'grep -rn "X" app';
  assert.equal(rebaseRelativePaths(cmd, 'docs', '/proj', exists), cmd);
});

test('rebaseRelativePaths: unquoted pattern word does not exist → untouched', () => {
  const exists = (p) => p.endsWith('/backend/app');
  const cmd = 'grep -rn retry_count app';
  const rebased = rebaseRelativePaths(cmd, 'backend', '/proj', exists);
  assert.equal(rebased, 'grep -rn retry_count backend/app');
});

// ── v0.48 bypass visibility: GREP_HEAD bare-prefix + commandHasBypass ──

test('shouldHint: bare KEY=VALUE prefixed grep now matches GREP_HEAD', () => {
  assert.equal(shouldHint('CODE_GRAPH_NO_BLOCK_GREP=1 grep -rn "fts5_search" src/'), true);
});

test('extractPatterns: bare KEY=VALUE prefix stripped with the verb', () => {
  assert.deepEqual(
    extractPatterns('CODE_GRAPH_NO_BLOCK_GREP=1 grep -rn "split_identifier" src/'),
    ['split_identifier']);
});

test('commandHasBypass: =1 prefix detected, other values / absence are not', () => {
  assert.equal(commandHasBypass('CODE_GRAPH_NO_BLOCK_GREP=1 grep -rn "X" src/'), true);
  assert.equal(commandHasBypass('FOO=1 CODE_GRAPH_NO_BLOCK_GREP=1 grep "X" src/'), true);
  assert.equal(commandHasBypass('CODE_GRAPH_NO_BLOCK_GREP=0 grep -rn "X" src/'), false);
  assert.equal(commandHasBypass('grep -rn "CODE_GRAPH_NO_BLOCK_GREP=1" src/'), false);
  assert.equal(commandHasBypass('grep -rn "X" src/'), false);
});

// ── v0.48 replay: the exact command behind the night's only deny ──
// (answered:false — glob path reached rg literally and exited 1)

test('replay: daagu denied glob command → block + sanitized search path', () => {
  const cmd = 'grep -rn "async def chat\\|def chat\\|retry\\|rate.limit\\|rate-limit\\|RateLimit\\|429\\|max_retries\\|backoff\\|fallback_model\\|temporarily" backend/app/services/llm_engine/*.py | head -40';
  assert.equal(shouldHint(cmd), true);
  assert.equal(shouldBlock(cmd), true);
  const raw = extractSearchPath(cmd);
  assert.equal(raw, 'backend/app/services/llm_engine/*.py');
  assert.equal(sanitizeSearchPath(raw), 'backend/app/services/llm_engine');
});

// ── v0.48 e2e: hook process spawned exactly as CC does ──

test('e2e: subdir cwd — hook resolves root, rebases path, records at root', () => {
  const uniq = `sub_dir_fix_${Date.now()}`;
  const fixture = e2eFixture(
    `process.stdout.write('backend/app/x.py:1  fn hit()\\n');`);
  const cmd = `grep -rn "${uniq}\\|max_retries" app`;
  try {
    fsE2e.mkdirSync(pathE2e.join(fixture.dir, 'backend', 'app'), { recursive: true });
    const res = runHook(cmd, fixture, pathE2e.join(fixture.dir, 'backend'));
    const out = JSON.parse(res.stdout);
    assert.equal(out.hookSpecificOutput.permissionDecision, 'deny');
    const recs = fsE2e.readFileSync(
      pathE2e.join(fixture.dir, '.code-graph', 'recommendations.jsonl'), 'utf8');
    assert.match(recs, /"action":"deny"/);
    // never creates .code-graph in the subdir
    assert.equal(fsE2e.existsSync(pathE2e.join(fixture.dir, 'backend', '.code-graph')), false);
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: bypassed grep is silent but recorded as action:bypass', () => {
  const uniq = `bypass_vis_${Date.now()}`;
  const fixture = e2eFixture(`process.stdout.write('never called\\n');`);
  const cmd = `CODE_GRAPH_NO_BLOCK_GREP=1 grep -rn "${uniq}\\|fts5_search" src/`;
  try {
    fsE2e.mkdirSync(pathE2e.join(fixture.dir, 'src'), { recursive: true });
    const res = runHook(cmd, fixture);
    assert.equal(res.stdout, '');
    const recs = fsE2e.readFileSync(
      pathE2e.join(fixture.dir, '.code-graph', 'recommendations.jsonl'), 'utf8');
    assert.match(recs, /"action":"bypass"/);
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('e2e: glob path arg → answer runs against the glob-truncated dir', () => {
  const uniq = `GlobTrunc${Date.now()}`;
  const fixture = e2eFixture(
    `process.stdout.write('args=' + JSON.stringify(process.argv.slice(2)) + '\\n');`);
  const cmd = `grep -rn "${uniq}" src/storage/*.rs`;
  try {
    fsE2e.mkdirSync(pathE2e.join(fixture.dir, 'src', 'storage'), { recursive: true });
    const res = runHook(cmd, fixture);
    const out = JSON.parse(res.stdout);
    assert.equal(out.hookSpecificOutput.permissionDecision, 'deny');
    assert.match(out.hookSpecificOutput.permissionDecisionReason,
      new RegExp(`args=\\["grep","${uniq}","src/storage"\\]`));
  } finally {
    cleanupFixture(fixture, cmd);
  }
});

test('rebaseRelativePaths: glob token rebases when its glob-truncated dir exists', () => {
  // daagu shape: shell in backend/, command scopes a glob under it. Without
  // the truncated probe the token stayed subdir-relative while the answer ran
  // from the root → rg ENOENT → answered:false (the original night bug).
  const exists = (p) => p.endsWith(pathE2e.join('backend', 'app', 'services', 'llm_engine'));
  const cmd = 'grep -rn "def chat\\|max_retries" app/services/llm_engine/*.py';
  const rebased = rebaseRelativePaths(cmd, 'backend', '/proj', exists);
  assert.equal(
    extractSearchPath(rebased), 'backend/app/services/llm_engine/*.py');
  assert.equal(
    sanitizeSearchPath(extractSearchPath(rebased)), 'backend/app/services/llm_engine');
});

// ── extractSedReadTargets (v0.49) — sed-range reads feed read-fanout ──

test('extractSedReadTargets: plain and quoted ranges, abs and rel paths', () => {
  assert.deepEqual(
    extractSedReadTargets('sed -n 620,700p /abs/proj/backend/app/services/market.py'),
    ['/abs/proj/backend/app/services/market.py']);
  assert.deepEqual(
    extractSedReadTargets("sed -n '230,310p' backend/app/services/tushare.py"),
    ['backend/app/services/tushare.py']);
});

test('extractSedReadTargets: multiple segments in one command, deduped', () => {
  const cmd = 'sed -n 60,200p src/a.py; echo ===; sed -n 250,300p src/b.py && sed -n 250,300p src/b.py';
  assert.deepEqual(extractSedReadTargets(cmd), ['src/a.py', 'src/b.py']);
});

test('extractSedReadTargets: non-range sed (substitution) ignored', () => {
  assert.deepEqual(extractSedReadTargets("sed -i 's/a/b/' src/a.py"), []);
  assert.deepEqual(extractSedReadTargets('sed -n /pattern/p src/a.py'), []);
});

test('extractSedReadTargets: pipeline sed after grep still extracted', () => {
  assert.deepEqual(
    extractSedReadTargets('grep -n "x" src/a.py | sed -n 1,5p src/b.py'),
    ['src/b.py']);
});

// ── splitTopLevelSegments (compound-grep PostToolUse §1) ─────────────
// Quote-aware top-level splitter shared by post-grep-inject. Splits on &&, ||,
// ;, newline, and for…in / do / done boundaries — NOT on a single `|` (so a
// pipe-into-grep keeps head=cargo and is recognized as an output filter).

test('splitTopLevelSegments: && joins two commands → two segments', () => {
  assert.deepEqual(
    splitTopLevelSegments('echo "x" && grep Sym tests/'),
    ['echo "x"', 'grep Sym tests/']);
});

test('splitTopLevelSegments: ; and || are top-level separators', () => {
  assert.deepEqual(
    splitTopLevelSegments('git diff; grep Sym src/ || echo none'),
    ['git diff', 'grep Sym src/', 'echo none']);
});

test('splitTopLevelSegments: a single pipe is NOT a separator (output filter)', () => {
  // cargo test | grep X must keep head=cargo so it reads as an output filter,
  // NOT a foldable grep segment.
  assert.deepEqual(
    splitTopLevelSegments('cargo test | grep FAIL'),
    ['cargo test | grep FAIL']);
});

test('splitTopLevelSegments: for … in / do / done are segment boundaries', () => {
  const segs = splitTopLevelSegments('for s in a b; do grep "$s" src/; done');
  // the grep body is isolated as its own segment
  assert.ok(segs.some(seg => /grep "\$s" src\//.test(seg)),
    `grep body not isolated: ${JSON.stringify(segs)}`);
  // the for-header / do / done keywords are not glued onto the grep
  assert.ok(!segs.some(seg => /for s in/.test(seg) && /grep/.test(seg)),
    `for-header glued to grep: ${JSON.stringify(segs)}`);
});

test('splitTopLevelSegments: separators inside quotes are literal, not splits', () => {
  assert.deepEqual(
    splitTopLevelSegments('grep "a && b; c" src/'),
    ['grep "a && b; c" src/']);
  assert.deepEqual(
    splitTopLevelSegments("grep 'x || y' src/"),
    ["grep 'x || y' src/"]);
});

test('splitTopLevelSegments: backslash-escaped quote inside double quotes does NOT close (no phantom segment)', () => {
  // One literal echo arg — the \" must not close the quote, so && stays inside
  // the string and no foldable `grep` segment is split out. (review L1)
  assert.deepEqual(
    splitTopLevelSegments('echo "x\\" && grep \\"Y\\" src/ rest"'),
    ['echo "x\\" && grep \\"Y\\" src/ rest"']);
  // Single quotes do NOT process backslashes (POSIX): a real separator after a
  // closed single-quoted string still splits.
  assert.deepEqual(
    splitTopLevelSegments("echo 'a\\' && grep Sym src/"),
    ["echo 'a\\'", 'grep Sym src/']);
});

test('splitTopLevelSegments: newline is a separator', () => {
  assert.deepEqual(
    splitTopLevelSegments('echo hi\ngrep Sym src/'),
    ['echo hi', 'grep Sym src/']);
});

test('splitTopLevelSegments: empty / non-string → empty array', () => {
  assert.deepEqual(splitTopLevelSegments(''), []);
  assert.deepEqual(splitTopLevelSegments(null), []);
  assert.deepEqual(splitTopLevelSegments(undefined), []);
});

test('splitTopLevelSegments: trims and drops empty segments', () => {
  assert.deepEqual(
    splitTopLevelSegments('  echo a  ;;  grep Sym src/  '),
    ['echo a', 'grep Sym src/']);
});

// The dark-hint fallthrough (action:'hint' + stdout buildHint) was DELETED in
// the compound-grep change: a grep that passes shouldHint but not classifyBlock
// now exits silently from PreToolUse (PostToolUse handles only classifyBlock
// non-null cases). buildHint stays exported (referenced above) but is never
// emitted by the runMain hint tier.
test('source-text: PreToolUse no longer emits the dark stdout hint fallthrough', () => {
  const fs = require('node:fs');
  const path = require('node:path');
  const src = fs.readFileSync(path.join(__dirname, 'pre-grep-guide.js'), 'utf8');
  assert.doesNotMatch(src, /process\.stdout\.write\(buildHint\(\)/,
    'the dark hint stdout emission must be removed (PreToolUse exit-0 stdout is debug-log-only)');
  assert.doesNotMatch(src, /action:\s*'hint'\s*\}\);\s*\n\s*process\.stdout\.write\(buildHint/,
    'the hint-tier recordRecommendation + buildHint pair must be removed');
});
