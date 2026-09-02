#!/usr/bin/env node
'use strict';
// Shared temp-dir helper for hook + auto-update scripts.
//
// Why this exists: Claude Code overrides $TMPDIR to ~/.claude/tmp/ so it can
// capture process stdout for transcript replay. That makes `os.tmpdir()`
// resolve to the same directory that holds 9000+ transcript subdirs. Putting
// hook cooldown flags directly there has two failure modes:
//
//   1. Diagnostic blindness — every doc / memory / debug query that checks
//      `/tmp/.code-graph-bash-*` for hook firing returns empty even when the
//      hook is working perfectly. v0.32.0's "PreToolUse dark under green
//      health" investigation chased this red herring for ~2 hours before the
//      $TMPDIR override was identified.
//   2. §8 SAFETY recursive-traversal trap — `~/.claude/tmp/<id>.output` is
//      where CC writes captured process output; scattering 0-byte flag files
//      alongside them amplifies the "grep -r ~/.claude/tmp/" footgun.
//
// Fix: pin all hook + auto-update artifacts to a `code-graph-mcp/` subdir of
// whatever `os.tmpdir()` resolves to. Contained, deterministic, easy to GC.
const crypto = require('crypto');
const fs = require('fs');
const os = require('os');
const path = require('path');

const CG_TMP_DIR = path.join(os.tmpdir(), 'code-graph-mcp');

function cgTmpDir() {
  try { fs.mkdirSync(CG_TMP_DIR, { recursive: true }); } catch { /* ok */ }
  return CG_TMP_DIR;
}

// Short, filename-safe digest of a project root. Cooldown flags live in ONE
// shared tmp dir for the whole machine, so a flag named only after its subject
// (a command, a symbol, a context type) is global: an `impact` push in project A
// silenced the identical prompt in project B for the next 30s, and a grep
// cooldown followed the developer across every repo they had open. Folding the
// cwd in makes the flag mean what it always claimed to — "recently done HERE".
// pre-read-guide.js already did this; the other four hooks did not.
function cwdHash(cwd) {
  return crypto.createHash('sha1').update(String(cwd)).digest('hex').slice(0, 12);
}

// The per-command cooldown quartet — commandHash / flagPath / isOnCooldown /
// markCooldown — was written twice, near-byte-identically, in pre-grep-guide.js
// and post-grep-inject.js (audit 2026-08-29 ARC-06). The only thing that
// differed was the filename prefix, and a prefix is a parameter; the four
// functions are not. They already disagreed in one place: one hashed `cmd`, the
// other `String(cmd)`, so a non-string command threw in one hook and hashed in
// the other. This takes the `String()` form for both.
//
// The prefix stays the caller's, and must stay DISTINCT per hook: a PreToolUse
// deny and a PostToolUse inject for different commands must not suppress each
// other. Flag paths are byte-identical to what each hook wrote before.
//
// pre-read-guide.js is deliberately NOT folded in: its `.code-graph-readfan-*`
// file is a JSON state document keyed on the cwd alone, not a per-command
// cooldown flag — same directory, different thing.
function makeCooldown(prefix, { windowMs: defaultWindowMs = 60000 } = {}) {
  const commandHash = (cmd) =>
    crypto.createHash('sha1').update(String(cmd)).digest('hex').slice(0, 12);
  // Project-scoped: the same `grep -rn "foo" src/` in two repos is two different
  // questions and must not share one cooldown (see cwdHash above).
  const flagPath = (cmd, cwd = process.cwd()) =>
    path.join(cgTmpDir(), `.code-graph-${prefix}-${cwdHash(cwd)}-${commandHash(cmd)}`);
  const isOnCooldown = (cmd, now = Date.now(), windowMs = defaultWindowMs, cwd = process.cwd()) => {
    try {
      return now - fs.statSync(flagPath(cmd, cwd)).mtimeMs < windowMs;
    } catch { return false; }
  };
  const markCooldown = (cmd, cwd = process.cwd()) => {
    try { fs.writeFileSync(flagPath(cmd, cwd), ''); } catch { /* ok */ }
  };
  return { commandHash, flagPath, isOnCooldown, markCooldown };
}

// Nothing ever deleted what cgTmpDir() collects. Every cooldown flag, read-fanout
// state file and interrupted `update-*` download stayed forever: measured 281
// entries on a working dev box, 232 of them older than a day. They are 0-byte
// flags, so the cost is inode churn and a directory that makes `ls` in the shared
// tmp useless — but the `update-*` dirs hold a partly-extracted release tarball,
// which is megabytes each.
//
// The window has to exceed the longest cooldown that reads these files, or the
// prune would silently shorten it. The longest is user-prompt-context's
// `overview` at 5 minutes, so 24h is ~288× the margin — this is garbage
// collection, not expiry.
const PRUNE_MAX_AGE_MS = 24 * 60 * 60 * 1000;

/**
 * Delete cgTmpDir() entries older than `maxAgeMs`. Best-effort and total: any
 * failure (missing dir, a file another process just removed, a permission
 * problem) is skipped rather than raised — this runs from a SessionStart hook,
 * where a throw is a visible failure and unreclaimed disk is not.
 * @returns {number} entries removed
 */
function pruneCgTmp({ now = Date.now(), maxAgeMs = PRUNE_MAX_AGE_MS, dir = CG_TMP_DIR } = {}) {
  // The recursive delete below is the reason for this check. `dir` is a
  // parameter only so tests can point it at a sandbox, and a future caller
  // passing the wrong path must not be able to turn this into `rm -rf` on
  // something that isn't ours.
  if (path.basename(dir) !== 'code-graph-mcp') return 0;
  let removed = 0;
  let entries;
  try {
    entries = fs.readdirSync(dir, { withFileTypes: true });
  } catch { return 0; }
  for (const entry of entries) {
    const target = path.join(dir, entry.name);
    try {
      if (now - fs.statSync(target).mtimeMs <= maxAgeMs) continue;
      // `update-*` staging dirs from an interrupted auto-update are the only
      // subdirectories here, and they are the ones that actually hold bytes.
      if (entry.isDirectory()) fs.rmSync(target, { recursive: true, force: true });
      else fs.unlinkSync(target);
      removed++;
    } catch { /* raced, unreadable, or already gone — the next run retries */ }
  }
  return removed;
}

module.exports = { cgTmpDir, CG_TMP_DIR, cwdHash, makeCooldown, pruneCgTmp, PRUNE_MAX_AGE_MS };
