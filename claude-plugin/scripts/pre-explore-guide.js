#!/usr/bin/env node
'use strict';
// PreToolUse hook: On FIRST Agent call per session window, suggest
// code-graph CLI commands for structural code understanding before spawning agents.
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
  '[code-graph] For code structure understanding, try CLI first (one Bash call vs agent):\n' +
  '  code-graph-mcp map                  \u2190 full architecture overview\n' +
  '  code-graph-mcp overview src/module  \u2190 module structure and exports\n' +
  '  code-graph-mcp callgraph symbol     \u2190 trace call chains\n' +
  'Explore agents remain best for: non-code files, runtime behavior, open-ended investigation.\n'
);
