#!/usr/bin/env node
'use strict';
// PreToolUse(Read) hook: detect read-fanout into the same source directory
// and suggest module_overview / `code-graph-mcp overview` once. The 7d audit
// (2026-05-12 → 2026-05-14, 141 sessions) found 16 sessions with 5+ Reads
// into one source dir without a preceding module_overview call — Claude burns
// context fanning out file-by-file instead of grabbing a structured overview.
//
// Fires when ALL conditions met:
//   1. file_path is a source-code extension (.rs/.py/.ts/.js/.go/...)
//   2. file_path is under CWD (no escape to absolute paths outside the project)
//   3. file_path is not at CWD root (top-level files = config / one-off scripts)
//   4. .code-graph/index.db exists in CWD (project is indexed)
//   5. ≥5 prior Reads to the SAME parent dir tracked in /tmp state
//   6. Same-dir cooldown not active (5 min)
//
// State scoping: per-cwd (NOT per-session). Cost: two concurrent sessions in
// the same project might share counters and over-trigger by ~1 hint each.
// Cheaper than threading session_id through hook plumbing, and the hint is
// skippable. Stale entries (no read in 30 min) get pruned on load.
//
// Escape hatch: CODE_GRAPH_QUIET_HOOKS=1 — matches user-prompt-context.js /
// pre-grep-guide.js convention.

const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const { cgTmpDir } = require('./tmp-dir');
const { recordRecommendation } = require('./recommendation-log');
const { resolveProjectRoot } = require('./project-root');
const { runOverviewAnswer } = require('./cg-answer');
const { emitPreToolAllowContext } = require('./hook-emit');

// --- Configuration ---

// Hint fires on the (FANOUT_THRESHOLD + 1)-th Read into the same dir.
// Set so that 4 reads stay quiet (legitimate "read a couple files to
// understand X" pattern); 5+ reads is the fanout we want to catch.
const FANOUT_THRESHOLD = 4;

// Per-dir cooldown after firing a hint. Prevents spam if Claude keeps
// reading the same dir after seeing the hint (e.g., still has 3 more
// files queued from a prior plan).
const COOLDOWN_MS = 5 * 60 * 1000;

// Entries older than this are pruned on load. Long enough to survive
// normal multi-step tasks (15-20 min typical), short enough that stale
// per-cwd state doesn't accumulate across days.
const STATE_TTL_MS = 30 * 60 * 1000;

// Source-code extensions. Whitelist (NOT blacklist) — config / docs /
// data files stay silent because Claude reading them is not a fanout
// signal worth converting to module_overview.
const SRC_EXT = /\.(rs|py|ts|tsx|js|jsx|mjs|cjs|go|java|kt|swift|rb|php|cs|cpp|cc|c|h|hpp|hxx|m|scala|clj|cljs|ex|exs|hs|ml|fs|r|lua|sh|bash|zsh|fish|sql|vue|svelte|astro|dart|elm|nim|zig)$/i;

// --- Pure logic (testable) ---

function isSourceFile(filePath) {
  if (!filePath || typeof filePath !== 'string') return false;
  return SRC_EXT.test(filePath);
}

function dirOf(filePath) {
  if (!filePath || typeof filePath !== 'string') return '';
  return path.dirname(filePath);
}

function cwdHash(cwd) {
  return crypto.createHash('sha1').update(String(cwd)).digest('hex').slice(0, 12);
}

function statePath(cwd) {
  return path.join(cgTmpDir(), `.code-graph-readfan-${cwdHash(cwd)}.json`);
}

function loadState(cwd, now = Date.now()) {
  let state;
  try {
    const raw = fs.readFileSync(statePath(cwd), 'utf8');
    state = JSON.parse(raw);
  } catch { return { by_dir: {} }; }
  if (!state || typeof state !== 'object' || !state.by_dir) return { by_dir: {} };
  // Prune stale entries — anything not Read in STATE_TTL_MS gets dropped.
  for (const dir of Object.keys(state.by_dir)) {
    const e = state.by_dir[dir];
    if (!e || (now - (e.last_read_at || 0) > STATE_TTL_MS)) {
      delete state.by_dir[dir];
    }
  }
  return state;
}

function saveState(cwd, state) {
  try {
    fs.writeFileSync(statePath(cwd), JSON.stringify(state));
  } catch { /* ok */ }
}

function recordRead(state, dir, now = Date.now()) {
  if (!state.by_dir[dir]) state.by_dir[dir] = { reads: 0, last_read_at: 0, last_hint_at: 0 };
  const e = state.by_dir[dir];
  e.reads += 1;
  e.last_read_at = now;
}

function shouldHint(state, dir, now = Date.now()) {
  if (!dir) return false;
  const e = state.by_dir[dir];
  if (!e) return false;
  if (e.reads < FANOUT_THRESHOLD + 1) return false;  // need >=5
  if (e.last_hint_at && (now - e.last_hint_at < COOLDOWN_MS)) return false;
  return true;
}

function markHint(state, dir, now = Date.now()) {
  if (!state.by_dir[dir]) return;
  state.by_dir[dir].last_hint_at = now;
}

