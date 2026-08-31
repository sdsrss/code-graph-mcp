#!/usr/bin/env node
'use strict';
// UserPromptSubmit hook: inject relevant code-graph RESULTS based on user's intent.
// Strategy: PUSH structural context (not suggestions) that Grep/Read cannot provide.
// This is a CODE INDEX — only inject structural code context (impact, overview, callgraph).
const { execFileSync } = require('child_process');
const fs = require('fs');
const path = require('path');
const os = require('os');
// Hook cooldown flags + the restart notice go under cgTmpDir() (a code-graph-mcp/
// subdir of os.tmpdir()), NOT bare os.tmpdir(): Claude Code overrides $TMPDIR to
// ~/.claude/tmp/, so bare-tmp flags interleave with transcript captures —
// diagnostic blindness + the §8 recursive-grep footgun (see tmp-dir.js). The
// other hook scripts already route through here; this one was the lone holdout.
const { cgTmpDir, cwdHash } = require('./tmp-dir');
const { hidden } = require('./proc-opts');

// Mid-session install detection: hook fires but no manifest yet.
const MANIFEST_PATH = path.join(os.homedir(), '.cache', 'code-graph', 'install-manifest.json');

// --- Per-type rate limiting (replaces single global cooldown) ---
const COOLDOWNS = {
  impact:    30 * 1000,     // 30s — impact context changes during rapid edits
  overview:  5 * 60 * 1000, // 5min — module structure rarely changes mid-session
  callgraph: 60 * 1000,     // 1min
  search:    60 * 1000,     // 1min
  symptom:   10 * 60 * 1000, // 10min — Phase E meta-advisory hint, low value to repeat
};

// Project-scoped (see cwdHash in tmp-dir.js). There are only five flag names
// here, so an un-scoped flag was effectively a machine-wide mute: one `impact`
// push in any repo silenced the next 30s of impact pushes in every other repo,
// and `overview` did it for five minutes.
function ctxFlagPath(type, cwd) {
  return path.join(cgTmpDir(), `.code-graph-ctx-${cwdHash(cwd)}-${type}`);
}

function isCoolingDown(type, cwd = process.cwd()) {
  try {
    const stat = fs.statSync(ctxFlagPath(type, cwd));
    return Date.now() - stat.mtimeMs < (COOLDOWNS[type] || 60000);
  } catch { return false; }
}

function markCooldown(type, cwd = process.cwd()) {
  try {
    fs.writeFileSync(ctxFlagPath(type, cwd), '');
  } catch { /* ok */ }
}

// Default ON. The v0.21 opt-in flip relied on routing-bench P@1=100% to argue
// "Sonnet 4.5 picks tools without push injection." That bench measures *triage
// accuracy once the agent has decided to query a tool*; it does not measure
// the prior question — *whether the agent reaches for a tool at all*.
// pre-grep-guide.js sees the real-world counter-evidence: a 15-day baseline of
// 429 raw `grep` vs 191 functional CLI calls on the same indexed source tree
// — a 13× pre-training bias toward grep. Push injection on the relevant turns
// is the corrective; cooldowns (impact 30s / overview 5min / callgraph 60s /
// search 60s) already cap per-type frequency. SessionStart `project_map` stays
// default OFF (lifecycle.js / session-init.js) — that one is a static dump
// duplicated by MEMORY.md; this hook is reactive and trigger-shaped, so the
// two defaults diverge intentionally.
//
// Priority (high → low):
//   1. CODE_GRAPH_QUIET_HOOKS=1 → forced quiet (escape hatch)
//   2. CODE_GRAPH_QUIET_HOOKS=0 → forced noisy (legacy back-compat, redundant with default)
//   3. CODE_GRAPH_VERBOSE_HOOKS=1 → noisy (legacy back-compat, redundant with default)
//   4. default → noisy
function computeQuietHooks(env = process.env) {
  const envQuiet = env.CODE_GRAPH_QUIET_HOOKS;
  if (envQuiet === '1') return true;
  if (envQuiet === '0') return false;
  if (env.CODE_GRAPH_VERBOSE_HOOKS === '1') return false;
  return false;
}

// --- Pure logic (exported for testing) ---

