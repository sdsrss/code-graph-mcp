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
function installHookFailOpen(label) {
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
    process.exit(0);
  };
  process.on('uncaughtException', bail);
  process.on('unhandledRejection', bail);
}

module.exports = { installHookFailOpen };
