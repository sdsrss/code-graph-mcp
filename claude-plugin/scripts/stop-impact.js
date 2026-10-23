#!/usr/bin/env node
'use strict';
// FIRST statement, before the other requires — same reason as pre-edit-guide.js.
if (require.main === module) require('./hook-fail-open').installHookFailOpen('Stop');

// Stop hook (P1 #3b): the end-of-turn half of the edit-impact check.
//
// pre-edit-guide.js pushes "this function has N callers" BEFORE an edit, when
// nobody yet knows whether the edit will change the signature. This runs when
// the turn ends and asks the question that can be answered then: of the
// symbols edited this turn, which ones' signatures did THIS TURN change, and
// which of their callers sit in files this turn did not touch? Those are
// listed as `file:line`, once per symbol per session, through the Stop
// event's `additionalContext` — documented as non-error feedback that lets
// the turn continue, as opposed to `decision: "block"`
// (https://code.claude.com/docs/en/hooks, "Stop decision control").
//
// "Changed this turn" compares the working tree with the signatures
// pre-edit-guide recorded just before the turn's FIRST Edit of the symbol
// (session-edits.js `sigs`) — not with HEAD. Both sides of the comparison are
// then scoped to the same turn: a signature changed in turn 1, with its
// callers fixed in turn 1, is not re-raised by a body-only edit in turn 2
// (review H1), and uncommitted work from before the session is not reported
// as this turn's.
//
// Silent when: `stop_hook_active` (a continuation this or another Stop hook
// caused — never loop), no session edit log, no index, not a git work tree,
// no signature changed (optional parameters appended at the end of a list do
// not count: every existing call stays valid), the language has no exact
// header reading (LANGS),
// every caller's file was touched this turn, or the symbol was already
// reported this session. CODE_GRAPH_QUIET_HOOKS=1 silences it like the other
// injecting hooks.
//
// "Touched this turn" = an Edit to the file was logged since the previous Stop,
// OR the file's mtime is at or after the turn's start (covers Write, Bash
// `sed -i`, formatters). The turn starts at the previous Stop; the first turn
// of a session starts at its first logged Edit. Both only ever SUPPRESS a
// line, so an over-wide "touched" set costs a missed report, never a wrong one.
const { execFileSync } = require('child_process');
const fs = require('fs');
const path = require('path');
const { hidden } = require('./proc-opts');
const { shellQuoteArg, formatCgCommand } = require('./cg-answer');

// Per-Stop work caps. The hook blocks the end of the turn, so its worst case
// is bounded by count as well as by the registered budget. The symbol cap
// bounds the `refs` queries, so it applies AFTER the text-only "did the
// signature change" filter (review M1): body-only edits do not use it up.
const MAX_SYMBOLS_CHECKED = 8;
const MAX_CALLERS_LISTED = 8;
// A header that has not ended within this many characters is not read at all
// (no verdict), rather than cut: a cut header can run into the body, and a
// body edit would then read as a signature change.
const MAX_SIGNATURE_CHARS = 600;
// A match on a line longer than this gives no reading (null): minified and
// generated one-line files. Re-finding the line per match made such a file
// quadratic (pre-tag review H2: a 1.5 MB one-line bundle took 9 s, past
// pre-edit-guide's 4 s budget); with the cap a 2 MB file of 1,999-character
// lines of definitions takes about 0.2 s.
const MAX_DEFINITION_LINE_CHARS = 2000;

