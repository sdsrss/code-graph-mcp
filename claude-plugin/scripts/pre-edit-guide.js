#!/usr/bin/env node
'use strict';
// FIRST statement, before this file's other requires (pre-tag review
// 2026-09-02): the handler installed after them could not catch a throw
// from `require('./lifecycle')` itself, which is exactly the broken-install
// case JS-12 exists for. Guarded on `require.main` so importing this module
// in a test does NOT install a process-wide handler that exits 0 — that
// would swallow the test's own failures.
if (require.main === module) require('./hook-fail-open').installHookFailOpen('PreToolUse:Edit');

// PreToolUse(Edit) hook: auto-inject impact analysis when editing function definitions.
// Only fires when:
//   1. The old_string contains a function/method definition (signature being modified)
//   2. The symbol has 2+ production callers (high impact)
//   3. Same symbol not queried in last 2 minutes
// Silently exits otherwise — zero noise for normal edits.
const { execFileSync } = require('child_process');
const fs = require('fs');
const path = require('path');
const { findBinary } = require('./find-binary');
const { cgTmpDir, cwdHash } = require('./tmp-dir');
const { resolveProjectRoot } = require('./project-root');
const { recordRecommendation } = require('./recommendation-log');
const { formatCoveringTests } = require('./covering-tests');
const { emitPreToolContext } = require('./hook-emit');
const { hidden } = require('./proc-opts');

// v0.49 — walk up from the shell cwd (subdir-cwd fix). The per-cwd index.db
// gate kept this hook dark for entire sessions after `cd backend/` — daagu
// 2026-06-12: 115 edits, zero impact injections.
const cwd = resolveProjectRoot(process.cwd());
if (cwd === null) process.exit(0);

// Resolve binary the same way the other hooks do — bare PATH lookup misses
// npm-global installs on systems where the global bin dir isn't on PATH for
// non-login shells (a real failure mode reported in mem #8187).
const binary = findBinary();
if (!binary) process.exit(0);

// Hook-internal CLI runs are deliveries, not model-initiated conversions —
// the marker keeps them out of the recommendations.jsonl `use` funnel leg.
const internalEnv = { ...process.env, CODE_GRAPH_INTERNAL: '1' };

// --- Parse tool input ---
let input;
try {
  // fd 0, not '/dev/stdin': the path form fails ENXIO on socketpair stdin.
  input = JSON.parse(fs.readFileSync(0, 'utf8'));
} catch { process.exit(0); }

const oldStr = (input.tool_input && input.tool_input.old_string) || '';
if (!oldStr || oldStr.length < 10) process.exit(0);

