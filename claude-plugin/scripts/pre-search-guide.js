#!/usr/bin/env node
'use strict';
// PreToolUse hook: On FIRST Grep call per session window, suggest
// code-graph tools as complementary options for code understanding.
const fs = require('fs');
const path = require('path');
const os = require('os');

const flag = path.join(os.tmpdir(), '.code-graph-search-guided');
const WINDOW_MS = 2 * 60 * 60 * 1000; // 2 hours

try {
  const stat = fs.statSync(flag);
  if (Date.now() - stat.mtimeMs < WINDOW_MS) process.exit(0);
} catch { /* first time */ }

fs.writeFileSync(flag, '');
process.stdout.write(
  '[code-graph] For understanding code relationships, these tools complement Grep:\n' +
  '  get_call_graph(symbol) → who calls X / what X calls (vs Grep + Read ×N)\n' +
  '  module_overview(path) → module exports, structure, hot paths\n' +
  '  semantic_code_search(query) → find code by concept across indexed files\n' +
  'Grep remains best for: exact strings, regex, constants, non-code files.\n'
);
