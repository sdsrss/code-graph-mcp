#!/usr/bin/env node
'use strict';
// PreToolUse hook: On FIRST Edit/Write call per session window, remind Claude
// to check impact_analysis before modifying functions. Fast and non-blocking.
const fs = require('fs');
const path = require('path');
const os = require('os');

const flag = path.join(os.tmpdir(), '.code-graph-edit-guided');
const WINDOW_MS = 2 * 60 * 60 * 1000; // 2 hours

try {
  const stat = fs.statSync(flag);
  if (Date.now() - stat.mtimeMs < WINDOW_MS) process.exit(0);
} catch { /* first time */ }

fs.writeFileSync(flag, '');
process.stdout.write(
  '[code-graph] Before modifying functions, consider running impact_analysis(symbol_name) ' +
  'to check blast radius (callers, affected routes, risk level).\n'
);
