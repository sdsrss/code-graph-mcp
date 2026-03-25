#!/usr/bin/env node
'use strict';
// PreToolUse(Edit) hook: auto-inject impact analysis when editing function definitions.
// Only fires when:
//   1. The old_string contains a function/method definition (signature being modified)
//   2. The symbol has 2+ production callers (high impact)
//   3. Same symbol not queried in last 2 minutes
// Silently exits otherwise — zero noise for normal edits.
const { execFileSync } = require('child_process');
const fs = require('fs');
const path = require('path');
const os = require('os');

const cwd = process.cwd();
const dbPath = path.join(cwd, '.code-graph', 'index.db');
if (!fs.existsSync(dbPath)) process.exit(0);

// --- Parse tool input ---
let input;
try {
  input = JSON.parse(fs.readFileSync('/dev/stdin', 'utf8'));
} catch { process.exit(0); }

const oldStr = (input.tool_input && input.tool_input.old_string) || '';
if (!oldStr || oldStr.length < 10) process.exit(0);

// --- Extract function/method signature from the edited text ---
// Match function definitions across languages: Rust, JS/TS, Python, Go, Java/C#/Kotlin, Ruby, PHP
const fnPatterns = [
  /(?:pub\s+)?(?:async\s+)?fn\s+(\w+)/,                        // Rust
  /(?:export\s+)?(?:async\s+)?function\s+(\w+)/,                // JS/TS
  /(?:const|let|var)\s+(\w+)\s*=\s*(?:async\s+)?(?:\([^)]*\)|_)\s*=>/, // JS arrow
  /(?:async\s+)?(\w+)\s*\([^)]*\)\s*\{/,                       // JS method / Go func
  /def\s+(\w+)/,                                                // Python/Ruby
  /func\s+(\w+)/,                                               // Go/Swift
  /(?:public|private|protected|static|override|virtual|abstract|internal)\s+\S+\s+(\w+)\s*\(/, // Java/C#/Kotlin
  /(?:public\s+)?function\s+(\w+)/,                             // PHP
];

let symbol = null;
for (const pat of fnPatterns) {
  const m = oldStr.match(pat);
  if (m) {
    // Find the first captured group
    symbol = m[1] || m[2];
    break;
  }
}

if (!symbol || symbol.length < 3) process.exit(0);

// Skip common patterns that aren't real function names
if (/^(if|for|while|switch|catch|else|return|new|get|set|try)$/i.test(symbol)) {
  process.exit(0);
}

// --- Per-symbol cooldown: 2 minutes ---
const cooldownFile = path.join(os.tmpdir(), `.cg-impact-${symbol}`);
try {
  if (Date.now() - fs.statSync(cooldownFile).mtimeMs < 120000) process.exit(0);
} catch { /* first time for this symbol */ }

// --- Run impact analysis (JSON mode for programmatic parsing) ---
let jsonResult;
try {
  const raw = execFileSync('code-graph-mcp', ['impact', symbol, '--json'], {
    cwd,
    timeout: 2500,
    encoding: 'utf8',
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  jsonResult = JSON.parse(raw);
} catch {
  // Symbol not found, timeout, or parse error — exit silently
  process.exit(0);
}

// --- Only inject if high-impact (2+ production callers) ---
const directCallers = jsonResult.direct_callers || 0;
const totalCallers = jsonResult.total_callers || 0;
const affectedFiles = jsonResult.affected_files || 0;
const risk = jsonResult.risk || 'low';

if (directCallers < 2) process.exit(0);

// Mark cooldown
try { fs.writeFileSync(cooldownFile, ''); } catch { /* ok */ }

// --- Inject compact impact summary ---
const routeCount = jsonResult.affected_routes || 0;
const testCount = jsonResult.tests_affected || 0;

let summary = `[code-graph:impact] ${symbol}() — Risk: ${risk}\n`;
summary += `  ${directCallers} direct callers, ${totalCallers} total across ${affectedFiles} files`;
if (routeCount > 0) summary += `, ${routeCount} routes affected`;
if (testCount > 0) summary += ` (${testCount} tests)`;
summary += '\n';

// List direct callers compactly
const callers = (jsonResult.callers || []).filter(c => c.depth === 1);
if (callers.length > 0) {
  summary += '  Callers: ' + callers.map(c => `${c.name} (${c.file})`).join(', ') + '\n';
}

process.stdout.write(summary);
