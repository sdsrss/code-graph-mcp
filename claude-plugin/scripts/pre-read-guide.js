#!/usr/bin/env node
'use strict';
// FIRST statement, before this file's other requires (pre-tag review
// 2026-09-02): the handler installed after them could not catch a throw
// from `require('./lifecycle')` itself, which is exactly the broken-install
// case JS-12 exists for. Guarded on `require.main` so importing this module
// in a test does NOT install a process-wide handler that exits 0 — that
// would swallow the test's own failures.
if (require.main === module) require('./hook-fail-open').installHookFailOpen('PreToolUse:Read');

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
//   5. ≥5 DISTINCT files read in the SAME parent dir, tracked in /tmp state
//   6. The dir has not fired before while its state entry lives
//   7. The CLI delivers an overview for the dir (no advice-only line)
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
const { resolveProjectRoot, indexKey, indexBuildInProgress } = require('./project-root');
const { runOverviewAnswer } = require('./cg-answer');
const { emitPreToolAllowContext } = require('./hook-emit');

// --- Configuration ---

// Hint fires on the (FANOUT_THRESHOLD + 1)-th DISTINCT file read in the same dir.
// Set so that 4 reads stay quiet (legitimate "read a couple files to
// understand X" pattern); 5+ files is the fanout we want to catch.
const FANOUT_THRESHOLD = 4;

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

// Distinct files remembered per dir. Past this the count is already far over
// the threshold, so the list stops growing and so does the state file.
const MAX_FILES_PER_DIR = 32;

/// Count a read of `file` (when given) in `dir`. Re-reading one file does not
/// count again: five chunked reads of one big file are not a fanout, and they
/// fired an overview of its whole parent dir (hook audit 2026-09-28).
function recordRead(state, dir, now = Date.now(), file) {
  if (!state.by_dir[dir]) state.by_dir[dir] = { reads: 0, last_read_at: 0, last_hint_at: 0 };
  const e = state.by_dir[dir];
  e.last_read_at = now;
  if (file) {
    if (!Array.isArray(e.files)) e.files = [];
    if (e.files.includes(file)) return;
    if (e.files.length < MAX_FILES_PER_DIR) e.files.push(file);
  }
  e.reads += 1;
}

function shouldHint(state, dir, now = Date.now()) {
  if (!dir) return false;
  const e = state.by_dir[dir];
  if (!e) return false;
  if (e.reads < FANOUT_THRESHOLD + 1) return false;  // need >=5
  // Once per dir while its entry lives (pruned STATE_TTL_MS after the last
  // read). The old 5-minute re-fire re-sent the same overview: 31.7% of fanout
  // hints in 2026-09 sessions repeated a text the session already had.
  if (e.last_hint_at) return false;
  return true;
}

function markHint(state, dir, now = Date.now()) {
  if (!state.by_dir[dir]) return;
  state.by_dir[dir].last_hint_at = now;
}

