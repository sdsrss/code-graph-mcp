#!/usr/bin/env node
'use strict';
// PreToolUse hook: On FIRST Glob call per session window, suggest
// code-graph tools when exploring project structure (not finding specific files).
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
  '[code-graph] If exploring project structure (not finding specific files):\n' +
  '  project_map → all modules, their files, key symbols, and dependencies\n' +
  '  module_overview(path) → files and exports within a module\n' +
  'Glob remains best for: finding specific files, configs, non-code assets.\n'
);
