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
 * The same split, but keeping the half `sanitizeSearchPath` throws away.
 *
 * Widening `tests/*.mjs` to `tests` keeps rg from exiting 1, and for years that
 * was the whole story — but the deny then PRINTS the widened command above an
 * answer covering files the user excluded (field report 2026-09-08: a
 * `tests/*.mjs` grep answered with 25+ hits from all of `tests/`). The glob has
 * a faithful destination: `code-graph-mcp grep -g <glob>`, which matches
 * basename-style across nested directories.
 *
 * `{scope, glob}`, either of which may be undefined: `tests/*.mjs` →
 * `{scope:'tests', glob:'*.mjs'}`, `src/**` + `/*.rs` → `{scope:'src',
 * glob:'**\/*.rs'}`, a leading glob (`*.py`) → `{scope:undefined,
 * glob:'*.py'}` (repo-wide, filtered), no glob → `{scope:<path>}`.
 */
function splitSearchPathGlob(searchPath) {
  if (!searchPath || typeof searchPath !== 'string') return {};
  const segs = searchPath.split('/');
  const i = segs.findIndex((s) => /[*?[\]{}]/.test(s));
  if (i === -1) return { scope: searchPath };
  const scope = segs.slice(0, i).join('/') || undefined;
  const glob = segs.slice(i).join('/') || undefined;
  return { scope, glob };
}

/**
 * The argv for the grep answer — the ONE place the answer's shape is decided.
 *
 * Both the child process and the command string the deny prints are built from
 * this array, because two renderings of one command are two things that drift:
 * v0.142.0 shipped a fix for two printers of a single `unadopt` string that had
 * done exactly that. Here the drift was live and user-visible in a different
 * form — the printed command never carried the grep's flags at all, so a
 * `grep -l` deny announced a file-list command and delivered hit lines.
 *
 * `flags` are already translated to cg spellings by the caller
 * (pre-grep-guide.extractCgFlags). A `-g` among them came from `--include` and
 * wins over one derived from the path: the user spelled that filter explicitly.
 */
function buildGrepArgs({ pattern, searchPath, flags = [] } = {}) {
  const { scope, glob } = splitSearchPathGlob(searchPath);
  const args = ['grep', ...flags, pattern];
  if (scope) args.push(scope);
  // `hasGlobFlag`, not `flags.includes('-g')`: `flags` is a flat argv fragment,
  // so a VALUE can equal a flag name. `rg -t -g "sym" src/*.rs` yields
  // `['-t','-g']` — `-g` there is the VALUE of `-t` — and the bare membership
  // test dropped the path-derived glob, widening `src/*.rs` to `src`. That is
  // the defect this function exists to fix, reintroduced one layer up (round 2
  // of pre-ship review).
  if (glob && !hasGlobFlag(flags)) args.push('-g', glob);
  return args;
}

// True when `flags` carries a real `-g` (an explicit `--include`/`--glob`), as
// opposed to the string `-g` sitting in a value position.
function hasGlobFlag(flags) {
  for (let i = 0; i < (flags || []).length; i++) {
    if (flags[i] === '-g') return true;
    if (flags[i] === '-t') i++;  // skip its value
  }
  return false;
}

// Render an argv as the command a human can paste. Single-quote anything that
// would not survive a shell verbatim — the glob especially, which is the whole
// point of passing it (`-g *.mjs` unquoted is expanded by the user's shell).
// Characters that survive a shell verbatim. A glob does not, which is the whole
// reason the argument is worth quoting: `-g *.mjs` unquoted is expanded by the
// reader's own shell before cg ever sees it.
const SHELL_SAFE_ARG = /^[A-Za-z0-9_.,:@%+=/-]+$/;
// POSIX single-quote escape: close, emit an escaped quote, reopen.
const SQ_ESCAPED = "'\\''";

function shellQuoteArg(a) {
  // Deliberately no nested template literal, and no regex holding a quote.
  // windows-hide.test.js masks literals with a character scanner that pairs
  // backticks like any other quote, so a `${…`…`…}` inside a template closes the
  // outer one early and swallows the rest of the file — which took all three of
  // THIS file's spawn call sites out of that guard's view until it was written
  // this way. Code a guard cannot parse is code the guard is not checking.
  const s = String(a);
  return SHELL_SAFE_ARG.test(s) ? s : "'" + s.split("'").join(SQ_ESCAPED) + "'";
}

