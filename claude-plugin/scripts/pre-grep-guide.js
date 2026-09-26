#!/usr/bin/env node
'use strict';
// FIRST statement, before this file's other requires (pre-tag review
// 2026-09-02): the handler installed after them could not catch a throw
// from `require('./lifecycle')` itself, which is exactly the broken-install
// case JS-12 exists for. Guarded on `require.main` so importing this module
// in a test does NOT install a process-wide handler that exits 0 — that
// would swallow the test's own failures.
if (require.main === module) require('./hook-fail-open').installHookFailOpen('PreToolUse:Bash');

// PreToolUse(Bash) hook: detect raw `grep`/`rg`/`ag` on the indexed source tree
// and either BLOCK with suggestion (v0.32+) or HINT (legacy path). Closes the
// "Bash comfort zone" leak — pre-training bias has Claude reach for `grep -rn`
// ~13× more than the indexed CLI on bash-heavy days (15-day baseline: 429 raw
// grep vs 191 functional CLI). v0.25.0 hint-only had ~0% transfer rate; v0.32.0
// upgrades the narrowest "I'm searching for a symbol" subset to block-with-reason.
// Since the rewrite change, an ANSWERED block no longer denies: the grep is
// rewritten (PreToolUse `updatedInput`) into the cg command that answers it, so
// the call succeeds instead of rendering as a red failed tool call. Only the
// opt-in static deny (CODE_GRAPH_NO_ANSWER_IN_DENY=1) still denies.
//
// HINT fires when ALL conditions met (shouldHint):
//   1. Command HEAD is grep/rg/ag (NOT piped — pipe-greps are output filters)
//   2. Args include an indexed source-tree path (src/ tests/ lib/ scripts/ ...)
//   3. Not searching only a config/lockfile (Cargo.toml/.gitignore/*.md/*.json)
//   4. Command doesn't already invoke code-graph-mcp (no double-suggest)
//   5. .code-graph/index.db exists in CWD or a parent up to $HOME (v0.48: the
//      hook's cwd follows the persistent shell — after `cd backend/` every
//      gate used to fail silently for the rest of the session; daagu
//      2026-06-11 replay: 38/40 head-greps dark to this)
//   6. Same command-hash not hinted within last 60s (per-command cooldown)
//
// BLOCK fires when shouldHint AND (classifyBlock, v0.49 intent-aware):
//   7. Pattern looks identifier-like (CamelCase ≥4ch, or snake_case with _, or
//      a declaration anchor like `fn X` / `class X` / `def X`), quoted
//   8. Pattern is not a bare marker word (TODO/FIXME/XXX/HACK/WARN/ERROR/NOTE)
//   9. No unanswerable-intent flag (-L / -v / --exclude*) — those stay hint
//  10. CODE_GRAPH_NO_BLOCK_GREP != "1" (block escape, independent of QUIET_HOOKS)
//  Mode: context flags (-A/-B/-C) + declaration anchors → 'show' (deny carries
//  the symbol BODIES via `cg show`); context flags without named decls → hint;
//  everything else (incl. -l / --include) → 'grep' (deny carries the hits).
//
// A `CODE_GRAPH_NO_BLOCK_GREP=1`-prefixed grep that would otherwise hint is
// recorded as `action:'bypass'` and allowed silently (v0.48) — previously the
// bare KEY=VALUE prefix failed GREP_HEAD and the escape was invisible to the
// conversion funnel (daagu 2026-06-11: 14 bypassed greps, 0 recorded).
//
// Exits silently otherwise — zero noise for build greps, log filters, config
// lookups, or the rare legitimate use of raw grep on indexed source.

const fs = require('fs');
const path = require('path');
const { cgTmpDir, cwdHash, makeCooldown } = require('./tmp-dir');
const { recordRecommendation } = require('./recommendation-log');
const {
  runGrepAnswer, runShowAnswer, sanitizeSearchPath, buildGrepArgs, formatCgCommand, shellQuoteArg,
  resolveAnswerBinary,
} = require('./cg-answer');
const { emitPreToolRewrite } = require('./hook-emit');

// --- Pure logic (testable) ---

// v0.48: also match bare `KEY=VALUE grep` prefixes (no `env` verb) — the shape
// the deny message itself teaches (`CODE_GRAPH_NO_BLOCK_GREP=1 grep …`). With
// the old `env`-only form those commands failed gate 1 and were invisible.
// v0.71: `git grep` shares the verb set — its head is `git`, so it leaked past
// the matcher until folded in here. cg grep is a SUPERSET (covers tracked AND
// gitignored files), so routing `git grep` to it is sound. GREP_VERB is the
// single source of truth for every parse site that recognizes the search verb.
const GREP_VERB = 'git\\s+grep|grep|rg|ag';
const GREP_HEAD = new RegExp(`^\\s*(?:env\\s+)?(?:[A-Za-z_][A-Za-z0-9_]*=\\S*\\s+)*(${GREP_VERB})\\b`);
// Verb + prefix strip (kept in sync with GREP_HEAD via GREP_VERB; non-capturing).
// Shared by extractPatterns and countNamedPaths so the verb is removed identically.
const VERB_STRIP = new RegExp(`^\\s*(?:env\\s+)?(?:[A-Za-z_][A-Za-z0-9_]*=\\S*\\s+)*(?:${GREP_VERB})\\s+`);
// Source-tree prefix list. Expanded v0.27+ Phase C: original `src/tests/lib/...`
// missed real-world backend conventions where the prefix list term is preceded
// by something else (`backend/app/...` — `app/` doesn't match because `/` isn't
// in the lookbehind). 7d audit found 5 of the worst missed sessions used the
// daagu `backend/app/services/...` layout. Added: backend/frontend/services/
// models/domain/controllers/views/handlers/middleware/routes/repositories/
// entities/migrations/tasks/jobs/workers/features/modules/api/web. Generic
// terms like `core`/`utils`/`shared`/`common`/`types` deliberately omitted —
// they appear in too many non-code contexts to be precise enough.
// v0.96 — added `skills` (Claude Code plugin / agent monorepos keep source
// under `skills/<name>/…`, e.g. `skills/moa/scripts/moa.py`); a grep there was
// invisible to the hook so it could never scope the answer to the real target.
const SRC_PREFIXES =
  'src|tests|lib|libs|scripts|skills|claude-plugin|tools|pkg|cmd|internal|app|apps|components?|server|client|crates|packages|backend|frontend|services|models|domain|controllers|views|handlers|middleware|routes|repositories|entities|migrations|tasks|jobs|workers|features|modules|api|web';
const SRC_PATH = new RegExp(`(?:^|\\s|["'])(${SRC_PREFIXES})/`);
// Anchored variant for whole-token matching in extractSearchPath.
const SRC_PATH_TOKEN = new RegExp(`^(?:\\./)?(${SRC_PREFIXES})/`);
// A path operand the rewrite grammar proved: a bare prefix word (`src`), or a
// `./`-rooted one with any subpath (`./src`, `./src/x/`), which SRC_PATH's
// lookbehind never matched.
const SRC_BARE_TOKEN = new RegExp(`^(?:(?:${SRC_PREFIXES})|\\./(?:${SRC_PREFIXES})(?:/.*)?)$`);

// D#73 — a source dir written as a bare word (`grep -rn X src`, `rg X tests`,
// `./src`): the shape models write when a prompt says "under src/", and it
// matched nothing before. Recognized ONLY where rewritePlan's grammar proves the
// word is the command's path operand. A regex over the text cannot tell: these
// prefixes are English words (`server`, `tasks`) and import-path fragments
// inside patterns, flag values (`rg -t cmd`, `ag --ignore tests`), halves of a
// two-path or `$(…)` search, or the pattern itself (`grep -n tasks f.py`) —
// pre-ship review round 1 reproduced each as a wrong hint or inject. Callers
// in a subdirectory shell must not use it on an un-rebased word: there a bare
// `src` is `<cwd>/src`, not the root's (see runMain). `.` and no path at all
// stay out: they reach non-source files, and grep -r ignores .gitignore where
// cg does not.
function bareSourceTarget(clause) {
  const plan = rewritePlan(clause);
  return plan && plan.target !== undefined && SRC_BARE_TOKEN.test(plan.target) ? plan.target : null;
}
function namesSourcePath(clause) {
  return SRC_PATH.test(clause) || bareSourceTarget(clause) !== null;
}
const PIPE_INTO_GREP = new RegExp(`\\|\\s*(?:${GREP_VERB})\\b`);
const CG_INVOKED = /\bcode-graph-mcp\b/;
// File argument(s) that end in a config/lockfile/data extension. If, after removing
// ALL of them, no source-tree path remains, the grep is searching config/data not code.
// v0.69 floor-hardening: (a) extended the extension list (ini/conf/xml/log/csv) and
// (b) made the strip GLOBAL so multiple data files (`grep X src/a.json src/b.json`) all
// peel off — previously only the first did, leaving the 2nd's `src/`-prefixed path to
// false-match SRC_PATH and fire. cg has no structural answer for these, so a deny is
// friction-without-value that teaches CODE_GRAPH_NO_BLOCK_GREP bypass (2026-06-23 reach
// audit: the unreached ~75% of greps are genuinely non-foldable — keep precision).
const NON_SOURCE_EXTS =
  'toml|md|json|yml|yaml|lock|txt|cfg|env|gitignore|properties|ini|conf|xml|log|csv';
const CONFIG_TARGET_ONLY = new RegExp(`(?:^|\\s)[^\\s|<>]*\\.(?:${NON_SOURCE_EXTS})(?:\\s|$)`, 'i');
// Global + trailing-lookahead variant for the strip: lookahead (not consume) so adjacent
// data-file tokens both match; global so every one is peeled before the SRC_PATH re-check.
const CONFIG_TARGET_STRIP = new RegExp(`(?:^|\\s)[^\\s|<>]*\\.(?:${NON_SOURCE_EXTS})(?=\\s|$)`, 'gi');

