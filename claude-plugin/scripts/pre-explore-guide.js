#!/usr/bin/env node
'use strict';
// PreToolUse hook: On FIRST Agent call per session window, suggest
// code-graph tools for structural code understanding before spawning agents.
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
  '[code-graph] For code structure understanding, try code-graph first (one call vs many):\n' +
  '  project_map(compact=true) \u2192 full architecture overview\n' +
  '  module_overview(path, compact=true) \u2192 module structure, exports, hot paths\n' +
  '  get_call_graph(symbol, compact=true) \u2192 trace call chains\n' +
  'Explore agents remain best for: non-code files, runtime behavior, open-ended investigation.\n'
);