const COMMENT_LINE = /^\s*(?:\/\/|#|\*|\/\*|--|;)/;
// First words that make a C-family `Type name(` line a statement, not a
// definition.
const NOT_A_TYPE = new Set([
  'return', 'new', 'throw', 'else', 'await', 'yield', 'case', 'delete', 'typeof',
  'echo', 'print', 'puts', 'if', 'while', 'for', 'switch', 'catch', 'when', 'not',
  'and', 'or', 'in', 'is', 'of', 'do', 'assert', 'raise', 'import', 'from', 'use',
  'let', 'const', 'var', 'mut', 'match', 'go', 'defer', 'sizeof', 'co_return',
  'co_await', 'co_yield', 'default', 'goto',
]);

// How each language's definition headers are found and where they end. Only
// languages whose headers these rules read exactly are listed; every other
// extension gets no verdict, and the Stop hook says nothing about its symbols
// (review M2: silent beats wrong).
//
//   keyword  a definition is `<keyword> name` (`receiver`: Go's `func (r T) name`;
//            `typeParams`: Kotlin's `fun <T> name`)
//   jsForms  also `const name = (…) =>` bindings and one-line class methods
//   typed    C-family `Type name(` lines — no keyword exists
//   ends     characters that end the header at bracket depth 0; `\n` included
//            only where a newline outside brackets cannot continue a header
//   arrowEnds  `=>` at depth 0 ends it (JS arrow bodies, C# expression bodies);
//            elsewhere `=>` is part of a type (Scala) and never an end
//   angles   `<…>` nests like a bracket (TS generics may hold `{`)
//   afterParams  a lone `:` after the parameter list ends it (C++ member
//            initializers, C# `: base(…)` — body, not signature)
//   closeParen  the parameter list's `)` ends it (Ruby, Lua: no return types,
//            and a one-line body may follow on the same line)
const LANGS = new Map();
function defineLang(exts, profile) {
  for (const e of exts) LANGS.set(e, profile);
}
defineLang(['.rs'], { keyword: 'fn', ends: '{;' });
defineLang(['.py', '.pyi'], { keyword: 'def', ends: ':' });
defineLang(['.go'], { keyword: 'func', receiver: true, ends: '{;\n' });
defineLang(['.js', '.jsx', '.mjs', '.cjs', '.ts', '.tsx', '.mts', '.cts'],
  { keyword: 'function\\*?', jsForms: true, ends: '{;', arrowEnds: true, angles: true });
defineLang(['.php'], { keyword: 'function', ends: '{;' });
defineLang(['.c', '.h', '.cc', '.cpp', '.cxx', '.hpp', '.hh', '.hxx', '.java', '.cs'],
  { typed: true, ends: '{;', arrowEnds: true, afterParams: true });
defineLang(['.kt', '.kts'], { keyword: 'fun', typeParams: true, ends: '{=\n' });
defineLang(['.scala', '.sc'], { keyword: 'def', ends: '{=\n' });
defineLang(['.rb'], { keyword: 'def', ends: ';=\n', closeParen: true });
defineLang(['.lua'], { keyword: 'function', ends: '\n', closeParen: true });

function escapeRe(s) {
  return String(s).replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

function definitionPatterns(S, lang) {
  const out = [];
  if (lang.keyword) {
    const receiver = lang.receiver ? '(?:\\([^)\\n]*\\)\\s*)?' : '';
    const typeParams = lang.typeParams ? '(?:<[^>\\n]*>\\s*)?' : '';
    out.push({ re: new RegExp(`\\b(?:${lang.keyword})\\s+${receiver}${typeParams}${S}\\b`, 'g') });
  }
  if (lang.jsForms) {
    out.push({ re: new RegExp(`\\b(?:const|let|var)\\s+${S}\\s*=\\s*(?:async\\s+)?(?:function\\b|\\([^)\\n]*\\)\\s*=>|[A-Za-z_$][\\w$]*\\s*=>)`, 'g') });
    out.push({ re: new RegExp(`^[ \\t]*(?:(?:async|static|get|set|public|private|protected|readonly|override)\\s+)*\\*?${S}\\s*\\([^()\\n]*\\)\\s*(?::\\s*[^{;=\\n]+)?\\{[ \\t]*$`, 'gm') });
  }
  if (lang.typed) {
    out.push({ re: new RegExp(`^[ \\t]*((?:[A-Za-z_][\\w<>\\[\\],.*&:?]*[ \\t]+)+)${S}[ \\t]*\\(`, 'gm'), typed: true });
  }
  return out;
}

/**
 * Normalized signatures of every definition of `symbol` in `text`, sorted —
 * or null when there is no exact reading: the language is not in LANGS, a
 * header did not end within MAX_SIGNATURE_CHARS, or a match of a definition
 * pattern sits on a line longer than MAX_DEFINITION_LINE_CHARS.
 *
 * A definition is located by the line it starts on (LANGS) and runs from that
 * line's start to the first end character at bracket depth 0. The key drops
 * all whitespace and a trailing comma, so a formatter re-wrapping the
 * parameter list is not a change.
 * @returns {string[]|null}
 */
function extractSignatures(text, symbol, ext = '') {
  const lang = LANGS.get(String(ext).toLowerCase());
  if (!lang) return null;
  if (typeof text !== 'string' || !symbol) return [];
  const starts = definitionStarts(text, lang, escapeRe(symbol), symbol);
  if (starts === null) return null;
  return headersAt(text, starts.get(symbol) || [], lang);
}

// The name slot of definitionPatterns when every definition is wanted.
const ANY_NAME = '([A-Za-z_$][\\w$]*)';
// Words the name slot must not take: the control words that open a
// `word (…) {` line, which the JS one-line-method pattern reads as a method.
// Not NOT_A_TYPE: `from`, `use`, `match` are ordinary method names (`fn from`).
const NOT_A_NAME = new Set([
  'if', 'for', 'while', 'switch', 'catch', 'with', 'function', 'foreach', 'elseif',
  'synchronized', 'using', 'lock', 'fixed', 'until', 'unless', 'return', 'await',
  'yield', 'typeof', 'sizeof', 'new', 'super', 'this',
]);

/**
 * Every definition in `text` with its signatures (Q1): Map name → sorted keys,
 * in order of first appearance in the file — for an edit that names no
 * definition (Write, `sed -i`). One pass per pattern; each name reads exactly
 * as extractSignatures(text, name) reads it, and a name whose header has no
 * reading is left out. null: the language is not in LANGS, or a definition
 * sits on a line over MAX_DEFINITION_LINE_CHARS (for every name at once: the
 * scan stops there, as extractSignatures does).
 * @returns {Map<string,string[]>|null}
 */
function allSignatures(text, ext = '') {
  const lang = LANGS.get(String(ext).toLowerCase());
  if (!lang || typeof text !== 'string') return null;
  const starts = definitionStarts(text, lang, ANY_NAME, null);
  if (starts === null) return null;
  const byFirst = [...starts].sort((a, b) => Math.min(...a[1]) - Math.min(...b[1]));
  const out = new Map();
  for (const [name, lineStarts] of byFirst) {
    const sigs = headersAt(text, lineStarts, lang);
    if (sigs && sigs.length > 0) out.set(name, sigs);
  }
  return out;
}

/**
 * Where definitions start: Map name → Set of line-start offsets. With `symbol`
 * the patterns carry it escaped in `S`; without, `S` is ANY_NAME and the name
 * is the pattern's last group. null when a match sits on an over-long line.
 */
function definitionStarts(text, lang, S, symbol) {
  const out = new Map();
  for (const { re, typed } of definitionPatterns(S, lang)) {
    for (const m of text.matchAll(re)) {
      const at = text.lastIndexOf('\n', m.index) + 1;
      const lineEnd = text.indexOf('\n', at);
      const line = text.slice(at, lineEnd === -1 ? text.length : lineEnd);
      // Checked first: finding the line is the per-match cost, so a match on
      // a long line must end the scan, or a one-line bundle is quadratic.
      if (line.length > MAX_DEFINITION_LINE_CHARS) return null;
      if (COMMENT_LINE.test(line)) continue;
      if (typed) {
        const first = (line.trim().match(/^[A-Za-z_]\w*/) || [''])[0];
        if (NOT_A_TYPE.has(first)) continue;
        // `label: f(`, `case X: f(`, `public: f(` — a token ending in a lone
        // `:` is a label or access specifier, never part of a return type.
        if (m[1].split(/[ \t]+/).some((tok) => /(?:^|[^:]):$/.test(tok))) continue;
      }
      const name = symbol || m[m.length - 1];
      if (!symbol && NOT_A_NAME.has(name)) continue;
      if (!out.has(name)) out.set(name, new Set());
      out.get(name).add(at);
    }
  }
  return out;
}

function headersAt(text, lineStarts, lang) {
  const out = [];
  for (const at of lineStarts) {
    const header = readHeader(text, at, lang);
    if (header === null) return null;
    out.push(signatureKey(header));
  }
  return out.sort();
}

function readHeader(text, at, lang) {
  let depth = 0;
  let angle = 0;
  let sawParams = false;
  const limit = Math.min(text.length, at + MAX_SIGNATURE_CHARS);
  for (let i = at; i < limit; i++) {
    const c = text[i];
    if (c === '=' && text[i + 1] === '>') {
      if (lang.arrowEnds && depth === 0 && angle === 0) return text.slice(at, i);
      i++; // part of a type or a default value: step over both characters
      continue;
    }
    if (c === '(' || c === '[') { depth++; continue; }
    if (c === ')' || c === ']') {
      depth = Math.max(0, depth - 1);
      if (c === ')' && depth === 0) {
        sawParams = true;
        if (lang.closeParen) return text.slice(at, i + 1);
      }
      continue;
    }
    if (depth !== 0) continue;
    if (lang.angles && c === '<') { angle++; continue; }
    if (lang.angles && c === '>') { angle = Math.max(0, angle - 1); continue; }
    if (angle !== 0) continue;
    if (lang.ends.includes(c) && !(c === '\n' && i === at)) return text.slice(at, i);
    if (lang.afterParams && sawParams && c === ':' && text[i - 1] !== ':' && text[i + 1] !== ':') {
      return text.slice(at, i);
    }
  }
  // End of text ends a header; running out of the character budget does not.
  return limit === text.length ? text.slice(at, limit) : null;
}

function signatureKey(header) {
  return header.replace(/\s+/g, '').replace(/,\)/g, ')');
}

/**
 * True when a definition that existed before is gone: its header changed, or
 * it was removed while others of the same name remain. A definition ADDED
 * beside unchanged ones (a new overload, a new `impl From<B>`) changes no
 * existing caller's target, so it is not a change (review M2). Nor is a header
 * whose only difference is optional parameters appended to its parameter list
 * (Q2, see appendsOptionalParams) — every existing call stays valid. That
 * reading needs `symbol` to find the parameter list; without it every
 * difference counts.
 */
function signatureChanged(oldSigs, newSigs, symbol = null) {
  const remaining = new Map();
  for (const s of newSigs) remaining.set(s, (remaining.get(s) || 0) + 1);
  const unmatched = [];
  for (const s of oldSigs) {
    const n = remaining.get(s) || 0;
    if (n === 0) unmatched.push(s);
    else remaining.set(s, n - 1);
  }
  if (unmatched.length === 0) return false;
  if (!symbol) return true;
  // Each old header may pair with one new header that only extends it.
  const pool = [];
  for (const [s, n] of remaining) for (let k = 0; k < n; k++) pool.push(s);
  for (const s of unmatched) {
    const at = pool.findIndex((t) => appendsOptionalParams(s, t, symbol));
    if (at === -1) return true;
    pool.splice(at, 1);
  }
  return false;
}

/**
 * True when signature key `newKey` is `oldKey` with parameters inserted just
 * before the `)` that closes `symbol`'s parameter list, and every inserted
 * parameter is optional at a call site (isOptionalParam). Anything else that
 * moved — a return type, a parameter before the end, a parameter list inside
 * a type — makes it false. Keys are whitespace-free (signatureKey).
 */
function appendsOptionalParams(oldKey, newKey, symbol) {
  if (newKey.length <= oldKey.length) return false;
  let i = 0;
  while (i < oldKey.length && oldKey[i] === newKey[i]) i++;
  if (oldKey[i] !== ')') return false;
  const tail = oldKey.slice(i);
  if (!newKey.endsWith(tail)) return false;
  // The `(` this `)` closes.
  let open = -1;
  for (let j = i - 1, depth = 0; j >= 0; j--) {
    if (oldKey[j] === ')') depth++;
    else if (oldKey[j] === '(') {
      if (depth === 0) { open = j; break; }
      depth--;
    }
  }
  if (open === -1) return false;
  // A parameter list opens right after the name, its generics, or — for a JS
  // binding — `name = [async] [function]`. A `(` after `->`, `:` or `)` opens
  // a return type or a Go receiver/result list, never the parameters.
  const S = escapeRe(symbol);
  const opener = new RegExp(`${S}(?:<.*>|\\[.*\\])?(?:=(?:async)?(?:function\\*?)?)?$`);
  if (!opener.test(oldKey.slice(0, open))) return false;
  let inserted = newKey.slice(i, newKey.length - tail.length);
  if (open < i - 1) {
    if (inserted[0] !== ',') return false;
    inserted = inserted.slice(1);
  }
  const params = splitTopLevel(inserted);
  return params.length > 0 && params.every(isOptionalParam);
}

// +1 for an opening bracket at s[i], -1 for a closing one, else 0. The `>` of
// `=>` / `->` closes nothing.
function bracketStep(s, i) {
  const c = s[i];
  if ('([{<'.includes(c)) return 1;
  if (')]}'.includes(c) || (c === '>' && s[i - 1] !== '=' && s[i - 1] !== '-')) return -1;
  return 0;
}

// Splits at commas outside brackets.
function splitTopLevel(s) {
  const out = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < s.length; i++) {
    const step = bracketStep(s, i);
    if (step !== 0) depth = Math.max(0, depth + step);
    else if (s[i] === ',' && depth === 0) {
      out.push(s.slice(start, i));
      start = i + 1;
    }
  }
  out.push(s.slice(start));
  return out;
}