// v0.96 — the grep's OWN args end at the first top-level shell separator
// (`;` `|` `&` `>` `<` newline). Everything after is a DIFFERENT command whose
// paths/flags/patterns must NOT be attributed to the grep. This closes a sibling
// hole: countNamedPaths stopped at the separator in v0.70, but the SRC_PATH gate
// in shouldHint, extractSearchPath, extractPatterns, and classifyBlock's flag
// checks all still scanned the WHOLE compound command. Real 2026-07-13 miss:
// `grep -n "VERSION" skills/moa/scripts/moa.py | head; …; python3 … scripts/bump-version.sh`
// — `skills/` was not an allowed prefix, so the gate/searchPath skipped the real
// target and latched onto `scripts/bump-version.sh` in the tail, then presented a
// confidently WRONG "already ran for you" answer for a file the user never grepped.
// Quote-aware (POSIX): a separator inside quotes is literal; inside DOUBLE quotes a
// backslash escapes the next char (so `\"` does not close) — mirrors
// splitTopLevelSegments so both share one notion of "quote-terminating vs escaped".
function firstShellClause(cmd) {
  if (!cmd || typeof cmd !== 'string') return cmd;
  let quote = null;
  for (let i = 0; i < cmd.length; i++) {
    const c = cmd[i];
    if (quote) {
      if (quote === '"' && c === '\\' && i + 1 < cmd.length) { i++; continue; }
      if (c === quote) quote = null;
      continue;
    }
    // Outside quotes a backslash escapes the next character: `\"` opens no
    // quote and `\;` / `\|` are literal. quotedSpans reads patterns by the same
    // rule; a splitter that disagreed let `grep \"X\" src/ && echo "Y"` hand the
    // echo's `Y` to the pattern reader (pre-ship review F1).
    if (c === '\\') { i++; continue; }
    if (c === '"' || c === "'") { quote = c; continue; }
    // Control operators END the grep's argument list → truncate. NOT redirects
    // (`>` `<`): `2>&1`, `>out`, and process substitution `-f <(cat pats) src/`
    // all keep grep path args AFTER them, so a redirect is not a boundary. NOT a
    // single background `&` either (it collides with `2>&1`/`&>` and a
    // backgrounded grep's args still precede it). `&&`/`||` DO terminate.
    if (c === ';' || c === '|' || c === '\n') return cmd.slice(0, i);
    if (c === '&' && cmd[i + 1] === '&') return cmd.slice(0, i);
  }
  return cmd;
}

// v0.71 — `git grep --cached`/`--staged` searches the STAGED index, and a treeish
// ref (`git grep "X" HEAD~3 -- src/`, `git grep "X" main -- src/`) searches another
// commit/branch — a scope the working-tree inline answer (`code-graph-mcp grep`)
// CANNOT honor. Folding them would substitute current-tree hits for a different
// revision with no signal. These are NOT the working-tree source searches this hook
// folds, so it stays out entirely (no hint, no deny) and the real git grep runs.
// (`--no-index` is working-tree scope → cg covers it → NOT excluded; plain grep/rg/ag
// have no revision concept.) A bare treeish without `--` (`git grep X main src/`) is
// genuinely ambiguous with a pathspec → left as the residual minority.
const GIT_GREP_HEAD = /^\s*(?:env\s+)?(?:[A-Za-z_][A-Za-z0-9_]*=\S*\s+)*git\s+grep\b/;
const GIT_GREP_STAGED = /(?:^|\s)--(?:cached|staged)(?:\s|$)/;

function isRevisionScopedGitGrep(cmd) {
  if (typeof cmd !== 'string' || !GIT_GREP_HEAD.test(cmd)) return false;
  if (GIT_GREP_STAGED.test(cmd)) return true;
  // treeish before the `--` pathspec separator: git grep [flags] PATTERN <ref>... -- <path>
  const sep = cmd.indexOf(' -- ');
  if (sep === -1) return false;
  const afterVerb = cmd.slice(0, sep).replace(GIT_GREP_HEAD, '').trimStart();
  let seenPattern = false;
  for (const tok of afterVerb.split(/\s+/)) {
    if (!tok || tok.startsWith('-')) continue;          // a flag
    if (!seenPattern) { seenPattern = true; continue; } // the search pattern
    return true;                                        // a 2nd non-flag token before `--` = treeish
  }
  return false;
}

function shouldHint(cmd) {
  if (!cmd || typeof cmd !== 'string') return false;
  if (cmd.length > 1000) return false;             // sanity — oversize commands are noise
  if (CG_INVOKED.test(cmd)) return false;          // already using cg
  if (PIPE_INTO_GREP.test(cmd)) return false;      // `cargo test | grep FAILED` is output filter
  if (!GREP_HEAD.test(cmd)) return false;          // not a search command
  if (isRevisionScopedGitGrep(cmd)) return false;  // v0.71 — git grep --cached/treeish: scope cg can't honor
  // v0.96 — the source-path gate must see ONLY the grep's own args, not a path in
  // a non-grep tail (`grep X skills/a.py; wc scripts/b` must not fire on scripts/b).
  const clause = firstShellClause(cmd);
  if (!namesSourcePath(clause)) return false;       // not against indexed source tree
  // If a config file appears AND no source path remains after stripping it, skip.
  if (CONFIG_TARGET_ONLY.test(clause)) {
    const stripped = clause.replace(CONFIG_TARGET_STRIP, ' ');
    if (!namesSourcePath(stripped)) return false;
  }
  return true;
}

// v0.49 intent-aware block tiers. The v0.32 rationale ("precision flags mean
// the user is filtering — a blanket *suggestion* would be wrong") was written
// for the suggestion era; in the answer era the deny CARRIES the result, so a
// flag only disqualifies when the answer cannot honor its intent. 2026-06-12
// daagu replay: 22/128 head-greps were `rg "def X|class Y" -A 25` — function-
// body reads the old rule exempted to (ignored) hints; `cg show` answers them.
//
// Context flags (-A/-B/-C): intent = read surrounding code. Answerable via
// `show` only when the pattern names declarations; bare-identifier + context
// stays hint (a grep-style answer can't honor ±N lines).
const CONTEXT_FLAG =
  /(?:^|\s)-[a-zA-Z]*[ABC][a-zA-Z]*(?:\s|=|\d|$)|--(?:after-context|before-context|context)\b/;
// Intents the cg answer cannot honor: inverted file lists, exclusion scoping.
const UNANSWERABLE_FLAGS =
  /(?:^|\s)-[a-zA-Z]*[Lv][a-zA-Z]*(?:\s|=|\d|$)|--(?:files-without-match|invert-match|exclude|exclude-dir)\b/;
// v0.32.1: drop the `type` declaration keyword (too common in English prose
// like "# type checking") and anchor declaration anchors to pattern start
// (otherwise `"some type X"` matches). CamelCase and snake_case still match
// anywhere — they're distinctive enough on their own.
const IDENTIFIER_LIKE =
  /[A-Z][a-zA-Z0-9]{3,}|[a-z][a-z0-9]*_[a-z0-9_]+|^\s*(?:fn|def|class|function|struct|impl|trait)\s+\w/;
const MARKER_ONLY =
  /^[^"']*["']\s*(?:TODO|FIXME|XXX|HACK|WARN|WARNING|ERROR|NOTE)\s*["']/i;

// v0.32.1: pull the pattern argument(s) out of the command before running
// IDENTIFIER_LIKE — testing the full cmd false-positives on CamelCase /
// snake_case in PATH ARGUMENTS like `src/EmbeddingModel.rs` or
// `src/some_module/`. The pattern arg is what the user is actually searching
// for, and that's the only thing we should evaluate against "is this a
// symbol-shaped target".
function extractPatterns(cmd) {
  if (!cmd || typeof cmd !== 'string') return [];
  // v0.96 — only the grep's own clause; a quoted string in a compound tail
  // (`; echo "SomeWord"`) is not a grep pattern and must not be screened.
  // Strip leading verb + env/assignment prefix (kept in sync with GREP_HEAD)
  const stripped = firstShellClause(cmd).replace(VERB_STRIP, '');
  // Collect every quoted argument — first one is the pattern in standard grep
  // usage; subsequent ones (e.g. `-e "second"`) are also patterns or filter
  // expressions and worth screening too.
  return quotedSpans(stripped).map(s => s.body).filter(Boolean);
}

/**
 * The quoted spans of a shell clause, read the way the shell reads them — the
 * rules firstShellClause, splitTopLevelSegments and extractUnansweredTail also
 * follow (lesson #9656 inside quotes; pre-ship review F1 outside), which a span regex (`"([^"]+)"|'([^']+)'`) did not: it closed a
 * double-quoted argument at the first `\"`, so
 * `grep "if:\s*'\|\"if\"\|statusMessage"` yielded `\|statusMessage`, and its
 * translation `|statusMessage` matches every line (observed 2026-09-25).
 *   - `'…'` is literal to the next `'`.
 *   - `"…"`: a backslash escapes `"` `\` `$` and backtick (the backslash is
 *     dropped) and, before a newline, removes both characters (a line
 *     continuation); before anything else it is literal (`"a\|b"`).
 *   - Outside quotes a backslash escapes the next character, so `\"` opens
 *     nothing — the shell hands grep the quote character itself.
 * An unterminated quote ends the scan: what follows is not a span we can read.
 * @returns {{start: number, end: number, body: string}[]} `end` is exclusive,
 *   covering both quote characters.
 */
function quotedSpans(s) {
  const spans = [];
  if (typeof s !== 'string') return spans;
  for (let i = 0; i < s.length; i++) {
    const c = s[i];
    if (c === '\\') { i++; continue; }
    if (c !== '"' && c !== "'") continue;
    let body = '';
    let j = i + 1;
    for (; j < s.length && s[j] !== c; j++) {
      if (c === '"' && s[j] === '\\' && j + 1 < s.length && '"\\$`\n'.includes(s[j + 1])) {
        j++;
        if (s[j] === '\n') continue;  // backslash-newline is a line continuation: both go
      }
      body += s[j];
    }
    if (j >= s.length) break;
    spans.push({ start: i, end: j + 1, body });
    i = j;
  }
  return spans;
}

// Declaration anchors inside a pattern (`def cascade_failure|class TaskState`)
// name the exact symbols the model wants to READ — extract them for `show`.
const DECL_SYMBOL = /(?:fn|def|class|function|struct|impl|trait)\s+([A-Za-z_][A-Za-z0-9_]*)/g;

function extractDeclSymbols(patterns) {
  const out = [];
  for (const p of patterns) {
    for (const m of p.matchAll(DECL_SYMBOL)) {
      if (!out.includes(m[1])) out.push(m[1]);
    }
  }
  return out;
}

