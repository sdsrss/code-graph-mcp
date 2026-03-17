#!/usr/bin/env node
'use strict';
// PreToolUse hook: On FIRST Glob call per session window, suggest
// code-graph CLI commands — but only when exploring project structure,
// NOT finding specific files by name.
const fs = require('fs');
const path = require('path');
const os = require('os');

const flag = path.join(os.tmpdir(), '.code-graph-glob-guided');
const WINDOW_MS = 2 * 60 * 60 * 1000; // 2 hours

try {
  const stat = fs.statSync(flag);
  if (Date.now() - stat.mtimeMs < WINDOW_MS) process.exit(0);
} catch { /* first time */ }

// Parse tool input to detect intent — skip for specific file lookups
try {
  const input = JSON.parse(fs.readFileSync('/dev/stdin', 'utf8'));
  const pattern = (input && input.tool_input && input.tool_input.pattern) || '';
  // Skip suggestion for: specific file patterns (has extension), config files, specific names
  if (/\.(json|yaml|yml|toml|md|txt|env|lock|config|rc)$/i.test(pattern)) {
    process.exit(0);
  }
  // Skip for patterns with specific filenames (not just wildcards like **/*.ts)
  if (!pattern.includes('*') && /[\w-]+\.\w{1,5}$/.test(pattern)) {
    process.exit(0);
  }
} catch { /* stdin not available or parse error — show guide anyway */ }

fs.writeFileSync(flag, '');
process.stdout.write(
  '[code-graph] If exploring project structure (not finding specific files):\n' +
  '  code-graph-mcp map              \u2190 project architecture (modules, deps, entry points)\n' +
  '  code-graph-mcp overview src/mcp \u2190 module symbols grouped by file and type\n' +
  'Glob remains best for: finding specific files, configs, non-code assets.\n'
);