/**
 * A parameter a caller may leave out: a default value (`x = 1`, `int y = 0`,
 * `strict: bool = False`), TS `x?: T`, a variadic (`*args`, `**kwargs`, a bare
 * `*` marker, `...rest`, Go `opts ...T`, Java `T... xs`). Whitespace is
 * already gone from the key.
 */
function isOptionalParam(p) {
  if (!p) return false;
  if (p[0] === '*') return true;
  let depth = 0;
  for (let i = 0; i < p.length; i++) {
    const c = p[i];
    const step = bracketStep(p, i);
    if (step !== 0) { depth = Math.max(0, depth + step); continue; }
    if (depth !== 0) continue;
    if (c === '.' && p.startsWith('...', i)) return true;
    if (c === '?' && (p[i + 1] === ':' || i === p.length - 1)) return true;
    // The first depth-0 `=` is the default, unless it starts `=>` (an arrow
    // type). No `==`/`>=` can come earlier; `number>=new Map()` is a generic's
    // `>` then a default.
    if (c === '=' && p[i + 1] !== '>') return true;
  }
  return false;
}

/**
 * The line of the first mention of `symbol` at or after the caller's own
 * start line — the call site, which is what a reader needs to open. Falls
 * back to the caller's start line.
 */
function findCallSiteLine(text, symbol, startLine) {
  const start = Math.max(1, Number(startLine) || 1);
  if (typeof text !== 'string') return start;
  const lines = text.split('\n');
  const re = new RegExp(`(?:^|[^\\w$])${escapeRe(symbol)}(?![\\w$])`);
  for (let i = start - 1; i < lines.length && i < start - 1 + 400; i++) {
    if (!COMMENT_LINE.test(lines[i]) && re.test(lines[i])) return i + 1;
  }
  return start;
}

