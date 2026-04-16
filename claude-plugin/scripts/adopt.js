#!/usr/bin/env node
'use strict';
// adopt / unadopt — writes plugin_code_graph_mcp.md into this project's
// claude-mem memory dir and maintains a sentinel-bracketed index entry in
// MEMORY.md. Idempotent. Used by invited-memory pattern with CODE_GRAPH_QUIET_HOOKS=1.
const fs = require('fs');
const path = require('path');
const os = require('os');

const SENTINEL_BEGIN = '<!-- code-graph-mcp:begin v1 -->';
const SENTINEL_END = '<!-- code-graph-mcp:end -->';
const INDEX_LINE = [
  '- [code-graph-mcp](plugin_code_graph_mcp.md) — 代码理解 12 工具（替代 Grep/Read 多步）：',
  '  - 问"谁调/改动/模块/概念/HTTP/相似" → `get_call_graph`/`impact_analysis`/`module_overview`/`semantic_code_search`/`trace_http_chain`/`find_similar_code`',
  '  - 问"返回型/引用/死码/依赖/架构/看签名" → `ast_search`/`find_references`/`find_dead_code`/`dependency_graph`/`project_map`/`get_ast_node`',
].join('\n');
const TEMPLATE_PATH = path.resolve(__dirname, '..', 'templates', 'plugin_code_graph_mcp.md');
const TARGET_NAME = 'plugin_code_graph_mcp.md';

// Claude Code slug convention: every non-alphanumeric-non-hyphen char → `-`.
// `/mnt/data_ssd/dev/proj` → `-mnt-data-ssd-dev-proj`
// `/home/sds/.claude/x`   → `-home-sds--claude-x`  (double-dash from `/.`)
function memoryDir(cwd = process.cwd(), home = os.homedir()) {
  const slug = cwd.replace(/[^a-zA-Z0-9-]/g, '-');
  return path.join(home, '.claude', 'projects', slug, 'memory');
}

function escapeRegex(s) {
  return s.replace(/[\\/[\]^$.*+?()|{}]/g, '\\$&');
}

// Strip our sentinel block — well-formed first, then self-heal orphan begin/end.
// Shared by adopt (so re-adopt rewrites a stale/malformed block) and unadopt.
function stripSentinelBlock(text) {
  const wellFormed = new RegExp(
    `${escapeRegex(SENTINEL_BEGIN)}[\\s\\S]*?${escapeRegex(SENTINEL_END)}\\n?`, 'g'
  );
  let out = text.replace(wellFormed, '');
  // Orphan BEGIN with no matching END (truncation / partial edit).
  // Strip from BEGIN to the next blank line or EOF — the file is shared with
  // claude-mem-lite, so we must not eat past a blank-line boundary.
  if (out.includes(SENTINEL_BEGIN)) {
    out = out.replace(
      new RegExp(`${escapeRegex(SENTINEL_BEGIN)}[\\s\\S]*?(?=\\n\\n|$)`, 'g'),
      ''
    );
  }
  // Orphan END line by itself.
  if (out.includes(SENTINEL_END)) {
    out = out.split('\n').filter(l => l.trim() !== SENTINEL_END).join('\n');
  }
  // Collapse blank-line runs introduced by stripping mid-paragraph blocks.
  return out.replace(/\n{3,}/g, '\n\n');
}

function platformGuard() {
  if (process.platform === 'win32') {
    return { ok: false, reason: 'windows-not-supported' };
  }
  return null;
}

function adopt({ cwd, home, templatePath } = {}) {
  const blocked = platformGuard();
  if (blocked) return blocked;

  const dir = memoryDir(cwd, home);
  if (!fs.existsSync(dir)) {
    return { ok: false, reason: 'no-memory-dir', dir };
  }
  const target = path.join(dir, TARGET_NAME);
  const tpl = templatePath || TEMPLATE_PATH;
  if (!fs.existsSync(tpl)) {
    return { ok: false, reason: 'no-template', template: tpl };
  }
  fs.copyFileSync(tpl, target);

  const indexPath = path.join(dir, 'MEMORY.md');
  const index = fs.existsSync(indexPath) ? fs.readFileSync(indexPath, 'utf8') : '# Memory Index\n';
  const desiredBlock = `${SENTINEL_BEGIN}\n${INDEX_LINE}\n${SENTINEL_END}`;

  // Already-adopted-and-well-formed: skip the write entirely.
  if (index.includes(desiredBlock)) {
    return { ok: true, target, indexPath, indexed: false, healed: false };
  }

  const cleaned = stripSentinelBlock(index);
  const healed = cleaned !== index;
  const base = cleaned.endsWith('\n') ? cleaned : cleaned + '\n';
  fs.writeFileSync(indexPath, base + desiredBlock + '\n');
  return { ok: true, target, indexPath, indexed: true, healed };
}

