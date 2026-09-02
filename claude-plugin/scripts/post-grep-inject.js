#!/usr/bin/env node
'use strict';
// PostToolUse(Bash) hook: deliver cg's AST-aware answer for a FOLDABLE grep that
// rode inside a COMPOUND command and therefore flew past the PreToolUse deny gate
// (`echo "..." && grep Sym tests/`, `git diff && grep ...`, `for s in …; do grep`).
//
// Why PostToolUse, not PreToolUse: the leading-grep deny path (pre-grep-guide)
// only fires when the command HEAD is grep — a grep buried after a side-effecting
// sibling has head=echo/git/for and is intentionally left alone there (denying the
// whole compound command would also block the sibling). Those greps RUN, so the
// only permission-neutral way to hand the model cg's structural view is a
// PostToolUse `additionalContext` injection (CC docs v2026-06: PostToolUse honors
// additionalContext; a PreToolUse `allow` would skip the default permission prompt
// for the underlying Bash call, which we must not do).
//
// Reuses the PreToolUse pure predicates wholesale (feedback_hook_class_bug_sweep —
// no inline copies of the grep gate): splitTopLevelSegments + classifyBlock pick
// the foldable segment; pickBlockPattern / translateBreToRg / extractSearchPath +
// sanitizeSearchPath + runGrepAnswer / runShowAnswer run the exact same answer the
// deny path would have. Best-effort: any miss (no hits / unavailable / no binary)
// exits silently with NO injection — an enhancement, never a new failure mode.

const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const { cgTmpDir, cwdHash, makeCooldown } = require('./tmp-dir');
const { recordRecommendation } = require('./recommendation-log');
const { runGrepAnswer, runShowAnswer, runCallgraphAnswer, sanitizeSearchPath } = require('./cg-answer');
const { emitPostToolContext } = require('./hook-emit');
const {
  splitTopLevelSegments,
  classifyBlock,
  pickBlockPattern,
  translateBreToRg,
  extractSearchPath,
  normalizeCommandPaths,
  rebaseRelativePaths,
  resolveProjectRoot,
} = require('./pre-grep-guide');

// The command HEAD is grep/rg/ag (or git grep, or a KEY=VALUE/env prefix). Kept
// loosely aligned with pre-grep-guide's GREP_VERB; this only gates "is this
// segment a search command" so the segment splitter doesn't have to.
const GREP_HEAD = /^\s*(?:env\s+)?(?:[A-Za-z_][A-Za-z0-9_]*=\S*\s+)*(?:git\s+grep|grep|rg|ag)\b/;

// --- Pure logic (testable) ---

/**
 * Find the FIRST foldable grep segment in a (possibly compound) command.
 * Splits via splitTopLevelSegments (NOT on a single `|` — that is an output
 * filter), then returns the first segment whose head is grep AND whose
 * classifyBlock is non-null (the answerable symbol/show tier).
 * @returns {{segment: string, block: {mode: string, symbols?: string[]}} | null}
 */
function findFoldableGrepSegment(cmd) {
  if (!cmd || typeof cmd !== 'string') return null;
  for (const segment of splitTopLevelSegments(cmd)) {
    if (!GREP_HEAD.test(segment)) continue;
    const block = classifyBlock(segment);
    if (block) return { segment, block };
  }
  return null;
}

// Callgraph is the marginal-value inject (cross-file caller tree the grep can't
// return); the grep/show echo modes measured redundant (2026-06-26 audit: 0
// CONSUMED). Prior gate required the WHOLE grep pattern to be one identifier, so
// an alternation / multi-symbol grep (`markSuperseded|created_at`, `foo|bar_baz`)
// fell to the echo. These bounds widen it: extract the identifier tokens and try
// callgraph on each until one has real edges. Cheap — callgraph is ~30ms/call.
const MAX_CALLGRAPH_SYMBOLS = 3;
const MIN_MULTI_SYMBOL_LEN = 3;

/**
 * Identifier tokens from a grep pattern, as callgraph candidates. A lone
 * identifier returns [itself] (any length — exact prior behavior). A multi-token
 * / regex pattern (alternation, word-boundaries, char classes) is stripped of
 * backslash escapes FIRST — so `\bdate` yields `date`, not `bdate` (the letter
 * after `\b`/`\d`/`\w` is a regex metachar, not part of the symbol) — then its
 * identifier tokens are collected: <3-char noise dropped, deduped, capped. Order
 * preserved so the FIRST alternand (usually the primary symbol) is tried first.
 * runCallgraphAnswer self-filters non-symbols (returns `hits` only with real
 * edges), so a junk token just costs one ~30ms no-hits call.
 * @param {string} rawPattern
 * @returns {string[]}
 */