/// Block-tier classification (strictly narrower than shouldHint):
///   {mode:'show', symbols} — declaration anchors + context flags → deliver bodies
///   {mode:'grep'}          — symbol search (incl. -l / --include) → deliver hits
///   null                   — hint tier (marker scans, unquoted, unanswerable flags)
function classifyBlock(cmd) {
  if (!shouldHint(cmd)) return null;              // narrower than hint
  // v0.96 — every flag/pattern check below must see the grep's OWN clause, not a
  // tail command's flags (`grep X src/a.py; grep -v Y src/b.py` — the tail's -v
  // must not disqualify the answerable head grep).
  const clause = firstShellClause(cmd);
  if (UNANSWERABLE_FLAGS.test(clause)) return null;  // intent the answer can't honor
  if (isAgFilenameSearch(clause)) return null;       // a filename search, not a content one
  if (MARKER_ONLY.test(clause)) return null;         // bare TODO/FIXME — no cg equivalent
  const patterns = extractPatterns(clause);
  if (patterns.length === 0) return null;         // unquoted pattern — conservative, hint
  if (!patterns.some(p => IDENTIFIER_LIKE.test(p))) return null;
  if (CONTEXT_FLAG.test(clause)) {
    const symbols = extractDeclSymbols(patterns);
    if (symbols.length === 0) return null;        // context read without named decls
    return { mode: 'show', symbols: symbols.slice(0, 3) };
  }
  // v0.70 — only DENY when the inline grep answer can cover the SAME scope. It scopes to one
  // path (extractSearchPath = first src-prefixed token), so a grep naming ≥2 file paths gets a
  // first-path-only answer (the rest silently dropped) — an incomplete substitute that
  // rationally teaches CODE_GRAPH_NO_BLOCK_GREP bypass. Downgrade to hint: the model's complete
  // grep runs and the hint still nudges. (show mode above is symbol-scoped, not path → unaffected.)
  if (countNamedPaths(clause, patterns) >= 2) return null;
  return { mode: 'grep' };
}

// Flags of the user's grep that `code-graph-mcp grep` can honor, in its own
// spelling. Verified against the built binary rather than against help text:
// `-l` prints bare paths, `-c` prints `path:count` exactly as `grep -c` does.
//
// Everything absent from this map is absent on purpose. `-r`/`-R`, `-n` and
// `-H` ARE accepted by cg as grep-parity no-ops (it is always recursive and
// always prints path and line number), so forwarding them would be harmless
// rather than an error — they are simply nothing to say. `-h` is NOT the same
// flag in the two tools: cg reads it as `--help`, so forwarding it would print
// usage instead of searching. `-L`/`-v`/`--exclude*` never arrive here —
// UNANSWERABLE_FLAGS sends them to the hint tier — and `-A/-B/-C` are routed to
// show-mode or hint before this runs.
const CG_SHORT_FLAGS = { i: '-i', w: '-w', F: '-F', l: '-l', c: '-c' };
const CG_LONG_FLAGS = {
  '--ignore-case': '-i',
  '--word-regexp': '-w',
  '--fixed-strings': '-F',
  '--files-with-matches': '-l',
  '--count': '-c',
};
// Flags that carry a VALUE and select which files are searched. All three spell
// the same filter cg spells `-g` / `-t`, and dropping one silently widens the
// answer to files the user excluded — the defect this whole flag map exists to
// stop. `rg`'s spellings matter because rg is a folded verb: `rg -g '*.rs' Sym
// src/` reached the answer as a bare `grep Sym src/`. cg takes `-g` repeatably,
// so every occurrence is forwarded rather than only the first.
const CG_VALUE_FLAGS = {
  '--include': '-g',   // grep
  '--glob': '-g',      // rg
  '--type': '-t',      // rg
};
// Short forms of the value-carrying flags. RIPGREP ONLY, and the verb is
// checked: `ag -t Sym src/` means "all text files" and takes no value, and ag's
// `-g` prints matching FILENAMES rather than filtering — mapping either would
// hand cg an argument the user never wrote. (Round 2 of pre-ship review; the
// symptom was mild — cg exits 2 on an unknown file type, the answer degrades to
// `unavailable` and the deny becomes an allow — but it is a fabricated
// argument.) grep has no short spelling for `--include`.
const CG_VALUE_SHORT = { g: '-g', t: '-t' };
const RG_VERB = /(?:^|\s)rg$/;
const AG_VERB = /(?:^|\s)ag$/;

/**
 * `ag -g PATTERN` searches FILENAMES, not file contents.
 *
 * Round 2 gated the short `-g`/`-t` map on ripgrep so ag's spellings would stop
 * being folded into cg arguments the user never wrote. Round 3 found what that
 * left behind: with the fabricated argument gone, `ag -g some_symbol src/` is a
 * plain answerable content grep as far as `classifyBlock` is concerned, so the
 * hook now DENIES it and answers with matching LINES — a different question
 * from the one asked. Before round 2 the fabricated `-g some_symbol` matched no
 * file, the answer came back empty and the deny degraded to an allow; the bug
 * was accidentally its own safety valve.
 *
 * cg has no filename-search mode, so this belongs in the hint tier with the
 * other intents the answer cannot honor: the user's `ag` runs untouched.
 */
function isAgFilenameSearch(clause) {
  if (typeof clause !== 'string') return false;
  if (!AG_VERB.test((clause.match(GREP_HEAD) || [])[1] || '')) return false;
  // A TOKEN scan, not a regex over the whole clause (round 4 of pre-ship
  // review). The first spelling of this guard matched `-g` only when it was
  // whitespace-delimited, so `ag -g"some_symbol" src/` — the attached form
  // `extractCgFlags` twelve lines below explicitly handles — still reached the
  // deny and was answered with content lines. Scanning the raw clause instead
  // had the mirror-image fault: a `-g` inside a quoted pattern
  // (`ag "some_symbol -g x" src/`) demoted a real content search. Same
  // flag-name-in-a-value-position class the sibling repairs fixed with
  // cgFlagSet/hasGlobFlag, so this uses the same convention: a token that
  // STARTS quoted is an argument, never a flag.
  // Quoted spans are blanked BEFORE tokenizing, not skipped after: a quoted
  // argument can contain whitespace, so `ag "some_symbol -g x" src/` splits into
  // three tokens and the middle one looks exactly like a flag. Blanking leaves
  // an attached value's flag behind (`-g"x"` → `-g`), which is what we want.
  // Spans come from quotedSpans, not a span regex: `"a\" -g x"` is ONE
  // argument, and a regex that closed it at the `\"` left `-g` bare.
  const unverbed = clause.replace(VERB_STRIP, '');
  let scan = '';
  let at = 0;
  for (const sp of quotedSpans(unverbed)) {
    scan += unverbed.slice(at, sp.start) + ' ';
    at = sp.end;
  }
  scan += unverbed.slice(at);
  for (const tok of scan.trim().split(/\s+/)) {
    if (!tok || tok[0] !== '-') continue;
    if (tok.startsWith('--')) {
      if (/^--(?:filename-pattern|file-search-regex)(?:=|$)/.test(tok)) return true;
      continue;
    }
    // Short cluster: ag's `-g` takes a value, so it ends the cluster — an
    // attached value (`-g'x'`, `-g=x`) or the next token. Capital `-G` is a
    // different ag flag (limit by filename) and must NOT match.
    if (/^-[a-zA-Z]*g/.test(tok)) return true;
  }
  return false;
}

/**
 * The boolean flags in an `extractCgFlags` result, without the VALUES.
 *
 * The result is a flat argv fragment (`['-i','-g','*.rs']`), which is what
 * `buildGrepArgs` needs — but it means a membership test sees value tokens too.
 * Round 2 of pre-ship review found both live consequences: `rg -t -g "sym"
 * src/*.rs` yields `['-t','-g']`, whose bare `includes('-g')` made buildGrepArgs
 * drop the path-derived glob and widen the scope — the exact defect this change
 * exists to fix — and `grep --include -F "sym" src/` yields `['-g','-F']`, where
 * `-F` is a filename glob and firing the literal-pattern guard on it is wrong.
 */
function cgFlagSet(flags) {
  const out = new Set();
  for (let i = 0; i < (flags || []).length; i++) {
    const f = flags[i];
    if (f === '-g' || f === '-t') { i++; continue; }  // skip its value
    out.add(f);
  }
  return out;
}
// Canonical emission order, so the argv (and therefore the printed command) is a
// function of WHICH flags were given, never of the order the user typed them.
const CG_FLAG_ORDER = ['-i', '-w', '-F', '-l', '-c'];

/**
 * The grep's flags, translated to cg's.
 *
 * Until v0.144 the answer ran `['grep', pattern, scope]` and the deny printed
 * the same three tokens, so every flag was dropped in both places at once — a
 * `grep -rln` deny announced "the AST-aware equivalent already ran for you"
 * above a command that was not the equivalent and returned hit lines where a
 * file list was asked for (field report 2026-09-08).
 *
 * Scanning rules that matter: only the grep's OWN clause (a tail command's
 * `-l` is not this grep's), and a token that STARTS quoted is an argument, not
 * a flag — `grep "-l" src/` searches for the string `-l`.
 */
