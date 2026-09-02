#!/usr/bin/env node
'use strict';
const { execFileSync } = require('child_process');
const { findBinary } = require('./find-binary');
const { hidden } = require('./proc-opts');

// v0.21 — gated default-off. v0.18.0 added query-time freshness
// (ensure_file_indexed) inside MCP tools that take a file_path arg, so a
// PostToolUse hook spawning a fresh process on every Edit/Write was redundant
// for the MCP-driven workflow and just burnt ~80ms cold-start per edit.
//
// CLI-only workflows (running `code-graph-mcp search` after Bash-side edits
// without going through MCP) need the hook to keep the DB fresh, so the knob
// lets users opt back in.
//
// Priority (high → low):
//   1. CODE_GRAPH_HOOK_INDEX=on  → run the hook (opt-in)
//   2. CODE_GRAPH_HOOK_INDEX=off → skip
//   3. default                   → skip (v0.21 flip)
function shouldRun(env = process.env) {
  const v = (env.CODE_GRAPH_HOOK_INDEX || '').toLowerCase();
  if (v === 'on' || v === '1' || v === 'true') return true;
  return false;
}

function runMain() {
  if (!shouldRun()) return;

  const bin = findBinary();
  if (!bin) return; // silent — binary not installed yet

  try {
    execFileSync(bin, ['incremental-index', '--quiet'], hidden({
      timeout: 8000,
      stdio: ['pipe', 'pipe', 'pipe']
    }));
  } catch { /* timeout or error — silent for hook */ }
}

if (require.main === module) {
  require('./hook-fail-open').installHookFailOpen('PostToolUse:Write|Edit');
  runMain();
}

module.exports = { shouldRun };