/**
 * The decision, with every side effect injected (unit-testable without git or
 * a binary). Returns the lines to report and the next Stop state.
 *
 * @param {object} a
 * @param {{ts:number,file:string,symbol:string|null,sigs:string[]|null}[]} a.edits
 *   whole session log; `sigs` = the symbol's signatures just before that Edit
 * @param {{lastStopAt:number|null, reported:string[]}} a.state
 * @param {number} a.now
 * @param {(file:string)=>string|null} a.workText  file in the working tree
 * @param {(symbol:string,file:string)=>({file:string,line:number,name:string}[]|null)} a.callers
 * @param {(file:string)=>number|null} a.mtimeMs
 * @param {(file:string)=>string} [a.indexKeyOf]  the key the index knows a
 *   file by: a linked worktree's copy drops the worktree's prefix (D#560)
 * @param {(file:string)=>string} [a.displayOf]  a file as the reader names it:
 *   a session working in a worktree reads paths from there (D#560 review)
 */
function computeStopReport({
  edits, state, now, workText, callers, mtimeMs, indexKeyOf = (f) => f, displayOf = (f) => f,
}) {
  const lastStopAt = state.lastStopAt;
  const turn = edits.filter((e) => lastStopAt === null || e.ts > lastStopAt);
  const next = { lastStopAt: now, reported: [...state.reported] };
  if (turn.length === 0) return { lines: [], state: next };

  const turnStart = lastStopAt !== null ? lastStopAt : Math.min(...edits.map((e) => e.ts));
  const editedThisTurn = new Set(turn.map((e) => e.file));
  const touched = (file) => {
    if (editedThisTurn.has(file)) return true;
    const m = mtimeMs(file);
    return m !== null && m >= turnStart;
  };

  // One candidate per (file, symbol). Its baseline is what pre-edit-guide
  // recorded before the turn's FIRST Edit of it — the signature as the turn
  // found it. Later records of the same symbol this turn were taken after
  // earlier edits had landed, so they are not the baseline.
  const seen = new Set();
  const changed = [];
  for (const e of turn) {
    if (!e.symbol) continue;
    const key = `${e.file}#${e.symbol}`;
    if (seen.has(key)) continue;
    seen.add(key);
    if (next.reported.includes(key)) continue;
    // null: no exact reading (language, unreadable file, a pre-repair log
    // line). [] (the definition was added this turn) needs no test of its
    // own: signatureChanged counts only definitions that went missing.
    if (!Array.isArray(e.sigs)) continue;
    const after = extractSignatures(workText(e.file), e.symbol, path.extname(e.file).toLowerCase());
    if (!after || after.length === 0) continue;     // removed, or no reading now
    // Body-only this turn, or only optional parameters appended (Q2).
    if (!signatureChanged(e.sigs, after, e.symbol)) continue;
    changed.push({ key, file: e.file, symbol: e.symbol });
  }

  const lines = [];
  const listed = new Set();
  for (const c of changed.slice(0, MAX_SYMBOLS_CHECKED)) {
    const refs = callers(c.symbol, c.file);
    if (!refs || refs.length === 0) continue;
    const untouched = [];
    const dedup = new Set();
    for (const r of refs) {
      if (r.file === c.file || touched(r.file)) continue;
      const id = `${r.file}:${r.line}`;
      if (dedup.has(id)) continue;
      dedup.add(id);
      untouched.push(r);
    }
    if (untouched.length === 0) continue;
    untouched.sort((x, y) => (x.file < y.file ? -1 : x.file > y.file ? 1 : x.line - y.line));
    // Every repo-derived token is shell-quoted (review M3): a file name is
    // text from the repository, and the model may paste it into a command.
    const shown = untouched.slice(0, MAX_CALLERS_LISTED).map((r) => {
      const name = r.name && r.name !== '<module>' ? ' (' + shellQuoteArg(r.name) + ')' : '';
      return shellQuoteArg(displayOf(r.file)) + ':' + r.line + name;
    });
    let line = '  ' + shellQuoteArg(c.symbol) + '() in ' + shellQuoteArg(displayOf(c.file)) + ': ' + shown.join(', ');
    if (untouched.length > MAX_CALLERS_LISTED) {
      line += ', and ' + (untouched.length - MAX_CALLERS_LISTED) + ' more (' +
        formatCgCommand(['refs', c.symbol, '--file', indexKeyOf(c.file)]) + ')';
    }
    lines.push(line);
    next.reported.push(c.key);
    for (const r of untouched) listed.add(r.file);
  }
  // Changed beyond the query cap: named, not dropped in silence (review M1).
  // Not marked reported, and a later turn does not see them as changed again
  // (their baseline moves on), so this line is the only mention they get.
  const unchecked = changed.slice(MAX_SYMBOLS_CHECKED);
  if (unchecked.length > 0) {
    lines.push('  ' + unchecked.length + ' more changed, callers not checked: ' +
      unchecked.map((c) => shellQuoteArg(c.symbol) + '() in ' + shellQuoteArg(displayOf(c.file))).join(', '));
  }
  // `symbols` / `files`: what the report names, for its telemetry and its
  // follow-up (D#163). Not part of `state`.
  return { lines, state: next, symbols: next.reported.length - state.reported.length, files: [...listed].sort() };
}

