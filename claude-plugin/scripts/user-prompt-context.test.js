'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const fs = require('node:fs');

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
  shouldSkip,
  extractFilePaths,
  extractSymbols,
  detectIntents,
  scoreIntent,
  INTENT_PATTERNS,
  INTENT_THRESHOLD,
  determineQueryType,
  computeQuietHooks,
  buildRunEnv,
} = require('./user-prompt-context');

// ── shouldSkip ──────────────────────────────────────────

test('shouldSkip: simple confirmations (EN)', () => {
  for (const msg of ['yes', 'no', 'ok', 'done', 'y', 'n', 'commit', 'push', 'thanks']) {
    assert.ok(shouldSkip(msg), `should skip "${msg}"`);
  }
});

test('shouldSkip: simple confirmations (ZH)', () => {
  for (const msg of ['继续', '确认', '好的', '好', '是的', '不', '可以', '行', '对', '提交', '推送', '没问题', '谢谢', '发布', '更新', '清理']) {
    assert.ok(shouldSkip(msg), `should skip "${msg}"`);
  }
});

test('shouldSkip: with trailing punctuation', () => {
  assert.ok(shouldSkip('好的。'));
  assert.ok(shouldSkip('ok!'));
  assert.ok(shouldSkip('确认？'));
});

test('shouldSkip: action-only without code entities', () => {
  assert.equal(shouldSkip('修复这些问题'), 'action-only');
  assert.equal(shouldSkip('按优先级实施'), 'action-only');
  assert.equal(shouldSkip('执行这个方案'), 'action-only');
  assert.equal(shouldSkip('开始吧'), 'action-only');
});

test('shouldSkip: action with 3+ Latin chars passes through', () => {
  assert.equal(shouldSkip('修复 parse_code 里的bug'), false);
  assert.equal(shouldSkip('修复这段逻辑的bug'), false); // "bug" = 3 chars
  assert.equal(shouldSkip('修复 API 的问题'), false);    // "API" = 3 chars
});

test('shouldSkip: NOT skip legitimate code tasks', () => {
  assert.equal(shouldSkip('帮我写一个工具函数'), false);
  assert.equal(shouldSkip('帮我优化一下这个查询'), false);
  assert.equal(shouldSkip('优化 parse_code 的性能'), false);
  assert.equal(shouldSkip('看看 src/mcp/ 模块的代码结构'), false);
  assert.equal(shouldSkip('重构一下这个模块'), false);
});

test('shouldSkip: messages below length threshold exit early in main', () => {
  // The 8-char minimum is checked in the main block, not in shouldSkip
  // shouldSkip itself doesn't enforce length
  assert.equal(shouldSkip('短消息很短'), false); // passes shouldSkip but would exit in main
});

// ── extractFilePaths ────────────────────────────────────

test('extractFilePaths: extracts src/ paths', () => {
  assert.deepEqual(extractFilePaths('看看 src/mcp/server.rs'), ['src/mcp/server.rs']);
  assert.deepEqual(extractFilePaths('修改 src/parser/relations.rs 和 src/storage/db.rs'), ['src/parser/relations.rs', 'src/storage/db.rs']);
});

test('extractFilePaths: extracts lib/test/pkg paths', () => {
  assert.deepEqual(extractFilePaths('check lib/utils/helpers.js'), ['lib/utils/helpers.js']);
  assert.deepEqual(extractFilePaths('test/integration.rs is failing'), ['test/integration.rs']);
});

test('extractFilePaths: limits to 2 paths', () => {
  const result = extractFilePaths('src/a.rs src/b.rs src/c.rs');
  assert.equal(result.length, 2);
});

test('extractFilePaths: no match for non-code paths', () => {
  assert.deepEqual(extractFilePaths('这个函数有问题'), []);
  assert.deepEqual(extractFilePaths('update the readme'), []);
});

// ── extractSymbols ──────────────────────────────────────

test('extractSymbols: snake_case', () => {
  const r = extractSymbols('修改 parse_code 函数');
  assert.deepEqual(r.symbols, ['parse_code']);
  assert.equal(r.lowConfidence, false);
});

test('extractSymbols: camelCase', () => {
  const r = extractSymbols('fix the handleMessage function');
  assert.ok(r.symbols.includes('handleMessage'));
  assert.equal(r.lowConfidence, false);
});

test('extractSymbols: PascalCase compound', () => {
  const r = extractSymbols('implement McpServer class');
  assert.ok(r.symbols.includes('McpServer'));
});

test('extractSymbols: qualified names (Foo::bar)', () => {
  const r = extractSymbols('check Foo::bar::baz');
  assert.ok(r.symbols.some(s => s.includes('::')));
});

test('extractSymbols: backtick-quoted fallback', () => {
  const r = extractSymbols('修改 `parse` 函数');
  assert.ok(r.symbols.includes('parse'));
});

test('extractSymbols: backtick with longer name', () => {
  const r = extractSymbols('看看 `fts5_search` 怎么实现的');
  assert.ok(r.symbols.includes('fts5_search'));
});

test('extractSymbols: plain word fallback (low confidence)', () => {
  const r = extractSymbols('write tests for the embedding module');
  assert.ok(r.symbols.includes('embedding'));
  assert.equal(r.lowConfidence, true);
});

test('extractSymbols: plain words excluded (common English verbs)', () => {
  const r = extractSymbols('help me understand the refactor approach');
  // "understand" and "refactor" are excluded, "approach" is excluded
  assert.equal(r.symbols.length, 0);
});

test('extractSymbols: stop words filtered', () => {
  const r = extractSymbols('fix the default function');
  // "default" and "function" are stop words
  assert.equal(r.symbols.length, 0);
});

