#!/usr/bin/env node
'use strict';
// PreToolUse hook: On FIRST Grep call per session window, suggest
// code-graph tools — but only when the pattern looks like code understanding
// (function names, module patterns), NOT exact string/constant searches.
const fs = require('fs');
const path = require('path');
const os = require('os');

const flag = path.join(os.tmpdir(), '.code-graph-search-guided');
const WINDOW_MS = 2 * 60 * 60 * 1000; // 2 hours

try {
  const stat = fs.statSync(flag);
  if (Date.now() - stat.mtimeMs < WINDOW_MS) process.exit(0);
} catch { /* first time */ }

// Parse tool input to detect intent — skip for literal/constant searches
try {
  const input = JSON.parse(fs.readFileSync('/dev/stdin', 'utf8'));
  const pattern = (input && input.tool_input && input.tool_input.pattern) || '';
  // Skip suggestion for: quoted strings, TODO/FIXME, constants, exact literals, error messages
  if (/^["']|^(TODO|FIXME|HACK|WARN|ERROR|const )|^\w+[=:]/i.test(pattern)) {
    process.exit(0);
  }
  // Skip for very short patterns (likely exact match)
  if (pattern.length <= 3) {
    process.exit(0);
  }
} catch { /* stdin not available or parse error — show guide anyway */ }

fs.writeFileSync(flag, '');
process.stdout.write(
  '[code-graph] For understanding code relationships, these tools complement Grep:\n' +
  '  get_call_graph(symbol, compact=true) \u2192 who calls X / what X calls\n' +
  '  semantic_code_search(query, compact=true) \u2192 find code by concept\n' +
  '  module_overview(path, compact=true) \u2192 module exports and structure\n' +
  'Grep remains best for: exact strings, regex, constants, non-code files.\n'
);
