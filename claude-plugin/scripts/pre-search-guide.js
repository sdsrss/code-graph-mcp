#!/usr/bin/env node
'use strict';
// PreToolUse hook: On FIRST Grep call per session window, remind Claude
// about code-graph alternatives. Runs fast (<10ms) and only outputs once.
const fs = require('fs');
const path = require('path');
const os = require('os');

const flag = path.join(os.tmpdir(), '.code-graph-search-guided');
const WINDOW_MS = 2 * 60 * 60 * 1000; // 2 hours (approximate session window)

try {
  const stat = fs.statSync(flag);
  if (Date.now() - stat.mtimeMs < WINDOW_MS) process.exit(0);
} catch { /* flag doesn't exist — first time */ }

fs.writeFileSync(flag, '');
process.stdout.write(
  '[code-graph] For code understanding, prefer code-graph tools over Grep:\n' +
  '  project_map → full project architecture overview (call FIRST)\n' +
  '  semantic_code_search → find code by concept (10x fewer tokens)\n' +
  '  get_call_graph → who calls X / what X calls (13x fewer tokens)\n' +
  '  module_overview → understand a module (20x fewer tokens)\n' +
  'Use Grep only for exact strings, constants, or regex patterns.\n'
);
