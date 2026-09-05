#!/usr/bin/env node
'use strict';
// Synchronous "answer in the deny" runner (v0.47.0).
//
// When pre-grep-guide denies a symbol-shaped raw grep, the measured
// recommend→use transfer rate of a bare suggestion is ~0% — the model rarely
// initiates a NEW tool call just because a deny message told it to. This module
// closes that gap by running the AST-aware equivalent (`code-graph-mcp grep
// "<pattern>" [path]`) inside the hook and handing the deny path the actual
// results, so the model never has to choose.
//
// Posture mirrors recommendation-log.js: bounded and best-effort. Any failure
// (no binary, nonzero exit, timeout, oversized pattern) degrades to
// `unavailable` and the caller falls back to the static deny — answering is an
// enhancement, never a new failure mode for the tool call.
//
// Verified non-polluting: the CLI `grep` subcommand does not write
// usage.jsonl (only the MCP server's SessionMetrics does), so hook-initiated
// runs cannot inflate the deny→use conversion funnel.

const { spawnSync } = require('child_process');
const { hidden } = require('./proc-opts');

// 2000 ms is a product decision, not a tuning knob: a PreToolUse hook that
// stalls longer than this costs the user more than the answer is worth, so the
// answer degrades instead.
//
// `_CG_ANSWER_TIMEOUT_MS` exists ONLY as a test seam, mirroring the
// `_CG_ANSWER_BINARY` override already used by the same tests. Without it the
// hint tests spawn a real `node` process under whatever load the machine
// happens to be under — a full cargo build saturating every core pushed cold
// node startup past 2 s and reddened `trackReadAndMaybeHint: fires on 5th read`
// roughly one run in seven, while 12/12 isolated runs passed. An intermittently
// red suite teaches people to re-run instead of read, which is the one habit
// this whole audit is about.
//
// FLOORED AT THE SOURCE. `Number('1500.5')` is finite and positive, and
// `child_process` rejects a fractional `timeout` with ERR_OUT_OF_RANGE — which
// each runner's own try/catch turns into a silent `unavailable`, so the hook
// exits 0 having answered nothing. That regression is what `remainingMs` was
// hardened for; this value happens to pass through it today, which means the
// property is currently held one file away by a function that has no obligation
// to keep holding it (audit 2026-09-05 NEW-02).
const DEFAULT_TIMEOUT_MS = (() => {
  const override = Number(process.env._CG_ANSWER_TIMEOUT_MS);
  return Number.isFinite(override) && override >= 1 ? Math.floor(override) : 2000;
})();
// ~1000 tokens. A deny reason carrying more than this stops being an answer
// and starts being a context tax.
const DEFAULT_MAX_BYTES = 4000;
const MAX_PATTERN_LEN = 200;
// CLI empty-result contract (text mode): stable prefix owned by this repo.
const NO_MATCH_PREFIX = '[code-graph] No matches';

/**
 * Truncate text to maxBytes, cutting at the last complete line that fits.
 * Falls back to a hard byte cut when even the first line is oversized.
 * @returns {{text: string, truncated: boolean}}
 */
function truncateAtLine(text, maxBytes) {
  if (Buffer.byteLength(text, 'utf8') <= maxBytes) {
    return { text, truncated: false };
  }
  const buf = Buffer.from(text, 'utf8');
  const head = buf.subarray(0, maxBytes).toString('utf8');
  // Drop a possibly half-cut trailing line (and any UTF-8 replacement char
  // from a mid-codepoint cut rides along with it).
  const lastNl = head.lastIndexOf('\n');
  if (lastNl > 0) {
    return { text: head.slice(0, lastNl), truncated: true };
  }
  // Hard cut, when even the first line does not fit. Back the cut off to a
  // UTF-8 character boundary instead of re-decoding the bytes: `latin1` maps
  // each byte to its own character, so a CJK line came back as mojibake rather
  // than as a shortened line, and `utf8` alone would leave a U+FFFD where the
  // cut landed mid-character. A continuation byte is `10xxxxxx`.
  let end = maxBytes;
  while (end > 0 && (buf[end] & 0xc0) === 0x80) end--;
  return { text: buf.subarray(0, end).toString('utf8'), truncated: true };
}

