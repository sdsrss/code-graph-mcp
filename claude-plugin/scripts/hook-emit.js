#!/usr/bin/env node
'use strict';
// Shared hook emit envelopes. One place defines the CC hookSpecificOutput JSON
// schema so the three sibling delivery hooks (pre-read-guide / pre-edit-guide /
// post-grep-inject) cannot drift apart (feedback_hook_class_bug_sweep — no
// inline copies of shared logic). DRY mirror of the project-root.js precedent.
//
// Why these three shapes:
//   - PreToolUse plain stdout on exit 0 goes to the DEBUG LOG ONLY — it never
//     reaches the model (CC docs, code.claude.com/docs/en/hooks.md, v2026-06).
//     `additionalContext` is what surfaces the carried text.
//   - `permissionDecision: 'allow'` is NOT a delivery detail. The CC hooks
//     reference defines it as: "skip the interactive permission prompt" (deny
//     rules and connector/`requiresUserInteraction` prompts still apply). On a
//     machine that prompts for a tool, a hook sending `allow` has answered the
//     user's prompt for them. That is defensible for READ-ONLY Read; it is not
//     for Edit, which writes to disk — a delivery hook must never buy context
//     visibility with the user's write consent (audit 2026-08-16 P0-2).
//     Therefore: `emitPreToolAllowContext` is for Read ONLY, and every
//     write-capable tool uses `emitPreToolContext` (no decision at all, which
//     the docs' own PreToolUse example marks as "no decision; normal permission
//     flow applies"). If a future CC drops additionalContext without a decision,
//     the correct outcome is that the Edit impact summary goes quiet — NOT that
//     it re-acquires the elevation.
//   - PostToolUse honors `additionalContext` permission-neutrally (no
//     permissionDecision), so the Bash-side grep answer can be injected without
//     skipping CC's default permission prompt for the underlying tool call.

// Ceiling on injected context, applied at the ONE place all three hooks emit
// through. `cg-answer.js` has capped its own output at 4000 bytes since it was
// written; the hook payloads it sits alongside had no cap at all, and they are
// assembled from unbounded lists — pre-edit-guide joins every direct caller's
// `name (file)` onto a single line, so editing a 200-caller symbol injected a
// multi-kilobyte wall into the model's context on every Edit (2026-08-16 audit
// §四). This is the model's context window, not a log: the whole value of an
// impact summary is that it is small enough to read.
//
// Truncation is announced, never silent — a summary that stops mid-list without
// saying so is worse than one that says it was cut, because the reader cannot
// tell a short blast radius from a clipped one.
const MAX_INJECTED_BYTES = 4000;

function capContext(text) {
  const s = String(text == null ? '' : text);
  if (Buffer.byteLength(s, 'utf8') <= MAX_INJECTED_BYTES) return s;
  const notice = `\n  … truncated at ${MAX_INJECTED_BYTES} bytes — re-run the CLI command above for the full result.\n`;
  const budget = MAX_INJECTED_BYTES - Buffer.byteLength(notice, 'utf8');
  // Slice on a UTF-16 code-unit boundary that fits the byte budget. This keeps
  // the byte cap exact and never splits a 1-3 byte UTF-8 character (ASCII, Latin,
  // CJK — what file paths and symbol names actually contain). It CAN split an
  // astral-plane character (emoji, 2 code units) into a lone surrogate:
  // `JSON.stringify` escapes that, so the envelope stays parseable and the model
  // sees one replacement character at the cut. Saying so rather than claiming
  // "never cut in half", which is what this comment used to claim (v0.118.0
  // pre-tag review verified the emoji case).
  let end = s.length;
  while (end > 0 && Buffer.byteLength(s.slice(0, end), 'utf8') > budget) {
    end -= Math.max(1, Math.ceil((Buffer.byteLength(s.slice(0, end), 'utf8') - budget) / 4));
  }
  // Prefer cutting at the last newline inside the budget, so the truncated text
  // ends on a whole line rather than mid-token.
  const nl = s.lastIndexOf('\n', end);
  if (nl > budget / 2) end = nl;
  return s.slice(0, end) + notice;
}

/**
 * PreToolUse additionalContext envelope with NO permissionDecision (string, no
 * trailing newline). The permission-neutral shape: the tool's normal permission
 * flow is untouched. Use this for every write-capable tool (Edit/Write/…).
 * @param {string} text
 * @returns {string} JSON line
 */
function emitPreToolContext(text) {
  return JSON.stringify({
    hookSpecificOutput: {
      hookEventName: 'PreToolUse',
      additionalContext: capContext(text),
    },
  });
}

/**
 * PreToolUse allow + additionalContext envelope (string, no trailing newline).
 *
 * READ-ONLY TOOLS ONLY (pre-read-guide). `allow` skips the user's interactive
 * permission prompt; for Read that grants nothing the model could not already
 * get, and it is what keeps the fanout hint visible. Do not reuse it for a tool
 * that mutates state — see the note above.
 * @param {string} text
 * @returns {string} JSON line
 */
function emitPreToolAllowContext(text) {
  return JSON.stringify({
    hookSpecificOutput: {
      hookEventName: 'PreToolUse',
      permissionDecision: 'allow',
      additionalContext: capContext(text),
    },
  });
}

/**
 * PostToolUse additionalContext envelope (string, no trailing newline).
 * Permission-neutral: NO permissionDecision, so the underlying Bash tool call's
 * permission flow is untouched while the answer still reaches the model.
 * @param {string} text
 * @returns {string} JSON line
 */
function emitPostToolContext(text) {
  return JSON.stringify({
    hookSpecificOutput: {
      hookEventName: 'PostToolUse',
      additionalContext: capContext(text),
    },
  });
}

module.exports = {
  emitPreToolContext, emitPreToolAllowContext, emitPostToolContext,
  capContext, MAX_INJECTED_BYTES,
};