// `invocation` is what names the binary. The default is the name a reader can
// paste; pre-grep-guide's rewrite passes an already-quoted absolute path when
// that name would not resolve in the Bash tool's shell.
function formatCgCommand(args, invocation = 'code-graph-mcp') {
  return invocation + ' ' + args.map(shellQuoteArg).join(' ');
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
    flags = [],
    timeoutMs = DEFAULT_TIMEOUT_MS,
    maxBytes = DEFAULT_MAX_BYTES,
  } = opts;
  try {
    if (!pattern || typeof pattern !== 'string' || pattern.length > MAX_PATTERN_LEN) {
      return { status: 'unavailable' };
    }
    const binary = resolveAnswerBinary(opts);
    if (!binary) return { status: 'no-binary' };

    // buildGrepArgs does the glob split itself, so a caller that passes a raw
    // `dir/*.py` still cannot put a literal glob into argv (the guaranteed
    // nonzero exit sanitizeSearchPath exists for) — it becomes scope + `-g`.
    const args = buildGrepArgs({ pattern, searchPath, flags });
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
/**
 * The files of the definitions a `show` printed: each definition opens with an
 * unindented header `<kind> <name>  <path>:<start>-<end>  <signature>`, its body
 * indented below. null when an unindented line is not such a header, so a
 * caller that must account for every definition can refuse.
 */
function showDefinitionFiles(out) {
  const defs = [];
  for (const line of out.split('\n')) {
    if (line === '' || /^\s/.test(line)) continue;
    const m = /^(\S+) \S+ {2}(\S+):\d+-\d+(?: |$)/.exec(line);
    if (!m) return null;
    defs.push({ kind: m[1], file: m[2] });
  }
  return defs.length ? defs : null;
}

/** Whether a root-relative file lies under a grep path ('' = the whole root). */
function pathWithin(file, within) {
  const w = within.replace(/^(\.\/)+/, '').replace(/\/+$/, '');
  return w === '' || w === '.' || file === w || file.startsWith(w + '/');
}

function runShowAnswer(opts = {}) {
  const {
    cwd,
    symbols,
    // A grep path the answer must stay inside (D#125 #2): `show` prints every
    // same-named definition in the project. With `within`, a symbol is answered
    // only when EVERY definition `show` prints lies inside the path and is one
    // the grep's declaration keyword matches (`kinds`: symbol → the label and
    // file-extension shapes it matches); a symbol with no definition inside is
    // skipped (the grep finds none either), and any other mix refuses the whole
    // answer. Narrowing with `show --file` was tried and removed: the label is
    // coarser than the keyword, so a narrowed answer dropped real matches, and
    // the inject printed the unnarrowed text (review of D#125, round 2).
    // undefined: unscoped, as before.
    within,
    kinds,
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
    // Which symbols actually resolved — the rewrite re-runs exactly these, so a
    // symbol that printed nothing here cannot turn into an exit-1 in the Bash call.
    const resolved = [];
    // The argv that re-runs each resolved symbol, scoped when `within` is set.
    const argvs = [];
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
      const argv = ['show', sym];
      if (within !== undefined) {
        const defs = showDefinitionFiles(out);
        if (!defs) return { status: 'unavailable', reason: 'scope' };
        if (!defs.some((d) => pathWithin(d.file, within))) continue;
        const shapes = (kinds && kinds[sym]) || [];
        const ext = (f) => (f.match(/\.([A-Za-z0-9]+)$/) || [])[1];
        const matches = (d) => pathWithin(d.file, within)
          && shapes.some((k) => k.labels.includes(d.kind) && k.exts.includes(ext(d.file)));
        if (!defs.every(matches)) return { status: 'unavailable', reason: 'scope' };
      }
      parts.push(`$ code-graph-mcp ${argv.join(' ')}\n${out}`);
      resolved.push(sym);
      argvs.push(argv);
    }
    if (parts.length === 0) return { status: 'no-hits' };
    const { text, truncated } = truncateAtLine(parts.join('\n\n'), maxBytes);
    return { status: 'hits', text, truncated, symbols: resolved, argvs };
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
      // Same arm as the three sibling runners: an exhausted hook budget reaches
      // `classifyRun` as a synthetic timeout, so without this it is reported as
      // a bare `unavailable` — indistinguishable from a wedged or broken binary.
      // This runner is the read-fanout hint's only answer path, and its budget is
      // the one most often already spent (pre-read runs after a SessionStart that
      // already burned six children), so it is the worst place to lose the cause.
      return { status: verdict, ...(res.budgetExhausted ? { reason: 'budget' } : {}) };
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
    // `callgraph` promotes a unique fuzzy match when the name has no edges of
    // its own (`task` → `run_startup_tasks`) and says so only on stderr, which
    // this run discards. A tree rooted at another symbol is not an answer about
    // the grepped one: real sessions got up to 27 of 98 injections this way.
    if (out.split('\n', 1)[0].split(' ', 1)[0] !== symbol) {
      return { status: 'no-hits' };
    }
    const { text, truncated } = truncateAtLine(out, maxBytes);
    return { status: 'hits', text, truncated };
  } catch {
    return { status: 'unavailable' };
  }
}

// A definition label longer than this is cut: FastAPI handlers carry 900+
// characters of `Query(...)` defaults in their signature.
const DEFINITION_LABEL_MAX = 160;
const DEFINITION_RET_MAX = 60;

