#!/usr/bin/env node
'use strict';
// PreToolUse hook: On FIRST Glob call per session window, remind Claude
// that code-graph tools can find files and modules more efficiently.
const fs = require('fs');
const path = require('path');
const os = require('os');

const flag = path.join(os.tmpdir(), '.code-graph-glob-guided');
const WINDOW_MS = 2 * 60 * 60 * 1000; // 2 hours

try {
  const stat = fs.statSync(flag);
  if (Date.now() - stat.mtimeMs < WINDOW_MS) process.exit(0);
} catch { /* first time */ }

fs.writeFileSync(flag, '');
process.stdout.write(
  '[code-graph] If searching for files to understand code, prefer code-graph tools:\n' +
  '  project_map → all modules with their files and key symbols\n' +
  '  module_overview(path) → file list, exports, and structure of a module\n' +
  '  semantic_code_search(query) → find code by concept across all files\n' +
  'Use Glob only for exact file name/extension patterns.\n'
);