const STOP_WORDS = new Set([
  'this', 'that', 'with', 'from', 'what', 'when', 'which', 'there',
  'their', 'these', 'those', 'have', 'been', 'some', 'will', 'would',
  'could', 'should', 'about', 'after', 'before', 'other', 'every',
  'where', 'while', 'first', 'under', 'still', 'between', 'without',
  'being', 'through', 'default', 'function', 'method', 'class',
]);

const PLAIN_WORD_EXCLUDE = /^(possible|together|actually|something|different|important|following|available|necessary|currently|implement|operation|otherwise|beginning|knowledge|attention|according|certainly|sometimes|direction|recommend|structure|describe|question|complete|generate|anything|continue|consider|response|approach|happened|recently|probably|expected|previous|original|specific|directly|received|required|supposed|separate|designed|finished|provided|included|prepared|combined|properly|remember|whatever|although|document|handling|existing|everyone|standard|research|personal|relative|absolute|practice|language|thousand|national|evidence|refactor|understand|validate|analysis|debugging|configure|improving|resolving|creating|building|checking|updating|removing|changing|searching|cleaning|optimize|migration|overview|introduce|reviewing|thinking|managing|starting|yourself|features|problems|breaking|requires|argument|settings|includes|examples|comments|patterns|tutorial|concepts|supports|priority|organize|scenario|tracking|internal|external|abstract|concrete|strategy|evaluate|diagnose|platform|variable|optional|multiple)$/;

function shouldSkip(msg) {
  const trimmed = msg.trim();
  if (/^(yes|no|ok|commit|push|y|n|done|thanks|thank you|继续|确认|好的|好|是的|不|可以|行|对|提交|推送|没问题|谢谢|发布|更新|编译|安装|卸载|重启|重连|清理)\s*[.!?。！？]?\s*$/i.test(trimmed)) return 'simple';
  if (/^(修复|实施|执行|开始|按|实测|进入|用|重新)/.test(trimmed) && !/[a-zA-Z_]{3,}/.test(trimmed)) return 'action-only';
  return false;
}

function extractFilePaths(msg) {
  return (msg.match(/(?:src|lib|test|pkg|cmd|internal|app|components?)\/[\w/.-]+/g) || []).slice(0, 2);
}

