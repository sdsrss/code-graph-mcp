#!/usr/bin/env node
'use strict';
// UserPromptSubmit hook: inject relevant code-graph RESULTS based on user's intent.
// Strategy: PUSH structural context (not suggestions) that Grep/Read cannot provide.
// This is a CODE INDEX — only inject structural code context (impact, overview, callgraph).
const { execFileSync } = require('child_process');
const fs = require('fs');
const path = require('path');
const os = require('os');

// --- Mid-session install detection ---
// If hooks are running but lifecycle install() hasn't executed yet (no manifest),
// the plugin was installed mid-session and the MCP server isn't connected.
// Claude Code only starts MCP servers at session startup; /mcp reconnect cannot
// start servers that were never initialized.
const MANIFEST_PATH = path.join(os.homedir(), '.cache', 'code-graph', 'install-manifest.json');
if (!fs.existsSync(MANIFEST_PATH)) {
  const noticeFile = path.join(os.tmpdir(), '.code-graph-mcp-restart-notice');
  try {
    // Show once per hour to avoid spam
    if (Date.now() - fs.statSync(noticeFile).mtimeMs < 3600000) process.exit(0);
  } catch { /* first notice */ }
  try { fs.writeFileSync(noticeFile, ''); } catch { /* ok */ }
  process.stdout.write(
    '[code-graph] Plugin installed — MCP server requires a session restart to connect.\n' +
    'MCP servers are only initialized at session startup. To activate:\n' +
    '  1. Press Ctrl+C to exit the current session\n' +
    '  2. Re-run `claude` to start a new session\n' +
    'Meanwhile, CLI tools work directly: code-graph-mcp search <query>, code-graph-mcp map, etc.\n'
  );
  process.exit(0);
}

// --- Per-type rate limiting (replaces single global cooldown) ---
const COOLDOWNS = {
  impact:    30 * 1000,     // 30s — impact context changes during rapid edits
  overview:  5 * 60 * 1000, // 5min — module structure rarely changes mid-session
  callgraph: 60 * 1000,     // 1min
  search:    60 * 1000,     // 1min
};

function isCoolingDown(type) {
  try {
    const flag = path.join(os.tmpdir(), `.code-graph-ctx-${type}`);
    const stat = fs.statSync(flag);
    return Date.now() - stat.mtimeMs < (COOLDOWNS[type] || 60000);
  } catch { return false; }
}

function markCooldown(type) {
  try {
    fs.writeFileSync(path.join(os.tmpdir(), `.code-graph-ctx-${type}`), '');
  } catch { /* ok */ }
}

// --- Read user message ---
let message;
try {
  const input = JSON.parse(fs.readFileSync('/dev/stdin', 'utf8'));
  message = (input && input.message) || '';
} catch {
  process.exit(0);
}
// Chinese chars are ~3 bytes but 1 char; "看看 fts5_search" is only 16 chars
if (!message || message.length < 8) process.exit(0);

// --- Check index ---
const cwd = process.cwd();
const dbPath = path.join(cwd, '.code-graph', 'index.db');
if (!fs.existsSync(dbPath)) process.exit(0);

// --- Constants ---

const STOP_WORDS = new Set([
  'this', 'that', 'with', 'from', 'what', 'when', 'which', 'there',
  'their', 'these', 'those', 'have', 'been', 'some', 'will', 'would',
  'could', 'should', 'about', 'after', 'before', 'other', 'every',
  'where', 'while', 'first', 'under', 'still', 'between', 'without',
  'being', 'through', 'default', 'function', 'method', 'class',
]);

// --- Detect intent + entities ---

// Skip non-code prompts (commit, push, simple confirmations, chat, instructions, etc.)
const trimmed = message.trim();
if (/^(yes|no|ok|commit|push|y|n|done|thanks|thank you|继续|确认|好的|好|是的|不|可以|行|对|提交|推送|没问题|谢谢|发布|更新|编译|安装|卸载|重启|重连|清理)\s*[.!?。！？]?\s*$/i.test(trimmed)) {
  process.exit(0);
}
// Skip action-only prompts without code entities (修复这些问题, 按优先级实施, etc.)
if (/^(修复|优化|实施|执行|开始|按|实测|帮我|进入|用|重新)/.test(trimmed) && !/[a-zA-Z_]{4,}/.test(trimmed)) {
  process.exit(0);
}

// Extract file paths from message
const filePaths = (message.match(/(?:src|lib|test|pkg|cmd|internal|app|components?)\/[\w/.-]+/g) || [])
  .slice(0, 2);

// Extract potential symbol names (camelCase, snake_case, PascalCase, qualified like Foo::bar, Foo.bar, Foo::bar::baz)
const symbolCandidates = (message.match(/\b(?:[A-Z]\w*(?:(?:::|\.)\w+)+|[a-z]\w*(?:_\w+){1,}|[a-z]\w*(?:[A-Z]\w*)+|[A-Z][a-z]+(?:[A-Z][a-z]+)+)\b/g) || [])
  .filter(s => s.length > 4)
  .filter(s => !STOP_WORDS.has(s.toLowerCase()))
  .slice(0, 3);