function extractCallgraphSymbols(rawPattern) {
  if (typeof rawPattern !== 'string' || !rawPattern) return [];
  if (/^[A-Za-z_][A-Za-z0-9_]*$/.test(rawPattern)) return [rawPattern];
  const cleaned = rawPattern.replace(/\\[A-Za-z]/g, ' ');
  const seen = new Set();
  const out = [];
  for (const m of cleaned.matchAll(/[A-Za-z_][A-Za-z0-9_]*/g)) {
    const tok = m[0];
    if (tok.length < MIN_MULTI_SYMBOL_LEN) continue;
    if (seen.has(tok)) continue;
    seen.add(tok);
    out.push(tok);
    if (out.length >= MAX_CALLGRAPH_SYMBOLS) break;
  }
  return out;
}

/**
 * The command's actual stdout, from the PostToolUse payload. VERIFIED against the
 * Claude Code runtime (v2.1.198 binary): the hook input carries `tool_response`,
 * and the Bash result is the OBJECT `{stdout, stderr, interrupted, ...}` — so
 * `tool_response.stdout` is the real, load-bearing path. (The published hooks doc
 * says a top-level `tool_output` string, which the runtime does NOT emit — checked
 * first only for forward-compat if a future version adopts the documented name.)
 * `tool_response` as a bare string / `.output` are extra defensive fallbacks. null
 * when no output field is present → the gate can't confirm redundancy → it injects
 * (pre-gate behavior, no regression on any unhandled shape).
 * @returns {string|null}
 */
function extractGrepOutput(input) {
  if (!input || typeof input !== 'object') return null;
  if (typeof input.tool_output === 'string') return input.tool_output;  // doc-stated, forward-compat
  const tr = input.tool_response;
  if (typeof tr === 'string') return tr;
  if (tr && typeof tr === 'object') {
    if (typeof tr.stdout === 'string') return tr.stdout;  // ← real runtime shape (Bash result obj)
    if (typeof tr.output === 'string') return tr.output;
  }
  return null;
}

// A stdout line that looks like a grep HIT, as opposed to a sibling `echo`/prose
// line. grep prints one of: `path:content` (-H), `path:line:content` (-rn),
// `line:content` (-n on a single named file — NO path prefix), or a bare `path`
// (-l). Recognizing all four is what lets the gate skip a real hit in ANY of these
// formats (the compound greps the model actually runs use all of them) while still
// NOT counting `echo "find Sym" && grep Sym wrongpath/` (grep MISSED → only the echo
// prose lands, which matches none of these shapes), so the additive grep-empty inject
// stays reachable and measurable post-ship.
const GREP_HIT_LINE = /(?:^|\s)[^\s:]*[/.][^\s:]*:|^\s*\d+:/;  // path:… OR linenum: (single-file -n)
const BARE_PATH_LINE = /^\S*[/.]\S+$/;                        // grep -l: a lone path, no spaces
// GREP_HIT_LINE has two `[^\s:]*` stars before a required `:`, so a long line that
// carries `/` or `.` but NO colon backtracks O(n²) (~33s on a 400KB line). grep's
// hit marker (`path:` / `NN:`) is always at the START of the line, so testing only a
// bounded prefix is faithful AND caps the scan — untrusted grep stdout on a blocking
// hook must not stall.
const HIT_SHAPE_SCAN_MAX = 256;

/**
 * Did the command's own output already surface the grepped symbol? The inject is
 * redundant exactly then — the model has the hits in front of it (2026-07-03 audit:
 * 18/18 injects 0 CONSUMED, all on greps that already hit). Two guards keep this from
 * over-suppressing the additive (grep-missed) case: (1) only GREP-HIT-SHAPED lines
 * count — a sibling `echo "…Sym…"` prose line does NOT (fixes the common
 * `echo <Symbol> && grep <Symbol>` shape); (2) the identifier is matched as a WHOLE
 * WORD, so a `date` alternand isn't swallowed by `update`/`validate`. When unsure it
 * returns false → the caller injects (safe side: tax, never a wrong/missing answer).
 * Uses the same identifier tokenization as the callgraph path.
 * @returns {boolean} true only when a grep hit for the symbol is CONFIRMED in output.
 */