test('extractSymbols: limits to 3 symbols', () => {
  const r = extractSymbols('modify parse_code and run_full_index and extract_relations and hash_file');
  assert.ok(r.symbols.length <= 3);
});

// ── detectIntents ───────────────────────────────────────

// --- Impact intent ---
test('detectIntents: impact (EN)', () => {
  assert.ok(detectIntents('what is the impact of this change').impact);
  assert.ok(detectIntents('check the risk of modifying this').impact);
  assert.ok(detectIntents('this bug is critical').impact);
});

test('detectIntents: impact (ZH)', () => {
  assert.ok(detectIntents('这个改动有什么影响').impact);
  assert.ok(detectIntents('改动范围有多大').impact);
  assert.ok(detectIntents('会不会跟其他模块冲突').impact);
  assert.ok(detectIntents('修改前先看看').impact);
  assert.ok(detectIntents('有什么风险').impact);
  assert.ok(detectIntents('这个bug怎么回事').impact);
});

// --- Modify intent ---
test('detectIntents: modify (EN)', () => {
  assert.ok(detectIntents('refactor this module').modify);
  assert.ok(detectIntents('rename the function').modify);
  assert.ok(detectIntents('fix the broken test').modify);
  assert.ok(detectIntents('update the config').modify);
  assert.ok(detectIntents('remove deprecated code').modify);
  assert.ok(detectIntents('replace with new impl').modify);
});

test('detectIntents: modify (ZH)', () => {
  const words = ['修改', '修复', '重构', '优化', '简化', '精简', '适配', '统一', '修正', '调整', '去掉', '整理', '清理', '解耦', '更新', '升级', '迁移', '拆分', '合并', '提取'];
  for (const w of words) {
    assert.ok(detectIntents(`${w}这个模块`).modify, `"${w}" should trigger modify`);
  }
});

test('detectIntents: modify (ZH compound)', () => {
  assert.ok(detectIntents('把这个函数改成异步的').modify);
  assert.ok(detectIntents('把返回值类型换成 Result').modify);
  assert.ok(detectIntents('把同步改成异步').modify);
});

// --- Implement intent ---
test('detectIntents: implement (EN)', () => {
  assert.ok(detectIntents('add a new tool').implement);
  assert.ok(detectIntents('implement error handling').implement);
  assert.ok(detectIntents('create a helper function').implement);
  assert.ok(detectIntents('build the CI pipeline').implement);
  assert.ok(detectIntents('write unit tests').implement);
});

test('detectIntents: implement (ZH)', () => {
  const words = ['新增', '添加', '实现', '创建', '编写', '开发', '增加', '加上', '加个', '搭建', '补充', '引入', '支持', '封装', '接入', '对接', '配置'];
  for (const w of words) {
    assert.ok(detectIntents(`${w}一个功能`).implement, `"${w}" should trigger implement`);
  }
});

test('detectIntents: implement - "写" variants', () => {
  assert.ok(detectIntents('写个测试').implement);
  assert.ok(detectIntents('写一个工具函数').implement);
  assert.ok(detectIntents('帮我写一个函数').implement);
});

// --- Understand intent ---
test('detectIntents: understand (EN)', () => {
  assert.ok(detectIntents('how does this module work').understand);
  assert.ok(detectIntents('explain the architecture').understand);
});

test('detectIntents: understand (ZH)', () => {
  const words = ['看看', '看一下', '理解', '了解', '分析', '评估', '检查', '审核', '审查', '验证', '诊断', '深入思考'];
  for (const w of words) {
    assert.ok(detectIntents(`${w}这段代码`).understand, `"${w}" should trigger understand`);
  }
});

test('detectIntents: understand (ZH question patterns)', () => {
  assert.ok(detectIntents('这个模块是干什么的').understand);
  assert.ok(detectIntents('工作原理是什么').understand);
  assert.ok(detectIntents('整个流程是怎么走的').understand);
  assert.ok(detectIntents('这个功能怎么实现的').understand);
});

// --- Callgraph intent ---
test('detectIntents: callgraph (EN)', () => {
  assert.ok(detectIntents('who calls this function').callgraph);
  assert.ok(detectIntents('what calls parse_code').callgraph);
  assert.ok(detectIntents('trace the request flow').callgraph);
});

test('detectIntents: callgraph (ZH)', () => {
  assert.ok(detectIntents('这个函数被谁调了').callgraph);
  assert.ok(detectIntents('看看调用链路').callgraph);
  assert.ok(detectIntents('追踪一下请求路径').callgraph);
  assert.ok(detectIntents('上下游依赖关系是什么').callgraph);
  assert.ok(detectIntents('这个事件怎么触发的').callgraph);
});

// --- Search intent ---
test('detectIntents: search (EN)', () => {
  assert.ok(detectIntents('where is the config defined').search);
  assert.ok(detectIntents('find the error handling code').search);
  assert.ok(detectIntents('search for all usages').search);
});

test('detectIntents: search (ZH)', () => {
  assert.ok(detectIntents('这个函数定义在哪').search);
  assert.ok(detectIntents('找一下处理错误的代码').search);
  assert.ok(detectIntents('搜索所有用到这个类型的地方').search);
  assert.ok(detectIntents('在哪里用了这个常量').search);
});

// --- Per-keyword scoring (v0.21 weighted-scorer refactor) ---
test('scoreIntent: matched keyword returns its weight, unmatched returns 0', () => {
  // Each pattern in INTENT_PATTERNS is testable in isolation now.
  assert.equal(scoreIntent('this bug is critical', 'impact'), 1.0);
  assert.equal(scoreIntent('hello world', 'impact'), 0);
  assert.equal(scoreIntent('refactor this module', 'modify'), 1.0);
  assert.equal(scoreIntent('refactor this module', 'implement'), 0);
});

