#!/usr/bin/env node
'use strict';
// adopt / unadopt — writes plugin_code_graph_mcp.md into this project's
// Claude Code auto-memory dir (~/.claude/projects/<slug>/memory/, also
// read/written by claude-mem-lite) and maintains a sentinel-bracketed index
// entry in MEMORY.md. Idempotent. Used by invited-memory pattern with
// CODE_GRAPH_QUIET_HOOKS=1.
const fs = require('fs');
const path = require('path');
const os = require('os');

const SENTINEL_BEGIN = '<!-- code-graph-mcp:begin v1 -->';
const SENTINEL_END = '<!-- code-graph-mcp:end -->';
// Collision-detection marker. Slug encoding `[^a-zA-Z0-9-]→'-'` is lossy,
// so two cwds (e.g. /foo/bar and /foo bar) can resolve to the same memory
// dir. Adopt records its absolute cwd as the file's first-line HTML comment;
// re-adopt from a different cwd surfaces a warning.
const ADOPTED_BY_RE = /^<!-- adopted-by: (.+?) -->\r?\n?/;
function readAdoptedBy(filePath) {
  try {
    const first = fs.readFileSync(filePath, 'utf8').split('\n', 1)[0];
    const m = first.match(/^<!-- adopted-by: (.+?) -->/);
    return m ? m[1] : null;
  } catch { return null; }
}
const INDEX_LINE = [
  '- [code-graph-mcp](plugin_code_graph_mcp.md) — v0.10.0 起 tools/list 默认 7 核心 + 5 隐藏可调（省启动 token）',
  '  - 核心 7（默认暴露）：`get_call_graph`/`module_overview`/`semantic_code_search`/`ast_search`/`find_references`/`get_ast_node`/`project_map`',
  '  - 进阶 5（隐藏按名可调）：`impact_analysis`/`trace_http_chain`/`dependency_graph`/`find_similar_code`/`find_dead_code`',
  '  - 场景速查（优先于 Grep）：',
  '    - 改 X 影响面 → `get_ast_node symbol=X include_impact=true`（或 CLI `code-graph-mcp impact X`）',
  '    - 谁调用 X / X 被谁用 → `get_call_graph X` 或 `find_references X`',
  '    - 看 X 源码 / 签名 → `get_ast_node symbol=X`',
  '    - Y 模块长啥样 → `module_overview` 或 CLI `code-graph-mcp overview Y/`',
  '    - 概念查询（不知精确名）→ `semantic_code_search "Z"`；字面匹配用 Grep',
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

// Project-marker check: cwd looks like a real project (not /tmp / $HOME).
// Used to gate auto-mkdir of the auto-memory dir so adopt doesn't pollute
// random directories. Mirrors the markers Claude Code itself recognizes.
const PROJECT_MARKERS = [
  '.git', '.code-graph', 'package.json', 'Cargo.toml',
  'pyproject.toml', 'go.mod', 'pom.xml', 'build.gradle',
];
function isProjectRoot(cwd) {
  return PROJECT_MARKERS.some(m => fs.existsSync(path.join(cwd, m)));
}

function adopt({ cwd, home, templatePath } = {}) {
  const blocked = platformGuard();
  if (blocked) return blocked;

  const effectiveCwd = cwd || process.cwd();
  const dir = memoryDir(cwd, home);
  if (!fs.existsSync(dir)) {
    // Auto-create only when cwd has a project marker. Without markers the
    // user is likely in /tmp or $HOME, where adopt would litter
    // ~/.claude/projects/ with bogus slugs.
    if (!isProjectRoot(effectiveCwd)) {
      return { ok: false, reason: 'not-a-project', dir, cwd: effectiveCwd };
    }
    fs.mkdirSync(dir, { recursive: true });
  }
  const target = path.join(dir, TARGET_NAME);
  const tpl = templatePath || TEMPLATE_PATH;
  if (!fs.existsSync(tpl)) {
    return { ok: false, reason: 'no-template', template: tpl };
  }
  // Slug-collision detection: read prior adopted-by marker before overwrite.
  let collisionWith = null;
  if (fs.existsSync(target)) {
    const prevCwd = readAdoptedBy(target);
    if (prevCwd && prevCwd !== effectiveCwd) collisionWith = prevCwd;
  }
  // Write marker + template. Marker is HTML comment → invisible in rendered
  // markdown but preserved by needsRefresh's bytewise compare (skipped via
  // ADOPTED_BY_RE strip below).
  const tplBody = fs.readFileSync(tpl);
  const marker = Buffer.from(`<!-- adopted-by: ${effectiveCwd} -->\n`);
  fs.writeFileSync(target, Buffer.concat([marker, tplBody]));

  const indexPath = path.join(dir, 'MEMORY.md');
  const index = fs.existsSync(indexPath) ? fs.readFileSync(indexPath, 'utf8') : '# Memory Index\n';
  const desiredBlock = `${SENTINEL_BEGIN}\n${INDEX_LINE}\n${SENTINEL_END}`;

  // Already-adopted-and-well-formed: skip the write entirely.
  if (index.includes(desiredBlock)) {
    return { ok: true, target, indexPath, indexed: false, healed: false, collisionWith };
  }

  const cleaned = stripSentinelBlock(index);
  const healed = cleaned !== index;
  const base = cleaned.endsWith('\n') ? cleaned : cleaned + '\n';
  fs.writeFileSync(indexPath, base + desiredBlock + '\n');
  return { ok: true, target, indexPath, indexed: true, healed, collisionWith };
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

// v0.11.0 — shipped template / INDEX_LINE 与已落地版本出现漂移时返回 true。
// 让已 adopt 的项目在下次 SessionStart 自动对齐到插件最新决策表，避免"老用户
// 永远停留在首次 adopt 时的 snapshot"。手动编辑会被覆盖——锁定方式：
// CODE_GRAPH_NO_TEMPLATE_REFRESH=1。
function needsRefresh({ cwd, home, templatePath } = {}) {
  const dir = memoryDir(cwd, home);
  const target = path.join(dir, TARGET_NAME);
  const indexPath = path.join(dir, 'MEMORY.md');
  const tpl = templatePath || TEMPLATE_PATH;
  if (!fs.existsSync(target) || !fs.existsSync(tpl) || !fs.existsSync(indexPath)) {
    return false;
  }
  const shipped = fs.readFileSync(tpl);
  const current = fs.readFileSync(target);
  // Strip the leading "<!-- adopted-by: ... -->\n" collision marker (D fix)
  // before bytewise comparing — its presence/path naturally diverges from
  // the shipped template.
  let body = current;
  const nl = current.indexOf(0x0a);
  if (nl > 0 && ADOPTED_BY_RE.test(current.subarray(0, nl + 1).toString())) {
    body = current.subarray(nl + 1);
  }
  if (!shipped.equals(body)) return true;
  const index = fs.readFileSync(indexPath, 'utf8');
  const desiredBlock = `${SENTINEL_BEGIN}\n${INDEX_LINE}\n${SENTINEL_END}`;
  return !index.includes(desiredBlock);
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
    // v0.11.0: shipped template / INDEX_LINE 漂移时重跑 adopt 对齐。
    // opt-out: CODE_GRAPH_NO_TEMPLATE_REFRESH=1（锁定手动编辑）。
    if (env.CODE_GRAPH_NO_TEMPLATE_REFRESH !== '1' && needsRefresh({ cwd, home })) {
      const result = adopt({ cwd, home });
      return { attempted: true, reason: 'refreshed', result };
    }
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
      if (result.reason === 'not-a-project') {
        return `[code-graph] Not a project root: ${result.cwd}\n` +
               '  No project marker (.git, Cargo.toml, package.json, pyproject.toml, ...).\n' +
               '  cd into a real project before running adopt.';
      }
      if (result.reason === 'no-template') {
        return `[code-graph] Template missing: ${result.template}`;
      }
      return `[code-graph] adopt failed: ${result.reason || 'unknown'}`;
    }
    const lines = [`[code-graph] Adopted → ${result.target}`];
    if (result.collisionWith) {
      lines.push(`[code-graph] ⚠ slug collision: this dir was previously adopted by ${result.collisionWith}.`);
      lines.push('[code-graph]   Memory dir is shared — sentinels overwritten. ' +
                 'Investigate path encoding clash (Claude Code slug = path with non-[a-zA-Z0-9-] → "-").');
    }
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
  isAdopted, isPluginModeInstall, maybeAutoAdopt, needsRefresh, isProjectRoot,
  SENTINEL_BEGIN, SENTINEL_END, INDEX_LINE, TEMPLATE_PATH, TARGET_NAME,
  PROJECT_MARKERS,
};
