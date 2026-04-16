'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const fs = require('node:fs');

const {
  shouldSkip,
  extractFilePaths,
  extractSymbols,
  detectIntents,
  determineQueryType,
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
  const query = determineQueryType(intents, sym, fp);
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

test('CODE_GRAPH_QUIET_HOOKS=1 short-circuits before reading stdin', () => {
  const { execFileSync } = require('node:child_process');
  const script = path.join(__dirname, 'user-prompt-context.js');
  const out = execFileSync(process.execPath, [script], {
    input: JSON.stringify({ message: 'impact analysis for fn_that_would_trigger_search' }),
    env: { ...process.env, CODE_GRAPH_QUIET_HOOKS: '1' },
    encoding: 'utf8',
    stdio: ['pipe', 'pipe', 'pipe'],
    timeout: 2000,
  });
  // Quiet mode must produce no stdout — no [code-graph:*] prefix, nothing.
  assert.equal(out, '');
});