function extractCgFlags(cmd) {
  if (!cmd || typeof cmd !== 'string') return [];
  const clause = firstShellClause(cmd);
  const isRg = RG_VERB.test((clause.match(GREP_HEAD) || [])[1] || '');
  const toks = clause.replace(VERB_STRIP, '').trim().split(/\s+/);
  const found = new Set();
  const filters = [];  // [cgFlag, value] pairs, in the order the user wrote them
  const unquote = (s) => (s || '').replace(/^["']|["']$/g, '');
  const addFilter = (flag, value) => {
    if (!value) return;
    // `-g '*.rs' -g '*.rs'` is one filter written twice; cg would honour both
    // identically, but the printed command should not look like a mistake.
    if (!filters.some(([f, v]) => f === flag && v === value)) filters.push([flag, value]);
  };
  for (let i = 0; i < toks.length; i++) {
    const tok = toks[i];
    if (!tok || tok[0] === '"' || tok[0] === "'" || tok[0] !== '-') continue;
    if (tok.startsWith('--')) {
      const eq = tok.indexOf('=');
      const name = eq === -1 ? tok : tok.slice(0, eq);
      if (CG_VALUE_FLAGS[name]) {
        addFilter(CG_VALUE_FLAGS[name], unquote(eq === -1 ? toks[i + 1] : tok.slice(eq + 1)));
        if (eq === -1) i++;  // the value was a separate token — do not rescan it
        continue;
      }
      if (CG_LONG_FLAGS[name]) found.add(CG_LONG_FLAGS[name]);
      continue;
    }
    // A short cluster: `-rln` is r + l + n, and only `l` has a cg spelling. A
    // value-carrying short flag ends the cluster and takes what follows —
    // attached (`-g*.rs`) or as the next token (`-g '*.rs'`).
    const letters = tok.slice(1);
    let consumed = false;
    for (let k = 0; k < letters.length; k++) {
      const ch = letters[k];
      if (isRg && CG_VALUE_SHORT[ch]) {
        const attached = letters.slice(k + 1);
        addFilter(CG_VALUE_SHORT[ch], attached ? unquote(attached) : unquote(toks[i + 1]));
        // Consume the value token so the next pass cannot read it as a flag
        // cluster: `rg -g -i 'sym' src/` must be `-g` with value `-i`, not `-g`
        // plus a phantom `-i` (round 2 of pre-ship review — the test that named
        // this property used a quoted value, which the quote guard already
        // skipped, so it passed with the increment removed).
        if (!attached) { i++; }
        consumed = true;
        break;
      }
      if (CG_SHORT_FLAGS[ch]) found.add(CG_SHORT_FLAGS[ch]);
    }
    if (consumed) continue;
  }
  const out = CG_FLAG_ORDER.filter((f) => found.has(f));
  for (const [flag, value] of filters) out.push(flag, value);
  return out;
}

/**
 * The deny gate, as runMain applies it — strictly narrower than classifyBlock.
 *
 * classifyBlock answers "can the inline answer cover this grep". Denying asks
 * something else: "may we cancel the WHOLE command". A top-level `;`/`&&` tail
 * makes those different questions, because the deny cancels a `sed`/`wc`/`echo`
 * the answer says nothing about. Until v0.144 the tail was detected and spent on
 * a NOTE apologising for the loss; the deny fired anyway.
 *
 * It is the same incompleteness the ≥2-named-paths rule above already refuses to
 * deny on ("only DENY when the inline grep answer can cover the SAME scope"),
 * and the fallback was already built: post-grep-inject.js is a PostToolUse hook
 * for compound greps whose segment walk has no head-is-grep exclusion, carries
 * its own redundancy gate, and uses a distinct cooldown prefix. These commands
 * reach it as soon as they are allowed to run.
 *
 * `|` and `||` are NOT tails: a pipe is one pipeline that the answer replaces
 * whole, and the `||` branch would not have run given the answer carried hits.
 *
 * classifyBlock itself is deliberately left alone — post-grep-inject calls it
 * per SEGMENT, where a top-level tail cannot exist by construction.
 */
function classifyDeny(cmd) {
  if (extractUnansweredTail(cmd)) return null;
  return classifyBlock(cmd);
}

function shouldBlock(cmd) {
  return classifyBlock(cmd) !== null;
}

// v0.47.1 — CC harness steers Bash toward ABSOLUTE paths (cd in compound
// commands triggers permission prompts), so `grep -rn "X" /abs/root/backend/…`
// is the dominant real shape — and SRC_PATH's lookbehind (^|\s|quote) never
// matched it (daagu 2026-06-11 replay: 42/42 head-greps absolute → 1 hint /
// 0 block as-is vs 30 / 16 after this strip). Strip `<cwd>/` everywhere before
// matching: the hook's cwd IS the project root, so this is exact — paths
// outside the project stay absolute and keep not firing (conservative edge).
// split/join, not regex: cwd may contain regex metacharacters.
function normalizeCommandPaths(cmd, cwd) {
  if (!cmd || typeof cmd !== 'string') return cmd;
  if (!cwd || typeof cwd !== 'string' || cwd === '/') return cmd;
  return cmd.split(cwd.endsWith('/') ? cwd : cwd + '/').join('');
}

// v0.48 — subdir-cwd fix; v0.49 — extracted to project-root.js so the read
// hook shares it. Re-exported below for test/back-compat.
const { resolveProjectRoot } = require('./project-root');

// v0.48 — companion to resolveProjectRoot: when the shell sits in a subdir,
// bare relative path args (`app --include=*.py` from backend/) are
// subdir-relative and never match the root-relative SRC_PATH prefixes. Rebase
// each candidate token onto the project root; a token only counts as a path
// when the rebased form EXISTS under the root — quoted patterns, flags,
// operators, absolute and traversal tokens are never touched. Existence is the
// workhorse gate: it keeps unquoted pattern words from masquerading as paths
// (the exact shape that would re-create the answered:false glob failure).
function rebaseRelativePaths(cmd, relPrefix, rootDir, exists = fs.existsSync) {
  if (!cmd || typeof cmd !== 'string' || !relPrefix || !rootDir) return cmd;
  // SEC-05 (audit 2026-08-29): one `exists()` syscall per surviving token, and
  // this runs BEFORE every length gate in the file — `shouldHint`'s 1000-char
  // sanity check (:159) and the 2000-char ones on the sed/tail extractors are all
  // downstream of it, so the guard sat below the thing it was guarding. Measured
  // with a counting stub: 100k tokens is 100,001 probes, 2.2 s of real
  // `fs.existsSync` on this box, paid inside a BLOCKING PreToolUse hook.
  //
  // The bound is the loosest one the file already uses, so nothing that any
  // downstream gate would still have processed changes behavior: a command this
  // long is past the sed/tail extractors' limit and twice past `shouldHint`'s.
  // Placed here rather than at the two call sites because both of them
  // (`pre-grep-guide` runMain, `post-grep-inject` runMain) need it, and a guard
  // that lives in the callers is one refactor away from being dropped.
  if (cmd.length > 2000) return cmd;
  const prefix = relPrefix.split(path.sep).join('/');
  // Shell sits outside any known source dir (docs/, target/, …) — don't guess.
  if (!SRC_PATH_TOKEN.test(prefix + '/')) return cmd;
  let verbSeen = false;
  return cmd.split(/(\s+)/).map((tok) => {
    if (!tok || /^\s+$/.test(tok)) return tok;
    if (!verbSeen) {
      if (/^(?:env|[A-Za-z_][A-Za-z0-9_]*=\S*)$/.test(tok)) return tok;
      verbSeen = true; // the verb itself (grep/rg/ag) — never a path
      return tok;
    }
    if (/^["']/.test(tok)) return tok;          // quoted → pattern
    if (tok.startsWith('-')) return tok;         // flag
    if (tok.startsWith('/')) return tok;         // absolute (foreign — root strip already ran)
    if (tok.includes('..')) return tok;          // traversal
    if (/[|;&<>=\\$`'"]/.test(tok)) return tok;  // operators / redirects / assignments / escapes
    const candidate = prefix + '/' + tok;
    // Probe existence on the glob-truncated form: `app/…/llm_engine/*.py`
    // must still rebase (its dir exists) or the deny-answer would run a
    // subdir-relative path from the root and fail (answered:false again).
    const probe = sanitizeSearchPath(candidate);
    try {
      if (!probe || !exists(path.join(rootDir, probe))) return tok;
    } catch { return tok; }
    return candidate;
  }).join('');
}

// v0.48 — bypass detection on the RAW command. (The deny copy stopped teaching
// the escape in v0.49, but models that already know it — or learned it from a
// session summary — must stay visible to the funnel.)
function commandHasBypass(cmd) {
  return typeof cmd === 'string' && /(?:^|\s)CODE_GRAPH_NO_BLOCK_GREP=1(?:\s|$)/.test(cmd);
}

// v0.49 — `sed -n X,Yp file.py` is a Read the Read hook can't see; the
// 2026-06-12 night used it heavily for structure exploration (four sed-range
// reads of stock_picker/ in 3 min). Extract targets so they count toward the
// shared read-fanout state.
const SED_RANGE = /(?:^|[|;&]\s*)sed\s+-n\s+(?:['"]\d+,\d+p['"]|\d+,\d+p)\s+("[^"]+"|'[^']+'|[^\s;|&]+)/g;

function extractSedReadTargets(cmd) {
  if (!cmd || typeof cmd !== 'string' || cmd.length > 2000) return [];
  const out = [];
  for (const m of cmd.matchAll(SED_RANGE)) {
    const tok = m[1].replace(/^["']|["']$/g, '');
    if (tok && !out.includes(tok)) out.push(tok);
  }
  return out;
}

// v0.50 — a compound command (`grep …; sed -n 1,60p f` / `grep … && wc`) is
// denied WHOLE, but the answer covers only the grep. The 2026-06-13 mem-project
// deny swallowed a `; sed` read while the copy said "use these results directly
// instead of re-running" — the tail's intent was silently dropped. Extract the
// first top-level `;`/`&&` tail (quote-aware) so the deny can flag it for
// re-issue. `||` tails are skipped: the answer delivered hits, so the on-failure
// branch would not have run anyway. Pipes/redirects are the same pipeline.
function extractUnansweredTail(cmd) {
  if (!cmd || typeof cmd !== 'string' || cmd.length > 2000) return null;
  let quote = null;
  for (let i = 0; i < cmd.length; i++) {
    const c = cmd[i];
    if (quote) {
      // v0.96 — same POSIX escape rule the rest of the quote-parser family uses
      // (firstShellClause / splitTopLevelSegments): inside DOUBLE quotes `\"` does
      // not close, so a `;`/`&&` inside a `grep "a\";b" …` pattern stays literal
      // and the re-issue NOTE isn't garbled by splitting mid-pattern.
      if (quote === '"' && c === '\\' && i + 1 < cmd.length) { i++; continue; }
      if (c === quote) quote = null;
      continue;
    }
    if (c === '\\') { i++; continue; }  // outside quotes: escapes the next char (see firstShellClause)
    if (c === '"' || c === "'") { quote = c; continue; }
    if (c === ';' || (c === '&' && cmd[i + 1] === '&')) {
      const tail = cmd.slice(i + (c === ';' ? 1 : 2)).trim();
      return tail || null;
    }
  }
  return null;
}

// v0.144 — the compound-tail apparatus that lived here is GONE, together with
// the deny it apologised for. It was a head-line marker plus a closing NOTE
// telling the model to re-issue the `; sed …` the deny had just cancelled, and
// the honest version of that message is not to cancel it: `classifyDeny` now
// refuses to deny a command with a top-level tail at all, so no deny reaching
// these builders can carry one. `extractUnansweredTail` survives as that gate.

// v0.47.0 — pull the first source-tree path token out of the denied command so
// the inline answer can scope its search the same way the raw grep would have.
function extractSearchPath(cmd) {
  if (!cmd || typeof cmd !== 'string') return undefined;
  // v0.96 — scope to the grep's own clause so the answer is never scoped to a
  // path in a non-grep tail (the file the user actually grepped is the only one
  // the "already ran for you" answer may claim to have searched).
  // D#73 — when the grammar proves a bare/`./` source operand, it is the scope.
  // It must win over the text scan below, which takes the first path-shaped
  // token and so can take a PATTERN (`grep -rn "./src/x" tests`) or cut a
  // quoted operand at its space (pre-ship review round 2 B1, B2). The grammar
  // rejects `..`, so no traversal reaches this return.
  const bare = bareSourceTarget(firstShellClause(cmd));
  if (bare) return bare;
  for (const raw of firstShellClause(cmd).split(/\s+/)) {
    const token = raw.replace(/^["']|["']$/g, '');
    if (!token || token.startsWith('-')) continue;
    if (token.includes('..')) return undefined; // traversal — don't scope, don't guess
    if (SRC_PATH_TOKEN.test(token)) return token;
  }
  return undefined;
}

// v0.70 — count the explicit file/dir path arguments a grep names (excluding flags and
// the quoted search pattern). The deny's inline answer scopes to ONE path
// (extractSearchPath returns only the first source-prefixed token), so a grep naming ≥2
// paths gets an answer covering only the first — an incomplete substitute that rationally
// drives CODE_GRAPH_NO_BLOCK_GREP bypass (2026-06-23: the dominant observed bypass was a
// multi-file named grep whose deny silently dropped the other files). classifyBlock uses
// this to downgrade those denies to a hint (which still nudges) so the complete grep runs.
function countNamedPaths(cmd, patterns) {
  if (!cmd || typeof cmd !== 'string') return 0;
  const pats = new Set(patterns || []);
  // Only the grep's OWN path args count. firstShellClause stops at the first
  // top-level separator so a path in a compound tail (`grep X src/a.py | sed …
  // src/b.py`) is NOT mistaken for a second grep target — that would wrongly
  // downgrade a complete single-file grep to a hint. (v0.96 — was an inline scan;
  // now shares the ONE clause definition with shouldHint/extractSearchPath.)
  const seg = firstShellClause(cmd).replace(VERB_STRIP, '');
  let n = 0;
  for (const raw of seg.split(/\s+/)) {
    const tok = raw.replace(/^["']|["']$/g, '');
    if (!tok || tok.startsWith('-')) continue;     // a flag
    if (pats.has(tok)) continue;                    // the search pattern, not a path
    if (tok.includes('/') || /\.[A-Za-z0-9]{1,6}$/.test(tok)) n++;  // dir-sep or file extension
  }
  // D#73 — the grammar's bare-dir operand is a named path too, counted like
  // its `src/` spelling (whose loop pass counts it). The grammar admits one
  // path operand, so this adds at most one.
  const bare = bareSourceTarget(firstShellClause(cmd));
  if (bare && !bare.includes('/') && !/\.[A-Za-z0-9]{1,6}$/.test(bare)) n++;
  return n;
}

// v0.47.0 — the pattern that justified the block: first identifier-like one.
function pickBlockPattern(cmd) {
  return extractPatterns(cmd).find(p => IDENTIFIER_LIKE.test(p));
}

// Compound-grep PostToolUse splitter. Split a command into top-level segments on
// `&&`, `||`, `;`, newline, and shell `for … in` / `do` / `done` control-word
// boundaries — but NOT on a single `|`: a `cargo test | grep X` is an OUTPUT
// FILTER (its head stays `cargo`, so it is excluded from folding), exactly as
// PIPE_INTO_GREP treats it in the PreToolUse path. Quote-aware: separators
// inside single/double quotes are literal command text, never split points.
// Returns trimmed, non-empty segments. Shared by post-grep-inject so the
// PostToolUse path reuses this splitter instead of copying it.
function splitTopLevelSegments(cmd) {
  if (!cmd || typeof cmd !== 'string') return [];
  const segs = [];
  let cur = '';
  let quote = null;
  for (let i = 0; i < cmd.length; i++) {
    const c = cmd[i];
    if (quote) {
      cur += c;
      // Inside DOUBLE quotes a backslash escapes the next char, so `\"` does NOT
      // close the quote (POSIX). Single quotes do no escaping — `\` is literal
      // and `'` always closes — so this only applies to `"`. Without it,
      // `echo "x\" && grep \"Y\" src/"` (one literal echo arg) mis-closes at
      // `\"`, splits on `&&`, and yields a phantom foldable grep segment.
      if (quote === '"' && c === '\\' && i + 1 < cmd.length) {
        cur += cmd[i + 1];
        i++;
        continue;
      }
      if (c === quote) quote = null;
      continue;
    }
    // Outside quotes: escapes the next char, kept verbatim (see firstShellClause)
    // — except a newline, where the pair is a line continuation and the shell
    // removes both. Keeping them left `&& \⏎ grep …` a segment that starts
    // with `\`, which GREP_HEAD rejects (pre-ship review round 2 F1).
    if (c === '\\' && i + 1 < cmd.length) {
      if (cmd[i + 1] !== '\n') cur += c + cmd[i + 1];
      i++;
      continue;
    }
    if (c === '"' || c === "'") { quote = c; cur += c; continue; }
    // `&&` and `||` (a single `&`/`|` is NOT a split — `|` is an output-filter
    // pipe, lone `&` is background and rare in tool calls).
    if ((c === '&' && cmd[i + 1] === '&') || (c === '|' && cmd[i + 1] === '|')) {
      segs.push(cur); cur = ''; i++; continue;
    }
    if (c === ';' || c === '\n') { segs.push(cur); cur = ''; continue; }
    cur += c;
  }
  segs.push(cur);
  // Split out `for … in` / `do` / `done` control words as their own boundaries
  // so a loop body grep is isolated (the head of `for s in …; do grep …` is the
  // `for` keyword, which would otherwise mask the grep). Quote-safety already
  // handled above — these run per already-split segment on whitespace-delimited
  // control words only.
  const out = [];
  const CTRL = /(?:^|\s)(for\s+\S+\s+in\b|do\b|done\b|then\b|fi\b)(?=\s|$)/g;
  for (const raw of segs) {
    let last = 0;
    let m;
    CTRL.lastIndex = 0;
    let pushed = false;
    while ((m = CTRL.exec(raw)) !== null) {
      const before = raw.slice(last, m.index);
      if (before.trim()) out.push(before);
      last = CTRL.lastIndex;
      pushed = true;
    }
    if (pushed) {
      const tail = raw.slice(last);
      if (tail.trim()) out.push(tail);
    } else {
      out.push(raw);
    }
  }
  return out.map(s => s.trim()).filter(Boolean);
}

// One implementation of the cooldown quartet, in tmp-dir.js (ARC-02/ARC-06);
// the `bash` prefix is what keeps this hook's flags distinct from
// post-grep-inject's.
const { commandHash, isOnCooldown, markCooldown } = makeCooldown('bash');

function buildHint() {
  // Terse, no banner spam. Single message budget ~600 bytes.
  return [
    '[code-graph] Raw `grep`/`rg` on indexed source — consider AST-aware equivalents:',
    '  • code-graph-mcp grep "<pat>" [paths...]      # grep + containing fn/module per hit (-F literal, -i, -w, -l, -C N)',
    '  • code-graph-mcp ast-search "<pat>" --type fn # filter by type/returns/params',
    '  • code-graph-mcp callgraph SYMBOL             # callers + callees, repo-wide',
    '  • code-graph-mcp show SYMBOL                  # one symbol: signature + source',
    'Repo-wide index (LSP only sees open files). Skip this hint if you specifically need raw-text regex.',
  ].join('\n');
}

function buildBlockReason() {
  // Shown to Claude via PreToolUse `decision: block` reason. Must give a
  // concrete alternate command Claude can re-issue without further thinking.
  // v0.49 — NO escape-hatch line anywhere in deny copy: the daagu 2026-06-12
  // night proved even the "THIS command only" scoping reads as a teachable
  // permanent prefix (adopted in 8s, reused 11×, incl. on the exact identifier
  // searches this hook targets). The env opt-out stays documented in README.
  return [
    '[code-graph] Raw `grep -rn` on indexed source — denied by code-graph hook.',
    'Use the AST-aware equivalent (returns containing fn/module per hit, repo-wide):',
    '  code-graph-mcp grep "<pattern>" [paths...]      # AST context per hit; -F literal, -i, -w, -l, -C N, --max-count 0',
    '  code-graph-mcp ast-search "<pattern>" --type fn # filter by node type',
    '  code-graph-mcp callgraph SYMBOL                 # callers + callees',
  ].join('\n');
}

// Answered interceptions are REWRITES, not denies. Until this release the hook
// ran the cg equivalent itself and denied the grep with the output in the
// reason. It worked — the model used the answer — but Claude Code renders every
// deny as a failed tool call, so each intercepted search printed a red `Error`
// block over a perfectly good result. Now the grep is replaced (PreToolUse
// `updatedInput`) by the command whose output it would have embedded, and the
// call runs as an ordinary success. The hook still runs the answer first: it is
// what tells an answerable grep (rewrite) from a dialect miss or a broken binary
// (let the raw grep run), and a rewrite must never turn into an empty result.
//
// v0.48/v0.63 copy rules still hold for the context line: no escape-hatch
// advertisement (one deny once taught a 14-grep bypass prefix) and no forced
// restatement of which hit the model will use.

// The command the grep is rewritten into. One argv per cg call (several for a
// multi-symbol `show`), each run with CODE_GRAPH_INTERNAL=1 so the CLI's `use`
// record does not count a delivered answer as a model-initiated conversion —
// per command, because a `;`-joined list would scope a bare prefix to the first
// one only, and `export` would leak into the persistent shell. Paths in the argv
// are root-relative, so a shell sitting in a subdirectory runs them from the
// root in a subshell, leaving its own cwd alone.
//
// `invocation` is the binary that ANSWERED, by quoted absolute path — runMain
// passes it. Pre-ship review of the first cut: naming it `code-graph-mcp` let
// the Bash shell resolve a different, older copy (exit 2 on `-g`, a red error
// again) or a non-executable match (exit 127), bypassing the version gate
// `findBinary` exists for. The bare default is for the printed copy only.
function buildRewriteCommand(argvList, { invocation = 'code-graph-mcp', root, shellCwd } = {}) {
  const body = argvList
    .map((args) => 'CODE_GRAPH_INTERNAL=1 ' + formatCgCommand(args, invocation))
    .join('; echo; ');
  if (!root || !shellCwd || path.resolve(shellCwd) === path.resolve(root)) return body;
  return '(cd ' + shellQuoteArg(root) + ' || exit 1; ' + body + ')';
}

// What the model is told alongside the rewritten call's output. `cmdShown` is
// rendered from the SAME argv the rewrite runs (no env prefix, bare name — the
// form a reader re-runs).
function buildRewriteContext(mode, cmdShown) {
  const lines = mode === 'show'
    ? ['[code-graph] Raw grep for symbol definitions on indexed source was rewritten to `code-graph-mcp show` — this call\'s output is the definitions from the AST index:',
      `$ ${cmdShown}`,
      'Use these directly instead of re-running the search.']
    : ['[code-graph] Raw `grep` on indexed source was rewritten to its AST-aware equivalent — this call\'s output comes from:',
      `$ ${cmdShown}`,
      'Each hit shows its containing fn/module — use these results directly instead of re-running the search.'];
  return lines.join('\n');
}

// A rewrite REPLACES the whole command and reports success, so it is only
// honest when the cg call does everything the command would have done. Three
// rounds of pre-ship review each found a way past the previous gate: a
// side-effecting pipe stage, a command after `|| …` on the next line, flags a
// denylist did not name, then a redirect in a flag's value slot, filters whose
// hit count did not survive cg's output format, and globs cg reads differently
// from the shell. So the accepted shape is deliberately NARROW — a command
// outside it is not intercepted and runs as typed, which also shows no red
// block; narrowing costs interception, never the user's command:
//
//   (grep | rg | ag | git grep) ARG… [2>&1 | 2>/dev/null]
//
// (`2>/dev/null` parses, but countNamedPaths counts it as a path, so it
// reaches a rewrite only when the command names no other path.)
//
// Every ARG is a flag from the verb's allowlist (values, where a flag takes
// one, are checked like any other word) or a plain/quoted operand: exactly one
// pattern and at most one path. Not accepted: any pipe, any other redirect, a
// NAME=value or `env` prefix (RIPGREP_CONFIG_PATH, GREP_OPTIONS, LC_ALL change
// the search), a glob in a path or an unquoted pattern, a pattern starting with
// `-`, and anything the tokenizer does not know — newline, `;`, `&`, `\`, `$`,
// a backtick, parentheses, braces, `~`. Returns a plan or null.
const WORD_CHAR = /[A-Za-z0-9_.,:@%+=/*?<>&-]/;
function shellWords(s) {
  const words = [];
  let cur = null;
  const push = () => { if (cur) words.push(cur); cur = null; };
  const start = (quoted) => { if (!cur) cur = { text: '', startsQuoted: quoted, anyQuoted: false, bareSpecial: false, bareOp: false }; };
  for (let i = 0; i < s.length;) {
    const c = s[i];
    if (c === ' ' || c === '\t') { push(); i++; continue; }
    if (c === '|') { push(); words.push({ op: '|' }); i++; continue; }
    if (c === "'" || c === '"') {
      const j = s.indexOf(c, i + 1);
      if (j === -1) return null;
      const body = s.slice(i + 1, j);
      // Inside double quotes the shell still expands `$`/backticks, and a
      // backslash escapes `"` `\` `$` backtick — so the span may not be what
      // it looks like. A backslash before anything else is literal (`"a\|b"`,
      // the BRE alternation models write constantly).
      if (c === '"' && (/[$`!]|\\["\\$`]/.test(body) || body.endsWith('\\'))) return null;
      start(true);
      cur.anyQuoted = true;
      cur.text += body;
      i = j + 1;
      continue;
    }
    if (!WORD_CHAR.test(c)) return null;
    start(false);
    if ('<>&*?'.includes(c)) cur.bareSpecial = true;
    // Per character, independent of quoting elsewhere in the word: `-g'!x'>out`
    // is one word whose quoted part must not excuse the redirect after it
    // (pre-ship review round 4 M1).
    if ('<>&'.includes(c)) cur.bareOp = true;
    cur.text += c;
    i++;
  }
  push();
  return words;
}

// Flags cg honors (`-i -w -F -l -c`, extractCgFlags) or that are no-ops for it
// (recursion, line numbers, filenames, binary skipping, the regex dialect).
// Per verb, because letters differ: ag's `-n` is --norecurse and `-H` is
// --heading, rg's `-r` is --replace. Context letters (A/B/C, a count value)
// only reach the rewrite in `show` mode, which answers with the body.
const ALLOWED_SHORT = { grep: 'rRnHsIiwFlcEPABC', git: 'rnHIiwFlcEPABC', rg: 'nHiwFlcsABC', ag: 'iwlcsABC' };
const VALUE_SHORT_BY_VERB = { grep: 'ABC', git: 'ABC', rg: 'gtABC', ag: 'ABC' };
const COMMON_LONG = ['ignore-case', 'word-regexp', 'fixed-strings', 'files-with-matches', 'count', 'line-number'];
const ALLOWED_LONG = {
  grep: new Set([...COMMON_LONG, 'recursive', 'with-filename', 'extended-regexp', 'perl-regexp', 'no-messages', 'include']),
  git: new Set([...COMMON_LONG, 'extended-regexp', 'perl-regexp']),
  rg: new Set([...COMMON_LONG, 'with-filename', 'no-heading', 'glob', 'type']),
  ag: new Set(['ignore-case', 'word-regexp', 'files-with-matches', 'count', 'case-sensitive']),
};
const VALUE_LONG = new Set(['include', 'glob', 'type']);

// A flag's value word gets the same scrutiny as any other word: round 3 put
// `&`/`>` in the `-A 3&` slot and dropped a command. Counts are digits; a glob
// value is fine only quoted (unquoted, the shell would expand it first).
function valueWordOk(w, isCount) {
  if (!w || w.op) return false;
  if (isCount) return !w.anyQuoted && /^\d+$/.test(w.text);
  return !(w.bareSpecial);
}

function rewritePlan(cmd) {
  if (!cmd || typeof cmd !== 'string' || cmd.length > 1000) return null;
  const words = shellWords(cmd);
  if (!words || words.some((w) => w.op)) return null;
  let i = 0;
  let verb = !words[0].anyQuoted ? words[0].text : '';
  if (verb === 'git') {
    if (!words[1] || words[1].anyQuoted || words[1].text !== 'grep') return null;
    i = 1;
  } else if (!['grep', 'rg', 'ag'].includes(verb)) {
    return null;
  }
  i++;
  const allowed = ALLOWED_SHORT[verb];
  const valueShort = VALUE_SHORT_BY_VERB[verb];
  const operands = [];
  let caseFlag = false;
  let context = false;
  let endOfFlags = false;
  for (; i < words.length; i++) {
    const w = words[i];
    if (!w.anyQuoted && (w.text === '2>&1' || w.text === '2>/dev/null')) continue;
    if (w.startsQuoted || endOfFlags || w.text[0] !== '-' || w.text === '-') {
      operands.push(w);
      continue;
    }
    if (w.anyQuoted || w.bareSpecial) {
      // Only an ATTACHED file-filter value may be quoted or carry a glob
      // (`--include='*.rs'`, rg `-g'*.rs'`); an unquoted `<>&` is shell syntax.
      const attachedFilter = /^--(?:include|glob|type)=/.test(w.text) || (verb === 'rg' && /^-[gt]./.test(w.text));
      if (!attachedFilter || w.bareOp) return null;
      // GNU grep's --include has no `!` negation; cg's -g does (round 4 L2).
      if (/^--include=!/.test(w.text)) return null;
    }
    if (w.text === '--') { endOfFlags = true; continue; }
    if (w.text.startsWith('--')) {
      const eq = w.text.indexOf('=');
      const name = w.text.slice(2, eq === -1 ? undefined : eq);
      if (!ALLOWED_LONG[verb].has(name)) return null;
      if (VALUE_LONG.has(name) && eq === -1) {
        const v = words[++i];
        if (!valueWordOk(v, false) || (name === 'include' && v.text.startsWith('!'))) return null;
      }
      if (name === 'ignore-case' || name === 'case-sensitive') caseFlag = true;
      continue;
    }
    const letters = w.text.slice(1);
    for (let k = 0; k < letters.length; k++) {
      const ch = letters[k];
      if (valueShort.includes(ch)) {
        const isCount = 'ABC'.includes(ch);
        if (isCount) context = true;
        const attached = letters.slice(k + 1);
        if (attached) {
          if (isCount && !/^\d+$/.test(attached)) return null;
        } else if (!valueWordOk(words[++i], isCount)) {
          return null;
        }
        break;
      }
      if (!allowed.includes(ch)) return null;
      if (ch === 'i' || ch === 's') caseFlag = true;
    }
  }
  if (operands.length === 0 || operands.length > 2) return null;
  const [pattern, target] = operands;
  // The pattern: unquoted glob characters would be expanded by the shell, and
  // a leading `-` would reach cg as flags.
  if ((!pattern.anyQuoted && pattern.bareSpecial) || pattern.text.startsWith('-')) return null;
  // The path: bash expands a glob at one level with no dotfiles; cg's `-g`
  // matches recursively and includes them (round 3 M5). Not reproducible.
  if (target && (/[*?[\]{}]/.test(target.text) || target.bareSpecial)) return null;
  // A `..` segment: extractSearchPath refuses to scope it, so the answer would
  // search the whole repo (round 4 M2).
  if (target && /(?:^|\/)\.\.(?:\/|$)/.test(target.text)) return null;
  // ag is smart-case by default: an all-lowercase pattern matches any case,
  // and cg's search is case-sensitive, so the answer would find less.
  if (verb === 'ag' && !caseFlag && !/[A-Z]/.test(pattern.text)) return null;
  return { pattern: pattern.text, target: target ? target.text : undefined, context };
}

// The plan must describe the same search classifyBlock decided on. The pattern
// the answer ran is picked from quoted spans (pickBlockPattern); the grammar's
// pattern operand is the one the shell passes — `"is"'Foo'` or an unquoted
// pattern with a quoted path made them differ (round 3 L3). A context count the
// raw-text CONTEXT_FLAG check missed (`-A"3"`) sent a context grep to grep mode,
// which drops the context (L2). `show` answers with bodies, so a grep asking
// for a file list or counts (`-l`/`-c`) is not its equivalent (M6).
function rewriteMatchesBlock(plan, block, cmd, rawPattern) {
  if (!plan || !block) return false;
  if (plan.pattern !== rawPattern) return false;
  // Same for the path: extractSearchPath takes the first src-prefixed token,
  // which can be a quoted PATTERN (`grep -rn "src/foo_mod" tmp/` searched
  // src/foo_mod — round 4 H1, inherited from the deny's answer).
  const norm = (p) => (p === undefined ? undefined : p.replace(/^\.\//, '').replace(/\/+$/, ''));
  if (norm(plan.target) !== norm(extractSearchPath(cmd))) return false;
  if (block.mode === 'grep' && plan.context) return false;
  if (block.mode === 'show') {
    const f = cgFlagSet(extractCgFlags(cmd));
    if (f.has('-l') || f.has('-c')) return false;
    // show answers at most three symbols; a fourth would silently vanish.
    if (extractDeclSymbols(extractPatterns(cmd)).length > 3) return false;
  }
  return true;
}

// v0.49 — plain `grep` speaks BRE: alternation/grouping arrive escaped
// (`a\|b`, `\(x\)`) and 0-hit against cg grep's rust-regex dialect, wasting
// the answer on the ALLOW fallthrough (2026-06-12: both answered:false denies
// were dialect/path-shape misses). Unescape for plain grep only — rg/ag and
// grep -E/-P are already extended.
function translateBreToRg(cmd, pattern) {
  if (typeof pattern !== 'string' || !pattern) return pattern;
  const verb = (cmd.match(GREP_HEAD) || [])[1];
  // git grep speaks BRE like plain grep; rg/ag are already extended-regex.
  if (!verb || !/grep$/.test(verb)) return pattern;
  // The grep's OWN clause, not the whole command. This was the one flag check
  // in this module that v0.96 did not clause-scope, and round 4 of pre-ship
  // review found what it costs: `grep -rln "a\|b" src/ | xargs -P4 wc -l` read
  // the tail's `-P4` as "this grep speaks Perl regex", so the pattern was left
  // escaped. On its own that is a wrong dialect decision; it also desynchronised
  // the two hooks, because post-grep-inject passes a SEGMENT here while the deny
  // path passes the whole command — the same pattern then filed under two
  // spellings and the funnel scored a verbatim re-grep as neutral.
  const clause = firstShellClause(cmd);
  if (/(?:^|\s)-[a-zA-Z]*[EP][a-zA-Z]*(?:\s|=|\d|$)|--(?:extended-regexp|perl-regexp)\b/.test(clause)) {
    return pattern;
  }
  return pattern.replace(/\\([|(){}+?])/g, '$1');
}

// v0.47.0 — cg grep found nothing. Regex-dialect differences (BRE `\|` vs
// ripgrep) mean 0 hits is NOT proof of absence, so denying here could mislead.
// Let the raw grep through with an honest one-liner.
function buildNoHitsFyi(pattern) {
  return `[code-graph] FYI: \`code-graph-mcp grep "${pattern}"\` found no matches — raw grep proceeding. (Regex-metachar patterns: \`code-graph-mcp grep -F\` searches literally.)`;
}

// v0.92 — cg was allowed to answer but the binary ran-and-failed ('unavailable')
// or could not be found ('no-binary'). Like buildNoHitsFyi this is a breadcrumb
// only (PreToolUse exit-0 stdout → debug log, never the model); the operative
// effect at the call site is the ALLOW (no deny emitted) so the raw grep runs
// intact instead of a static deny that would hand the model nothing.
function buildUnavailableFyi(pattern, status, reason) {
  // Three causes, not two. A hook whose budget was already spent at startup
  // (cold node on a loaded machine — see the reserve note in cg-answer.js)
  // deliberately runs no children at all; calling that "ran but failed" blames
  // the binary for something it was never asked to do (audit 2026-09-05 NEW-08).
  const why = status === 'no-binary' ? 'binary not found'
    : reason === 'budget' ? 'no time left in the hook budget'
      : 'ran but failed';
  return `[code-graph] FYI: \`code-graph-mcp grep "${pattern}"\` unavailable (${why}) — raw grep proceeding.`;
}

// --- Main execution (only when run directly) ---

// Kill switch: matches user-prompt-context.js convention. =1 forces silence
// even when the rest of the hook tier is noisy. Default (unset) is noisy here
// — this hook only fires on raw grep against the source tree, which is the
// exact comfort-zone leak it was designed to catch.
function isSilenced(env = process.env) {
  return env.CODE_GRAPH_QUIET_HOOKS === '1';
}

// v0.32.0 — independent of QUIET_HOOKS. =1 downgrades block tier to hint
// (legacy v0.25.0–v0.31 behavior). Useful when raw-text scan is intentional
// but the user still wants the hint for future commands.
function isBlockDisabled(env = process.env) {
  return env.CODE_GRAPH_NO_BLOCK_GREP === '1';
}

// v0.47.0 — opt-out for the inline-answer tier only: =1 restores the v0.46
// static deny (no CLI run inside the hook). Independent of NO_BLOCK_GREP.
function isAnswerDisabled(env = process.env) {
  return env.CODE_GRAPH_NO_ANSWER_IN_DENY === '1';
}

function runMain() {
  if (isSilenced()) return;
  // v0.48 — process.cwd() follows the persistent shell; resolve the project
  // root by walking up so `cd backend/` no longer darkens the whole session.
  const shellCwd = process.cwd();
  const root = resolveProjectRoot(shellCwd);
  if (root === null) return;  // no index anywhere up to $HOME — no hint

  let input;
  try {
    // fd 0, not '/dev/stdin': the path form open(2)s the symlink target, which
    // fails with ENXIO when stdin is a socketpair (e.g. spawnSync {input}).
    // Reading the fd directly works for pipes, sockets, and files alike.
    input = JSON.parse(fs.readFileSync(0, 'utf8'));
  } catch { return; }

  const rawCmd = (input.tool_input && input.tool_input.command) || '';

  // v0.49 — sed-range reads count toward the read-fanout state (the Read hook
  // never sees Bash-side file reads). A fired fanout hint already delivered an
  // overview — skip grep hinting for this command to avoid double output.
  const sedTargets = extractSedReadTargets(rawCmd);
  if (sedTargets.length > 0) {
    const readGuide = require('./pre-read-guide');
    let fanoutFired = false;
    for (const t of sedTargets) {
      if (!readGuide.isSourceFile(t)) continue;
      const abs = path.isAbsolute(t) ? t : path.resolve(shellCwd, t);
      if (readGuide.trackReadAndMaybeHint(root, path.relative(root, abs))) {
        fanoutFired = true;
      }
    }
    if (fanoutFired) return;
  }

  // v0.47.1 — match against the root-stripped form so absolute paths under the
  // project root behave exactly like their relative spelling. v0.48 — then
  // rebase bare subdir-relative tokens onto the root. Cooldown stays keyed on
  // the raw command (what Claude actually sent).
  let cmd = normalizeCommandPaths(rawCmd, root);
  const relPrefix = path.relative(root, shellCwd);
  if (relPrefix) cmd = rebaseRelativePaths(cmd, relPrefix, root);
  // D#73 — from a subdirectory, a bare dir the rebase left alone is `<cwd>/src`,
  // which the root-relative answer would not search (pre-ship review round 1 H1).
  if (relPrefix && bareSourceTarget(firstShellClause(cmd))) return;
  if (!shouldHint(cmd)) return;

  // v0.64 — fingerprint the grep's pattern once, shared by the emit points below.
  // The funnel (aggregate_recommendations_jsonl) uses it to tell a verbatim re-grep
  // of an answered deny (inline answer ignored → fall-through) from a deeper
  // drill-down. undefined when there's no identifier-like pattern (unquoted / prose
  // grep) → omitted from the event, so the funnel stays back-compatible.
  // `-F` asks for a LITERAL pattern, and the BRE→rust-regex unescape must not
  // also run on it: `grep -F 'x\|y'` searches for the five characters `x\|y`,
  // while the unescaped `x|y` is a different string (GNU grep on a file holding
  // the literal: `grep -Fc 'x\|y'` is 1, `grep -Fc 'x|y'` is 0). Before this
  // release the flag was dropped entirely, so the answer was merely broader;
  // forwarding `-F` without this guard would make it search the wrong text.
  const cgFlags = extractCgFlags(cmd);
  const rawGrepPattern = pickBlockPattern(cmd);
  const grepPattern = cgFlagSet(cgFlags).has('-F')
    ? rawGrepPattern
    : translateBreToRg(cmd, rawGrepPattern);

  // v0.48 — deliberate escape: record it (funnel visibility) and stay silent.
  // Before GREP_HEAD accepted bare KEY=VALUE prefixes these were invisible.
  if (commandHasBypass(rawCmd)) {
    recordRecommendation(root, { hook: 'grep', action: 'bypass' });
    return;
  }

  if (isOnCooldown(rawCmd, Date.now(), 60000, root)) {
    // Outcome proxy: a source grep re-issued within the cooldown window runs
    // silently (no deny/hint). Record it so `stats` sees the model's grep
    // fan-out — especially a re-grep right after cg answered the same query.
    recordRecommendation(root, { hook: 'grep', action: 'observe', ...(grepPattern ? { pattern: grepPattern } : {}) });
    return;
  }

  markCooldown(rawCmd, root);

  // classifyDeny, not classifyBlock: a command with a top-level `;`/`&&` tail is
  // never denied, because cancelling it would cancel the tail too. Those run and
  // are answered by post-grep-inject instead — which is also where they are
  // RECORDED.
  //
  // Round 1 added an `observe compound:true` row here. Round 2 showed no counter
  // reads the field, and round 3 showed the row double-counts: a head-grep
  // compound whose grep hits writes this row AND post-grep-inject's redundancy
  // observe, so one Bash call became two entries in a counter `usage.rs`
  // documents as the model's raw search fan-out. The post-side rows carry more
  // (they say what happened to the answer), so this side stays silent — exactly
  // as it did before the compound change.
  //
  // One configuration where that leaves NO trace, named because round 4 caught
  // the claim overstated: under `CODE_GRAPH_NO_INJECT=1` the post side does not
  // run either, so a first-run compound is invisible to the funnel. A repeat
  // within 60 s still lands in the cooldown `observe` branch above. A kill
  // switch silencing the hook that carries the telemetry is coherent; silently
  // claiming coverage it does not have is not.
  const block = isBlockDisabled() ? null : classifyDeny(cmd);
  // The rewrite replaces the WHOLE command. One it cannot reproduce runs as
  // typed (see rewritePlan) — decided before the answer is spent on it. The
  // opt-in static deny keeps its own, older scope.
  const plan = block && !isAnswerDisabled() ? rewritePlan(cmd) : null;
  if (block && !isAnswerDisabled() && !rewriteMatchesBlock(plan, block, cmd, rawGrepPattern)) return;
  if (block) {
    // v0.47.0 — run the AST-aware equivalent inside the hook and embed the
    // results in the deny reason ("answer in the deny"). Degrades to the
    // v0.46 static deny on any failure; downgrades to allow+FYI on 0 hits
    // (regex-dialect differences mean 0 hits ≠ proof of absence).
    // v0.49 — intent-aware: declaration+context greps get `show` bodies,
    // falling back to the grep answer, then the static deny.
    let answer = { status: 'unavailable' };
    const pattern = grepPattern; // computed once above; reused for the answer + deny fingerprint
    // The path is passed RAW (globs and all). buildGrepArgs splits `tests/*.mjs`
    // into scope `tests` + `-g '*.mjs'`, which is both what keeps a literal glob
    // out of argv (the exit-1 shape sanitizeSearchPath was added for) and what
    // stops the answer from quietly searching files the user excluded.
    const searchPath = extractSearchPath(cmd);
    const flags = cgFlags;  // computed once above, beside the -F pattern guard
    // ONE argv, used to run the child AND to render the command the deny prints.
    const args = buildGrepArgs({ pattern, searchPath, flags });
    const answeredMode = block.mode;
    if (!isAnswerDisabled()) {
      if (block.mode === 'show') {
        // No fallback to a grep answer: a context grep (`-A5`) asked for the
        // body, and the grep rewrite would drop the context silently (pre-ship
        // review round 2). A show miss lets the raw grep run.
        answer = runShowAnswer({ cwd: root, symbols: block.symbols });
      } else if (pattern) {
        answer = runGrepAnswer({ cwd: root, pattern, searchPath, flags });
      }
    }

    // v0.92 — cg was allowed to answer but couldn't deliver hits: 'no-hits'
    // (regex-dialect miss ≠ proof of absence), 'unavailable' (binary ran but
    // failed/timed out), or 'no-binary' (binary missing). In every case a static
    // deny hands the model NOTHING — pure friction that teaches the
    // CODE_GRAPH_NO_BLOCK_GREP bypass (ubuntu-sec 2026-07 dogfood: the only 2
    // non-converting denies were `unavailable`, one a `def render` compound cmd
    // that then half-ran — grep blocked, `; python3 …` tail dropped, no result).
    // ALLOW the raw grep so the command runs intact; record the fallthrough
    // reason so the funnel still tells no-hits / unavailable / no-binary apart.
    // Exception: CODE_GRAPH_NO_ANSWER_IN_DENY=1 means the user opted into the
    // static deny (the answer never ran → status stays the default 'unavailable')
    // — that path falls through to the v0.46 static deny below.
    if (answer.status !== 'hits' && !isAnswerDisabled()) {
      recordRecommendation(root, {
        hook: 'grep', action: 'hint', fallthrough: answer.status,
        // So the funnel can tell a starved hook from a broken binary; both
        // arrive as `unavailable` and only one of them means anything is wrong.
        ...(answer.reason ? { fallthrough_reason: answer.reason } : {}),
      });
      process.stdout.write(
        (answer.status === 'no-hits'
          ? buildNoHitsFyi(pattern)
          : buildUnavailableFyi(pattern, answer.status, answer.reason)) + '\n');
      return;
    }

    const answered = answer.status === 'hits';
    recordRecommendation(root, {
      // Still `deny` in the funnel: the event it counts — a raw grep intercepted
      // and answered in place — is unchanged, and the Rust aggregator keys on
      // it. `delivery` says HOW the answer arrived.
      hook: 'grep', action: 'deny', answered,
      // pattern fingerprints the denied search so the funnel can score a verbatim
      // re-grep of it (the inline answer was ignored) as fall-through, not a win.
      ...(pattern ? { pattern } : {}),
      // mode segments which answer type converts (show=bodies, grep=hits).
      ...(answered ? { mode: answeredMode, delivery: 'rewrite' } : {}),
      // reason segments WHY an unanswered deny fell back to the static copy:
      // 'no-binary' (flagship answer-in-deny dark — binary missing) vs
      // 'unavailable' (binary ran but failed/timed out). Without this the two
      // are indistinguishable in the funnel ("broken" looks like "no hits").
      ...(answered ? {} : { reason: answer.status }),
    });

    if (!answered) {
      // CODE_GRAPH_NO_ANSWER_IN_DENY=1 — the user opted into the v0.46 static
      // deny, so nothing ran and there is nothing to rewrite into. Current CC
      // schema (`hookSpecificOutput.permissionDecision`): the legacy
      // `{decision:"block"}` was ignored (verified 2026-05-24). Exit 0 — a
      // routing decision, not a hook failure.
      process.stdout.write(JSON.stringify({
        hookSpecificOutput: {
          hookEventName: 'PreToolUse',
          permissionDecision: 'deny',
          permissionDecisionReason: buildBlockReason(),
        },
      }) + '\n');
      return;
    }

    // Re-run exactly what answered: the resolved symbols for `show`, the same
    // argv for `grep`.
    // `-m 0`: cg caps matches at 100 per file by default and says so only on
    // stderr; the grep being replaced has no cap (round 3 M4). Only here, not
    // in the in-hook answer, which is a has-hits probe.
    const argvList = answeredMode === 'show'
      ? answer.symbols.map((sym) => ['show', sym])
      : [[...args.slice(0, 1), '-m', '0', ...args.slice(1)]];
    const command = buildRewriteCommand(argvList, {
      invocation: shellQuoteArg(resolveAnswerBinary({})),
      root,
      shellCwd,
    });
    const cmdShown = argvList.map((a) => formatCgCommand(a)).join('; ');
    // Not carried over: a model-set `dangerouslyDisableSandbox`. The call this
    // hook auto-allows is its own read-only cg command, and it needs no escape
    // from a sandbox the model's grep would have run inside.
    const { dangerouslyDisableSandbox, ...toolInput } = input.tool_input || {};
    process.stdout.write(emitPreToolRewrite({
      updatedInput: { ...toolInput, command },
      reason: '[code-graph] raw grep → AST-aware equivalent',
      context: buildRewriteContext(answeredMode, cmdShown),
    }) + '\n');
    return;
  }

  // Compound-grep change: the dark-stdout HINT fallthrough was DELETED. A grep
  // that passes shouldHint but NOT classifyBlock used to record action:'hint'
  // and write buildHint() to stdout — but PreToolUse exit-0 plain stdout goes to
  // the DEBUG LOG ONLY and never reaches the model (CC docs v2026-06). It was
  // pure noise. These hint-tier greps (unanswerable-flag / marker / multi-path)
  // are exactly the cases cg cannot fold, so silence is correct: the model's own
  // grep runs unimpeded. classifyBlock-positive compound greps are now picked up
  // permission-neutrally by the PostToolUse post-grep-inject hook.
}

if (require.main === module) {
  runMain();
}

module.exports = {
  shouldHint,
  shouldBlock,
  classifyBlock,         // v0.49 — intent-aware block tiers
  classifyDeny,          // v0.144 — the deny gate: block-tier AND nothing discarded
  extractCgFlags,        // v0.144 — the grep's flags in cg's spelling
  cgFlagSet,             // v0.144 — the boolean flags of that result, without values
  splitTopLevelSegments, // compound-grep — quote-aware top-level segment splitter (PostToolUse reuse)
  firstShellClause,      // v0.96 — grep's own clause (up to first top-level separator)
  extractDeclSymbols,    // v0.49 — show-mode symbol extraction
  translateBreToRg,      // v0.49 — BRE→rust-regex dialect bridge
  buildRewriteCommand,   // rewrite — the command the grep becomes
  buildRewriteContext,   // rewrite — what the model is told about it
  shellWords,            // rewrite — the tokenizer rewritePlan's grammar reads
  rewritePlan,           // rewrite — does the whole command parse as one the rewrite reproduces
  rewriteMatchesBlock,   // rewrite — does that plan describe the search classifyBlock chose
  extractSedReadTargets, // v0.49 — sed-range reads feed the read-fanout state
  extractUnansweredTail, // v0.50 — compound-tail honesty in answered denies
  extractPatterns,    // v0.32.1 — exposed for tests
  countNamedPaths,    // v0.70 — multi-path deny→hint downgrade
  bareSourceTarget,      // D#73 — a bare source dir the rewrite grammar proves is the path operand
  isRevisionScopedGitGrep, // v0.71 — git grep --cached/treeish exclusion
  extractSearchPath,  // v0.47.0 — deny-with-answer
  normalizeCommandPaths, // v0.47.1 — abs-path matcher fix
  resolveProjectRoot,    // v0.48 — subdir-cwd dark fix
  rebaseRelativePaths,   // v0.48 — subdir-cwd dark fix
  commandHasBypass,      // v0.48 — bypass funnel visibility
  pickBlockPattern,
  buildHint,
  buildBlockReason,
  buildNoHitsFyi,
  buildUnavailableFyi,   // v0.92 — allow-on-unavailable breadcrumb
  commandHash,
  isOnCooldown,
  markCooldown,
  isSilenced,
  isBlockDisabled,
  isAnswerDisabled,
};
