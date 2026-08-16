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
      additionalContext: text,
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
      additionalContext: text,
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
      additionalContext: text,
    },
  });
}

module.exports = { emitPreToolContext, emitPreToolAllowContext, emitPostToolContext };
