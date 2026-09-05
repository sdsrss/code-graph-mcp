#!/usr/bin/env node
'use strict';
// Fail-open wrapper for hook entry points (audit 2026-08-29 JS-12).
//
// A hook is optional housekeeping running inside somebody else's tool call. Its
// internal operations each carry their own guards, so this is defence in depth
// rather than a known crash — but the cost asymmetry is total: an unhandled
// throw prints a node stack trace into the user's session and exits non-zero,
// for work whose entire value is being unobtrusive. `session-init.js` learned
// this in audit 2026-08-16 P1-16 and grew a try/catch; the other eight entry
// points did not.
//
// A `process.on` handler rather than a try/catch around each `main()`, for two
// reasons. Two of the entry points (`pre-edit-guide.js`, `statusline.js`) are
// straight-line scripts with no main function to wrap, so a try/catch means
// re-indenting the whole file — a large diff through the exact hooks that gate
// Edit and the statusline. And a handler also covers the async escape, which is
// the one that actually happens: a rejected promise from a spawn or a read.
//
// EPIPE is silent by design. It means the consumer closed the pipe — Claude
// Code moved on, or a `| head` upstream exited. There is nobody left to tell.
// The budget Claude Code kills each hook at, in SECONDS — the single source for
// both halves of that contract: `lifecycle.js` registers these numbers into
// settings.json, and `remainingMs` below spends against them. `hooks.test.js`
// pins both registration sites to this table so a bump in one cannot drift from
// the other.
//
// `session-init.js` is registered from `claude-plugin/hooks/hooks.json` rather
// than by `lifecycle.js` (SessionStart is the one event Claude Code loads from
// plugin-cache), so `hooks.test.js` pins that file to this table too. It was
// the last unclamped hook — 21.5 s of serial children against the 5 s below,
// the largest overrun of the seven — until audit 2026-09-05 NEW-05 wired it.
// Its skips are not uniform: see the budget block at the top of
// `session-init.js` for which children may be dropped silently and which two
// report a distinct result instead of a fabricated all-clear.
const HOOK_TIMEOUT_SECONDS = {
  'pre-edit-guide.js': 4,
  'pre-grep-guide.js': 3,
  'pre-read-guide.js': 3,
  'incremental-index.js': 10,
  'post-grep-inject.js': 5,
  'user-prompt-context.js': 5,
  'session-init.js': 5,
};

// Left for the hook to render its answer and exit after the last child returns.
const WRITE_RESERVE_MS = 400;

// Wall-clock instant this process must be finished by, or null when nothing
// armed one (a hook `require`d by a test, or an unlisted script).
let deadlineAt = null;

/**
 * Arm the process deadline from the registered budget of `script`.
 *
 * The hooks' internal timeouts were each sized in isolation and run in SERIES:
 * pre-edit-guide alone could spend 5 × 2000 ms of candidate greps plus 2500 ms
 * of impact against a 4 s budget, and post-grep-inject looped callgraph over
 * every symbol in the pattern before its show/grep fallbacks — 2–3× the budget
 * either way. Nothing enforced the sum, so a binary wedged on `index.lock` got
 * the hook killed by Claude Code, which surfaces to the user as a hook error on
 * THEIR tool call (audit 2026-09-05 JS-03).
 *
 * `process.uptime()` is subtracted because the budget starts when Claude Code
 * spawns us, not when this line runs: cold node startup is real time already
 * spent, and on a loaded machine it is hundreds of milliseconds.
 */
function armHookDeadline(script) {
  const seconds = HOOK_TIMEOUT_SECONDS[script];
  if (!seconds) return;
  deadlineAt = Math.floor(Date.now() + seconds * 1000 - process.uptime() * 1000 - WRITE_RESERVE_MS);
}

/**
 * How long a child may run: `defaultMs`, or whatever is left of the budget.
 *
 * Returns `null` when there is nothing left — meaning DO NOT RUN, not "run
 * unbounded". Callers must branch on it: node reads `timeout: 0` as no timeout
 * at all, so a numeric zero here would turn the last child into the unbounded
 * one, which is the exact failure this exists to prevent.
 *
 * Always an INTEGER. `child_process` validates `timeout` with
 * `validateTimeout` and throws `ERR_OUT_OF_RANGE` on a fraction — and the one
 * fractional term here (`process.uptime()`) made every spawn throw, which the
 * runners' own try/catch turned into a silent `unavailable`: the hooks stopped
 * answering and still exited 0. Caught by pre-grep-guide's e2e suite.
 */
function remainingMs(defaultMs) {
  if (deadlineAt === null) return Math.floor(defaultMs);
  const left = deadlineAt - Date.now();
  if (left <= 0) return null;
  return Math.floor(Math.min(defaultMs, left));
}

/**
 * Test seam: drop any armed deadline so one test file can't leak into another.
 *
 * `at` (an absolute epoch ms) arms one directly instead. `armHookDeadline`
 * derives its instant from `process.uptime()`, so a test that wants the
 * budget-EXHAUSTED branch would otherwise have to wait out a real budget — a
 * clock race dressed up as a test. Pass `Date.now() - 1` to make every
 * `remainingMs` return null deterministically.
 */
function resetHookDeadline(at = null) {
  deadlineAt = at === null ? null : Math.floor(at);
}

function installHookFailOpen(label) {
  armHookDeadline(require('path').basename(process.argv[1] || ''));
  const bail = (err) => {
    const code = (err && err.code) || (err && err.name) || 'Error';
    if (code !== 'EPIPE') {
      try {
        process.stderr.write(
          `[code-graph] ${label} hook error (${code}): ${(err && err.message) || String(err)}\n` +
          '            The tool call continues; run `code-graph-mcp doctor` if this repeats.\n'
        );
      } catch { /* stderr is gone too — there is nothing further to do */ }
    }
    // Exit 0, not the throw's non-zero: for a PreToolUse hook a non-zero exit is
    // a DECISION (2 = deny), so crashing must not read as a verdict.
    //
    // Known limit: `process.exit` does not flush a pending stdout write, so an
    // async throw AFTER a partial decision write could truncate it mid-JSON.
    // Every entry point here writes its decision in one final call, so the
    // window is "threw between that write and process exit". Draining instead
    // (setting `process.exitCode` and returning) trades that for a hang risk
    // against the hook's own 3-10s timeout, which is the worse failure — a
    // timeout blocks the user's tool call, a truncated write does not.
    process.exit(0);
  };
  process.on('uncaughtException', bail);
  process.on('unhandledRejection', bail);
}

module.exports = {
  installHookFailOpen,
  HOOK_TIMEOUT_SECONDS,
  armHookDeadline,
  remainingMs,
  resetHookDeadline,
};
