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
const DEFAULT_TIMEOUT_MS = (() => {
  const override = Number(process.env._CG_ANSWER_TIMEOUT_MS);
  return Number.isFinite(override) && override > 0 ? override : 2000;
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
    let binary = opts.binary;
    if (binary === undefined) {
      binary = process.env._CG_ANSWER_BINARY || require('./find-binary').findBinary();
    }
    if (!binary) return { status: 'no-binary' };

    // Defensive re-sanitize: callers should pass a clean path, but a glob
    // reaching argv is a guaranteed nonzero exit (see sanitizeSearchPath).
    const scope = sanitizeSearchPath(searchPath);
    const args = ['grep', pattern];
    if (scope) args.push(scope);
    const res = spawnSync(binary, args, hidden({
      cwd,
      timeout: timeoutMs,
      encoding: 'utf8',
      maxBuffer: 4 * 1024 * 1024,
      stdio: ['ignore', 'pipe', 'ignore'],
      // Hook-internal run: a delivered answer, not a model-initiated conversion.
      // The CLI skips its recommendations.jsonl `use` record when this is set.
      env: { ...process.env, CODE_GRAPH_INTERNAL: '1' },
    }));
    if (res.error || res.signal) {
      return { status: 'unavailable' };
    }
    // v0.50 grep-parity exit codes: 0 = matched, 1 = no match, 2 = error.
    // Older binaries exit 0 on no-match with the NO_MATCH_PREFIX on stderr
    // (stdout empty) — both shapes resolve to 'no-hits' below.
    if (res.status === 1) {
      return { status: 'no-hits' };
    }
    if (res.status !== 0) {
      return { status: 'unavailable' };
    }
    const out = (res.stdout || '').trim();
    if (!out || out.startsWith(NO_MATCH_PREFIX)) {
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
    let binary = opts.binary;
    if (binary === undefined) {
      binary = process.env._CG_ANSWER_BINARY || require('./find-binary').findBinary();
    }
    if (!binary) return { status: 'no-binary' };

    const parts = [];
    for (const sym of symbols.slice(0, 3)) {
      if (typeof sym !== 'string' || !/^[A-Za-z_][A-Za-z0-9_]*$/.test(sym)) continue;
      const res = spawnSync(binary, ['show', sym], hidden({
        cwd,
        timeout: timeoutMs,
        encoding: 'utf8',
        maxBuffer: 4 * 1024 * 1024,
        stdio: ['ignore', 'pipe', 'ignore'],
        env: { ...process.env, CODE_GRAPH_INTERNAL: '1' },
      }));
      if (res.error || res.signal || res.status !== 0) continue;
      const out = (res.stdout || '').trim();
      if (!out || out.startsWith(NO_MATCH_PREFIX)) continue;
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
    let binary = opts.binary;
    if (binary === undefined) {
      binary = process.env._CG_ANSWER_BINARY || require('./find-binary').findBinary();
    }
    if (!binary) return { status: 'no-binary' };
    const res = spawnSync(binary, ['overview', dir], hidden({
      cwd,
      timeout: timeoutMs,
      encoding: 'utf8',
      maxBuffer: 4 * 1024 * 1024,
      stdio: ['ignore', 'pipe', 'ignore'],
      env: { ...process.env, CODE_GRAPH_INTERNAL: '1' },
    }));
    if (res.error || res.signal || res.status !== 0) {
      return { status: 'unavailable' };
    }
    const out = (res.stdout || '').trim();
    if (!out || out.startsWith(NO_MATCH_PREFIX)) return { status: 'no-hits' };
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
    let binary = opts.binary;
    if (binary === undefined) {
      binary = process.env._CG_ANSWER_BINARY || require('./find-binary').findBinary();
    }
    if (!binary) return { status: 'no-binary' };

    const res = spawnSync(binary, ['callgraph', symbol], hidden({
      cwd,
      timeout: timeoutMs,
      encoding: 'utf8',
      maxBuffer: 4 * 1024 * 1024,
      stdio: ['ignore', 'pipe', 'ignore'],
      env: { ...process.env, CODE_GRAPH_INTERNAL: '1' },
    }));
    if (res.error || res.signal) return { status: 'unavailable' };
    // grep-parity exit codes: 1 = symbol not found (no graph node).
    if (res.status === 1) return { status: 'no-hits' };
    if (res.status !== 0) return { status: 'unavailable' };
    const out = (res.stdout || '').trim();
    // Only an edge-bearing tree is marginal over the grep the model already ran.
    if (!out || out.startsWith(NO_MATCH_PREFIX) ||
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
};
