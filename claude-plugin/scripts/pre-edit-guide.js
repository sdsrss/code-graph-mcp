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

// Fallback: if old_string is inside a function body (not a definition),
// extract a unique identifier from the code and grep for it to find the containing function
if (!symbol || symbol.length < 3) {
  const filePath = (input.tool_input && input.tool_input.file_path) || '';
  if (filePath && oldStr.length >= 10) {
    try {
      // Extract identifiers from old_string, try the most specific one first
      const identifiers = (oldStr.match(/\b([a-z]\w*(?:_\w+)+|[a-z]\w*(?:[A-Z]\w*)+|[A-Z]\w+\.\w+|[A-Z]\w+::\w+)\b/g) || [])
        .filter(id => id.length >= 6);
      const skipWords = new Set(['return', 'function', 'default', 'require', 'module', 'exports', 'import', 'console']);
      // Sort by length descending (longer = more specific = fewer matches)
      const candidates = [...new Set(identifiers)]
        .filter(id => !skipWords.has(id.toLowerCase()))
        .sort((a, b) => b.length - a.length);
      for (const candidate of candidates.slice(0, 5)) {
        try {
          const raw = execFileSync('code-graph-mcp', ['grep', candidate, filePath, '--json'], {
            cwd, timeout: 2000, encoding: 'utf8', stdio: ['pipe', 'pipe', 'pipe'],
          });
          const grepResult = JSON.parse(raw);
          // Pick this candidate if it has few matches (precise location)
          const withContainer = (grepResult || []).filter(m => m.container && m.container.name);
          if (withContainer.length > 0 && withContainer.length <= 5) {
            // If multiple containers, vote for the most common one
            const votes = {};
            for (const m of withContainer) {
              const cn = m.container.name;
              votes[cn] = (votes[cn] || 0) + 1;
            }
            const best = Object.entries(votes).sort((a, b) => b[1] - a[1])[0][0];
            symbol = best.includes('.') ? best.split('.').pop() : best.includes('::') ? best.split('::').pop() : best;
            break;
          }
        } catch { /* try next candidate */ }
      }
    } catch { /* grep failed or no match — fall through */ }
  }
}

if (!symbol || symbol.length < 3) process.exit(0);

// Skip common patterns that aren't real function names
if (isCommonKeyword(symbol)) {
  process.exit(0);
}

function isCommonKeyword(s) {
  return /^(if|for|while|switch|catch|else|return|new|get|set|try)$/i.test(s);
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