function grepFoundPattern(output, rawPattern) {
  if (typeof output !== 'string' || !output) return false;
  const ids = extractCallgraphSymbols(rawPattern);
  if (ids.length === 0) return false;
  // ids are pure `[A-Za-z_]\w*` tokens (no regex metachars) → safe to embed in \b…\b.
  const wordRes = ids.map((id) => new RegExp(`\\b${id}\\b`));
  return output.split('\n').some((line) => {
    // Decide hit-shape from a bounded prefix (ReDoS guard — see HIT_SHAPE_SCAN_MAX).
    // A `-l` bare path is short, so a line longer than the cap is never one.
    const head = line.length > HIT_SHAPE_SCAN_MAX ? line.slice(0, HIT_SHAPE_SCAN_MAX) : line;
    const hitShaped = GREP_HIT_LINE.test(head)
      || (line.length <= HIT_SHAPE_SCAN_MAX && BARE_PATH_LINE.test(line));
    if (!hitShaped) return false;
    return wordRes.some((re) => re.test(line));  // \b…\b is linear — safe on the full line
  });
}

// Short header so the model recognizes this as cg's parallel structural view of
// the grep it just ran (the grep already executed; this is additive context).
const INJECT_HEADER = '[code-graph] AST-aware view of your grep (ran alongside):';
// callgraph mode carries the cross-file caller/callee tree — the marginal signal
// the grep CANNOT return (2026-06-26 audit: the grep-echo above was redundant
// with the model's own hits; the caller tree is what moves behavior).
const CALLGRAPH_HEADER =
  '[code-graph] Cross-file call graph for the symbol you grepped (grep can\'t show this):';

function buildInjectText(answer, mode) {
  if (mode === 'callgraph') {
    const lines = [CALLGRAPH_HEADER, answer.text];
    if (answer.truncated) {
      lines.push('(truncated — run `code-graph-mcp callgraph <symbol>` yourself for the full tree)');
    }
    lines.push('`← called by` = callers, `→ calls` = callees, across all files — use these directly.');
    return lines.join('\n');
  }
  const lines = [INJECT_HEADER, answer.text];
  if (answer.truncated) {
    lines.push(mode === 'show'
      ? '(truncated — re-run the `code-graph-mcp show <symbol>` command above for full source)'
      : '(truncated — run `code-graph-mcp grep "<pattern>"` yourself for the full list)');
  }
  lines.push('Each hit shows its containing fn/module — use these results directly.');
  return lines.join('\n');
}

// Kill switch — matches the sibling-hook convention (pre-grep-guide.isSilenced).
function isSilenced(env = process.env) {
  return env.CODE_GRAPH_QUIET_HOOKS === '1';
}

// New per-this-hook opt-out (released-artifact requirement): =1 disables the
// PostToolUse compound-grep injection entirely, independent of QUIET_HOOKS.
function isInjectDisabled(env = process.env) {
  return env.CODE_GRAPH_NO_INJECT === '1';
}

// Per-command cooldown, mirror of pre-grep-guide's flag pattern but with a
// DISTINCT prefix so the two hooks never share a flag (a PreToolUse deny and a
// PostToolUse inject for different commands must not suppress each other).
// Shared implementation (ARC-06). The DISTINCT prefix is the load-bearing part:
// a PreToolUse deny and a PostToolUse inject for different commands must not
// suppress each other.
const { commandHash, flagPath, isOnCooldown, markCooldown } = makeCooldown('postinject');

// --- Main execution ---