// The follow-up remembers at most this many of a report's caller files.
const MAX_FOLLOWUP_FILES = 64;

/**
 * What a report leaves behind (D#163): the `stop_check` record — for every
 * report the model is shown, the one made only of the "more changed, callers
 * not checked" line included — and the follow-up to judge at the next Stop,
 * only when the report named caller files. `callers` counts every caller file
 * the report found, the ones past the text's per-symbol cap included.
 * @param {{lines:string[], state:{lastStopAt:number}, symbols?:number, files?:string[]}} report
 * @returns {{check: object|null, pending: {at:number, files:string[]}|null}}
 */
function reportRecords(report) {
  if (!report.lines.length) return { check: null, pending: null };
  const files = report.files || [];
  return {
    check: { hook: 'stop', action: 'stop_check', symbols: report.symbols || 0, callers: files.length },
    pending: files.length ? { at: report.state.lastStopAt, files: files.slice(0, MAX_FOLLOWUP_FILES) } : null,
  };
}

/**
 * Whether a report was acted on: a caller file it listed was edited after it —
 * an Edit logged since `pending.at`, or an mtime at or after it (Write, `sed
 * -i`, formatters; the same reading as "touched this turn"). Judged at the
 * next Stop, which is the end of the continuation the feedback starts.
 * @param {{at:number, files:string[]}} pending  left in the Stop state by the report
 * @param {{ts:number, file:string}[]} edits  the session's edit log
 * @param {(file:string)=>number|null} mtimeMs
 * @returns {{adopted:boolean, listed:number, edited:number}}
 */
