#!/usr/bin/env node
'use strict';
// PreToolUse hook: On FIRST Agent(Explore) call per session window, remind Claude
// that code-graph tools provide faster, structured codebase exploration.
const fs = require('fs');
const path = require('path');
const os = require('os');

const flag = path.join(os.tmpdir(), '.code-graph-explore-guided');
const WINDOW_MS = 2 * 60 * 60 * 1000; // 2 hours

try {
  const stat = fs.statSync(flag);
  if (Date.now() - stat.mtimeMs < WINDOW_MS) process.exit(0);
} catch { /* first time */ }

fs.writeFileSync(flag, '');
process.stdout.write(
  '[code-graph] Before spawning an Explore agent, try code-graph tools first:\n' +
  '  project_map → full architecture overview in one call\n' +
  '  module_overview(path) → module structure, exports, hot paths\n' +
  '  get_call_graph(symbol) → trace call chains instantly\n' +
  '  dependency_graph(file) → file-level import/export map\n' +
  'Explore agents cost many tool calls; code-graph returns structured results in one.\n'
);