function runMain() {
  if (isSilenced() || isInjectDisabled()) return;
  // Walk up from the persistent shell cwd (subdir-cwd fix — shared resolver).
  const shellCwd = process.cwd();
  const root = resolveProjectRoot(shellCwd);
  if (root === null) return;  // no index anywhere up to $HOME

  let input;
  try {
    // fd 0, not '/dev/stdin': the path form fails ENXIO on socketpair stdin.
    input = JSON.parse(fs.readFileSync(0, 'utf8'));
  } catch { return; }

  const rawCmd = (input.tool_input && input.tool_input.command) || '';
  if (!rawCmd) return;

  // Normalize abs paths under the root + rebase subdir-relative tokens, exactly
  // like the PreToolUse path, so the segment classifier and the answer scope see
  // root-relative paths. Cooldown stays keyed on the raw command.
  let cmd = normalizeCommandPaths(rawCmd, root);
  const relPrefix = path.relative(root, shellCwd);
  if (relPrefix) cmd = rebaseRelativePaths(cmd, relPrefix, root);

  const found = findFoldableGrepSegment(cmd);
  if (!found) return;

  if (isOnCooldown(rawCmd, Date.now(), 60000, root)) return;
  markCooldown(rawCmd, root);

  const { segment, block } = found;
  // Run the answer exactly like the deny path.
  const rawPattern = pickBlockPattern(segment);
  // Grep-response gate (2026-07-03 audit: 18/18 injects were 0 CONSUMED because they
  // re-stated hits the model already had). If the command's OWN output already
  // surfaced the grepped symbol, the inject is redundant → skip it, saving the
  // ~1KB context tax. Only a grep that found NOTHING (or an unreadable output —
  // no regression on older CC) proceeds: then cg's structural answer (the real
  // location / cross-file callers a failed grep never showed) is genuinely additive.
  if (grepFoundPattern(extractGrepOutput(input), rawPattern)) return;
  const pattern = translateBreToRg(segment, rawPattern);
  const searchPath = sanitizeSearchPath(extractSearchPath(segment));
  let answer = { status: 'unavailable' };
  let answeredMode = block.mode;

  // PREFER the cross-file caller/callee tree — the marginal signal a raw grep can't
  // return (2026-06-26 inject audit: 13 events / 0 CONSUMED because the grep-echo
  // just re-stated the model's own hits). Try every identifier the pattern
  // carries (alternation / multi-symbol grep), not just a lone-identifier pattern,
  // stopping at the first symbol with real edges. runCallgraphAnswer returns `hits`
  // ONLY when the symbol has edges → a leaf/absent symbol self-filters to the
  // show/grep echo below.
  for (const symbol of extractCallgraphSymbols(rawPattern)) {
    const cg = runCallgraphAnswer({ cwd: root, symbol });
    if (cg.status === 'hits') {
      answer = cg;
      answeredMode = 'callgraph';
      break;
    }
  }

  if (answer.status !== 'hits') {
    if (block.mode === 'show') {
      answeredMode = 'show';
      answer = runShowAnswer({ cwd: root, symbols: block.symbols });
      if (answer.status !== 'hits' && pattern) {
        answeredMode = 'grep';
        answer = runGrepAnswer({ cwd: root, pattern, searchPath });
      }
    } else if (pattern) {
      answeredMode = 'grep';
      answer = runGrepAnswer({ cwd: root, pattern, searchPath });
    }
  }

  // Only inject on hits — no-hits / unavailable / no-binary stay silent (the grep
  // already ran and produced its own output; a failed cg answer adds no value and
  // 0 hits ≠ proof of absence given regex-dialect differences). But RECORD the
  // skip (roadmap 2026-07-18 §1.6): without a record the funnel cannot tell
  // "hook dark (binary missing)" from "ran, genuinely nothing" — the sibling
  // hooks all record their non-answered paths. `fallthrough`+`reason` carry the
  // status in the shapes the Rust aggregator already scores as inconclusive
  // (`fallthrough:"no-hits"` / `reason:"unavailable"`); answered:false keeps it
  // out of funnel arming and lands it in `inject_skipped`.
  if (answer.status !== 'hits') {
    recordRecommendation(root, {
      hook: 'grep', action: 'inject', answered: false,
      ...(pattern ? { pattern } : {}),
      fallthrough: answer.status,
      reason: answer.status,
      // Attribute the skip to the mode that burned the attempt — the LAST mode
      // tried, so a callgraph miss that fell through to grep is charged to grep,
      // matching the fallback order above. Without this the aggregator files
      // every skip under one '?' bucket and you can see THAT injects fail but
      // not WHICH mode is failing (D#147). The Rust side keeps this out of
      // `inject_by_mode` (delivered-only) — see src/cli/usage.rs.
      mode: answeredMode,
    });
    return;
  }

  recordRecommendation(root, {
    hook: 'grep', action: 'inject', answered: true,
    ...(pattern ? { pattern } : {}),
    mode: answeredMode,
  });
  process.stdout.write(emitPostToolContext(buildInjectText(answer, answeredMode)) + '\n');
}

if (require.main === module) {
  require('./hook-fail-open').installHookFailOpen('PostToolUse:Bash');
  runMain();
}

module.exports = {
  findFoldableGrepSegment,
  extractCallgraphSymbols,
  extractGrepOutput,
  grepFoundPattern,
  buildInjectText,
  isSilenced,
  isInjectDisabled,
  commandHash,
  isOnCooldown,
  markCooldown,
};