// Fallback: extract backtick-quoted symbols (common in mixed Chinese+code: "修改 `parse_code` 函数")
if (symbolCandidates.length === 0) {
  const backtickSymbols = (message.match(/`([a-zA-Z_]\w{2,})`/g) || [])
    .map(s => s.replace(/`/g, ''))
    .filter(s => s.length >= 3 && !STOP_WORDS.has(s.toLowerCase()));
  symbolCandidates.push(...backtickSymbols.slice(0, 3));
}

// Fallback: plain lowercase words (8+ chars) likely to be function/type names.
// Only when strict patterns found nothing — avoids false positives from English prose.
// Minimum 8 chars filters most common English words while keeping technical terms
// (authenticate, serialize, initialize, dispatch, resolver, etc.)
if (symbolCandidates.length === 0) {
  const plain = (message.match(/\b[a-z][a-z]{7,}\b/g) || [])
    .filter(s => !STOP_WORDS.has(s))
    .filter(s => !/^(possible|together|actually|something|different|important|following|available|necessary|currently|implement|operation|otherwise|beginning|knowledge|attention|according|certainly|sometimes|direction|recommend|structure|describe|question|complete|generate|anything|continue|consider|response|approach|happened|recently|probably|expected|previous|original|specific|directly|received|required|supposed|separate|designed|finished|provided|included|prepared|combined|properly|remember|whatever|although|document|handling|existing|everyone|standard|research|personal|relative|absolute|practice|language|thousand|national|evidence)$/.test(s));
  symbolCandidates.push(...plain.slice(0, 2));
}

// Detect intent keywords (EN + ZH, derived from user's actual prompt history)
const intentImpact = /(?:impact|影响|修改前|改之前|blast radius|before (?:edit|chang|modif)|risk|风险|改动范围|波及|问题在|bug|干扰|冲突|卡)/i.test(message);
const intentModify = /(?:改(?!变)|修改|重构|\brefactor\b|\bchange\b|\brename\b|移动|\bmove\b|删(?!除文件)|\bremove\b|替换|\breplace\b|\bupdate\b|升级|\bmigrate\b|迁移|拆分|\bsplit\b|合并|\bmerge\b|提取|\bextract\b|改成|改为|换成|转为|异步|同步)/i.test(message);
const intentUnderstand = /(?:how does|怎么工作|怎么实现|怎么做|什么|理解|看看|看一下|了解|分析|explain|understand|架构|architecture|structure|overview|模块|概览|干什么|做什么|工作原理|逻辑|机制|流程|功能|结合度|效率|评估|调研|是什么|有什么|能用不|高效不|达标|起作用|科学|深入思考|源码)/i.test(message);
const intentCallgraph = /(?:who calls|what calls|调用|call(?:graph|er|ee)|trace|链路|追踪|谁调|被谁调|调了谁|上下游|依赖关系|触发|路径|覆盖|介入)/i.test(message);
const intentSearch = /(?:where is|在哪|find|search|搜索|找|locate|哪里用|哪里定义|定义在|实现在|处理没|在源码|加不加)/i.test(message);

// Need entities AND intent, or strong entity signal (qualified names like Foo::bar)
const hasQualifiedSymbol = symbolCandidates.some(s => s.includes('::'));
const hasIntent = intentImpact || intentModify || intentUnderstand || intentCallgraph || intentSearch;
if (!hasIntent && !hasQualifiedSymbol && filePaths.length === 0) {
  process.exit(0);
}

// --- Semantic output prefixes ---
const PREFIXES = {
  impact:    '[code-graph:impact] Blast radius — review before editing:',
  overview:  '[code-graph:structure] Module structure:',
  callgraph: '[code-graph:callgraph] Call relationships:',
  search:    '[code-graph:search] Relevant code:',
};

// --- Run ONE targeted CLI query (per-type cooldown allows different types to fire) ---
let queryType = null;
let result = '';
try {
  // Priority: impact/modify > callgraph > understand/overview > search
  // intentModify + symbol → inject impact so Claude knows blast radius before editing
  if ((intentImpact || intentModify) && symbolCandidates.length > 0 && !isCoolingDown('impact')) {
    queryType = 'impact';
    result = run('code-graph-mcp', ['impact', symbolCandidates[0]]);
  } else if (intentCallgraph && symbolCandidates.length > 0 && !isCoolingDown('callgraph')) {
    queryType = 'callgraph';
    result = run('code-graph-mcp', ['callgraph', symbolCandidates[0], '--depth', '2']);
  } else if (filePaths.length > 0 && (intentUnderstand || !hasIntent) && !isCoolingDown('overview')) {
    queryType = 'overview';
    const dir = filePaths[0].replace(/\/[^/]+$/, '/');
    result = run('code-graph-mcp', ['overview', dir]);
  } else if ((intentSearch || hasQualifiedSymbol) && symbolCandidates.length > 0 && !isCoolingDown('search')) {
    queryType = 'search';
    result = run('code-graph-mcp', ['search', symbolCandidates[0], '--limit', '8']);
  } else if (intentUnderstand && symbolCandidates.length > 0 && !isCoolingDown('search')) {
    queryType = 'search';
    result = run('code-graph-mcp', ['search', symbolCandidates[0], '--limit', '8']);
  }
} catch {
  process.exit(0);
}

if (result && result.trim() && queryType) {
  markCooldown(queryType);
  process.stdout.write(`${PREFIXES[queryType]}\n${result.trim()}\n`);
}

// --- Helpers ---

function run(cmd, args) {
  return execFileSync(cmd, args, {
    cwd,
    timeout: 3000,
    encoding: 'utf8',
    stdio: ['pipe', 'pipe', 'pipe'],
  });
}
