#!/usr/bin/env node
'use strict';
// Replays pre-grep-guide's decision chain (runMain, up to the answer) over
// extracted commands, with no side effects: no cooldown flags, no
// recommendations.jsonl rows, no answer child. Steps after the answer
// (no-hits, unavailable, searchesSameFiles, cooldown) are not replayed, so an
// `ANSWER:*` row is "would have been answered", an upper bound on rewrites.
//
// Mirrors runMain as of 0.180.0. Re-read runMain before trusting it on a
// later tree: a new early return there is a step missing here.
//
// usage: node funnel.js <tree-with-claude-plugin/> <commands.jsonl> <cut-ISO-ts>
// writes <commands.jsonl>.funnel.jsonl (each row + `step`) and prints counts
// per era (before / after the cut), split by worktree cwd.
const fs = require('fs');
const path = require('path');

const [, , tree, inFile, cut] = process.argv;
const g = require(path.join(path.resolve(tree), 'claude-plugin/scripts/pre-grep-guide.js'));

function step(r) {
  const shellCwd = r.cwd;
  if (!shellCwd || !fs.existsSync(shellCwd)) return 'cwd-gone';
  const root = g.resolveProjectRoot(shellCwd);
  if (root === null) return 'no-root';
  g.useSourceRoots(g.readSourceRoots(root));
  const rawCmd = r.cmd;
  if (g.extractSedReadTargets(rawCmd).length > 0) return 'sed-read';
  let cmd = g.normalizeCommandPaths(rawCmd, root);
  const relPrefix = path.relative(root, shellCwd);
  if (relPrefix) cmd = g.rebaseRelativePaths(cmd, relPrefix, root);
  if (relPrefix && g.bareSourceTarget(g.firstShellClause(cmd))) return 'subdir-bare';
  if (!g.shouldHint(cmd)) return 'no-hint';
  const cgFlags = g.extractCgFlags(cmd);
  const rawGrepPattern = g.pickBlockPattern(cmd);
  if (!g.rootOnlyInSearchPath(rawCmd, root, g.extractSearchPath(cmd))) return 'root-only';
  const grepPattern = g.cgFlagSet(cgFlags).has('-F') ? rawGrepPattern : g.translateBreToRg(cmd, rawGrepPattern);
  if (g.commandHasBypass(rawCmd)) return 'bypass';
  const block = g.classifyDeny(cmd);
  if (!block) return 'no-block';
  const plan = g.rewritePlan(cmd, { isDir: (t) => g.isDirectory(path.resolve(root, t)) });
  if (!g.rewriteMatchesBlock(plan, block, cmd, rawGrepPattern)) return 'plan-mismatch';
  if (block.mode === 'grep' && grepPattern === null) return 'pattern-null';
  if (plan && relPrefix && !g.operandMatches(rawCmd, plan.target, root, shellCwd)) return 'operand';
  return `ANSWER:${block.mode}`;
}

const GREP_FIRST = /^\s*(?:grep|egrep|rg)\b/;
const rows = [];
const table = {};
for (const line of fs.readFileSync(inFile, 'utf8').split('\n')) {
  if (!line) continue;
  const r = JSON.parse(line);
  r.step = step(r);
  rows.push(r);
  if (!GREP_FIRST.test(r.cmd)) continue;
  const era = r.ts >= cut ? 'after' : 'before';
  const where = /\/worktrees\//.test(r.cwd || '') ? 'worktree' : 'checkout';
  const t = (table[`${era} ${where}`] ||= { commands: 0, rewritten: 0, steps: {} });
  t.commands += 1;
  if (r.rewritten) t.rewritten += 1;
  t.steps[r.step] = (t.steps[r.step] || 0) + 1;
}
fs.writeFileSync(`${inFile}.funnel.jsonl`, rows.map((r) => JSON.stringify(r)).join('\n') + '\n');
// A rewrite the replay put outside ANSWER means the mirror is missing a path.
const outside = rows.filter((r) => r.rewritten && !r.step.startsWith('ANSWER'));
console.log(JSON.stringify(table, null, 1));
console.log('rewritten but not ANSWER in the replay:', outside.length);
