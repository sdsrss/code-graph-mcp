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
// Write reaches it too (Q1): only logged for the Stop check, never answered. For Edit, it only fires when:
//   1. The old_string contains a function/method definition AND the edit changes
//      that definition's header (a body-only edit cannot break a caller)
//   2. The edited file is inside the project
//   3. The symbol has 1+ production callers
//   4. Same symbol not queried in last 2 minutes
// Silently exits otherwise — zero noise for normal edits. CODE_GRAPH_QUIET_HOOKS=1
// silences it like the other injecting hooks.
const { execFileSync } = require('child_process');
const fs = require('fs');
const path = require('path');
const { findBinary } = require('./find-binary');
const { cgTmpDir, cwdHash } = require('./tmp-dir');
const { resolveProjectRoot, indexKey, indexBuildInProgress } = require('./project-root');
const { recordRecommendation } = require('./recommendation-log');
const { formatCoveringTests } = require('./covering-tests');
const { emitPreToolContext } = require('./hook-emit');
const { hidden } = require('./proc-opts');
const { remainingMs } = require('./hook-fail-open');
const { definitionHeaders } = require('./stop-impact');

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

// P1 #3 — the Stop hook (stop-impact.js) checks, at the end of the turn, which
// edited symbols changed signature and which of their callers were left alone.
// It needs to know what was edited in THIS session, so every Edit is logged
// here — first file-only (so "files touched this turn" is complete even for
// edits the signature extraction below gives up on), then again with the
// symbol once one is known. Append-only, best-effort: it cannot fail the Edit.
const sessionEdits = require('./session-edits');
const toolInput = input.tool_input || {};
const editedAbs = toolInput.file_path || '';
const editedRel = editedAbs ? path.relative(cwd, path.resolve(cwd, editedAbs)) : '';
const insideProject = editedRel && !editedRel.startsWith('..') && !path.isAbsolute(editedRel);
const editedKey = insideProject ? editedRel.split(path.sep).join('/') : '';

// Q1 — a Write (matcher `Edit|Write`) is logged for the Stop check and never
// answered: it carries the whole file, and the pick below would name
// whichever definition its first matching pattern finds, not the one that
// changed. A Write over
// an existing file records a baseline for each definition whose header its
// content changes.
if (input.tool_name === 'Write') {
  if (editedKey) {
    const newText = typeof toolInput.content === 'string' ? toolInput.content : null;
    sessionEdits.recordFileEdit(cwd, input.session_id, editedKey, { newText });
  }
  process.exit(0);
}

const logEdit = (symbol) => {
  if (editedKey) sessionEdits.recordEdit(cwd, input.session_id, { file: editedKey, symbol });
};
logEdit(null);

// An edit outside the project cannot be about this project's symbols; asking
// the index anyway injected a same-named project function's callers.
if (!insideProject) process.exit(0);

const oldStr = toolInput.old_string || '';
const newStr = toolInput.new_string || '';
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