/**
 * v0.48 — drop glob segments from a search path. The hook extracts path tokens
 * verbatim from the denied command, and spawnSync runs WITHOUT a shell, so a
 * literal `backend/…/llm_engine/*.py` reaches rg as a nonexistent file →
 * exit 1 → `unavailable` → static deny with no answer (daagu 2026-06-11: the
 * night's only deny failed exactly this way). Truncate at the first segment
 * containing a glob metacharacter; widening the scope to the parent dir is
 * always safe. A leading glob (`*.py`) drops the scope entirely (repo-wide).
 */
function sanitizeSearchPath(searchPath) {
  if (!searchPath || typeof searchPath !== 'string') return undefined;
  const segs = searchPath.split('/');
  const i = segs.findIndex((s) => /[*?[\]{}]/.test(s));
  if (i === -1) return searchPath;
  const kept = segs.slice(0, i).join('/');
  return kept || undefined;
}

/**
 * The four runners below each spawned the binary and mapped the result by hand
 * — the same fifteen lines, four times (audit 2026-08-29 ARC-07). Hoisted into
 * three pieces, with the one genuine difference between them made a PARAMETER
 * rather than a divergence you have to notice.
 */

/** Explicit `opts.binary` → `_CG_ANSWER_BINARY` → `findBinary()`; null if none. */
function resolveAnswerBinary(opts) {
  let binary = opts.binary;
  if (binary === undefined) {
    binary = process.env._CG_ANSWER_BINARY || require('./find-binary').findBinary();
  }
  return binary || null;
}

/**
 * One spawn, one options block, one `CODE_GRAPH_INTERNAL` stamp.
 *
 * The timeout is the SMALLER of this call's own budget and whatever is left of
 * the hook process's registered budget, because the callers run these in
 * series: post-grep-inject loops callgraph over every symbol in the pattern
 * before falling back to show and then grep, so three 2 s answers overran a 5 s
 * hook and Claude Code killed it — which the user sees as a hook error on their
 * own tool call, not as a missing hint (audit 2026-09-05 JS-03). Out of budget
 * returns a synthetic timeout the exit-code table already reads as
 * `unavailable`, so the caller degrades to the static path exactly as it does
 * for a real timeout.
 *
 * `killSignal: 'SIGKILL'`: the child we are giving up on is most often one
 * wedged waiting on `index.lock`, and node's `timeout` sends SIGTERM and then
 * WAITS. It reads no stdin and holds no lock file worth unwinding — the same
 * reasoning statusline.js and doctor.js already apply (see proc-opts.js).
 */
function runCg(binary, args, { cwd, timeoutMs }) {
  const budget = require('./hook-fail-open').remainingMs(timeoutMs);
  if (budget === null) {
    // `error` is what classifyRun reads (→ `unavailable`); `budgetExhausted` is
    // for the one caller that loops and must not mistake this for "that symbol
    // did not resolve" — see runShowAnswer.
    return {
      error: new Error('hook budget exhausted'),
      budgetExhausted: true,
      status: null,
      stdout: '',
    };
  }
  return spawnSync(binary, args, hidden({
    cwd,
    timeout: budget,
    killSignal: 'SIGKILL',
    encoding: 'utf8',
    maxBuffer: 4 * 1024 * 1024,
    stdio: ['ignore', 'pipe', 'ignore'],
    // Hook-internal run: a delivered answer, not a model-initiated conversion.
    // The CLI skips its recommendations.jsonl `use` record when this is set.
    env: { ...process.env, CODE_GRAPH_INTERNAL: '1' },
  }));
}