test('scoreIntent: max weight wins when multiple patterns match', () => {
  // "this bug needs a fix and impact analysis" matches `impact`, `bug`,
  // `risk`-no, all three impact rows are weight 1.0 currently — score is 1.0.
  // Spec: scoreIntent returns max(weight) of matching patterns, never sum.
  const score = scoreIntent('this bug needs impact analysis', 'impact');
  assert.equal(score, 1.0);
});

test('scoreIntent: unknown intent returns 0 (no throw)', () => {
  assert.equal(scoreIntent('anything', 'nonexistent_intent'), 0);
});

test('INTENT_PATTERNS: every intent has at least 5 patterns and uniform weights', () => {
  // v0.21 starts with uniform weights; future tuning can vary them per-pattern.
  // This test guards against regression to the giant single-regex form.
  const intents = ['impact', 'modify', 'implement', 'understand', 'callgraph', 'search'];
  for (const intent of intents) {
    const patterns = INTENT_PATTERNS[intent];
    assert.ok(Array.isArray(patterns), `${intent} must have patterns array`);
    assert.ok(patterns.length >= 5, `${intent} must have >=5 patterns, got ${patterns.length}`);
    for (const [pattern, weight] of patterns) {
      assert.ok(pattern instanceof RegExp, `${intent} pattern must be RegExp`);
      assert.ok(typeof weight === 'number' && weight > 0 && weight <= 1, `${intent} weight must be (0, 1]`);
    }
  }
});

test('INTENT_THRESHOLD is 0.5 — single weight-1.0 match fires the intent', () => {
  // Threshold contract: any pattern @ weight >= 0.5 → intent fires.
  // If we lower a pattern to weight 0.4, it must NOT fire alone.
  assert.equal(INTENT_THRESHOLD, 0.5);
});

// --- No false positives ---
test('detectIntents: simple confirmations have no code intent', () => {
  const r = detectIntents('好的');
  // "什么" would match in some words, but "好的" shouldn't trigger understand
  assert.equal(r.modify, false);
  assert.equal(r.implement, false);
  assert.equal(r.callgraph, false);
  assert.equal(r.search, false);
});

// ── determineQueryType (priority logic) ─────────────────

test('priority: impact/modify + strict symbol → impact', () => {
  const intents = { impact: true, modify: false, implement: false, understand: false, callgraph: false, search: false };
  const symbols = { symbols: ['parse_code'], lowConfidence: false };
  const result = determineQueryType(intents, symbols, []);
  assert.equal(result.type, 'impact');
  assert.equal(result.symbol, 'parse_code');
});

test('priority: modify + strict symbol → impact', () => {
  const intents = { impact: false, modify: true, implement: false, understand: false, callgraph: false, search: false };
  const symbols = { symbols: ['handleMessage'], lowConfidence: false };
  const result = determineQueryType(intents, symbols, []);
  assert.equal(result.type, 'impact');
});

test('priority: modify + low-confidence symbol → NOT impact (falls to overview/search)', () => {
  const intents = { impact: false, modify: true, implement: false, understand: false, callgraph: false, search: false };
  const symbols = { symbols: ['embedding'], lowConfidence: true };
  const result = determineQueryType(intents, symbols, ['src/embed/']);
  // Should fall through to overview (file paths exist)
  assert.equal(result.type, 'overview');
});

test('priority: callgraph + strict symbol → callgraph', () => {
  const intents = { impact: false, modify: false, implement: false, understand: false, callgraph: true, search: false };
  const symbols = { symbols: ['parse_code'], lowConfidence: false };
  const result = determineQueryType(intents, symbols, []);
  assert.equal(result.type, 'callgraph');
});

test('priority: file paths → overview (regardless of intent)', () => {
  const intents = { impact: false, modify: true, implement: false, understand: false, callgraph: false, search: false };
  const symbols = { symbols: [], lowConfidence: false };
  const result = determineQueryType(intents, symbols, ['src/storage/queries.rs']);
  assert.equal(result.type, 'overview');
  assert.equal(result.path, 'src/storage/');
});

test('priority: search intent + symbol → search', () => {
  const intents = { impact: false, modify: false, implement: false, understand: false, callgraph: false, search: true };
  const symbols = { symbols: ['parse_code'], lowConfidence: false };
  const result = determineQueryType(intents, symbols, []);
  assert.equal(result.type, 'search');
});

test('priority: implement intent + symbol → search', () => {
  const intents = { impact: false, modify: false, implement: true, understand: false, callgraph: false, search: false };
  const symbols = { symbols: ['embedding'], lowConfidence: true };
  const result = determineQueryType(intents, symbols, []);
  assert.equal(result.type, 'search');
});

test('priority: understand + symbol → search', () => {
  const intents = { impact: false, modify: false, implement: false, understand: true, callgraph: false, search: false };
  const symbols = { symbols: ['pipeline'], lowConfidence: true };
  const result = determineQueryType(intents, symbols, []);
  assert.equal(result.type, 'search');
});

test('priority: no intent, no symbol, no path → null', () => {
  const intents = { impact: false, modify: false, implement: false, understand: false, callgraph: false, search: false };
  const symbols = { symbols: [], lowConfidence: false };
  const result = determineQueryType(intents, symbols, []);
  assert.equal(result, null);
});