function followUpOf(pending, edits, mtimeMs) {
  const listed = new Set(pending.files);
  const edited = new Set();
  for (const e of edits) {
    if (e.ts >= pending.at && listed.has(e.file)) edited.add(e.file);
  }
  for (const f of listed) {
    if (edited.has(f)) continue;
    const m = mtimeMs(f);
    if (m !== null && m >= pending.at) edited.add(f);
  }
  return { adopted: edited.size > 0, listed: listed.size, edited: edited.size };
}

function formatStopContext(lines) {
  if (!lines.length) return null;
  // No apostrophe in the fixed text: file names are single-quoted in it.
  return '[code-graph] Signatures changed this turn (each compared with its file before ' +
    'the first edit of it this turn); their callers in files not edited this turn, from the code-graph index:\n' +
    lines.join('\n') + '\n';
}

// --- Side-effecting shell ---------------------------------------------------

function readStdinJson() {
  try {
    // fd 0, not '/dev/stdin' (ENXIO on socketpair stdin) — as the other hooks.
    return JSON.parse(fs.readFileSync(0, 'utf8'));
  } catch { return null; }
}

function runMain() {
  if (process.env.CODE_GRAPH_QUIET_HOOKS === '1') return;
  const input = readStdinJson();
  if (!input || !input.session_id) return;

  const { resolveProjectRoot, indexKey, worktreePrefix } = require('./project-root');
  const cwd = typeof input.cwd === 'string' && input.cwd ? input.cwd : process.cwd();
  const root = resolveProjectRoot(cwd);
  if (root === null) return;
  // A session working in a linked worktree (EnterWorktree) reads the report
  // from there; files in that worktree are named from its top.
  const shownFrom = worktreePrefix(root, path.relative(root, cwd).split(path.sep).join('/'));

  const sessionEdits = require('./session-edits');
  const { recordRecommendation } = require('./recommendation-log');
  const mtimeMs = (file) => {
    try { return fs.statSync(path.join(root, file)).mtimeMs; } catch { return null; }
  };
  const state = sessionEdits.readStopState(root, input.session_id);
  // The previous report's follow-up (D#163), at every Stop — the one ending
  // the continuation that report started included. It only records: nothing
  // is emitted, so a continuation cannot loop through it.
  if (state.pending) {
    const f = followUpOf(state.pending, sessionEdits.readEdits(root, input.session_id), mtimeMs);
    recordRecommendation(root, { hook: 'stop', action: 'stop_followup', ...f });
    delete state.pending;
    sessionEdits.writeStopState(root, input.session_id, state);
  }
  // A continuation caused by a Stop hook is never re-checked.
  if (input.stop_hook_active === true) return;

  const edits = sessionEdits.readEdits(root, input.session_id);
  if (edits.length === 0) return; // no log → nothing to check, and nothing written

  const { remainingMs } = require('./hook-fail-open');
  const run = (cmd, args, defaultMs) => {
    const budget = remainingMs(defaultMs);
    if (budget === null) return null;
    try {
      return execFileSync(cmd, args, hidden({
        cwd: root,
        timeout: budget,
        encoding: 'utf8',
        stdio: ['ignore', 'pipe', 'ignore'],
        env: { ...process.env, CODE_GRAPH_INTERNAL: '1' },
        maxBuffer: 16 * 1024 * 1024,
      }));
    } catch { return null; }
  };

  // The comparison no longer reads HEAD (baselines come from the edit log),
  // but "silent outside a git work tree" is the spec's contract for this
  // hook, so the gate stays.
  const inGit = run('git', ['rev-parse', '--is-inside-work-tree'], 1000);
  let binary = null;
  let report = { lines: [], state: { lastStopAt: Date.now(), reported: state.reported } };
  if (inGit !== null && inGit.trim() === 'true') {
    binary = require('./find-binary').findBinary();
  }
  if (binary) {
    report = computeStopReport({
      edits,
      state,
      now: Date.now(),
      workText: (file) => {
        try { return fs.readFileSync(path.join(root, file), 'utf8'); } catch { return null; }
      },
      // D#560 — an isolated agent's edits are logged by their path through its
      // worktree. The index is asked by the key it knows, and the callers it
      // names are that worktree's copies: the ones the agent's change reaches.
      callers: (symbol, file) => {
        const key = indexKey(root, file);
        const worktree = file.slice(0, file.length - key.length);
        const raw = run(binary, ['refs', symbol, '--file', key, '--relation', 'calls',
          '--min-confidence', 'inferred', '--json'], 1500);
        if (raw === null) return null;
        let parsed;
        try { parsed = JSON.parse(raw); } catch { return null; }
        if (!parsed || !Array.isArray(parsed.references)) return null;
        return parsed.references
          .filter((r) => r && typeof r.file_path === 'string')
          .map((r) => {
            const caller = worktree + r.file_path;
            let text = null;
            try { text = fs.readFileSync(path.join(root, caller), 'utf8'); } catch { /* gone */ }
            return { file: caller, name: r.name, line: findCallSiteLine(text, symbol, r.start_line) };
          });
      },
      mtimeMs,
      indexKeyOf: (file) => indexKey(root, file),
      displayOf: (file) => (shownFrom && file.startsWith(shownFrom) ? file.slice(shownFrom.length) : file),
    });
  }
  const rec = reportRecords(report);
  if (rec.check) recordRecommendation(root, rec.check);
  if (rec.pending) report.state.pending = rec.pending;
  sessionEdits.writeStopState(root, input.session_id, report.state);

  const text = formatStopContext(report.lines);
  if (!text) return;
  const { emitEventContext } = require('./hook-emit');
  process.stdout.write(emitEventContext('Stop', text) + '\n');
}

if (require.main === module) runMain();

module.exports = {
  extractSignatures, allSignatures, signatureChanged, findCallSiteLine, computeStopReport, formatStopContext,
  followUpOf, reportRecords, MAX_SYMBOLS_CHECKED, MAX_CALLERS_LISTED, MAX_FOLLOWUP_FILES,
};