/**
 * The exit-code table: 0 = answered, 1 = the query found nothing, anything else
 * (plus a spawn error or a signal) = the tool did not run.
 *
 * `exitOneIsNoHits` is the whole reason this is a parameter and not a constant.
 * `grep` and `callgraph` treat exit 1 as an empty result — the v0.50
 * grep-parity contract. `overview` does NOT: its exit 1 means "no indexed files
 * under that path", which the read-fanout hint reports as unavailable rather
 * than as an answered-but-empty query. That difference predates this hoist and
 * lived in four separate copies; folding it away silently would have changed
 * one of them.
 */
function classifyRun(res, { exitOneIsNoHits }) {
  if (res.error || res.signal) return 'unavailable';
  if (res.status === 1) return exitOneIsNoHits ? 'no-hits' : 'unavailable';
  if (res.status !== 0) return 'unavailable';
  return 'ok';
}

/** stdout carrying no answer: empty, or the CLI's own no-match line. */
function isEmptyAnswer(out) {
  return !out || out.startsWith(NO_MATCH_PREFIX);
}

/**
 * Run `code-graph-mcp grep <pattern> [searchPath]` synchronously.
 *
 * @param {object} opts
 * @param {string} opts.cwd          project root (hook process.cwd())
 * @param {string} opts.pattern      the symbol-shaped pattern that triggered the deny
 * @param {string} [opts.searchPath] optional path scope extracted from the denied command
 * @param {string|null} [opts.binary] binary path; tests inject a stub. Defaults to
 *                                    `_CG_ANSWER_BINARY` env override, then findBinary().
 * @param {number} [opts.timeoutMs]
 * @param {number} [opts.maxBytes]
 * @returns {{status: 'hits', text: string, truncated: boolean}
 *         | {status: 'no-hits'}
 *         | {status: 'no-binary'}
 *         | {status: 'unavailable'}}
 *   `no-binary` (binary not installed / not locatable) is kept distinct from
 *   `unavailable` (runtime failure) so the deny funnel can tell "flagship
 *   answer-in-deny is dark because the binary is missing" apart from "binary
 *   ran, query just had no hits". Both still fall back to the static deny.
 */
function runGrepAnswer(opts = {}) {
  const {
    cwd,
    pattern,
    searchPath,
    timeoutMs = DEFAULT_TIMEOUT_MS,
    maxBytes = DEFAULT_MAX_BYTES,
  } = opts;
  try {
    if (!pattern || typeof pattern !== 'string' || pattern.length > MAX_PATTERN_LEN) {
      return { status: 'unavailable' };
    }
    const binary = resolveAnswerBinary(opts);
    if (!binary) return { status: 'no-binary' };

    // Defensive re-sanitize: callers should pass a clean path, but a glob
    // reaching argv is a guaranteed nonzero exit (see sanitizeSearchPath).
    const scope = sanitizeSearchPath(searchPath);
    const args = ['grep', pattern];
    if (scope) args.push(scope);
    const res = runCg(binary, args, { cwd, timeoutMs });
    // Older binaries exit 0 on no-match with NO_MATCH_PREFIX on stdout — that
    // shape resolves to 'no-hits' through isEmptyAnswer below.
    const verdict = classifyRun(res, { exitOneIsNoHits: true });
    // `reason` separates the two things `unavailable` covers. The binary failing
    // and the binary never being given time are different facts, and the caller
    // renders one of them to the user — "ran but failed" is simply untrue of a
    // run that never started (audit 2026-09-05 NEW-08).
    if (verdict !== 'ok') {
      return { status: verdict, ...(res.budgetExhausted ? { reason: 'budget' } : {}) };
    }
    const out = (res.stdout || '').trim();
    if (isEmptyAnswer(out)) {
      return { status: 'no-hits' };
    }
    const { text, truncated } = truncateAtLine(out, maxBytes);
    return { status: 'hits', text, truncated };
  } catch {
    return { status: 'unavailable' };
  }
}