test('priority: cooldown blocks query', () => {
  const intents = { impact: true, modify: false, implement: false, understand: false, callgraph: false, search: false };
  const symbols = { symbols: ['parse_code'], lowConfidence: false };
  const result = determineQueryType(intents, symbols, [], (type) => type === 'impact');
  // Impact blocked by cooldown, falls through; no file path, no search intent → try search via understand fallback
  // Actually: no understand intent and hasAny=true, so the last condition (!hasAny) is false → null
  // But symbol exists and we have filePaths=[] → falls to search via implement/qualified check → no
  // Actually it should return null since all fallbacks require conditions not met
  assert.equal(result, null);
});

test('priority: cooldown on impact → falls to overview when file paths exist', () => {
  const intents = { impact: true, modify: false, implement: false, understand: false, callgraph: false, search: false };
  const symbols = { symbols: ['parse_code'], lowConfidence: false };
  const result = determineQueryType(intents, symbols, ['src/parser/mod.rs'], (type) => type === 'impact');
  assert.equal(result.type, 'overview');
});

// ── Full integration: message → query type ──────────────

function analyze(msg) {
  if (shouldSkip(msg)) return { skipped: true };
  const fp = extractFilePaths(msg);
  const sym = extractSymbols(msg);
  const intents = detectIntents(msg);
  // Phase E: pass message into determineQueryType so symptom-hint fallback fires.
  const query = determineQueryType(intents, sym, fp, undefined, msg);
  return { query, intents, symbols: sym, filePaths: fp };
}

test('integration: 修改 parse_code 函数增加错误处理 → impact', () => {
  const r = analyze('修改 parse_code 函数增加错误处理');
  assert.equal(r.query.type, 'impact');
  assert.equal(r.query.symbol, 'parse_code');
});

test('integration: 看看 src/mcp/ 模块的代码结构 → overview', () => {
  const r = analyze('看看 src/mcp/ 模块的代码结构');
  assert.equal(r.query.type, 'overview');
});

test('integration: refactor src/storage/queries.rs → overview (not impact on "refactor")', () => {
  const r = analyze('refactor src/storage/queries.rs to use parameterized queries');
  assert.equal(r.query.type, 'overview');
  assert.ok(r.query.path.includes('src/storage/'));
});

test('integration: help me understand the indexer pipeline → search', () => {
  const r = analyze('help me understand the indexer pipeline');
  assert.equal(r.query.type, 'search');
  assert.equal(r.query.symbol, 'pipeline');
});

test('integration: write tests for the embedding module → search', () => {
  const r = analyze('write tests for the embedding module');
  assert.equal(r.query.type, 'search');
  assert.equal(r.query.symbol, 'embedding');
});

test('integration: 修复这段逻辑的bug → not skipped (bug=3 chars)', () => {
  const r = analyze('修复这段逻辑的bug');
  assert.ok(!r.skipped);
  assert.ok(r.intents.impact); // "bug"
  assert.ok(r.intents.modify); // "修复"
});

test('integration: 按优先级修复这些问题 → skipped (no code entity)', () => {
  const r = analyze('按优先级修复这些问题');
  assert.ok(r.skipped);
});

test('integration: 帮我写一个工具函数 → implement intent', () => {
  const r = analyze('帮我写一个工具函数');
  assert.ok(!r.skipped);
  assert.ok(r.intents.implement);
});

test('integration: 对整个项目进行一次完整的代码审核 → understand', () => {
  const r = analyze('对整个项目进行一次完整的代码审核');
  assert.ok(r.intents.understand);
});

test('integration: 更新一下readme.md → modify intent', () => {
  const r = analyze('更新一下readme.md这个文件');
  assert.ok(r.intents.modify);
});

test('integration: 配置 pre-commit hook → implement intent', () => {
  const r = analyze('配置提交代码时的git pre-commit hook检查');
  assert.ok(r.intents.implement);
});

test('integration: 检查下我们插件上下文token占用情况 → understand', () => {
  const r = analyze('检查下我们插件上下文token占用情况');
  assert.ok(r.intents.understand);
});

test('integration: 诊断一下性能问题 → understand', () => {
  const r = analyze('诊断一下性能问题');
  assert.ok(r.intents.understand);
});

test('integration: simple confirmation → skipped', () => {
  assert.ok(analyze('好的').skipped);
  assert.ok(analyze('继续').skipped);
  assert.ok(analyze('ok').skipped);
});

// ── Skill files validation ──────────────────────────────

test('skills: explore.md has correct frontmatter', () => {
  const content = fs.readFileSync(path.join(__dirname, '../skills/explore.md'), 'utf8');
  assert.match(content, /^---\nname: explore/);
  assert.match(content, /description:/);
});

test('skills: index.md has correct frontmatter', () => {
  const content = fs.readFileSync(path.join(__dirname, '../skills/index.md'), 'utf8');
  assert.match(content, /^---\nname: index/);
  assert.match(content, /description:/);
});

test('skills: commands directory is empty (all converted to skills)', () => {
  const commandsDir = path.join(__dirname, '../commands');
  const exists = fs.existsSync(commandsDir);
  if (exists) {
    const files = fs.readdirSync(commandsDir).filter(f => f.endsWith('.md'));
    assert.equal(files.length, 0, 'commands/ should have no .md files');
  }
  // Directory not existing is also valid
});

test('skills: only expected skills exist', () => {
  const skillsDir = path.join(__dirname, '../skills');
  const files = fs.readdirSync(skillsDir).filter(f => f.endsWith('.md')).sort();
  assert.deepEqual(files, ['explore.md', 'index.md']);
});

// ── computeQuietHooks priority chain (default-noisy flip) ────────