/**
 * `name(params)…` for a `show --json` row. Most extractors write the signature
 * from the parameter list on (`(a, b) -> T`); C and C++ write the return type
 * first (`char (int a)`), where `param_types` is the trailing `(…)` and the
 * name goes between them (`char f(int a)`). Any other shape gets the name alone.
 */
function definitionSignature(r) {
  const sig = typeof r.signature === 'string' ? r.signature.trim() : '';
  if (sig.startsWith('(')) return `${r.name}${sig}`;
  const params = typeof r.param_types === 'string' ? r.param_types.trim() : '';
  if (params.startsWith('(') && sig.endsWith(params) && sig.length > params.length) {
    const ret = sig.slice(0, -params.length).trim();
    // A return type this long would push the name past the label's cap.
    return ret.length > DEFINITION_RET_MAX ? `${r.name}${params}` : `${ret} ${r.name}${params}`;
  }
  return r.name;
}

/**
 * Where exactly `symbol` is defined in `file`: `show <symbol> --file <file>
 * --json`, kept only when ONE row in that file carries the name (`X`, a
 * qualified `Cls.X` or `mod::X`). The outside-path grep inject names this in
 * place of a command to run (tasks/specs/outside-path-definition-answer.md):
 * the model's next step after that pointer was a read of the definition, not
 * a code-graph call (26 vs 4 of 60 real events).
 *
 * `label` is `<type> <name><signature>` on one line, whitespace collapsed,
 * at most DEFINITION_LABEL_MAX characters (`…` when cut).
 * @returns {{status: 'hits', start: number, end: number, label: string}
 *         | {status: 'several'} | {status: 'no-hits'}
 *         | {status: 'no-binary'} | {status: 'unavailable', reason?: 'budget'}}
 */
function runDefinitionAnswer(opts = {}) {
  const { cwd, symbol, file, timeoutMs = DEFAULT_TIMEOUT_MS } = opts;
  try {
    if (typeof symbol !== 'string' || !/^[A-Za-z_][A-Za-z0-9_]*$/.test(symbol)
        || typeof file !== 'string' || !file) {
      return { status: 'unavailable' };
    }
    const binary = resolveAnswerBinary(opts);
    if (!binary) return { status: 'no-binary' };
    // --compact: the rows without `code_content` (a whole class body otherwise).
    const res = runCg(binary, ['show', symbol, '--file', file, '--json', '--compact'], { cwd, timeoutMs });
    const verdict = classifyRun(res, { exitOneIsNoHits: true });
    if (verdict !== 'ok') {
      return { status: verdict, ...(res.budgetExhausted ? { reason: 'budget' } : {}) };
    }
    let rows;
    try {
      rows = JSON.parse(res.stdout || '');
    } catch {
      return { status: 'unavailable' };
    }
    if (!Array.isArray(rows)) return { status: 'unavailable' };
    const named = rows.filter((r) => r && r.file_path === file && typeof r.name === 'string'
      && (r.name === symbol || r.name.endsWith(`.${symbol}`) || r.name.endsWith(`::${symbol}`)));
    if (named.length === 0) return { status: 'no-hits' };
    if (named.length > 1) return { status: 'several' };
    const r = named[0];
    if (!Number.isInteger(r.start_line) || !Number.isInteger(r.end_line)) return { status: 'unavailable' };
    let label = `${r.type || 'symbol'} ${definitionSignature(r)}`
      // U+0085 and U+001C-1E too: JS `\s` lacks them, and a reader splitting
      // lines (Python's splitlines) breaks on them.
      .replace(/[\s\u0085\u001c-\u001e]+/g, ' ')
      // A signature written one parameter per line collapses to `( a, b, )`:
      // drop the padding inside the parentheses and the trailing comma.
      .replace(/\( /g, '(').replace(/,? \)/g, ')').trim();
    const chars = [...label];
    if (chars.length > DEFINITION_LABEL_MAX) label = chars.slice(0, DEFINITION_LABEL_MAX - 1).join('') + '…';
    return { status: 'hits', start: r.start_line, end: r.end_line, label };
  } catch {
    return { status: 'unavailable' };
  }
}

module.exports = {
  runGrepAnswer, runShowAnswer, runOverviewAnswer, runCallgraphAnswer, runDefinitionAnswer,
  truncateAtLine, sanitizeSearchPath,
  // The one argv builder + its renderer, so the deny copy and the child process
  // cannot describe different commands.
  splitSearchPathGlob, buildGrepArgs, formatCgCommand, shellQuoteArg,
  // pre-grep-guide names this binary in the command it rewrites a grep into.
  resolveAnswerBinary,
  // Exported so the integer property can be asserted where it is ESTABLISHED.
  // Asserting it end-to-end instead passes either way: `remainingMs` floors
  // again downstream, so such a test cannot fail and proves nothing (NEW-02).
  DEFAULT_TIMEOUT_MS,
};