/**
 * v0.49 — Run `code-graph-mcp show <symbol>` for up to 3 declaration symbols
 * and concatenate the bodies. Powers the show-mode deny (declaration-anchor +
 * context-flag greps: the model wants to READ the functions, so hand it the
 * functions). Same bounded/best-effort posture as runGrepAnswer; symbols that
 * fail to resolve are skipped, all-fail → no-hits (caller falls back to grep).
 * @returns {{status: 'hits', text: string, truncated: boolean}
 *         | {status: 'no-hits'}
 *         | {status: 'no-binary'}
 *         | {status: 'unavailable'}}
 *   `no-binary` distinguishes a missing/unlocatable binary from a runtime
 *   `unavailable`, so the deny funnel can see a dark flagship answer-in-deny.
 */
function runShowAnswer(opts = {}) {
  const {
    cwd,
    symbols,
    timeoutMs = DEFAULT_TIMEOUT_MS,
    maxBytes = DEFAULT_MAX_BYTES,
  } = opts;
  try {
    if (!Array.isArray(symbols) || symbols.length === 0) {
      return { status: 'unavailable' };
    }
    const binary = resolveAnswerBinary(opts);
    if (!binary) return { status: 'no-binary' };

    const parts = [];
    for (const sym of symbols.slice(0, 3)) {
      if (typeof sym !== 'string' || !/^[A-Za-z_][A-Za-z0-9_]*$/.test(sym)) continue;
      const res = runCg(binary, ['show', sym], { cwd, timeoutMs });
      // Out of hook budget is not "this symbol did not resolve". This loop
      // `continue`s past every failure and reports `no-hits` when none of the
      // three produced output, so without this arm an exhausted budget reached
      // recordRecommendation as a genuine empty result — and the whole reason
      // `no-binary` is kept distinct from `unavailable` (see this function's
      // docs) is that the deny funnel has to tell those causes apart. Nothing
      // later in the loop can succeed either: the budget is gone for all three
      // (pre-ship review 2026-09-05).
      if (res.budgetExhausted) return { status: 'unavailable', reason: 'budget' };
      // A symbol that did not resolve is SKIPPED, not fatal — exit 1 included,
      // which is why this asks for `exitOneIsNoHits: false` and then treats
      // every non-`ok` verdict the same way.
      if (classifyRun(res, { exitOneIsNoHits: false }) !== 'ok') continue;
      const out = (res.stdout || '').trim();
      if (isEmptyAnswer(out)) continue;
      parts.push(`$ code-graph-mcp show ${sym}\n${out}`);
    }
    if (parts.length === 0) return { status: 'no-hits' };
    const { text, truncated } = truncateAtLine(parts.join('\n\n'), maxBytes);
    return { status: 'hits', text, truncated };
  } catch {
    return { status: 'unavailable' };
  }
}

/**
 * v0.49 — Run `code-graph-mcp overview <dir>` for the read-fanout hint, so the
 * hint DELIVERS the module map instead of advising a tool call (hints measured
 * 0/40 transfer on 2026-06-12; delivered answers satisfied 5/5 in place).
 * @returns {{status: 'hits', text: string, truncated: boolean}
 *         | {status: 'no-hits'}
 *         | {status: 'no-binary'}
 *         | {status: 'unavailable'}}
 *   `no-binary` distinguishes a missing/unlocatable binary from a runtime
 *   `unavailable`, so the read-fanout funnel can see a dark delivered hint.
 */