test('computeQuietHooks: default (no env) is NOISY', () => {
  // Default flipped back to push-on. The v0.21 opt-in default relied on
  // routing-bench P@1=100% but that measures triage accuracy, not whether
  // the agent reaches for a tool at all. pre-grep-guide.js sees 13× raw-grep
  // bias on the same source tree — push is the corrective.
  assert.equal(computeQuietHooks({}), false);
});

test('computeQuietHooks: CODE_GRAPH_QUIET_HOOKS=1 forces quiet (escape hatch)', () => {
  assert.equal(computeQuietHooks({ CODE_GRAPH_QUIET_HOOKS: '1' }), true);
});

test('computeQuietHooks: CODE_GRAPH_QUIET_HOOKS=0 stays noisy (back-compat, same as default)', () => {
  assert.equal(computeQuietHooks({ CODE_GRAPH_QUIET_HOOKS: '0' }), false);
});

test('computeQuietHooks: CODE_GRAPH_VERBOSE_HOOKS=1 stays noisy (back-compat, same as default)', () => {
  assert.equal(computeQuietHooks({ CODE_GRAPH_VERBOSE_HOOKS: '1' }), false);
});

test('computeQuietHooks: QUIET_HOOKS=1 wins over VERBOSE_HOOKS=1 (priority chain)', () => {
  // Priority: CODE_GRAPH_QUIET_HOOKS=1 (escape) > QUIET_HOOKS=0 / VERBOSE_HOOKS=1 > default.
  assert.equal(computeQuietHooks({ CODE_GRAPH_QUIET_HOOKS: '1', CODE_GRAPH_VERBOSE_HOOKS: '1' }), true);
  assert.equal(computeQuietHooks({ CODE_GRAPH_QUIET_HOOKS: '0', CODE_GRAPH_VERBOSE_HOOKS: '0' }), false);
});

test('CODE_GRAPH_QUIET_HOOKS=1 short-circuits silently on stdout, stderr, exit 0 (escape hatch verified end-to-end)', () => {
  // End-to-end: the escape hatch must produce zero stdout/stderr noise
  // (any leak would land in Claude's display). Was the only e2e check before
  // the default-noisy flip — kept under the new default to guarantee that
  // setting the env still fully silences the hook.
  const { spawnSync } = require('node:child_process');
  const script = path.join(__dirname, 'user-prompt-context.js');
  const proc = spawnSync(process.execPath, [script], {
    input: JSON.stringify({ prompt: 'impact of refactoring parse_code function' }),
    env: { ...process.env, CODE_GRAPH_QUIET_HOOKS: '1' },
    encoding: 'utf8',
    timeout: 2000,
  });
  assert.equal(proc.stdout, '', 'quiet must be silent on stdout');
  assert.equal(proc.stderr, '', 'quiet must be silent on stderr');
  assert.equal(proc.status, 0, 'quiet must exit 0');
});

test('CODE_GRAPH_QUIET_HOOKS=1 silences even the fresh-install (no-manifest) notice', () => {
  // Regression: the mid-session install notice printed BEFORE the quiet check,
  // so on a fresh checkout (no ~/.cache/code-graph manifest) it leaked to stdout
  // despite the escape hatch. CI hit this; the dev box has a manifest and masked
  // it. Force the no-manifest path with a throwaway HOME so it reproduces locally.
  const { spawnSync } = require('node:child_process');
  const os = require('node:os');
  const fs = require('node:fs');
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-upc-nohome-'));
  try {
    const script = path.join(__dirname, 'user-prompt-context.js');
    const proc = spawnSync(process.execPath, [script], {
      input: JSON.stringify({ prompt: 'impact of refactoring parse_code function' }),
      // HOME (POSIX) + USERPROFILE (Windows) → os.homedir() points at an empty
      // dir, so MANIFEST_PATH is absent and runMain() enters the install branch.
      env: { ...process.env, HOME: home, USERPROFILE: home, CODE_GRAPH_QUIET_HOOKS: '1' },
      encoding: 'utf8',
      timeout: 2000,
    });
    assert.equal(proc.stdout, '', 'quiet must silence the no-manifest install notice on stdout');
    assert.equal(proc.stderr, '', 'quiet must be silent on stderr');
    assert.equal(proc.status, 0, 'quiet must exit 0');
  } finally {
    fs.rmSync(home, { recursive: true, force: true });
  }
});

// ── Phase E: hasSymptom + symptom-hint fallback ──────────────

const { hasSymptom, SYMPTOM_PATTERNS } = require('./user-prompt-context');

test('hasSymptom: 报告数据不准', () => {
  assert.equal(hasSymptom('今天的报告数据不准'), true);
});

test('hasSymptom: test 又挂了', () => {
  assert.equal(hasSymptom('test 又挂了'), true);
});

test('hasSymptom: Why does this not work?', () => {
  assert.equal(hasSymptom('Why does this not work?'), true);
});

test('hasSymptom: 有 bug', () => {
  assert.equal(hasSymptom('有 bug，帮我看看'), true);
});

test('hasSymptom: 为什么 (vague-question marker)', () => {
  assert.equal(hasSymptom('为什么会这样'), true);
});

test('hasSymptom: 哪里写错了', () => {
  assert.equal(hasSymptom('find 一下哪里写错了'), true);
});

test('hasSymptom: doesn\'t work / not working', () => {
  assert.equal(hasSymptom("this doesn't work as expected"), true);
  assert.equal(hasSymptom('the service is not working'), true);
});