// v0.49 — the hint DELIVERS the overview instead of advising a tool call
// (advice measured 0/40 transfer on 2026-06-12; delivered answers satisfied
// 5/5 in place). Falls back to the advice-only line when the CLI is
// unavailable or the dir has no overview.
function buildHintWithAnswer(dir, answer) {
  const lines = [
    `[code-graph] 5+ Reads into ${dir}/ — module overview from the AST index:`,
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

/// Record one read of `rel` (project-root-relative source path). Returns the
/// dir when this read crossed the fanout threshold (the hint is marked as
/// delivered), else null. Writes nothing to stdout: a hook's stdout is parsed as
/// ONE JSON value, so only the entry point may write, and only once.
function trackRead(root, rel, now = Date.now()) {
  if (!rel || rel.startsWith('..') || path.isAbsolute(rel)) return null;
  const dir = path.dirname(rel);
  if (!dir || dir === '.' || dir === '') return null;  // top-level file: not fanout

  const state = loadState(root, now);
  recordRead(state, dir, now, rel);
  let fired = false;
  // During the startup build the overview would list part of the dir as the
  // whole module (D5); leave the hint unspent for the first read after it.
  if (!indexBuildInProgress(root, now) && shouldHint(state, dir, now)) {
    markHint(state, dir, now);
    fired = true;
  }
  saveState(root, state);
  if (!fired) {
    // Outcome proxy: a source read that didn't trip the fanout hint still ran.
    // Record it (best-effort) so `stats` can measure the model's read fan-out —
    // e.g. a read right after cg answered a grep in-place (search-decay).
    recordRecommendation(root, { hook: 'read', action: 'observe' });
    return null;
  }
  return dir;
}

/// The fanout hint text for `dir` with the overview answer embedded, or null
/// when the CLI delivers none. `maxBytes` bounds that answer — the pre-grep sed
/// path splits one envelope's budget between several dirs.
///
/// No overview, no hint. The advice-only line measured no effect twice: 0/40
/// transfer on 2026-06-12, and 18/672 follow-through against a 2.76% baseline in
/// 2026-09 sessions, where 572 of the 672 advised `overview tests/` — which
/// answers "No symbols found", since overview leaves test symbols out. The
/// unanswered hint is still recorded, so the funnel keeps its reasons.
function buildFanoutHint(root, dir, { maxBytes } = {}) {
  let answer = { status: 'unavailable' };
  if (maxBytes === 0) {
    // No room left in a shared envelope: the advice line, and no CLI run.
    answer = { status: 'unavailable', reason: 'budget' };
  } else if (!isAnswerDisabled()) {
    answer = runOverviewAnswer({ cwd: root, dir, ...(maxBytes ? { maxBytes } : {}) });
  }
  const answered = answer.status === 'hits';
  recordRecommendation(root, {
    hook: 'read', action: 'hint', answered,
    // reason segments WHY an unanswered hint fell back to the bare advice:
    // 'no-binary' (delivered overview dark — binary missing) vs 'unavailable'
    // (binary ran but failed/timed out) vs 'no-hits'. Mirrors pre-grep-guide
    // so the read-fanout funnel can tell a dark flagship apart from no result.
    //
    // `fallthrough_reason` sub-divides 'unavailable' rather than replacing it:
    // a spent hook budget and a wedged binary are the same word here, and only
    // the former is the healthy-hook-answers-nothing case NEW-08 traded a kill
    // for. It cannot go in `reason` — src/cli/usage.rs:448 scores
    // `reason:"unavailable"` as an inconclusive follow-up, so a new value there
    // would re-file every budget skip as "the answer was insufficient".
    ...(answered ? {} : { reason: answer.status }),
    ...(answered || !answer.reason ? {} : { fallthrough_reason: answer.reason }),
  });
  return answered ? buildHintWithAnswer(dir, answer) : null;
}

/// trackRead + buildFanoutHint for one read. Returns the hint text when the
/// hint fired with an overview, else null. The caller emits it.
function trackReadAndMaybeHint(root, rel, now = Date.now()) {
  const dir = trackRead(root, rel, now);
  return dir === null ? null : buildFanoutHint(root, dir);
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
  // outside the project (other repos, ~/.claude/) stay silent. A linked
  // worktree's copy counts toward the directory the index knows (D#560).
  let rel;
  try {
    rel = indexKey(root, path.relative(root, filePath));
  } catch { return; }

  const hint = trackReadAndMaybeHint(root, rel);
  // Emit via the PreToolUse allow+additionalContext envelope (shared
  // hook-emit.js). Bare stdout on a PreToolUse exit-0 lands in the debug log
  // only and never reaches the model (CC docs v2026-06); the additionalContext
  // channel is what actually surfaces the fanout hint. Read is a safe tool, so
  // the allow elevation is negligible.
  if (hint) process.stdout.write(emitPreToolAllowContext(hint) + '\n');
}

if (require.main === module) {
  runMain();
}

module.exports = {
  isSourceFile, dirOf, cwdHash, statePath,
  loadState, saveState, recordRead, shouldHint, markHint,
  buildHintWithAnswer, isSilenced, isAnswerDisabled,
  trackReadAndMaybeHint,   // v0.49 — shared with pre-grep-guide's sed-range path
  trackRead, buildFanoutHint,  // the sed path's two halves: one envelope for every dir that fired
  FANOUT_THRESHOLD, STATE_TTL_MS, SRC_EXT,
};