function extractSymbols(msg) {
  const candidates = (msg.match(/\b(?:[A-Z]\w*(?:(?:::|\.)\w+)+|[a-z]\w*(?:_\w+){1,}|[a-z]\w*(?:[A-Z]\w*)+|[A-Z][a-z]+(?:[A-Z][a-z]+)+)\b/g) || [])
    .filter(s => s.length > 4)
    .filter(s => !STOP_WORDS.has(s.toLowerCase()))
    .slice(0, 3);

  if (candidates.length === 0) {
    const backtickSymbols = (msg.match(/`([a-zA-Z_]\w{2,})`/g) || [])
      .map(s => s.replace(/`/g, ''))
      .filter(s => s.length >= 3 && !STOP_WORDS.has(s.toLowerCase()));
    candidates.push(...backtickSymbols.slice(0, 3));
  }

  let lowConfidence = false;
  if (candidates.length === 0) {
    const plain = (msg.match(/\b[a-z][a-z]{7,}\b/g) || [])
      .filter(s => !STOP_WORDS.has(s))
      .filter(s => !PLAIN_WORD_EXCLUDE.test(s));
    candidates.push(...plain.slice(0, 2));
    if (candidates.length > 0) lowConfidence = true;
  }

  return { symbols: candidates, lowConfidence };
}

// v0.21 — replaced 6 mixed-language regex piles with per-keyword weighted
// patterns. Each row is testable in isolation, weights ready for tuning when
// false-positive data accumulates. Threshold 0.5 + uniform weight 1.0
// preserves the original OR-of-alternatives behavior 1:1; future tuning can
// downweight noisy short keywords like "bug" or "什么" that currently fire
// too eagerly. Maintenance cost: ~150 lines of table vs 6 × 200-char regex —
// regression history (#5754, #7713) shows the regex form was the higher
// silent-bug surface.
const INTENT_PATTERNS = {
  impact: [
    [/impact/i, 1.0],
    [/影响/, 1.0],
    [/修改前/, 1.0],
    [/改之前/, 1.0],
    [/blast radius/i, 1.0],
    [/before (?:edit|chang|modif)/i, 1.0],
    [/risk/i, 1.0],
    [/风险/, 1.0],
    [/改动范围/, 1.0],
    [/波及/, 1.0],
    [/问题在/, 1.0],
    [/bug/i, 1.0],
    [/干扰/, 1.0],
    [/冲突/, 1.0],
    [/卡/, 1.0],
  ],
  modify: [
    [/改(?!变)/, 1.0],
    [/修改/, 1.0],
    [/修复/, 1.0],
    [/重构/, 1.0],
    [/优化/, 1.0],
    [/简化/, 1.0],
    [/精简/, 1.0],
    [/适配/, 1.0],
    [/统一/, 1.0],
    [/修正/, 1.0],
    [/调整/, 1.0],
    [/去掉/, 1.0],
    [/整理/, 1.0],
    [/清理/, 1.0],
    [/解耦/, 1.0],
    [/更新/, 1.0],
    [/\brefactor\b/i, 1.0],
    [/\bchange\b/i, 1.0],
    [/\brename\b/i, 1.0],
    [/\bfix\b/i, 1.0],
    [/移动/, 1.0],
    [/\bmove\b/i, 1.0],
    [/删(?!除文件)/, 1.0],
    [/\bremove\b/i, 1.0],
    [/替换/, 1.0],
    [/\breplace\b/i, 1.0],
    [/\bupdate\b/i, 1.0],
    [/升级/, 1.0],
    [/\bmigrate\b/i, 1.0],
    [/迁移/, 1.0],
    [/拆分/, 1.0],
    [/\bsplit\b/i, 1.0],
    [/合并/, 1.0],
    [/\bmerge\b/i, 1.0],
    [/提取/, 1.0],
    [/\bextract\b/i, 1.0],
    [/改成/, 1.0],
    [/改为/, 1.0],
    [/换成/, 1.0],
    [/转为/, 1.0],
    [/异步/, 1.0],
    [/同步/, 1.0],
  ],
  implement: [
    [/\badd\b/i, 1.0],
    [/\bimplement\b/i, 1.0],
    [/\bcreate\b/i, 1.0],
    [/\bbuild\b/i, 1.0],
    [/\bwrite\b/i, 1.0],
    [/新增/, 1.0],
    [/添加/, 1.0],
    [/实现/, 1.0],
    [/创建/, 1.0],
    [/编写/, 1.0],
    [/开发/, 1.0],
    [/增加/, 1.0],
    [/加上/, 1.0],
    [/加个/, 1.0],
    [/写/, 1.0],
    [/做个/, 1.0],
    [/搭建/, 1.0],
    [/补充/, 1.0],
    [/引入/, 1.0],
    [/支持/, 1.0],
    [/封装/, 1.0],
    [/接入/, 1.0],
    [/对接/, 1.0],
    [/配置/, 1.0],
  ],
  understand: [
    [/how does/i, 1.0],
    [/怎么工作/, 1.0],
    [/怎么实现/, 1.0],
    [/怎么做/, 1.0],
    [/什么/, 1.0],
    [/理解/, 1.0],
    [/看看/, 1.0],
    [/看一下/, 1.0],
    [/了解/, 1.0],
    [/分析/, 1.0],
    [/explain/i, 1.0],
    [/understand/i, 1.0],
    [/架构/, 1.0],
    [/architecture/i, 1.0],
    [/structure/i, 1.0],
    [/overview/i, 1.0],
    [/模块/, 1.0],
    [/概览/, 1.0],
    [/干什么/, 1.0],
    [/做什么/, 1.0],
    [/工作原理/, 1.0],
    [/逻辑/, 1.0],
    [/机制/, 1.0],
    [/流程/, 1.0],
    [/功能/, 1.0],
    [/结合度/, 1.0],
    [/效率/, 1.0],
    [/评估/, 1.0],
    [/调研/, 1.0],
    [/是什么/, 1.0],
    [/有什么/, 1.0],
    [/能用不/, 1.0],
    [/高效不/, 1.0],
    [/达标/, 1.0],
    [/起作用/, 1.0],
    [/科学/, 1.0],
    [/深入思考/, 1.0],
    [/源码/, 1.0],
    [/检查/, 1.0],
    [/审核/, 1.0],
    [/审查/, 1.0],
    [/验证/, 1.0],
    [/诊断/, 1.0],
  ],
  callgraph: [
    [/who calls/i, 1.0],
    [/what calls/i, 1.0],
    [/调用/, 1.0],
    [/call(?:graph|er|ee)/i, 1.0],
    [/trace/i, 1.0],
    [/链路/, 1.0],
    [/追踪/, 1.0],
    [/谁调/, 1.0],
    [/被谁调/, 1.0],
    [/调了谁/, 1.0],
    [/上下游/, 1.0],
    [/依赖关系/, 1.0],
    [/触发/, 1.0],
    [/路径/, 1.0],
    [/覆盖/, 1.0],
    [/介入/, 1.0],
  ],
  search: [
    [/where is/i, 1.0],
    [/在哪/, 1.0],
    [/find/i, 1.0],
    [/search/i, 1.0],
    [/搜索/, 1.0],
    [/找/, 1.0],
    [/locate/i, 1.0],
    [/哪里用/, 1.0],
    [/哪里定义/, 1.0],
    [/定义在/, 1.0],
    [/实现在/, 1.0],
    [/处理没/, 1.0],
    [/在源码/, 1.0],
    [/加不加/, 1.0],
  ],
};

const INTENT_THRESHOLD = 0.5;

function scoreIntent(msg, intent) {
  const patterns = INTENT_PATTERNS[intent];
  if (!patterns) return 0;
  let max = 0;
  for (const [pattern, weight] of patterns) {
    if (pattern.test(msg) && weight > max) max = weight;
  }
  return max;
}

function detectIntents(msg) {
  return {
    impact: scoreIntent(msg, 'impact') >= INTENT_THRESHOLD,
    modify: scoreIntent(msg, 'modify') >= INTENT_THRESHOLD,
    implement: scoreIntent(msg, 'implement') >= INTENT_THRESHOLD,
    understand: scoreIntent(msg, 'understand') >= INTENT_THRESHOLD,
    callgraph: scoreIntent(msg, 'callgraph') >= INTENT_THRESHOLD,
    search: scoreIntent(msg, 'search') >= INTENT_THRESHOLD,
  };
}

// Phase E: symptom-driven fallback. The 7d audit + TriggerRate hard-oracle
// baseline (60%) show the failure mode "user phrases a problem without giving
// a symbol/path/file — Claude defaults to bash grep". The 4 existing channels
// (intent / qualified symbol / file path / any symbol) all miss on these
// prompts. SYMPTOM_PATTERNS catches the bug-flavored prompts so we can emit
// ONE-LINE prose hint (no CLI execution — different from the other 4 paths
// that inject results). Keeps noise low while planting the routing seed.
const SYMPTOM_PATTERNS = [
  // English symptom / failure-mode markers
  /\bbug\b/i,
  /\bcrash(?:ed|ing|es)?\b/i,
  /\bbroken\b/i,
  /\bnot work(?:ing)?\b/i,
  /\bdoesn'?t work\b/i,
  /\bfail(?:ed|ing|s|ure)?\b/i,
  /\bwhy (?:does|is|are|isn'?t|doesn'?t|won'?t)/i,
  /\bmissing\b/i,
  // Chinese symptom / failure markers
  /有\s?bug/i,
  /又挂/,
  /又失败/,
  /挂了/,
  /失败了/,
  /卡死/,
  /卡住/,
  /不准/,
  /不对/,
  /缺失/,
  /丢失/,
  /没响应/,
  /出错/,
  /报错/,
  /为什么/,
  /怎么(?:修|解决)/,
  /哪里[\s\S]{0,5}(?:错|有问题|不对)/, // 哪里写错 / 哪里出错 / 哪里有问题 / 哪里报错了
  /出了什么问题/,
];

function hasSymptom(msg) {
  if (!msg || typeof msg !== 'string') return false;
  return SYMPTOM_PATTERNS.some(p => p.test(msg));
}

function determineQueryType(intents, symbols, filePaths, isCoolingDownFn, message = '') {
  const hasStrict = symbols.symbols.length > 0 && !symbols.lowConfidence;
  const hasQualified = symbols.symbols.some(s => s.includes('::'));
  const hasAny = intents.impact || intents.modify || intents.implement || intents.understand || intents.callgraph || intents.search;

  const cd = isCoolingDownFn || (() => false);

  if ((intents.impact || intents.modify) && hasStrict && !cd('impact')) return { type: 'impact', symbol: symbols.symbols[0] };
  if (intents.callgraph && hasStrict && !cd('callgraph')) return { type: 'callgraph', symbol: symbols.symbols[0] };
  if (filePaths.length > 0 && !cd('overview')) return { type: 'overview', path: filePaths[0].replace(/\/[^/]+$/, '/') };
  if ((intents.search || intents.implement || hasQualified) && symbols.symbols.length > 0 && !cd('search')) return { type: 'search', symbol: symbols.symbols[0] };
  if ((intents.understand || !hasAny) && symbols.symbols.length > 0 && !cd('search')) return { type: 'search', symbol: symbols.symbols[0] };

  // Phase E fallback: nothing actionable above, but message has symptom phrasing.
  // Emit ONE-LINE prose hint (no CLI execution). Default-empty `message` keeps
  // backward-compat for callers that don't pass it (legacy `analyze` helpers).
  if (message && hasSymptom(message) && !cd('symptom')) {
    return { type: 'symptom-hint' };
  }

  return null;
}

// Hook-internal CLI runs are PUSH deliveries, not model-initiated conversions.
// Tagging them CODE_GRAPH_INTERNAL=1 keeps record_cli_use (src/cli.rs) from
// logging them as `use` events — otherwise this hook's own injected callgraph/
// overview/search results read back as genuine consumer adoption (2026-06-23 mem
// audit: 100 phantom "model CLI calls" were this hook crediting its own deliveries).
function buildRunEnv(base = process.env) {
  return { ...base, CODE_GRAPH_INTERNAL: '1' };
}

// --- Main execution (only when run directly) ---
// All exit-on-condition checks (manifest, computeQuietHooks, message length,
// db presence) live INSIDE this guard so `require()` from tests doesn't
// terminate the test process on module load.
function runMain() {
  // Escape hatch first: CODE_GRAPH_QUIET_HOOKS=1 must silence EVERYTHING,
  // including the mid-session install notice below. On a fresh install (no
  // manifest) that notice otherwise leaks to stdout even under quiet — and any
  // stdout/stderr leak lands in Claude's display. (The dev box has a manifest,
  // so this only surfaced once user-prompt-context.test.js ran in CI.)
  if (computeQuietHooks()) return;

  // Mid-session install: lifecycle.js install() hasn't run yet (no manifest).
  // MCP server only starts at session startup — tell the user to restart.
  if (!fs.existsSync(MANIFEST_PATH)) {
    const noticeFile = path.join(cgTmpDir(), '.code-graph-mcp-restart-notice');
    try {
      // Show once per hour to avoid spam
      if (Date.now() - fs.statSync(noticeFile).mtimeMs < 3600000) return;
    } catch { /* first notice */ }
    try { fs.writeFileSync(noticeFile, ''); } catch { /* ok */ }
    process.stdout.write(
      '[code-graph] Plugin installed — MCP server requires a session restart to connect.\n' +
      'MCP servers are only initialized at session startup. To activate:\n' +
      '  1. Press Ctrl+C to exit the current session\n' +
      '  2. Re-run `claude` to start a new session\n' +
      'Meanwhile, CLI tools work directly: code-graph-mcp search <query>, code-graph-mcp map, etc.\n'
    );
    return;
  }

  // --- Read user message ---
  let message;
  try {
    // fd 0, not '/dev/stdin': the path form open(2)s the symlink target, which
    // fails with ENXIO when stdin is a socketpair (e.g. spawnSync {input}).
    // Reading the fd directly works for pipes, sockets, and files alike. Five
    // sibling hooks already did this; this one was the holdout, so it read
    // nothing under exactly the conditions verifyHooksFire spawns it with.
    const input = JSON.parse(fs.readFileSync(0, 'utf8'));
    // `prompt` FIRST: that is the field Claude Code puts the user's text in on
    // UserPromptSubmit, and this hook read only `message` — so in production it
    // parsed a payload, found nothing, and exited 0 in silence. The whole
    // intent-driven injection surface (impact / callgraph / overview / search +
    // the symptom hint) was dead from the day it shipped, and its adoption
    // metrics read as a structural zero.
    //
    // Two things hid it, both worth naming because either alone would have been
    // enough: the entire test suite fed `{message:…}` — a self-consistent copy
    // of the defect — and `verifyHooksFire` only asserts exit 0, which is
    // exactly what a hook that reads nothing does. The production tell was
    // outside both: zero `.code-graph-ctx-*` cooldown flags in a heavily
    // dogfooded tmp dir that held 93 flags from the sibling hooks
    // (audit 2026-08-29 JS-01).
    //
    // `message` is kept as a fallback rather than replaced — it costs one `||`
    // and covers any host that spells it the other way.
    message = (input && (input.prompt || input.message)) || '';
  } catch {
    return;
  }
  // Chinese chars are ~3 bytes but 1 char; "看看 fts5_search" is only 16 chars
  if (!message || message.length < 8) return;

  // --- Check index --- (canonical root: worktree → main checkout, subdir →
  // project root — the bare cwd gate left this hook dark there, sibling of the
  // pre-*-guide subdir-cwd class)
  const { resolveProjectRoot } = require('./project-root');
  const cwd = resolveProjectRoot(process.cwd()) || process.cwd();
  const dbPath = path.join(cwd, '.code-graph', 'index.db');
  if (!fs.existsSync(dbPath)) return;

  if (shouldSkip(message)) return;

  const filePaths = extractFilePaths(message);
  const symbols = extractSymbols(message);
  const intents = detectIntents(message);
  // Key the cooldowns on the resolved project root, not the raw shell cwd, so a
  // `cd` into a subdir does not read as a different project and re-fire.
  const query = determineQueryType(intents, symbols, filePaths, (type) => isCoolingDown(type, cwd), message);

  if (!query) return;

  // Phase E: symptom-hint is prose-only (no CLI execution). Emit + cooldown
  // before the result-fetching paths so it can short-circuit cleanly.
  if (query.type === 'symptom-hint') {
    markCooldown('symptom', cwd);
    process.stdout.write(
      '[code-graph:hint] indexed repo — for vague-symptom prompts, try `semantic_code_search "<symptom>"` ' +
      'or `module_overview <suspected-dir>` to surface candidate code structurally. Skip if not searching code.\n'
    );
    return;
  }

  const PREFIXES = {
    impact:    '[code-graph:impact] Blast radius — review before editing:',
    overview:  '[code-graph:structure] Module structure:',
    callgraph: '[code-graph:callgraph] Call relationships:',
    search:    '[code-graph:search] Relevant code:',
  };

  // Resolve the binary the way every other hook does. The bare name relied on
  // `code-graph-mcp` being on PATH, which it is not for plugin-only installs
  // (the binary lives in ~/.cache/code-graph/bin), and when it IS on PATH it may
  // be an unrelated or years-stale global shim — the same finding cg-answer.js
  // and session-init.js were already fixed for. Null means no engine: nothing to
  // run, and nothing to say about it.
  const binary = require('./find-binary').findBinary();
  if (!binary) return;

  function run(args) {
    return execFileSync(binary, args, hidden({
      cwd,
      timeout: 3000,
      encoding: 'utf8',
      stdio: ['pipe', 'pipe', 'pipe'],
      env: buildRunEnv(),
    }));
  }

  // Stamp the cooldown on ATTEMPT, not on success. Keyed on the outcome, a
  // binary that fails or times out produced no flag, so the very next prompt of
  // the same shape re-ran it — a 3s execFileSync timeout on EVERY turn, which is
  // the worst case wearing the cheapest disguise. The cooldown's job is rate
  // limiting; whether the run had something to say is a separate question.
  markCooldown(query.type, cwd);

  try {
    let result = '';
    if (query.type === 'impact') result = run(['impact', query.symbol]);
    else if (query.type === 'callgraph') result = run(['callgraph', query.symbol, '--depth', '2']);
    else if (query.type === 'overview') result = run(['overview', query.path]);
    else if (query.type === 'search') result = run(['search', query.symbol, '--limit', '8']);

    if (result && result.trim()) {
      process.stdout.write(`${PREFIXES[query.type]}\n${result.trim()}\n`);
    }
  } catch {
    /* return silently */
  }
}

if (require.main === module) {
  runMain();
}

module.exports = { COOLDOWNS, shouldSkip, extractFilePaths, extractSymbols, detectIntents, scoreIntent, INTENT_PATTERNS, INTENT_THRESHOLD, determineQueryType, computeQuietHooks, STOP_WORDS, PLAIN_WORD_EXCLUDE, hasSymptom, SYMPTOM_PATTERNS, buildRunEnv };