test('hasSymptom: 挂了 / 失败 / 卡死', () => {
  assert.equal(hasSymptom('test 挂了'), true);
  assert.equal(hasSymptom('又失败了'), true);
  assert.equal(hasSymptom('整个服务卡死了'), true);
});

// Precision: must NOT flag normal task statements as symptoms.
test('hasSymptom: 修改 parse_code → false', () => {
  assert.equal(hasSymptom('修改 parse_code 函数增加错误处理'), false);
});

test('hasSymptom: 看看 src/mcp/ → false', () => {
  assert.equal(hasSymptom('看看 src/mcp/ 模块的代码结构'), false);
});

test('hasSymptom: write tests → false', () => {
  assert.equal(hasSymptom('write tests for the embedding module'), false);
});

test('hasSymptom: empty / non-string → false', () => {
  assert.equal(hasSymptom(''), false);
  assert.equal(hasSymptom(null), false);
  assert.equal(hasSymptom(undefined), false);
});

test('SYMPTOM_PATTERNS: exported + non-empty array', () => {
  assert.ok(Array.isArray(SYMPTOM_PATTERNS));
  assert.ok(SYMPTOM_PATTERNS.length >= 8,
    `SYMPTOM_PATTERNS has ${SYMPTOM_PATTERNS.length} entries; want ≥8 for coverage`);
});

// ── determineQueryType: symptom-hint fallback ────────────────

test('symptom-fallback: pure symptom message, no anchor → symptom-hint', () => {
  const intents = { impact: false, modify: false, implement: false, understand: false, callgraph: false, search: false };
  const symbols = { symbols: [], lowConfidence: false };
  const result = determineQueryType(intents, symbols, [], undefined, '今天的报告数据不准');
  assert.equal(result && result.type, 'symptom-hint');
});

test('symptom-fallback: intent + no symbol/path + symptom → symptom-hint', () => {
  // "find 一下哪里写错了" — search intent fires but no symbol or path is extractable.
  const intents = { impact: false, modify: false, implement: false, understand: false, callgraph: false, search: true };
  const symbols = { symbols: [], lowConfidence: false };
  const result = determineQueryType(intents, symbols, [], undefined, 'find 一下哪里写错了');
  assert.equal(result && result.type, 'symptom-hint');
});

test('symptom-fallback: actionable path beats symptom-hint (precedence)', () => {
  // Impact path with strict symbol must take precedence even when symptom phrasing is present.
  const intents = { impact: true, modify: false, implement: false, understand: false, callgraph: false, search: false };
  const symbols = { symbols: ['parse_code'], lowConfidence: false };
  const result = determineQueryType(intents, symbols, [], undefined, '修改前看看 parse_code 的 bug 影响');
  assert.equal(result.type, 'impact');
});

test('symptom-fallback: no symptom + no anchor → null (unchanged)', () => {
  const intents = { impact: false, modify: false, implement: false, understand: false, callgraph: false, search: false };
  const symbols = { symbols: [], lowConfidence: false };
  const result = determineQueryType(intents, symbols, [], undefined, 'hello there');
  assert.equal(result, null);
});

test('symptom-fallback: cooldown blocks symptom-hint', () => {
  const intents = { impact: false, modify: false, implement: false, understand: false, callgraph: false, search: false };
  const symbols = { symbols: [], lowConfidence: false };
  const result = determineQueryType(intents, symbols, [], (t) => t === 'symptom', '今天的报告数据不准');
  assert.equal(result, null);
});

test('symptom-fallback: omitted message arg → backward-compat null', () => {
  // Existing callers (and the legacy bench harness) call determineQueryType
  // without the 5th arg. The fallback must NOT fire — preserve prior behavior.
  const intents = { impact: false, modify: false, implement: false, understand: false, callgraph: false, search: false };
  const symbols = { symbols: [], lowConfidence: false };
  const result = determineQueryType(intents, symbols, []);
  assert.equal(result, null);
});

// ── Integration: analyze() with symptom-only messages ──

test('integration: 今天的报告数据不准 → symptom-hint', () => {
  const r = analyze('今天的报告数据不准');
  assert.equal(r.query && r.query.type, 'symptom-hint');
});

test('integration: test 又挂了 → symptom-hint', () => {
  const r = analyze('test 又挂了');
  assert.equal(r.query && r.query.type, 'symptom-hint');
});

test('integration: Why does this not work? → symptom-hint', () => {
  const r = analyze('Why does this not work?');
  assert.equal(r.query && r.query.type, 'symptom-hint');
});

// ── buildRunEnv: hook-internal delivery marker (anti phantom-conversion) ──

test('buildRunEnv: tags CODE_GRAPH_INTERNAL=1 so deliveries are not logged as model `use`', () => {
  const env = buildRunEnv({ PATH: '/usr/bin', HOME: '/home/x', USERPROFILE: '/home/x' });
  assert.equal(env.CODE_GRAPH_INTERNAL, '1');
  // preserves the base env (binary still resolves on PATH, cwd inherited, etc.)
  assert.equal(env.PATH, '/usr/bin');
  assert.equal(env.HOME, '/home/x');
});

test('buildRunEnv: defaults to process.env when no base given', () => {
  const env = buildRunEnv();
  assert.equal(env.CODE_GRAPH_INTERNAL, '1');
});

test('run() wires buildRunEnv() into execFileSync (no phantom use-event leak)', () => {
  // run() lives inside runMain() (the file top-level-executes on require), so assert
  // the wiring at the source level: every code-graph-mcp invocation this hook makes
  // must carry the internal marker, else its PUSH injections read back as model
  // adoption (the 2026-06-23 mem audit: 100 phantom "model CLI calls"). Mirrors the
  // cg-answer.js / pre-edit-guide.js internal-env guard.
  const src = fs.readFileSync(path.join(__dirname, 'user-prompt-context.js'), 'utf8');
  const i = src.indexOf('function run(');
  assert.ok(i >= 0, 'run() helper present');
  assert.match(src.slice(i, i + 320), /env:\s*buildRunEnv\(\)/);
});

