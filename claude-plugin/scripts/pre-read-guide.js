#!/usr/bin/env node
'use strict';
// PreToolUse hook: On FIRST Read call per session window, remind Claude
// that code-graph tools are more token-efficient for understanding code.
const fs = require('fs');
const path = require('path');
const os = require('os');

const flag = path.join(os.tmpdir(), '.code-graph-read-guided');
const WINDOW_MS = 2 * 60 * 60 * 1000; // 2 hours

try {
  const stat = fs.statSync(flag);
  if (Date.now() - stat.mtimeMs < WINDOW_MS) process.exit(0);
} catch { /* first time */ }

fs.writeFileSync(flag, '');
process.stdout.write(
  '[code-graph] If reading to understand code (not to edit), prefer code-graph tools:\n' +
  '  module_overview(path) → understand a module\'s exports and structure\n' +
  '  get_ast_node(file_path, symbol_name) → one function\'s code + callers/callees\n' +
  '  semantic_code_search(query) → find code by concept\n' +
  'Use Read only for files you intend to edit.\n'
);