// Which definition the hunk names: the first pattern, in array order, that
// matches — 0.163.0's rule. Three replacements (the earliest match, then an
// allow-list of what may precede a method name, then keyword-anchored arms
// first) each fixed shapes and broke others, and three review rounds found a
// regression in each (pre-tag review 2026-09-29), so this rule still names the
// pick; the Stop check's per-language reader decides below, where it reads the
// language, which definition the edit changed.
//
// Where the definition starts is its name's line: `// TODO: make this
// async\nfn f(` matches from the comment line, and the same-file pick below
// counts lines from here. That moves no name, only the line.
let symbol = null;
let symbolAt = -1;
let nameAt = -1;
for (const pat of fnPatterns) {
  const m = scanned.match(new RegExp(pat.source, `${pat.flags}d`));
  if (m) {
    symbol = m[1] || m[2];
    nameAt = (m.indices[1] || m.indices[2])[0];
    symbolAt = Math.max(m.index, scanned.lastIndexOf('\n', nameAt - 1) + 1);
    break;
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

// Before the cooldown: the cooldown throttles the impact PUSH, not the record
// of what was edited — a second signature edit inside two minutes is exactly
// the one the Stop check must still see.
logEdit(symbol);

// The definition's header, from where it starts to where its body opens. An
// edit that leaves it verbatim changes only the body, which no caller can see.
// (A body-only hunk with no definition in it exited above: the grep fallback
// that used to guess its enclosing function named a wrong one for 108 of 608
// TypeScript definitions, 2026-09-28 hook audit.)
function headerAt(text, at) {
  const rest = text.slice(at, at + 400);
  const ends = [rest.indexOf('{'), rest.indexOf('=>'), rest.search(/:\s*(?:\n|$)/)]
    .filter((i) => i >= 0);
  return ends.length ? rest.slice(0, Math.min(...ends)) : rest.split('\n', 1)[0];
}
const bodyOnly = newStr.includes(headerAt(scanned, symbolAt));

// Which definition the edit changed, where the Stop check's per-language reader
// (stop-impact.js LANGS) reads this file: a definition of the hunk whose
// signature key the new text no longer has. The pick above sent 35 of its 103
// impact queries for names that are no definition there (a Rust `match f(…)
// {`, a comment's "function already", a string) and missed 24 changed headers
// (a later definition, a `{` inside the parameters, an old header still inside
// the new one), measured on every Edit hunk in this machine's transcripts
// (tasks/specs/pre-edit-pick-by-reader.md).
//   - a changed definition of the picked name → it, placed where it starts;
//   - the pick's header changed as the first-match rule reads it, and the pick
//     is not proven no definition → the pick, as before. The reader may not
//     see it (a method whose parameters span lines, `#[inline] fn` on one
//     line); where it reads it unchanged, the old reading can have run into
//     the next definition, which 0.178.0 did too;
//   - else the first changed definition, logged for the Stop check too;
//   - else none.
// The pick is no definition when its line opens a comment that has not closed
// before it, or, in Rust and Python, when its name does not follow `fn` /
// `def`. A comment here is not stop-impact.js's COMMENT_LINE, which takes any
// line opening with `*` or `#`: `*entries(prefix) {` and StandardJS's
// `* entries (prefix) {` are JS generator methods, `#[inline] pub fn` and
// `#![allow(…)] fn` Rust definitions; a doc comment's `*` is followed by a
// space, a `/` or the line's end, and never directly by the pick. A JS line
// opening with `#` or `;` is taken for a comment too, though it opens none:
// the pick there is no definition the index knows by that name (it names a
// private method `#parse`, so asking for `parse` finds nothing or another
// `parse`; `;(function main (` names a function expression, which has no node).
//
// Only in the extensions of the reader's languages that corpus had hunks in
// (not C: 2 hunks). Elsewhere the reader misread shapes no hunk there had
// (pre-tag review of 0.179.0: Kotlin's `fun Config.validate(` reads as
// `Config`, PHP's `function &name(` has no keyword before the name, a C++
// statement `Type name(args);` reads as a definition), so the pick keeps its
// own verdict, as in 0.178.0; so does a hunk the reader has no reading of (a
// header past its cap).
const READER_MEASURED = new Set(['.rs', '.py', '.js', '.mjs', '.cjs', '.ts']);
const OPENS_A_COMMENT = /^\s*(?:\/\/|\/\*|\*(?=\s|\/|$)|#(?!!?\[)|--|;)/;
function onComment(line, nameCol) {
  const closed = /^\s*(?:\/\*.*?\*\/\s*)+/.exec(line.slice(0, nameCol));
  const from = closed ? closed[0].length : 0;
  if (/^\s*\*\s+$/.test(line.slice(from, nameCol))) return false; // `* entries (`
  return OPENS_A_COMMENT.test(line.slice(from));
}
function changedDefinition() {
  const ext = path.extname(editedAbs).toLowerCase();
  const before = READER_MEASURED.has(ext) ? definitionHeaders(oldStr, ext) : null;
  const after = before && definitionHeaders(newStr, ext);
  if (!before || !after) return bodyOnly ? null : { name: symbol, at: symbolAt };
  const count = (defs) => {
    const m = new Map();
    for (const d of defs) m.set(`${d.name}\0${d.key}`, (m.get(`${d.name}\0${d.key}`) || 0) + 1);
    return m;
  };
  const has = count(after.defs);
  const left = new Map(has);
  let changed = [];
  for (const d of before.defs) {
    const k = `${d.name}\0${d.key}`;
    if (left.get(k) > 0) left.set(k, left.get(k) - 1);
    else changed.push(d);
  }
  // The count matches headers, not definitions: of two definitions of one name
  // with one old header, one kept, it marks the later, whichever the edit
  // changed. Such a name's definitions pair in order when the hunk keeps their
  // number and pairing finds as many changed as the count; otherwise which one
  // changed is not in the text (one went or came, or the hunk moved them), and
  // the reader names none of that name (the rule below can still keep the
  // pick).
  const twinned = new Set(changed
    .filter((c) => (has.get(`${c.name}\0${c.key}`) || 0) > 0)
    .map((c) => c.name));
  if (twinned.size) {
    const counted = new Map();
    for (const d of changed) if (twinned.has(d.name)) counted.set(d.name, (counted.get(d.name) || 0) + 1);
    const byName = (defs) => {
      const m = new Map();
      for (const d of defs) if (twinned.has(d.name)) (m.get(d.name) || m.set(d.name, []).get(d.name)).push(d);
      return m;
    };
    const olds = byName(before.defs);
    const news = byName(after.defs);
    changed = changed.filter((d) => !twinned.has(d.name));
    for (const [name, o] of olds) {
      const n = news.get(name) || [];
      const paired = o.length === n.length ? o.filter((d, i) => d.key !== n[i].key) : [];
      if (paired.length === counted.get(name)) changed.push(...paired);
    }
  }
  changed.sort((x, y) => x.at - y.at);
  const same = changed.find((d) => d.name === symbol);
  if (same) return { name: symbol, at: same.at };
  const lineStart = scanned.lastIndexOf('\n', nameAt - 1) + 1;
  const lineEnd = scanned.indexOf('\n', nameAt);
  const pickLine = scanned.slice(lineStart, lineEnd === -1 ? scanned.length : lineEnd);
  const notADefinition = onComment(pickLine, nameAt - lineStart)
    || (before.keywordOnly && !before.keywordLeads(scanned.slice(lineStart, nameAt)));
  if (!bodyOnly && !notADefinition) return { name: symbol, at: symbolAt };
  return changed.length ? { name: changed[0].name, at: changed[0].at } : null;
}
const edited = changedDefinition();
if (!edited) process.exit(0);
if (edited.name !== symbol) {
  symbol = edited.name;
  logEdit(symbol);
}
symbolAt = edited.at;

if (process.env.CODE_GRAPH_QUIET_HOOKS === '1') process.exit(0);

// --- Per-symbol cooldown: 2 minutes ---
// Project-scoped (see cwdHash in tmp-dir.js). A symbol name is the LEAST
// project-unique key there is — `main`, `run`, `new`, `parse` collide across
// every repo on the machine, so editing `parse` in one project suppressed the
// impact push for a completely different `parse` in another for two minutes.
const cooldownFile = path.join(cgTmpDir(), `.cg-impact-${cwdHash(cwd)}-${symbol}`);
try {
  if (Date.now() - fs.statSync(cooldownFile).mtimeMs < 120000) process.exit(0);
} catch { /* first time for this symbol */ }

// A half-built index gives a wrong caller count and risk, not a short one (D5).
// The edit itself is already logged above for the Stop hook.
if (indexBuildInProgress(cwd)) process.exit(0);

// --- Run impact analysis (JSON mode for programmatic parsing) ---
// Disambiguate via --file: file_path from tool_input is absolute, but the
// indexer stores files as repo-relative paths — converting here is what makes
// short generic symbol names (open, new, create, parse, from, init) resolve
// to a unique node instead of triggering the CLI's "Ambiguous symbol" error
// path, which previously caused silent exits for the most common edit cases.
// D#560 — in a linked worktree the index knows the file by its path from the
// worktree, not from the main checkout `cwd` resolved to.
const editedFile = (input.tool_input && input.tool_input.file_path) || '';
const relFile = editedFile ? indexKey(cwd, path.relative(cwd, editedFile)) : '';
// Whatever is left of the registered 4 s, capped at this call's own 2500 ms.
// `null` = the candidate loop above already spent the budget; running anyway is
// what got the hook killed by Claude Code mid-Edit (audit 2026-09-05 JS-03).
// A refusal exits 1 with its JSON on stdout, which is read too.
function runImpact(args) {
  const budget = remainingMs(2500);
  if (budget === null) return null;
  let raw;
  try {
    // v0.49 — use the resolved binary (bare 'code-graph-mcp' was PATH-dependent,
    // diverging from the findBinary() result the rest of the hook trusts).
    raw = execFileSync(binary, args, hidden({
      cwd,
      timeout: budget,
      encoding: 'utf8',
      stdio: ['pipe', 'pipe', 'pipe'],
      env: internalEnv,
    }));
  } catch (e) {
    raw = e && typeof e.stdout === 'string' ? e.stdout : '';
  }
  try { return JSON.parse(raw); } catch { return null; }
}

// Q4 — the definition this edit changes, among same-file same-name ones: the
// last candidate starting at or before the edited header's line. null when
// the old_string is not in the file (the Edit will fail) or no candidate fits.
// The CLI lists at most five, in source order, and says how many exist in
// `total`: past the last listed one an unlisted definition may start, so an
// edit there is not guessed at (pre-tag review 2026-09-29). The line is
// counted in the copy the index describes, whose start lines the candidates
// carry: in a worktree, the main checkout's. Counted in the worktree's copy, a
// header whose lines had moved named another same-named definition's callers
// (review of D#560). A copy without the edited text gets no guess, and in a
// worktree nor does one that holds it twice: the main checkout's copy is not
// the edited one, so which occurrence matches the edited definition cannot be
// told (review of 0.178.0). In the main checkout that is a `replace_all` Edit,
// and the first occurrence is one of the definitions it changes.
function editedNodeId(suggestions, fileKey, total) {
  const indexedKey = indexKey(cwd, fileKey);
  let text;
  try { text = fs.readFileSync(path.join(cwd, indexedKey), 'utf8'); } catch { return null; }
  const at = text.indexOf(oldStr);
  if (at === -1) return null;
  if (indexedKey !== fileKey && text.indexOf(oldStr, at + 1) !== -1) return null;
  const line = text.slice(0, at + symbolAt).split('\n').length;
  let best = null;
  for (const c of suggestions) {
    if (!c || c.file_path !== indexedKey || !Number.isInteger(c.node_id) || !Number.isInteger(c.start_line)) continue;
    if (c.start_line <= line && (best === null || c.start_line > best.start_line)) best = c;
  }
  const lastListed = Math.max(...suggestions.map((c) => (c && Number.isInteger(c.start_line) ? c.start_line : -Infinity)));
  if (Number.isInteger(total) && total > suggestions.length && line > lastListed) return null;
  return best ? best.node_id : null;
}

const impactArgs = ['impact', symbol, '--json'];
if (relFile && !relFile.startsWith('..')) impactArgs.push('--file', relFile);
let jsonResult = runImpact(impactArgs);
// A file that defines the name more than once (two classes' `get`) makes
// `--file` refuse and list each definition's node_id (174f9b6). Ask again for
// the one being edited.
if (jsonResult && jsonResult.error && Array.isArray(jsonResult.suggestions)) {
  const nodeId = editedNodeId(jsonResult.suggestions, editedKey, jsonResult.total);
  jsonResult = nodeId === null ? null : runImpact(['impact', '--node-id', String(nodeId), '--json']);
}

// Symbol not found, another refusal, timeout, or unparsable output — silent.
if (!jsonResult || jsonResult.error) process.exit(0);

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
// The counts above are what `impact` could resolve, and it says when that is a
// floor; the CLI prints both caveats, so the model must get them too. Without
// them, flask's `url_for` read as 7 callers with 32 more excluded.
const ambiguous = jsonResult.ambiguous_callers_excluded || 0;
if (ambiguous > 0) {
  summary += `  ⚠ ${ambiguous} more call edge(s) matched only by name were not counted; the blast radius may be larger\n`;
}
if (jsonResult.callers_truncated) {
  summary += '  ⚠ the caller traversal stopped early, so these counts are a floor, not a total\n';
}

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