// ── End-to-end: binary resolution, cooldown timing, flag scoping ──────────
// These three findings all live in runMain(), which the unit tests above cannot
// reach (the file top-level-executes on require), so they are exercised by
// actually spawning the hook against a sandboxed HOME + project fixture.

const { spawnSync } = require('node:child_process');
const os = require('node:os');

/**
 * A sandbox in which user-prompt-context.js reaches its CLI path: a redirected
 * HOME carrying the install manifest plus a find-binary cache entry, a
 * redirected TMPDIR for the cooldown flags, and a project fixture holding
 * .code-graph/index.db.
 *
 * The binary is pinned through ~/.cache/code-graph/binary-path — the FIRST
 * thing findBinary() consults — so what the hook runs does not depend on
 * whether this machine happens to have target/release/code-graph-mcp built.
 */
function upcSandbox(t, binaryScript) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-upc-e2e-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));

  const home = path.join(root, 'home');
  const cache = path.join(home, '.cache', 'code-graph');
  fs.mkdirSync(path.join(cache, 'bin'), { recursive: true });
  fs.writeFileSync(path.join(cache, 'install-manifest.json'), '{"version":"9.9.9","config":{}}');
  const fake = path.join(cache, 'bin', 'code-graph-mcp');
  fs.writeFileSync(fake, binaryScript, { mode: 0o755 });
  fs.writeFileSync(path.join(cache, 'binary-path'), fake);

  const markers = path.join(root, 'markers');
  const tmp = path.join(root, 'tmp');
  fs.mkdirSync(markers);
  fs.mkdirSync(tmp);

  return { root, home, markers, tmp, cgTmp: path.join(tmp, 'code-graph-mcp'),
           project: upcProject(root, 'project') };
}

function upcProject(root, name) {
  const dir = path.join(root, name);
  fs.mkdirSync(path.join(dir, '.code-graph'), { recursive: true });
  fs.writeFileSync(path.join(dir, '.code-graph', 'index.db'), '');
  return dir;
}

function runUpc(sb, prompt, { cwd = sb.project, env = {} } = {}) {
  return spawnSync(process.execPath, [path.join(__dirname, 'user-prompt-context.js')], {
    // The field Claude Code actually sends on UserPromptSubmit. The whole
    // suite fed `message` for years — a self-consistent copy of the defect,
    // which is why 100% of it passed while the hook never fired once in
    // production (audit 2026-08-29 JS-01).
    input: JSON.stringify({ prompt }),
    cwd,
    env: {
      ...process.env,
      HOME: sb.home, USERPROFILE: sb.home,
      TMPDIR: sb.tmp, TMP: sb.tmp, TEMP: sb.tmp,
      CG_MARKER_DIR: sb.markers,
      CODE_GRAPH_QUIET_HOOKS: '0',
      ...env,
    },
    encoding: 'utf8',
    timeout: 15000,
  });
}

const posixOnly = process.platform === 'win32' && 'POSIX shell fixtures';

test('the CLI is resolved through find-binary, never as a bare name on PATH', { skip: posixOnly }, (t) => {
  // The four call sites passed the literal string 'code-graph-mcp' to
  // execFileSync, so they ran whatever PATH resolved — nothing for a
  // plugin-only install (the binary lives in ~/.cache/code-graph/bin and was
  // never on PATH), or a years-stale global shim when PATH did have one. Two
  // markers make the difference observable: only the resolved binary may run.
  const sb = upcSandbox(t, '#!/bin/sh\ntouch "$CG_MARKER_DIR/resolved"\nexit 1\n');
  const shimDir = path.join(sb.root, 'shim');
  fs.mkdirSync(shimDir);
  fs.writeFileSync(path.join(shimDir, 'code-graph-mcp'),
    '#!/bin/sh\ntouch "$CG_MARKER_DIR/path-shim"\nexit 0\n', { mode: 0o755 });

  const proc = runUpc(sb, 'where is parseConfig defined',
    { env: { PATH: `${shimDir}${path.delimiter}${process.env.PATH}` } });

  assert.equal(proc.status, 0, `hook must exit 0\n${proc.stderr}`);
  assert.equal(fs.existsSync(path.join(sb.markers, 'path-shim')), false,
    'the hook ran whatever `code-graph-mcp` PATH happened to point at');
  assert.equal(fs.existsSync(path.join(sb.markers, 'resolved')), true,
    'the hook must run the binary find-binary resolved');
});

test('the per-type cooldown is stamped on ATTEMPT, not only on a non-empty result', { skip: posixOnly }, (t) => {
  // markCooldown() sat inside `if (result && result.trim())`, so a binary that
  // failed, timed out, or simply had nothing to say left no flag — and the very
  // next prompt of the same shape re-ran it. A broken binary therefore cost a
  // 3s blocking execFileSync on EVERY turn, which is the expensive case wearing
  // the cheap case's disguise.
  const sb = upcSandbox(t, '#!/bin/sh\nexit 1\n');   // always fails, never prints

  const proc = runUpc(sb, 'where is parseConfig defined');

  assert.equal(proc.status, 0, `hook must stay silent-and-successful\n${proc.stderr}`);
  assert.equal(proc.stdout, '', 'a failed run injects nothing');
  const flags = fs.readdirSync(sb.cgTmp).filter((f) => f.startsWith('.code-graph-ctx-'));
  assert.equal(flags.length, 1,
    `a failing binary must still start the cooldown, else it re-runs every prompt (found: ${flags.join(', ')})`);
  assert.match(flags[0], /^\.code-graph-ctx-[0-9a-f]{12}-search$/,
    'and the flag must carry the project hash (see cwdHash in tmp-dir.js)');
});

