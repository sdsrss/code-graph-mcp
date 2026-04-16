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
const INDEX_LINE = '- [code-graph-mcp](plugin_code_graph_mcp.md) — "谁调 X / 改 X 炸啥 / 模块结构" → `get_call_graph` / `impact_analysis` / `module_overview`，替代多步 Grep/Read';
const TEMPLATE_PATH = path.resolve(__dirname, '..', 'templates', 'plugin_code_graph_mcp.md');
const TARGET_NAME = 'plugin_code_graph_mcp.md';

function memoryDir(cwd = process.cwd(), home = os.homedir()) {
  const slug = cwd.replace(/\//g, '-');
  return path.join(home, '.claude', 'projects', slug, 'memory');
}

function escapeRegex(s) {
  return s.replace(/[\\/[\]^$.*+?()|{}]/g, '\\$&');
}

function adopt({ cwd, home, templatePath } = {}) {
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
  let index = fs.existsSync(indexPath) ? fs.readFileSync(indexPath, 'utf8') : '# Memory Index\n';
  let indexed = false;
  if (!index.includes(SENTINEL_BEGIN)) {
    if (!index.endsWith('\n')) index += '\n';
    index += `${SENTINEL_BEGIN}\n${INDEX_LINE}\n${SENTINEL_END}\n`;
    fs.writeFileSync(indexPath, index);
    indexed = true;
  }
  return { ok: true, target, indexPath, indexed };
}

function unadopt({ cwd, home } = {}) {
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
    const re = new RegExp(`${escapeRegex(SENTINEL_BEGIN)}[\\s\\S]*?${escapeRegex(SENTINEL_END)}\\n?`, 'g');
    const after = before.replace(re, '');
    if (after !== before) {
      fs.writeFileSync(indexPath, after);
      indexPruned = true;
    }
  }
  return { ok: true, fileRemoved, indexPruned, target, indexPath };
}

function formatResult(action, result) {
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
    if (result.indexed) lines.push(`[code-graph] Indexed → ${result.indexPath}`);
    else lines.push(`[code-graph] Index already contains sentinel — left as-is`);
    lines.push('[code-graph] Activate: set CODE_GRAPH_QUIET_HOOKS=1 in ~/.claude/settings.json env');
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
  adopt, unadopt, memoryDir, formatResult,
  SENTINEL_BEGIN, SENTINEL_END, INDEX_LINE, TEMPLATE_PATH, TARGET_NAME,
};