// v0.9.0 — "已 adopt" 判定：template 文件在 + MEMORY.md 内有我们的 sentinel 块。
// 用在 maybeAutoAdopt 里做幂等门，也用在 session-init 里推导 quietHooks。
function isAdopted({ cwd, home } = {}) {
  const dir = memoryDir(cwd, home);
  const target = path.join(dir, TARGET_NAME);
  const indexPath = path.join(dir, 'MEMORY.md');
  if (!fs.existsSync(target) || !fs.existsSync(indexPath)) return false;
  const index = fs.readFileSync(indexPath, 'utf8');
  return index.includes(SENTINEL_BEGIN) && index.includes(SENTINEL_END);
}

// 检测脚本是否从 Claude Code 插件 cache 运行。
// 走 __dirname 而非 CLAUDE_PLUGIN_ROOT — 后者在多插件共存时会互相污染
// （见 feedback_plugin_env_isolation.md）。
function isPluginModeInstall(scriptPath = __dirname) {
  const sep = path.sep;
  return scriptPath.includes(`${sep}.claude${sep}plugins${sep}`);
}

// C' 上下文感知默认（v0.9.0）：插件模式下首次 SessionStart 静默 adopt。
// /plugin install 本身已构成知情同意；npm / npx / 裸 checkout 保持 opt-in。
// 退出：CODE_GRAPH_NO_AUTO_ADOPT=1。
function maybeAutoAdopt({ cwd, home, env, scriptPath } = {}) {
  env = env || process.env;
  if (env.CODE_GRAPH_NO_AUTO_ADOPT === '1') {
    return { attempted: false, reason: 'opted-out' };
  }
  if (!isPluginModeInstall(scriptPath || __dirname)) {
    return { attempted: false, reason: 'not-plugin-mode' };
  }
  if (isAdopted({ cwd, home })) {
    return { attempted: false, reason: 'already-adopted' };
  }
  const result = adopt({ cwd, home });
  return { attempted: true, reason: 'adopted', result };
}

function unadopt({ cwd, home } = {}) {
  const blocked = platformGuard();
  if (blocked) return blocked;

  const dir = memoryDir(cwd, home);
  const target = path.join(dir, TARGET_NAME);
  const indexPath = path.join(dir, 'MEMORY.md');
  let fileRemoved = false;
  let indexPruned = false;

  if (fs.existsSync(target)) {
    fs.unlinkSync(target);
    fileRemoved = true;
  }
  if (fs.existsSync(indexPath)) {
    const before = fs.readFileSync(indexPath, 'utf8');
    const after = stripSentinelBlock(before);
    if (after !== before) {
      fs.writeFileSync(indexPath, after);
      indexPruned = true;
    }
  }
  return { ok: true, fileRemoved, indexPruned, target, indexPath };
}

function formatResult(action, result) {
  if (!result.ok && result.reason === 'windows-not-supported') {
    return '[code-graph] adopt/unadopt are POSIX-only — claude-mem-lite slug ' +
           'convention on Windows is unverified. Edit MEMORY.md manually to opt in.';
  }
  if (action === 'adopt') {
    if (!result.ok) {
      if (result.reason === 'no-memory-dir') {
        return `[code-graph] Memory dir not found: ${result.dir}\n` +
               '  Run \`claude\` at least once in this project to create it.';
      }
      if (result.reason === 'no-template') {
        return `[code-graph] Template missing: ${result.template}`;
      }
      return `[code-graph] adopt failed: ${result.reason || 'unknown'}`;
    }
    const lines = [`[code-graph] Adopted → ${result.target}`];
    if (result.healed) lines.push(`[code-graph] Healed malformed sentinel block → ${result.indexPath}`);
    else if (result.indexed) lines.push(`[code-graph] Indexed → ${result.indexPath}`);
    else lines.push(`[code-graph] Index already up-to-date — no write`);
    // v0.9.0: adoption auto-implies quietHooks; no env var needed for the common case.
    lines.push('[code-graph] Active — quietHooks auto-enabled via adopted state.');
    lines.push('[code-graph] Force inject:  CODE_GRAPH_QUIET_HOOKS=0   Force silent: =1');
    return lines.join('\n');
  }
  if (action === 'unadopt') {
    const lines = [];
    if (result.fileRemoved) lines.push(`[code-graph] Removed → ${result.target}`);
    if (result.indexPruned) lines.push(`[code-graph] De-indexed → ${result.indexPath}`);
    if (!result.fileRemoved && !result.indexPruned) lines.push('[code-graph] Nothing to unadopt');
    return lines.join('\n');
  }
  return '';
}

if (require.main === module) {
  const action = process.argv[2] === 'unadopt' ? 'unadopt' : 'adopt';
  const result = action === 'unadopt' ? unadopt() : adopt();
  process.stdout.write(formatResult(action, result) + '\n');
  process.exit(result.ok === false ? 1 : 0);
}

module.exports = {
  adopt, unadopt, memoryDir, formatResult, stripSentinelBlock,
  isAdopted, isPluginModeInstall, maybeAutoAdopt,
  SENTINEL_BEGIN, SENTINEL_END, INDEX_LINE, TEMPLATE_PATH, TARGET_NAME,
};
