#!/usr/bin/env node
'use strict';
// UserPromptSubmit hook: inject relevant code-graph context based on user's question.
// Only activates when user message references code entities + has understanding intent.
// This is a CODE INDEX, not a memory store — only inject structural code context.
const { execFileSync } = require('child_process');
const fs = require('fs');
const path = require('path');
const os = require('os');

// --- Rate limiting ---
const flag = path.join(os.tmpdir(), '.code-graph-prompt-ctx');
const COOLDOWN_MS = 60 * 1000; // 1 minute between injections
try {
  const stat = fs.statSync(flag);
  if (Date.now() - stat.mtimeMs < COOLDOWN_MS) process.exit(0);
} catch { /* first time */ }

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

// Extract potential symbol names (camelCase, snake_case, PascalCase, qualified like Foo::bar)
const symbolCandidates = (message.match(/\b(?:[A-Z]\w*(?:::\w+)+|[a-z]\w*(?:_\w+){1,}|[a-z]\w*(?:[A-Z]\w*)+|[A-Z][a-z]+(?:[A-Z][a-z]+)+)\b/g) || [])
  .filter(s => s.length > 4)
  .filter(s => !STOP_WORDS.has(s.toLowerCase()))
  .slice(0, 3);

// Detect intent keywords (EN + ZH, derived from user's actual prompt history)
const intentImpact = /(?:impact|影响|修改前|改之前|blast radius|before (?:edit|chang|modif)|risk|风险|改动范围|波及|问题在|bug|干扰|冲突|卡)/i.test(message);
const intentUnderstand = /(?:how does|怎么工作|怎么实现|怎么做|什么|理解|看看|看一下|了解|分析|explain|understand|架构|architecture|structure|overview|模块|概览|干什么|做什么|工作原理|逻辑|机制|流程|功能|结合度|效率|评估|调研|是什么|有什么|能用不|高效不|达标|起作用|科学|深入思考|源码)/i.test(message);
const intentCallgraph = /(?:who calls|what calls|调用|call(?:graph|er|ee)|trace|链路|追踪|谁调|被谁调|调了谁|上下游|依赖关系|触发|路径|覆盖|介入)/i.test(message);
const intentSearch = /(?:where is|在哪|find|search|搜索|找|locate|哪里用|哪里定义|定义在|实现在|处理没|在源码|加不加)/i.test(message);

// Need entities AND intent, or strong entity signal (qualified names like Foo::bar)
const hasQualifiedSymbol = symbolCandidates.some(s => s.includes('::'));
const hasIntent = intentImpact || intentUnderstand || intentCallgraph || intentSearch;
if (!hasIntent && !hasQualifiedSymbol && filePaths.length === 0) {
  process.exit(0);
}

// --- Run ONE targeted CLI query ---
let result = '';
try {
  if (intentImpact && symbolCandidates.length > 0) {
    result = run(`code-graph-mcp impact "${symbolCandidates[0]}"`);
  } else if (filePaths.length > 0 && intentUnderstand) {
    // Overview of the mentioned file's directory
    const dir = filePaths[0].replace(/\/[^/]+$/, '/');
    result = run(`code-graph-mcp overview "${dir}"`);
  } else if (intentCallgraph && symbolCandidates.length > 0) {
    result = run(`code-graph-mcp callgraph "${symbolCandidates[0]}" --depth 2`);
  } else if ((intentSearch || hasQualifiedSymbol) && symbolCandidates.length > 0) {
    result = run(`code-graph-mcp search "${symbolCandidates[0]}" --limit 8`);
  } else if (filePaths.length > 0) {
    const dir = filePaths[0].replace(/\/[^/]+$/, '/');
    result = run(`code-graph-mcp overview "${dir}"`);
  }
} catch {
  process.exit(0);
}

if (result && result.trim()) {
  fs.writeFileSync(flag, ''); // update cooldown
  process.stdout.write(result.trim() + '\n');
}

// --- Helpers ---

function run(cmd) {
  const parts = cmd.match(/(?:[^\s"]+|"[^"]*")+/g) || [];
  const args = parts.slice(1).map(a => a.replace(/^"|"$/g, ''));
  return execFileSync(parts[0], args, {
    cwd,
    timeout: 3000,
    encoding: 'utf8',
    stdio: ['pipe', 'pipe', 'pipe'],
  });
}