function runOverviewAnswer(opts = {}) {
  const {
    cwd,
    dir,
    timeoutMs = DEFAULT_TIMEOUT_MS,
    maxBytes = DEFAULT_MAX_BYTES,
  } = opts;
  try {
    if (!dir || typeof dir !== 'string' || dir.length > 300) {
      return { status: 'unavailable' };
    }
    const binary = resolveAnswerBinary(opts);
    if (!binary) return { status: 'no-binary' };
    const res = runCg(binary, ['overview', dir], { cwd, timeoutMs });
    // Deliberately NOT exitOneIsNoHits: `overview` exits 1 for "no indexed
    // files under that path", and this hint reports that as unavailable. It is
    // the one arm that differs, preserved from the pre-hoist code.
    //
    // The verdict is RETURNED rather than collapsed to a literal
    // `'unavailable'`. Collapsing it reads the same on this exit path and is
    // not: it makes the flag above decorative, so flipping it changes nothing
    // and no test can see the difference. Measured — the first version of this
    // hoist had exactly that shape, and the mutation that flips the flag stayed
    // green against it.
    const verdict = classifyRun(res, { exitOneIsNoHits: false });
    if (verdict !== 'ok') {
      return { status: verdict };
    }
    const out = (res.stdout || '').trim();
    if (isEmptyAnswer(out)) return { status: 'no-hits' };
    const { text, truncated } = truncateAtLine(out, maxBytes);
    return { status: 'hits', text, truncated };
  } catch {
    return { status: 'unavailable' };
  }
}

/**
 * v0.75 — Run `code-graph-mcp callgraph <symbol>` for the cross-file caller/callee
 * tree. This is the ONE thing a raw grep CANNOT return: a symbol-targeted grep
 * hands the model the definition + same-file usages it already scoped to, but NOT
 * "who calls this across the repo". The 2026-06-26 inject audit (13 events, 0
 * CONSUMED) found the grep-echo payload redundant precisely because it re-stated
 * the model's own hits; the caller tree is the marginal signal grep can't give.
 *
 * "hits" requires an actual EDGE line (`← called by` / `→ calls`) — a bare symbol
 * header with no edges (leaf symbol, or name not in the graph) carries no marginal
 * value over the grep the model already ran, so it degrades to `no-hits` and the
 * caller falls back to the grep/show echo. Same bounded/best-effort posture as the
 * sibling runners; any failure → `unavailable` / `no-binary`, never a new failure.
 * @returns {{status: 'hits', text: string, truncated: boolean}
 *         | {status: 'no-hits'}
 *         | {status: 'no-binary'}
 *         | {status: 'unavailable'}}
 */
function runCallgraphAnswer(opts = {}) {
  const {
    cwd,
    symbol,
    timeoutMs = DEFAULT_TIMEOUT_MS,
    maxBytes = DEFAULT_MAX_BYTES,
  } = opts;
  try {
    if (typeof symbol !== 'string' || !/^[A-Za-z_][A-Za-z0-9_]*$/.test(symbol)) {
      return { status: 'unavailable' };
    }
    const binary = resolveAnswerBinary(opts);
    if (!binary) return { status: 'no-binary' };

    const res = runCg(binary, ['callgraph', symbol], { cwd, timeoutMs });
    // grep-parity exit codes: 1 = symbol not found (no graph node).
    const verdict = classifyRun(res, { exitOneIsNoHits: true });
    // `reason` separates the two things `unavailable` covers. The binary failing
    // and the binary never being given time are different facts, and the caller
    // renders one of them to the user — "ran but failed" is simply untrue of a
    // run that never started (audit 2026-09-05 NEW-08).
    if (verdict !== 'ok') {
      return { status: verdict, ...(res.budgetExhausted ? { reason: 'budget' } : {}) };
    }
    const out = (res.stdout || '').trim();
    // Only an edge-bearing tree is marginal over the grep the model already ran.
    if (isEmptyAnswer(out) ||
        !(out.includes('← called by') || out.includes('→ calls'))) {
      return { status: 'no-hits' };
    }
    const { text, truncated } = truncateAtLine(out, maxBytes);
    return { status: 'hits', text, truncated };
  } catch {
    return { status: 'unavailable' };
  }
}

module.exports = {
  runGrepAnswer, runShowAnswer, runOverviewAnswer, runCallgraphAnswer,
  truncateAtLine, sanitizeSearchPath,
  // Exported so the integer property can be asserted where it is ESTABLISHED.
  // Asserting it end-to-end instead passes either way: `remainingMs` floors
  // again downstream, so such a test cannot fail and proves nothing (NEW-02).
  DEFAULT_TIMEOUT_MS,
};