test('cooldown flags are project-scoped: a push in one repo must not silence another', { skip: posixOnly }, (t) => {
  // There are only five ctx flag names, and one shared tmp dir for the whole
  // machine, so an un-scoped flag was a machine-wide mute: an `impact` push in
  // any repo silenced impact pushes in every other repo for the next 30s, and
  // `overview` did it for five minutes.
  const sb = upcSandbox(t, '#!/bin/sh\nexit 1\n');
  const projectB = upcProject(sb.root, 'project-b');

  runUpc(sb, 'where is parseConfig defined');
  runUpc(sb, 'where is parseConfig defined', { cwd: projectB });

  const flags = fs.readdirSync(sb.cgTmp).filter((f) => f.startsWith('.code-graph-ctx-')).sort();
  assert.equal(flags.length, 2,
    `each project keeps its own cooldown; found ${flags.length}: ${flags.join(', ')}`);
  assert.notEqual(flags[0], flags[1], 'the two projects must hash differently');
});

test('every hook reads stdin from fd 0, not the /dev/stdin path', () => {
  // Source-level on purpose: the failure only reproduces when stdin is a
  // socketpair (open(2) on /dev/stdin then fails ENXIO), and neither
  // spawnSync({input}) nor a shell pipe produces one — so a behavioral test
  // here would pass against the broken spelling and prove nothing. Five hooks
  // already carried the fd-0 form with that comment; user-prompt-context.js was
  // the holdout, and the sixth is exactly how a fixed class regrows.
  const hooks = ['user-prompt-context.js', 'pre-grep-guide.js', 'post-grep-inject.js',
                 'pre-read-guide.js', 'pre-edit-guide.js', 'session-init.js'];
  const offenders = hooks.filter((h) =>
    /readFileSync\(\s*['"]\/dev\/stdin['"]/.test(fs.readFileSync(path.join(__dirname, h), 'utf8')));
  assert.deepEqual(offenders, [], 'these hooks re-open /dev/stdin instead of reading fd 0');
});

// ── the payload contract itself (audit 2026-08-29 JS-01) ─────────────────────
//
// Every e2e above now feeds `{prompt:…}`, so the field name is covered by the
// suite as a whole. This test states the contract on its own terms so a future
// reader sees WHICH field is load-bearing without reverse-engineering the
// helper, and so the back-compat arm and the silent arm are pinned too.
//
// Note the assertion is on non-empty STDOUT plus a dropped cooldown flag. Exit
// status is not evidence here: a hook that reads nothing exits 0, which is
// precisely why `verifyHooksFire` — which only checks exit 0 — reported this
// surface healthy for its whole dead lifetime.
test('the documented UserPromptSubmit field drives the hook; message still works; {} stays silent',
  { skip: posixOnly }, (t) => {
    const sb = upcSandbox(t, '#!/bin/sh\necho "callers: alpha beta"\n');
    const ask = 'impact of refactoring parse_code function';

    const viaPrompt = runUpc(sb, ask);
    assert.notEqual(viaPrompt.stdout.trim(), '', 'a `prompt` payload must produce injection');

    // A fresh sandbox per arm: the cooldown flag the first run drops would
    // silence the second, and a green-by-cooldown arm proves nothing.
    const sb2 = upcSandbox(t, '#!/bin/sh\necho "callers: alpha beta"\n');
    const viaMessage = spawnSync(
      process.execPath, [path.join(__dirname, 'user-prompt-context.js')],
      {
        input: JSON.stringify({ message: ask }),
        cwd: sb2.project,
        env: {
          ...process.env,
          HOME: sb2.home, USERPROFILE: sb2.home,
          TMPDIR: sb2.tmp, TMP: sb2.tmp, TEMP: sb2.tmp,
          CG_MARKER_DIR: sb2.markers,
          CODE_GRAPH_QUIET_HOOKS: '0',
        },
        encoding: 'utf8',
        timeout: 15000,
      },
    );
    assert.notEqual(viaMessage.stdout.trim(), '', 'the `message` fallback must keep working');

    // And the production tell: a run that fired leaves a cooldown flag. Zero of
    // these in a heavily dogfooded tmp dir is how the defect was found.
    const flags = fs.readdirSync(sb.cgTmp).filter((f) => f.startsWith('.code-graph-ctx-'));
    assert.equal(flags.length, 1, `expected one cooldown flag, got ${JSON.stringify(flags)}`);

    // A payload with neither field must stay silent rather than inject noise.
    const sb3 = upcSandbox(t, '#!/bin/sh\necho "callers: alpha beta"\n');
    const empty = spawnSync(
      process.execPath, [path.join(__dirname, 'user-prompt-context.js')],
      {
        input: '{}',
        cwd: sb3.project,
        env: {
          ...process.env,
          HOME: sb3.home, USERPROFILE: sb3.home,
          TMPDIR: sb3.tmp, TMP: sb3.tmp, TEMP: sb3.tmp,
          CG_MARKER_DIR: sb3.markers,
          CODE_GRAPH_QUIET_HOOKS: '0',
        },
        encoding: 'utf8',
        timeout: 15000,
      },
    );
    assert.equal(empty.stdout.trim(), '', 'an empty payload must inject nothing');
  });