function buildHint(dir) {
  // Single-line, ~190-byte budget. Skip-clause matches pre-grep-guide voice.
  return `[code-graph] 5+ Reads into ${dir}/ — \`code-graph-mcp overview ${dir}/\` gives symbols+callers in one call (MCP: \`module_overview path=${dir}\`). Skip if you need raw file contents.`;
}

// v0.49 — the hint DELIVERS the overview instead of advising a tool call
// (advice measured 0/40 transfer on 2026-06-12; delivered answers satisfied
// 5/5 in place). Falls back to the advice-only line when the CLI is
// unavailable or the dir has no overview.
function buildHintWithAnswer(dir, answer) {
  const lines = [
    `[code-graph] 5+ Reads into ${dir}/ — module overview from the AST index (saves the remaining file-by-file reads):`,
    answer.text,
  ];
  if (answer.truncated) {
    lines.push(`(truncated — \`code-graph-mcp overview ${dir}/\` for the full map)`);
  }
  return lines.join('\n');
}

function isSilenced(env = process.env) {
  return env.CODE_GRAPH_QUIET_HOOKS === '1';
}

// v0.49 — answer tier opt-out, shared name with the grep hook's deny-answer
// opt-out: =1 restores advice-only hints (no CLI run inside the hook).
function isAnswerDisabled(env = process.env) {
  return env.CODE_GRAPH_NO_ANSWER_IN_DENY === '1';
}

// --- Shared tracking core (also driven by pre-grep-guide's sed-range path) ---

/// Record one read of `rel` (project-root-relative source path) and fire the
/// fanout hint when the threshold crosses. Emits to stdout + records the
/// recommendation. Returns true when a hint fired.
function trackReadAndMaybeHint(root, rel, now = Date.now()) {
  if (!rel || rel.startsWith('..') || path.isAbsolute(rel)) return false;
  const dir = path.dirname(rel);
  if (!dir || dir === '.' || dir === '') return false;  // top-level file: not fanout

  const state = loadState(root, now);
  recordRead(state, dir, now);
  let fired = false;
  if (shouldHint(state, dir, now)) {
    markHint(state, dir, now);
    fired = true;
  }
  saveState(root, state);
  if (!fired) {
    // Outcome proxy: a source read that didn't trip the fanout hint still ran.
    // Record it (best-effort) so `stats` can measure the model's read fan-out —
    // e.g. a read right after cg answered a grep in-place (search-decay).
    recordRecommendation(root, { hook: 'read', action: 'observe' });
    return false;
  }

  let answer = { status: 'unavailable' };
  if (!isAnswerDisabled()) {
    answer = runOverviewAnswer({ cwd: root, dir });
  }
  const answered = answer.status === 'hits';
  recordRecommendation(root, {
    hook: 'read', action: 'hint', answered,
    // reason segments WHY an unanswered hint fell back to the bare advice:
    // 'no-binary' (delivered overview dark — binary missing) vs 'unavailable'
    // (binary ran but failed/timed out) vs 'no-hits'. Mirrors pre-grep-guide
    // so the read-fanout funnel can tell a dark flagship apart from no result.
    ...(answered ? {} : { reason: answer.status }),
  });
  // Compound-grep sibling sweep: emit via the PreToolUse allow+additionalContext
  // envelope (shared hook-emit.js). Bare stdout on a PreToolUse exit-0 lands in
  // the debug log only and never reaches the model (CC docs v2026-06); the
  // additionalContext channel is what actually surfaces the fanout hint. Read is
  // a safe tool, so the allow elevation is negligible.
  const hintText = answered ? buildHintWithAnswer(dir, answer) : buildHint(dir);
  process.stdout.write(emitPreToolAllowContext(hintText) + '\n');
  return true;
}

// --- Main execution ---

function runMain() {
  if (isSilenced()) return;
  // v0.49 — walk up from the shell cwd (subdir-cwd fix; the read hook had
  // recorded NOTHING in daagu history because sessions sat in backend/).
  const root = resolveProjectRoot(process.cwd());
  if (root === null) return;

  let input;
  try {
    // fd 0, not '/dev/stdin': the path form fails ENXIO on socketpair stdin.
    input = JSON.parse(fs.readFileSync(0, 'utf8'));
  } catch { return; }

  const filePath = (input.tool_input && input.tool_input.file_path) || '';
  if (!isSourceFile(filePath)) return;

  // Normalize to a root-relative path. Read sends absolute paths; files
  // outside the project (other repos, ~/.claude/) stay silent.
  let rel;
  try {
    rel = path.relative(root, filePath);
  } catch { return; }

  trackReadAndMaybeHint(root, rel);
}

if (require.main === module) {
  require('./hook-fail-open').installHookFailOpen('PreToolUse:Read');
  runMain();
}

module.exports = {
  isSourceFile, dirOf, cwdHash, statePath,
  loadState, saveState, recordRead, shouldHint, markHint,
  buildHint, buildHintWithAnswer, isSilenced, isAnswerDisabled,
  trackReadAndMaybeHint,   // v0.49 — shared with pre-grep-guide's sed-range path
  FANOUT_THRESHOLD, COOLDOWN_MS, STATE_TTL_MS, SRC_EXT,
};
