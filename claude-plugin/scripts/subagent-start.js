#!/usr/bin/env node
'use strict';
// FIRST statement, before the other requires — same reason as pre-edit-guide.js.
if (require.main === module) require('./hook-fail-open').installHookFailOpen('SubagentStart');

// SubagentStart hook (P1 #3a), registered by lifecycle.js with the matcher
// `Explore|Plan|general-purpose`.
//
// Explore and Plan subagents do not load CLAUDE.md, so the adopt block that
// tells the main session about the index never reaches them, and they fall
// back to multi-round grep/Read for structural questions. This hands them the
// same facts at spawn: the repo has an index, how big and how fresh, and the
// three CLI commands that answer structural questions from it.
//
// Facts only — no "you must". The hooks reference says imperative text framed
// as out-of-band instructions can trip Claude's prompt-injection defenses
// (https://code.claude.com/docs/en/hooks, "Add context for Claude").
//
// Silent when: no index up the tree, no binary (the commands would not run),
// the health check fails or reports 0 files, or CODE_GRAPH_QUIET_HOOKS=1 (the
// same switch that skips the SessionStart project-map injection).
const { execFileSync } = require('child_process');
const fs = require('fs');
const { hidden } = require('./proc-opts');

// Hard ceiling on the injected text, in characters (spec: ≤400). Every
// subagent spawn pays it in context, and a subagent's context is small.
const MAX_CONTEXT_CHARS = 400;

/**
 * The text a subagent receives, or null when there is nothing true to say.
 * @param {{files?: number, index_age?: string, index_version_stale?: boolean}} report
 *   `health-check --format json` output
 * @param {number} [maxChars] ceiling; a parameter only so tests can reach the
 *   check — the builder's longest output (389) is below the real one
 * @returns {string|null}
 */
function buildSubagentContext(report, maxChars = MAX_CONTEXT_CHARS) {
  if (!report || typeof report !== 'object') return null;
  const files = Number(report.files);
  if (!Number.isFinite(files) || files <= 0) return null;
  const age = typeof report.index_age === 'string' && /^[\w .-]{1,24}$/.test(report.index_age)
    ? report.index_age : null;
  let text = `This repository has a code-graph AST index of ${files} files`;
  text += age ? `, last updated ${age}.` : '.';
  if (report.index_version_stale === true) text += ' A rebuild for a newer extractor is pending.';
  text += ' Structural questions are answered from it through Bash:' +
    ' `code-graph-mcp callgraph <fn>` (callers and callees),' +
    ' `code-graph-mcp show <fn>` (a symbol\'s source and signature),' +
    ' `code-graph-mcp overview <dir>` (symbols of a module grouped by file).';
  return text.length <= maxChars ? text : null;
}

function readStdinJson() {
  try {
    // fd 0, not '/dev/stdin' (ENXIO on socketpair stdin) — as the other hooks.
    return JSON.parse(fs.readFileSync(0, 'utf8'));
  } catch { return null; }
}

function runMain() {
  if (process.env.CODE_GRAPH_QUIET_HOOKS === '1') return;
  const input = readStdinJson();
  if (!input) return;

  const { resolveProjectRoot, indexBuildInProgress } = require('./project-root');
  const root = resolveProjectRoot(typeof input.cwd === 'string' && input.cwd ? input.cwd : process.cwd());
  if (root === null) return;
  // A partial index's counts are no facts, and the commands below would refuse
  // while it is built (D5, as every other hook; E2E 2026-10-16 round 2).
  if (indexBuildInProgress(root)) return;

  const binary = require('./find-binary').findBinary();
  if (!binary) return;

  // Spend the registered budget, not a literal (hooks.test.js "armed must spend").
  const budget = require('./hook-fail-open').remainingMs(1500);
  if (budget === null) return;
  let report;
  try {
    report = JSON.parse(execFileSync(binary, ['health-check', '--format', 'json'], hidden({
      cwd: root,
      timeout: budget,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
      env: { ...process.env, CODE_GRAPH_INTERNAL: '1' },
    })));
  } catch { return; }

  const text = buildSubagentContext(report);
  if (!text) return;
  const { emitEventContext } = require('./hook-emit');
  process.stdout.write(emitEventContext('SubagentStart', text) + '\n');
  // D#163: how many subagents were handed the facts. Whether they then used
  // the index is in their transcripts, not here (scripts/subagent_share.py).
  // The agent type is harness input; only a plain name is kept.
  const agent = typeof input.agent_type === 'string' && /^[\w:.-]{1,64}$/.test(input.agent_type)
    ? input.agent_type : null;
  require('./recommendation-log').recordRecommendation(root, {
    hook: 'subagent', action: 'subagent_context', ...(agent ? { agent } : {}),
  });
}

if (require.main === module) runMain();

module.exports = { buildSubagentContext, MAX_CONTEXT_CHARS };