// --- Extract function/method signature from the edited text ---
// Match function definitions across languages: Rust, JS/TS, Python, Go, Java/C#/Kotlin, Ruby, PHP
//
// Every unbounded run that is FOLLOWED BY A REQUIRED LITERAL carries an explicit
// `{1,128}` cap — longer than any real identifier, short enough that the engine
// gives up after 128 steps per start position. Without it those three patterns
// are quadratic: on a long
// unbroken \w run with no `name(...)` construct in it, the greedy run swallows to
// the end at EVERY start position and then backtracks a character at a time.
// Measured at HEAD on this box, pattern 4 alone: 10 KB 28 ms, 100 KB 2.8 s,
// 200 KB 11.0 s, 400 KB 43.4 s — doubling the input quadrupled the time.
//
// This needs no malice to hit. `old_string` is whatever the model is editing, so
// one benign blob without brackets — a base64 asset, a hex dump, a minified
// bundle, a long snake_case table — stalls a BLOCKING PreToolUse hook for
// seconds. Real code is unaffected either way (225 KB of this repo's own
// source: 0.3 ms), because a bracket ends the run almost immediately.
//
// The runs that are NOT capped are the ones nothing is required after
// (`fn\s+(\w+)`, `def\s+(\w+)`, …): those anchor on a keyword first and their
// trailing capture cannot backtrack, so a cap there would only truncate a long
// symbol name.
const fnPatterns = [
  /(?:pub\s+)?(?:async\s+)?fn\s+(\w+)/,                        // Rust
  /(?:export\s+)?(?:async\s+)?function\s+(\w+)/,                // JS/TS
  /(?:const|let|var)\s+(\w{1,128})\s*=\s*(?:async\s+)?(?:\([^)]*\)|_)\s*=>/, // JS arrow
  /(?:async\s+)?(\w{1,128})\s*\([^)]*\)\s*\{/,                 // JS method / Go func
  /def\s+(\w+)/,                                                // Python/Ruby
  /func\s+(\w+)/,                                               // Go/Swift
  /(?:public|private|protected|static|override|virtual|abstract|internal)\s+\S{1,128}\s+(\w{1,128})\s*\(/, // Java/C#/Kotlin
  /(?:public\s+)?function\s+(\w+)/,                             // PHP
];

// Second bound, on the INPUT rather than the patterns: a signature sits at the
// head of the edited hunk, so matching past the first 8 KB buys nothing and
// costs linearly. Belt to the caps' braces — it also bounds whatever pattern a
// future author adds to the array without reading the note above.
const scanned = oldStr.length > 8192 ? oldStr.slice(0, 8192) : oldStr;

let symbol = null;
for (const pat of fnPatterns) {
  const m = scanned.match(pat);
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
          const raw = execFileSync(binary, ['grep', candidate, filePath, '--json'], hidden({
            cwd, timeout: 2000, encoding: 'utf8', stdio: ['pipe', 'pipe', 'pipe'],
            env: internalEnv,
          }));
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
// Project-scoped (see cwdHash in tmp-dir.js). A symbol name is the LEAST
// project-unique key there is — `main`, `run`, `new`, `parse` collide across
// every repo on the machine, so editing `parse` in one project suppressed the
// impact push for a completely different `parse` in another for two minutes.
const cooldownFile = path.join(cgTmpDir(), `.cg-impact-${cwdHash(cwd)}-${symbol}`);
try {
  if (Date.now() - fs.statSync(cooldownFile).mtimeMs < 120000) process.exit(0);
} catch { /* first time for this symbol */ }

// --- Run impact analysis (JSON mode for programmatic parsing) ---
// Disambiguate via --file: file_path from tool_input is absolute, but the
// indexer stores files as repo-relative paths — converting here is what makes
// short generic symbol names (open, new, create, parse, from, init) resolve
// to a unique node instead of triggering the CLI's "Ambiguous symbol" error
// path, which previously caused silent exits for the most common edit cases.
const editedFile = (input.tool_input && input.tool_input.file_path) || '';
const relFile = editedFile ? path.relative(cwd, editedFile) : '';
let jsonResult;
try {
  const args = ['impact', symbol, '--json'];
  if (relFile && !relFile.startsWith('..')) args.push('--file', relFile);
  // v0.49 — use the resolved binary (bare 'code-graph-mcp' was PATH-dependent,
  // diverging from the findBinary() result the rest of the hook trusts).
  const raw = execFileSync(binary, args, hidden({
    cwd,
    timeout: 2500,
    encoding: 'utf8',
    stdio: ['pipe', 'pipe', 'pipe'],
    env: internalEnv,
  }));
  jsonResult = JSON.parse(raw);
} catch {
  // Symbol not found, timeout, or parse error — exit silently
  process.exit(0);
}

// CLI returns {"error": "..."} on ambiguous / not-found instead of throwing.
// Treat as silent skip — direct_callers will be undefined.
if (jsonResult && jsonResult.error) process.exit(0);

// --- Inject when the symbol has any caller (1+) ---
// Earlier gate was 2+ direct callers; reality is that editing a function with
// even one production caller benefits from a one-line impact summary, and the
// per-symbol 2-minute cooldown caps frequency. The 2+ floor was a remnant of
// the v0.21 "agent picks tools without push" assumption — same bias mem #8234
// records as bounded leverage at the bench level.
const directCallers = jsonResult.direct_callers || 0;
const totalCallers = jsonResult.total_callers || 0;
const affectedFiles = jsonResult.affected_files || 0;
const risk = jsonResult.risk || 'low';

if (directCallers < 1) process.exit(0);

// Mark cooldown
try { fs.writeFileSync(cooldownFile, ''); } catch { /* ok */ }

// Funnel visibility (v0.49): an injected impact summary is a delivered answer.
// v0.63 — ack:true marks that this injection carries a salience-forcing directive
// (the per-caller verdict line below), so a later A/B can segment ack vs non-ack.
// test_targets: how many covering tests this injection offered — the forward
// signal for whether covering-test targeting reduces test-name guessing (read on
// consumer projects; this dogfood repo's metrics are dark).
recordRecommendation(cwd, {
  hook: 'edit', action: 'hint', answered: true, ack: true,
  test_targets: (jsonResult.test_callers || []).length,
});

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

// Covering tests — turn the bare "(N tests)" count above into an actionable,
// targeted run command so the fix-test-iterate loop runs exactly the tests that
// exercise the edited symbol (not the whole suite or a guessed name). Empty/absent
// test_callers → appends nothing.
summary += formatCoveringTests(jsonResult.test_callers, editedFile);

// Salience forcing (v0.63) — an injected impact summary that the model merely
// reads is wasted context. mem's PreToolUse edit hook lifts cite-recall to ~94%
// by making the model ACT on the injection ("apply each lesson or rule it out")
// rather than passively receive it. Mirror that: force an explicit per-caller
// verdict so the blast radius is reconciled against the edit, not skimmed.
// Wording references "each caller of X()" not "above" (finding #5): the name list
// is only printed when callers[] is populated, but the directCallers>=1 gate can
// fire with the count alone — the verdict must stay coherent either way.
summary += `  → Before this edit: confirm each caller of ${symbol}() still holds with your change, or note why it is unaffected.\n`;

// Deliver via the PERMISSION-NEUTRAL PreToolUse additionalContext envelope
// (shared hook-emit.js). Bare stdout on a PreToolUse exit-0 lands in the debug
// log only and never reaches the model (CC docs v2026-06); additionalContext is
// what surfaces the impact summary, and it is delivered without any
// permissionDecision — the tool's normal permission flow is untouched.
//
// It used to send `permissionDecision: 'allow'` alongside it. That is documented
// as "skip the interactive permission prompt", so on a machine that prompts for
// Edit this hook silently answered that prompt for the user, for every symbol
// with >=1 caller outside the 2-minute cooldown (audit 2026-08-16 P0-2). Context
// delivery is never worth a write consent: if a future CC requires a decision to
// carry additionalContext, this summary goes quiet rather than elevating again.
// Impact must stay PRE-edit (the reconciliation happens before the change), so a
// PostToolUse inject is not an alternative here.
process.stdout.write(emitPreToolContext(summary) + '\n');
