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

const DEFAULT_TIMEOUT_MS = 2000;
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
  return { text: buf.subarray(0, maxBytes).toString('latin1'), truncated: true };
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
 *         | {status: 'unavailable'}}
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
    if (!binary) return { status: 'unavailable' };

    const args = ['grep', pattern];
    if (searchPath) args.push(searchPath);
    const res = spawnSync(binary, args, {
      cwd,
      timeout: timeoutMs,
      encoding: 'utf8',
      maxBuffer: 4 * 1024 * 1024,
      stdio: ['ignore', 'pipe', 'ignore'],
    });
    if (res.error || res.signal || res.status !== 0) {
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

module.exports = { runGrepAnswer, truncateAtLine };
