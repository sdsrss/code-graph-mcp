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
const { spawnSync } = require('child_process');
const { hidden } = require('./proc-opts');
const { cgTmpDir, cwdHash, makeCooldown } = require('./tmp-dir');
const { recordRecommendation } = require('./recommendation-log');
const {
  runGrepAnswer, runShowAnswer, sanitizeSearchPath, buildGrepArgs, formatCgCommand, shellQuoteArg,
  resolveAnswerBinary,
} = require('./cg-answer');
const { emitPreToolRewrite, emitPreToolContext, MAX_INJECTED_BYTES } = require('./hook-emit');

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
// The three source-path tests below are built from SRC_PREFIXES plus the
// project's own source roots (useSourceRoots), so they are `let`.
let SRC_PATH;
// Anchored variant for whole-token matching in extractSearchPath.
let SRC_PATH_TOKEN;
// A path operand the rewrite grammar proved: a bare prefix word (`src`), or a
// `./`-rooted one with any subpath (`./src`, `./src/x/`), which SRC_PATH's
// lookbehind never matched.
let SRC_BARE_TOKEN;

// F1 (tasks/specs/grep-hook-source-roots.md) — the prefix list names
// conventional source dirs, but a Python package lives in a dir named after it
// (`networkx/`, `django/`): in the 2026-09-28 coding eval not one grep of 15
// runs reached this hook. The indexer writes the top-level dirs that hold
// indexed code to `.code-graph/source-roots.json`, and those names join the
// list. A name must be one plain shell word (no quote, space, slash, leading
// dot); past MAX_SOURCE_ROOTS the list alone applies, as it does when the file
// is missing or unreadable.
const SOURCE_ROOTS_FILE = 'source-roots.json';
const MAX_SOURCE_ROOTS = 64;
const SOURCE_ROOT_NAME = /^[A-Za-z0-9_][A-Za-z0-9_.+@-]{0,63}$/;
const PREFIX_WORDS = new Set(SRC_PREFIXES.replace('components?', 'components|component').split('|'));

function readSourceRoots(root) {
  if (!root) return [];
  let parsed;
  try {
    parsed = JSON.parse(fs.readFileSync(path.join(root, '.code-graph', SOURCE_ROOTS_FILE), 'utf8'));
  } catch { return []; }
  const roots = parsed && Array.isArray(parsed.roots) ? parsed.roots : null;
  if (!roots || roots.length > MAX_SOURCE_ROOTS) return [];
  return roots.filter((r) => typeof r === 'string' && SOURCE_ROOT_NAME.test(r) && !PREFIX_WORDS.has(r));
}

function useSourceRoots(roots) {
  const escaped = (roots || []).map((r) => r.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'));
  const alt = [SRC_PREFIXES, ...escaped].join('|');
  SRC_PATH = new RegExp(`(?:^|\\s|["'])(${alt})/`);
  SRC_PATH_TOKEN = new RegExp(`^(?:\\./)?(${alt})/`);
  SRC_BARE_TOKEN = new RegExp(`^(?:(?:${alt})|\\./(?:${alt})(?:/.*)?)$`);
}
useSourceRoots([]);

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
  // expressions and worth screening too. Whole words, as the shell passes them
  // (D#63): `"Foo"'BarBaz'` is `FooBarBaz`, not two patterns; a flag's attached
  // quoted value (`-e"Foo"`, `--include='*.rs'`) is the value. A clause the
  // shell would reject, or whose quoted words it expands (`"$MAX"`, D#64), has
  // no pattern we can read.
  const words = clauseWords(stripped);
  if (!words) return [];
  const quoted = words.filter((w) => w.quotedFrom !== -1);
  if (quoted.some((w) => w.expands)) return [];
  return quoted
    .map((w) => (!w.startsQuoted && w.text[0] === '-' ? w.text.slice(w.quotedFrom) : w.text))
    .filter(Boolean);
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

/**
 * The words of a shell clause, split and unquoted the way quotedSpans reads
 * quotes: whitespace separates words only outside quotes, and adjacent quoted
 * and bare parts join into one word (`-r"l"` is `-rl`, `"Foo"'Bar'` is
 * `FooBar`). Per word:
 *   - `startsQuoted`: the first character was a quote — an argument, never a
 *     flag, so `"-l"` searches for the string `-l`;
 *   - `quotedFrom`: where in `text` the first quoted part begins (-1 if none),
 *     so `-e"Foo"` yields the value `Foo`;
 *   - `expands`: the shell substitutes something into it — `$NAME`, `${…}`,
 *     `$(…)`, a backtick, or `$'…'`/`$"…"` quoting — outside single quotes,
 *     so `text` is not what the command received.
 * Returns null for an unterminated quote: the shell would not run it at all.
 *
 * A whitespace split read `-l` out of `grep -rn "FooBar -l x" src/` and the
 * rewrite answered with a file list (D#62).
 * @returns {{text: string, startsQuoted: boolean, quotedFrom: number, expands: boolean}[]|null}
 */
function clauseWords(s) {
  const words = [];
  if (typeof s !== 'string') return words;
  let cur = null;
  // Inside double quotes `$"` is a literal `$` before the closing quote
  // (`"FooBar$"`); bare, `$'…'` and `$"…"` are quoting forms that translate.
  const expandsAt = (k, inDouble) => s[k] === '`'
    || (s[k] === '$' && k + 1 < s.length
      && (inDouble ? /[A-Za-z0-9_{(@*#?$!-]/ : /[A-Za-z0-9_{(@*#?$!'"-]/).test(s[k + 1]));
  for (let i = 0; i < s.length; i++) {
    const c = s[i];
    if (c === ' ' || c === '\t' || c === '\n') {
      if (cur) words.push(cur);
      cur = null;
      continue;
    }
    if (!cur) cur = { text: '', startsQuoted: c === '"' || c === "'", quotedFrom: -1, expands: false };
    if (c === '\\') {
      if (i + 1 < s.length && s[i + 1] !== '\n') cur.text += s[i + 1];
      i++;
      continue;
    }
    if (c !== '"' && c !== "'") {
      if (expandsAt(i, false)) cur.expands = true;
      cur.text += c;
      continue;
    }
    if (cur.quotedFrom === -1) cur.quotedFrom = cur.text.length;
    let j = i + 1;
    for (; j < s.length && s[j] !== c; j++) {
      if (c === '"' && s[j] === '\\' && j + 1 < s.length && '"\\$`\n'.includes(s[j + 1])) {
        j++;
        if (s[j] === '\n') continue;
      } else if (c === '"' && expandsAt(j, true)) {
        cur.expands = true;
      }
      cur.text += s[j];
    }
    if (j >= s.length) return null;
    i = j;
  }
  if (cur) words.push(cur);
  return words;
}

// Declaration anchors inside a pattern (`def cascade_failure|class TaskState`)
// name the exact symbols the model wants to READ — extract them for `show`.
const DECL_SYMBOL = /(?:fn|def|class|function|struct|impl|trait)\s+([A-Za-z_][A-Za-z0-9_]*)/g;

// What a `show` definition must be for the grep's declaration keyword to match
// its line: the kind label `show` prints (src/cli/symbols.rs
// format_node_compact) AND a file of a language that spells the declaration
// with that keyword. The label alone is coarser than the keyword — `fn` is also
// a Python `def` and a TS method, and a Swift `struct` is labelled `class`
// (review of D#125, round 2). `impl` names no definition of its own.
const DECL_KEYWORD_SHAPES = {
  fn: { labels: ['fn'], exts: ['rs'] },
  def: { labels: ['fn'], exts: ['py', 'rb'] },
  function: { labels: ['fn'], exts: ['js', 'mjs', 'cjs', 'jsx', 'ts', 'tsx', 'php', 'sh', 'bash'] },
  class: {
    labels: ['class'],
    exts: ['py', 'js', 'mjs', 'cjs', 'jsx', 'ts', 'tsx', 'java', 'kt', 'cs', 'php', 'rb', 'dart',
      'cpp', 'cc', 'hpp', 'h', 'scala'],
  },
  struct: { labels: ['struct'], exts: ['rs', 'go', 'c', 'h', 'cpp', 'cc', 'hpp', 'cs'] },
  trait: { labels: ['trait', 'iface'], exts: ['rs', 'php', 'scala'] },
  impl: { labels: [], exts: [] },
};

// Symbol → the (label, extension) shapes its declaration keywords match
// (`fn foo\|def foo` → foo: Rust fn, Python/Ruby def).
function declKindsBySymbol(patterns) {
  const out = {};
  for (const p of patterns) {
    for (const m of p.matchAll(/\b(fn|def|class|function|struct|impl|trait)\s+([A-Za-z_][A-Za-z0-9_]*)/g)) {
      (out[m[2]] = out[m[2]] || []).push(DECL_KEYWORD_SHAPES[m[1]]);
    }
  }
  return out;
}

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
 * `-l` is not this grep's), words as the shell splits them (clauseWords — a
 * quoted pattern holding ` -l ` is one word), and a word that STARTS quoted is
 * an argument, not a flag — `grep "-l" src/` searches for the string `-l`.
 */
function extractCgFlags(cmd) {
  if (!cmd || typeof cmd !== 'string') return [];
  const clause = firstShellClause(cmd);
  const isRg = RG_VERB.test((clause.match(GREP_HEAD) || [])[1] || '');
  const words = clauseWords(clause.replace(VERB_STRIP, ''));
  if (!words) return [];
  const toks = words.map((w) => (w.startsQuoted ? '' : w.text));
  const found = new Set();
  const filters = [];  // [cgFlag, value] pairs, in the order the user wrote them
  // A value is read from the word, whatever its quoting: `--include '*.rs'`.
  const valueAt = (k) => (words[k] ? words[k].text : '');
  const addFilter = (flag, value) => {
    if (!value) return;
    // `-g '*.rs' -g '*.rs'` is one filter written twice; cg would honour both
    // identically, but the printed command should not look like a mistake.
    if (!filters.some(([f, v]) => f === flag && v === value)) filters.push([flag, value]);
  };
  for (let i = 0; i < toks.length; i++) {
    const tok = toks[i];
    if (!tok || tok[0] !== '-') continue;
    if (tok.startsWith('--')) {
      const eq = tok.indexOf('=');
      const name = eq === -1 ? tok : tok.slice(0, eq);
      if (CG_VALUE_FLAGS[name]) {
        addFilter(CG_VALUE_FLAGS[name], eq === -1 ? valueAt(i + 1) : tok.slice(eq + 1));
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
        addFilter(CG_VALUE_SHORT[ch], attached || valueAt(i + 1));
        // Consume the value word so the next pass cannot read it as a flag
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

// D#125 #1 — the strip above cannot tell a path operand from a PATTERN, and
// strips both: `grep -rn "<root>/Foo" src/` searches the literal text
// `<root>/Foo`, but was answered for `Foo`. Re-picking the pattern with the
// root marked was tried first and missed shapes where the mark changes which
// word is picked (`"<root>/def foo"`, `"abc_<root>/def"`: review of D#125). The
// narrow form: every shell word that holds `<root>/` must BE the search path
// the answer scopes to; anything else runs as typed.
function rootOnlyInSearchPath(cmd, cwd, target) {
  if (!cmd || typeof cmd !== 'string') return true;
  if (!cwd || typeof cwd !== 'string' || cwd === '/') return true;
  const prefix = cwd.endsWith('/') ? cwd : cwd + '/';
  if (!cmd.includes(prefix)) return true;
  // Round 2: counting words was not enough — a lone root-holding PATTERN whose
  // stripped text equals the path (`grep -rn "<root>/src/x" src/x`) passed. The
  // rewrite grammar names which word is the path operand: accept only a command
  // it reads, with the root once, in that operand, and nowhere in the pattern.
  if (cmd.split(prefix).length !== 2) return false;
  const plan = rewritePlan(firstShellClause(cmd), { isDir: () => true });
  if (!plan || plan.target === undefined || plan.pattern.includes(prefix)) return false;
  if (!plan.target.startsWith(prefix)) return false;
  const norm = (p) => p.replace(/^(?:\.\/)+/, '').replace(/\/+$/, '');
  return target !== undefined && norm(plan.target.slice(prefix.length)) === norm(target);
}

// v0.48 — subdir-cwd fix; v0.49 — extracted to project-root.js so the read
// hook shares it. Re-exported below for test/back-compat.
const { resolveProjectRoot, indexKey } = require('./project-root');

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
  // Split on whitespace outside quotes only: a quoted pattern is one token and
  // its inner words are never paths (D#63 — `"fn main utils here"` from src/
  // became `"fn main src/utils here"`).
  return splitKeepingQuotes(cmd).map((tok) => {
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

// Words and the whitespace between them, text unchanged, splitting on
// whitespace outside quotes only (the quotedSpans rules).
function splitKeepingQuotes(s) {
  const out = [];
  let cur = '';
  let quote = null;
  for (let i = 0; i < s.length; i++) {
    const c = s[i];
    if (quote) {
      cur += c;
      if (quote === '"' && c === '\\' && i + 1 < s.length) { cur += s[++i]; continue; }
      if (c === quote) quote = null;
      continue;
    }
    if (c === '\\' && i + 1 < s.length) { cur += c + s[++i]; continue; }
    if (c === '"' || c === "'") { quote = c; cur += c; continue; }
    if (/\s/.test(c)) {
      if (cur) out.push(cur);
      cur = c;
      while (i + 1 < s.length && /\s/.test(s[i + 1])) cur += s[++i];
      out.push(cur);
      cur = '';
      continue;
    }
    cur += c;
  }
  if (cur) out.push(cur);
  return out;
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

// Q1 — `sed -i` and `perl -pi` edit files the edit hooks never see. Their
// targets are logged for the Stop hook's signature check (never answered).
// Shapes: tasks/specs/edit-log-coverage.md. A word only counts as a target
// when it names an existing regular file, so a misread costs a few baseline
// records the Stop check then finds unchanged; a command the tokenizer cannot
// read exactly (`$x`, globs) is skipped, which is the state before Q1.
const MAX_IN_PLACE_TARGETS = 8;
const ENV_ASSIGNMENT = /^[A-Za-z_][A-Za-z0-9_]*=/;

/**
 * Absolute paths of the files an in-place `sed`/`perl` in `cmd` edits, at
 * most MAX_IN_PLACE_TARGETS, in command order.
 * @param {string} cmd
 * @param {string} shellCwd  where the command starts
 * @param {{isFile?: (abs:string)=>boolean, isDir?: (abs:string)=>boolean}} [opts]
 * @returns {string[]}
 */
function extractInPlaceEditTargets(cmd, shellCwd, { isFile = isRegularFile, isDir = isDirectory } = {}) {
  if (!cmd || typeof cmd !== 'string' || cmd.length > 4000 || !/\b(?:sed|perl)\b/.test(cmd)) return [];
  const segments = splitTopLevelSegments(cmd);
  const seps = segmentSeparators(cmd);
  const out = [];
  for (let i = 0; i < segments.length && out.length < MAX_IN_PLACE_TARGETS; i++) {
    const words = shellWords(segments[i]);
    if (!words) continue;
    let k = 0;
    while (k < words.length && !words[k].op && !words[k].anyQuoted && ENV_ASSIGNMENT.test(words[k].text)) k++;
    const head = words[k];
    if (!head || head.op || head.anyQuoted || (head.text !== 'sed' && head.text !== 'perl')) continue;
    const args = [];
    for (let j = k + 1; j < words.length && !words[j].op; j++) args.push(words[j]);
    const operands = head.text === 'sed' ? sedInPlaceFiles(args) : perlInPlaceFiles(args);
    if (operands.length === 0) continue;
    const cwd = segmentCwd(segments, i, shellCwd, { isDir, seps });
    if (cwd === null) continue;
    for (const w of operands) {
      const abs = path.resolve(cwd, w);
      if (!out.includes(abs) && out.length < MAX_IN_PLACE_TARGETS && isFile(abs)) out.push(abs);
    }
  }
  return out;
}

// Quoting does not decide what is an option: the shell removes the quotes
// before sed or perl sees `--expression='s/a/b/'` or `'-i'`.
//
// Plain operand words, a redirect and its target dropped. A word with a bare
// glob or redirect character is not a file name the shell passes unchanged.
function operandWords(words) {
  const out = [];
  for (let j = 0; j < words.length; j++) {
    const w = words[j];
    if (w.bareOp) {
      if (/[<>]$/.test(w.text)) j++;  // `> out`: the next word is the redirect's target
      continue;
    }
    if (!w.bareSpecial) out.push(w.text);
  }
  return out;
}

// GNU sed: `-i[SUFFIX]` / `--in-place[=SUFFIX]` (a suffix is attached, so in a
// cluster the rest of the word is it: `-ie` = suffix `e`); `-e`/`-f`/`-l` and
// their long forms take a value. The first operand is the script unless `-e`
// or `-f` gave one. BSD `-i ''` reads as GNU `-i` with script `''`, and its
// real script then fails the existing-file test.
function sedInPlaceFiles(words) {
  let inPlace = false;
  let haveScript = false;
  let endOfOpts = false;
  const operands = [];
  for (let j = 0; j < words.length; j++) {
    const w = words[j];
    const t = w.text;
    if (endOfOpts || !t.startsWith('-') || t === '-') { operands.push(w); continue; }
    if (t === '--') { endOfOpts = true; continue; }
    if (t.startsWith('--')) {
      const [name, value] = [t.slice(2).split('=')[0], t.includes('=')];
      if (name === 'in-place') inPlace = true;
      else if (name === 'expression' || name === 'file') { haveScript = true; if (!value) j++; }
      else if (name === 'line-length' && !value) j++;
      continue;
    }
    for (let c = 1; c < t.length; c++) {
      const ch = t[c];
      if (ch === 'i') { inPlace = true; break; }
      if (ch === 'e' || ch === 'f' || ch === 'l') {
        if (ch !== 'l') haveScript = true;
        if (c === t.length - 1) j++;
        break;
      }
    }
  }
  if (!inPlace) return [];
  return operandWords(haveScript ? operands : operands.slice(1));
}

// perl: `-i[EXT]` (the rest of the word is the extension: `-pie` = `-p -i`
// with extension `e`); `-e`/`-E` take the code (rest of the word, else the
// next word), `-M`/`-I`/`-F` a value the same way. With no `-e`, the first
// operand is the script file.
function perlInPlaceFiles(words) {
  let inPlace = false;
  let haveCode = false;
  let endOfOpts = false;
  const operands = [];
  for (let j = 0; j < words.length; j++) {
    const w = words[j];
    const t = w.text;
    if (endOfOpts || !t.startsWith('-') || t === '-') { operands.push(w); continue; }
    if (t === '--') { endOfOpts = true; continue; }
    for (let c = 1; c < t.length; c++) {
      const ch = t[c];
      if (ch === 'i') { inPlace = true; break; }
      if ('eEMIF'.includes(ch)) {
        if (ch === 'e' || ch === 'E') haveCode = true;
        if (c === t.length - 1) j++;
        break;
      }
    }
  }
  if (!inPlace) return [];
  return operandWords(haveCode ? operands : operands.slice(1));
}

function isRegularFile(p) {
  try { return fs.statSync(p).isFile(); } catch { return false; }
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
//
// A heredoc body is skipped (D#66): it is data on the reading command's stdin,
// not commands. Lexed as shell, a Python `\'` in a `<<'PY'` body flipped the
// quote parity for the rest of the command, and a `grep` line in a script body
// was folded as if the shell had run it. The `<<WORD` stays in its segment;
// the lines after that segment's newline, through the terminator line, go.
//
// Also read as the shell reads them, because each misreading drops or invents
// commands: a comment (`#` at the start of a word) runs to the end of its line;
// `<<<` is a here-string and `<<` inside `((…))` a shift, neither a heredoc;
// the heredoc delimiter is a whole word with its quoting removed (`E"O"F`,
// `$'EOF'`, `\EOF` all end at a line `EOF`).
function readHeredocDelim(cmd, i) {
  let j = i;
  let delim = '';
  while (j < cmd.length && !/[\s;&|<>()]/.test(cmd[j])) {
    const c = cmd[j];
    if (c === '$' && (cmd[j + 1] === "'" || cmd[j + 1] === '"')) { j++; continue; }
    if (c === "'" || c === '"') {
      const close = cmd.indexOf(c, j + 1);
      if (close === -1) return null;
      delim += cmd.slice(j + 1, close);
      j = close + 1;
      continue;
    }
    if (c === '\\' && j + 1 < cmd.length) { delim += cmd[j + 1]; j += 2; continue; }
    delim += c;
    j++;
  }
  return delim ? { delim, end: j } : null;
}

// Stronger of two separators joining the same pair of segments (an empty
// segment between them collapses): anything conditional beats `;`/newline.
const SEP_RANK = { '': 0, ';': 1, '\n': 1, '&&': 2, '||': 2, ctrl: 3 };
function joinSep(a, b) {
  if (a === undefined) return b;
  if (SEP_RANK[a] === 2 && SEP_RANK[b] === 2 && a !== b) return 'ctrl';
  return SEP_RANK[b] > SEP_RANK[a] ? b : a;
}

/**
 * Top-level segments with the separator before each: '' (the first), ';',
 * '\n', '&&', '||', or 'ctrl' (after a `do`/`then`/… control word, where
 * whether the segment runs is not a matter of the previous one).
 * @returns {{text: string, sep: string}[]}
 */
function splitTopLevelSegmentsWithSeps(cmd) {
  if (!cmd || typeof cmd !== 'string') return [];
  const segs = [];
  let cur = '';
  let sep = '';
  let quote = null;
  let arith = 0;      // depth of `((` … `))`
  let heredocs = [];  // [{delim, stripTabs}] started on the current line
  // The last char appended to `cur`, kept apart: reading `cur` itself flattens
  // the string `+=` built, once per `#` — a 300 KB `echo a#a#…` took 7.9 s in
  // a 5 s hook (pre-release review #11).
  let last = '';
  const add = (s) => { cur += s; last = s[s.length - 1]; };
  const cut = (next) => { segs.push({ text: cur, sep }); cur = ''; last = ''; sep = next; };
  for (let i = 0; i < cmd.length; i++) {
    const c = cmd[i];
    if (!quote) {
      if (c === '#' && (last === '' || /[\s|(]/.test(last))) {
        const nl = cmd.indexOf('\n', i);
        i = (nl === -1 ? cmd.length : nl) - 1;
        continue;
      }
      if (c === '(' && cmd[i + 1] === '(') { arith++; add('(('); i++; continue; }
      if (c === ')' && cmd[i + 1] === ')' && arith > 0) { arith--; add('))'); i++; continue; }
      if (c === '<' && cmd[i + 1] === '<' && cmd[i + 2] === '<') { add('<<<'); i += 2; continue; }
      if (c === '<' && cmd[i + 1] === '<' && arith === 0) {
        let j = i + 2;
        const stripTabs = cmd[j] === '-';
        if (stripTabs) j++;
        while (cmd[j] === ' ' || cmd[j] === '\t') j++;
        const d = readHeredocDelim(cmd, j);
        if (d) {
          heredocs.push({ delim: d.delim, stripTabs });
          add(cmd.slice(i, d.end));
          i = d.end - 1;
          continue;
        }
      }
      if (c === '\n' && heredocs.length > 0) {
        cut('\n');
        let j = i + 1;
        for (const { delim, stripTabs } of heredocs) {
          while (j < cmd.length) {
            const nl = cmd.indexOf('\n', j);
            const end = nl === -1 ? cmd.length : nl;
            const line = cmd.slice(j, end);
            j = end + 1;
            if ((stripTabs ? line.replace(/^\t+/, '') : line) === delim) break;
          }
        }
        heredocs = [];
        i = j - 1;
        continue;
      }
    }
    if (quote) {
      add(c);
      // Inside DOUBLE quotes a backslash escapes the next char, so `\"` does NOT
      // close the quote (POSIX). Single quotes do no escaping — `\` is literal
      // and `'` always closes — so this only applies to `"`. Without it,
      // `echo "x\" && grep \"Y\" src/"` (one literal echo arg) mis-closes at
      // `\"`, splits on `&&`, and yields a phantom foldable grep segment.
      if (quote === '"' && c === '\\' && i + 1 < cmd.length) {
        add(cmd[i + 1]);
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
      if (cmd[i + 1] !== '\n') add(c + cmd[i + 1]);
      i++;
      continue;
    }
    if (c === '"' || c === "'") { quote = c; add(c); continue; }
    // `&&` and `||` (a single `&`/`|` is NOT a split — `|` is an output-filter
    // pipe, lone `&` is background and rare in tool calls).
    if ((c === '&' && cmd[i + 1] === '&') || (c === '|' && cmd[i + 1] === '|')) {
      cut(c + c); i++; continue;
    }
    if (c === ';' || c === '\n') { cut(c); continue; }
    add(c);
  }
  cut(undefined);
  // Split out `for … in` / `do` / `done` control words as their own boundaries
  // so a loop body grep is isolated (the head of `for s in …; do grep …` is the
  // `for` keyword, which would otherwise mask the grep). Quote-safety already
  // handled above — these run per already-split segment on whitespace-delimited
  // control words only. A `for` header takes its word list with it: the list
  // is data, and left behind as a segment it read as a command that segmentCwd
  // cannot place (D#76).
  const pieces = [];
  const CTRL = /(?:^|\s)(for\s+\S+\s+in\b[\s\S]*$|do\b|done\b|then\b|fi\b)(?=\s|$)/g;
  for (const { text: raw, sep: rawSep } of segs) {
    let last = 0;
    let m;
    let pieceSep = rawSep;
    CTRL.lastIndex = 0;
    while ((m = CTRL.exec(raw)) !== null) {
      pieces.push({ text: raw.slice(last, m.index), sep: pieceSep });
      last = CTRL.lastIndex;
      pieceSep = 'ctrl';
    }
    pieces.push({ text: raw.slice(last), sep: pieceSep });
  }
  const out = [];
  let carried;
  for (const { text, sep: s } of pieces) {
    carried = joinSep(carried, s);
    if (!text.trim()) continue;
    out.push({ text: text.trim(), sep: out.length === 0 ? '' : carried });
    carried = undefined;
  }
  return out;
}

function splitTopLevelSegments(cmd) {
  return splitTopLevelSegmentsWithSeps(cmd).map((s) => s.text);
}

function segmentSeparators(cmd) {
  return splitTopLevelSegmentsWithSeps(cmd).map((s) => s.sep);
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
    'Repo-wide index. Skip this hint if you specifically need raw-text regex.',
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
function isDirectory(p) {
  try { return fs.statSync(p).isDirectory(); } catch { return false; }
}
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
// only reach the rewrite in `show` mode, which answers with the body. Not
// `-R`: it follows every symlink under the path, which cg's walk does not
// (review of D#133, M-1).
const ALLOWED_SHORT = { grep: 'rnHsIiwFlcEPABC', git: 'rnHIiwFlcEPABC', rg: 'nHiwFlcsABC', ag: 'iwlcsABC' };
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

// `isDir(target)` answers whether the path operand is a directory. runMain asks
// the filesystem, resolved against the root its command is relative to; the
// default is the spelling alone (a trailing `/`, or a last segment with no
// extension), which keeps the classifiers that call this pure.
const looksLikeDir = (t) => t.endsWith('/') || !/\.[A-Za-z0-9]{1,6}$/.test(t);
function rewritePlan(cmd, { isDir = looksLikeDir } = {}) {
  if (!cmd || typeof cmd !== 'string' || cmd.length > 1000) return null;
  const words = shellWords(cmd);
  if (!words || words.length === 0 || words.some((w) => w.op)) return null;
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
  let include = false;
  let recursive = false;
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
      // GNU grep's --include has no `!` negation and no `{a,b}` alternatives;
      // cg's -g has both (round 4 L2; review of D#78). Checked with the other
      // --include value rules below.
    }
    if (w.text === '--') { endOfFlags = true; continue; }
    if (w.text.startsWith('--')) {
      const eq = w.text.indexOf('=');
      const name = w.text.slice(2, eq === -1 ? undefined : eq);
      if (!ALLOWED_LONG[verb].has(name)) return null;
      if (name === 'include') include = true;
      if (name === 'recursive') recursive = true;
      // GNU grep and ugrep print a `file:0` row for every file searched; cg,
      // rg and git grep print only files with matches (D#133 #12).
      if (name === 'count' && verb === 'grep') return null;
      if (VALUE_LONG.has(name) && eq === -1) {
        const v = words[++i];
        if (!valueWordOk(v, false)) return null;
      }
      // GNU grep matches --include against the file's base name, so a glob
      // with `/` or `**` finds nothing there; cg's -g matches the path
      // (D#133 #7). Checked however the value is quoted or attached.
      if (name === 'include') {
        const v = eq === -1 ? words[i].text : w.text.slice(eq + 1);
        if (/^!|[{/]|\*\*/.test(v)) return null;
      }
      if (name === 'ignore-case' || name === 'case-sensitive') caseFlag = true;
      continue;
    }
    const letters = w.text.slice(1);
    for (let k = 0; k < letters.length; k++) {
      const ch = letters[k];
      if (ch === 'c' && verb === 'grep') return null;  // D#133 #12, as --count above
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
      if (ch === 'r' || ch === 'R') recursive = true;
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
  // GNU grep applies --include to a file named on the command line too, so
  // `grep --include='*.py' X src/a.rs` finds nothing; rg — and so cg — searches
  // a named file whatever its -g says (D#78). Equivalent only for a directory.
  if (include && target && !isDir(target.text)) return null;
  // Plain grep without -r reads no directory ("Is a directory") and, with no
  // path, reads stdin; the answer would search the tree (D#125 #3). rg and ag
  // recurse by default, and git grep searches the tracked tree.
  if (verb === 'grep' && !recursive && (!target || isDir(target.text))) return null;
  // ag is smart-case by default: an all-lowercase pattern matches any case,
  // and cg's search is case-sensitive, so the answer would find less.
  if (verb === 'ag' && !caseFlag && !/[A-Z]/.test(pattern.text)) return null;
  return { verb, pattern: pattern.text, target: target ? target.text : undefined, context };
}

/**
 * Does the answer search the same files the grep would (D#133 #8)?
 *
 * cg searches ripgrep's walk (not hidden, not ignored) plus every git-tracked
 * file. The verbs it replaces each search another set, measured on one fixture
 * with each tool:
 *   - grep -r (GNU, and ugrep as Claude Code's shell runs it) also reads
 *     untracked hidden and ignored files, and ugrep skips a tracked ignored
 *     file when the .gitignore naming it sits inside the searched path;
 *   - git grep reads tracked files only, not untracked ones;
 *   - rg and ag skip tracked hidden and tracked ignored files.
 * So the answer is equivalent only when the path holds none of the files that
 * set the two apart. Outside a git work tree cg searches ripgrep's walk alone:
 * a hidden entry, or an ignore file ripgrep, ag or ugrep reads, differs from
 * grep -r. Any failure to tell (git missing, a timeout, a walk too large) is
 * a difference: the grep runs.
 *
 * Review of D#133 found what git's view leaves out, and each now declines for
 * every verb:
 *   - H-1: ripgrep reads `.ignore` and `.rgignore` (ag `.agignore`) inside
 *     the path and in every directory above it, up to `/`; git reads none of
 *     them. cg passes rg no `--no-config`, so RIPGREP_CONFIG_PATH changes its
 *     search too.
 *   - M-1: grep -r and rg skip a symlink met while recursing, cg names a
 *     tracked one on rg's command line (followed) and git grep searches the
 *     link's text; git grep does not enter a submodule, cg's walk does.
 * With `show`, the answer is the index's, whose walk (src/indexer/merkle.rs)
 * also skips hidden entries even when tracked, the INDEX_EXCLUDED_DIRS
 * segments, files of no detected language (src/utils/config.rs), files over
 * max_file_size (src/domain.rs) and tracked ignored files; any of them in the
 * path declines (H-2). The three lists are pinned to the Rust source by test.
 * @param {{root: string, target?: string, verb: string, show?: boolean}} opts
 *   `target` is root-relative (undefined: the whole root).
 */
const HIDDEN_SEGMENT = /(?:^|\/)\.(?!\.?(?:\/|$))/;
const RG_ONLY_IGNORE_FILES = ['.ignore', '.rgignore', '.agignore'];
const WALK_LIMIT = 20000;
const INDEX_EXCLUDED_DIRS = ['node_modules', 'vendor', 'target', 'bower_components'];
const INDEX_EXTENSIONS = new Set(['rs', 'ts', 'tsx', 'js', 'jsx', 'mjs', 'cjs', 'go', 'py', 'pyi', 'java',
  'c', 'h', 'cpp', 'cc', 'cxx', 'hpp', 'hh', 'hxx', 'html', 'htm', 'css', 'cs', 'kt', 'kts', 'rb', 'php',
  'swift', 'dart', 'md', 'mdx', 'markdown', 'sh', 'bash', 'json']);
const INDEX_DEFAULT_MAX_FILE_SIZE = 1048576;
// As src/domain.rs reads it: a u64, else the default. The hook sees its own
// environment, not the index process's.
function indexMaxFileSize(env = process.env) {
  const raw = env.CODE_GRAPH_MAX_FILE_SIZE;
  return typeof raw === 'string' && /^\+?\d+$/.test(raw) ? Number(raw) : INDEX_DEFAULT_MAX_FILE_SIZE;
}
// Does the index read this root-relative file (by name; size is separate)?
function indexReadsPath(rel) {
  const segs = rel.split('/');
  if (segs.some((s) => s.startsWith('.') || INDEX_EXCLUDED_DIRS.includes(s))) return false;
  const base = segs[segs.length - 1];
  const dot = base.lastIndexOf('.');
  return dot > 0 && INDEX_EXTENSIONS.has(base.slice(dot + 1));
}
// Is there a ripgrep-only ignore file in `dir` or any directory above it?
function rgIgnoreAbove(dir) {
  for (let d = dir; ; d = path.dirname(d)) {
    if (RG_ONLY_IGNORE_FILES.some((f) => fs.existsSync(path.join(d, f)))) return true;
    if (path.dirname(d) === d) return false;
  }
}
// A listed entry that cannot be read counts as a symlink (the caller declines);
// a missing path does not.
function isSymlink(p, { missing = true } = {}) {
  try { return fs.lstatSync(p).isSymbolicLink(); } catch { return missing; }
}
// F1b — `grep -r` reads git-ignored files and the rewrite does not, so an
// ignored entry under the path declines. The one nearly every Python tree has
// once its tests ran is a `__pycache__/` of `.pyc` files (the coding-eval
// fixtures carry 286), and a binary file yields no matched line: grep notes
// "binary file matches" instead (stderr in GNU grep 3.5+, stdout in BSD grep
// and ugrep), a notice, not a hit. So a bytecode cache may differ between the
// two — for a grep that does not list files (see grepListsBinaryMatches).
const BYTECODE_FILE = /\.py[co]$/;
function isBytecodeCache(root, rel) {
  const r = rel.replace(/\/+$/, '');
  if (BYTECODE_FILE.test(r)) return true;
  if (path.posix.basename(r) !== '__pycache__') return false;
  let entries;
  try { entries = fs.readdirSync(path.join(root, r), { withFileTypes: true }); } catch { return false; }
  return entries.length <= WALK_LIMIT && entries.every((e) => e.isFile() && BYTECODE_FILE.test(e.name));
}

/// True when the grep would PRINT a matching binary file: it lists files
/// (`-l`, `--files-with-matches`) and does not skip binaries (`-I`). The
/// rewrite grammar admits no other output mode that prints one for grep
/// (`-c`, `-L`, `-a`, `-o` are refused). Only the grep's own clause counts.
function grepListsBinaryMatches(cmd) {
  const words = clauseWords(firstShellClause(cmd || '').replace(VERB_STRIP, ''));
  if (!words) return true;
  let lists = false;
  let skipsBinary = false;
  // Quoted words count too: the shell strips the quotes and grep still reads
  // `"-l"` as an option. A quoted PATTERN that merely contains ` -l ` has a
  // space in it and never matches the one-cluster shape below.
  for (const w of words) {
    const t = w.text;
    if (t === '--files-with-matches') lists = true;
    else if (t === '--binary-files=without-match') skipsBinary = true;
    else if (/^-[A-Za-z0-9]+$/.test(t)) {
      if (t.includes('l')) lists = true;
      if (t.includes('I')) skipsBinary = true;
    }
  }
  return lists && !skipsBinary;
}

function searchesSameFiles({ root, target, verb, show = false, binaryCacheOk = false } = {}) {
  if (!root || !['grep', 'git', 'rg', 'ag'].includes(verb)) return false;
  const rel = target === undefined ? '' : String(target).replace(/^(?:\.\/)+/, '').replace(/\/+$/, '');
  if (rel.split('/').includes('..') || path.isAbsolute(rel)) return false;
  if (process.env.RIPGREP_CONFIG_PATH) return false;
  if (rgIgnoreAbove(path.join(root, rel))) return false;
  const maxSize = indexMaxFileSize();
  // For `show`: every file the grep reads must be one the index holds.
  const indexHolds = (files) => files.length <= WALK_LIMIT && files.every((f) => {
    if (!indexReadsPath(f)) return false;
    try { return fs.statSync(path.join(root, f)).size <= maxSize; } catch { return false; }
  });
  const git = (args) => {
    const budget = require('./hook-fail-open').remainingMs(1500);
    if (budget === null) return { status: null, stdout: '' };
    return spawnSync('git', args, hidden({
      cwd: root, encoding: 'utf8', timeout: budget, killSignal: 'SIGKILL',
      maxBuffer: 16 * 1024 * 1024, stdio: ['ignore', 'pipe', 'ignore'],
    }));
  };
  const inside = git(['rev-parse', '--is-inside-work-tree']);
  if (inside.status === 0 && String(inside.stdout).trim() === 'true') {
    const ls = (...opts) => {
      const r = git(['ls-files', '-z', ...opts, '--', rel || '.']);
      return r.status === 0 ? String(r.stdout).split('\0').filter(Boolean) : null;
    };
    const staged = ls('--cached', '--stage');           // "<mode> <sha> <n>\t<path>"
    const others = ls('--others', '--exclude-standard'); // untracked, not ignored
    const ignoredOthers = ls('--others', '--ignored', '--exclude-standard', '--directory');
    const ignoredTracked = ls('--cached', '--ignored', '--exclude-standard');
    if (!staged || !others || !ignoredOthers || !ignoredTracked) return false;
    const tracked = [];
    for (const e of staged) {
      // 120000: a symlink; 160000: a submodule (M-1).
      if (/^1[26]0000 /.test(e)) return false;
      tracked.push(e.slice(e.indexOf('\t') + 1));
    }
    // An untracked nested repository is listed as `dir/`; a symlink as a file.
    if (others.length > WALK_LIMIT || others.some((f) => f.endsWith('/') || isSymlink(path.join(root, f)))) return false;
    const base = (f) => path.posix.basename(f.replace(/\/+$/, ''));
    if ([tracked, others, ignoredOthers].some((l) => l.some((f) => RG_ONLY_IGNORE_FILES.includes(base(f))))) return false;
    const anyHidden = (l) => l.some((f) => HIDDEN_SEGMENT.test(f));
    let reads;
    if (verb === 'grep') {
      const ignored = binaryCacheOk ? ignoredOthers.filter((f) => !isBytecodeCache(root, f)) : ignoredOthers;
      if (anyHidden(others) || ignored.length || ignoredTracked.length) return false;
      reads = [...tracked, ...others];
    } else if (verb === 'git') {
      if (others.length) return false;
      reads = tracked;
    } else {
      if (anyHidden(tracked) || ignoredTracked.length) return false;
      reads = [...tracked, ...others.filter((f) => !HIDDEN_SEGMENT.test(f))];
    }
    return !show || (ignoredTracked.length === 0 && indexHolds(reads));
  }
  // 128 is git's "not a git repository"; anything else is a failure to tell.
  if (inside.status !== 128) return false;
  // Not a work tree: ripgrep's walk. A .gitignore from the root down to the
  // path (ugrep reads it; ripgrep's own ignore files were checked above), a
  // hidden or symlinked directory on the way, or any hidden entry or symlink
  // inside it sets the verbs apart.
  let dir = root;
  for (const seg of ['', ...(rel ? rel.split('/') : [])]) {
    if (seg) dir = path.join(dir, seg);
    if (seg.startsWith('.') || (seg && isSymlink(dir, { missing: false }))) return false;
    if (fs.existsSync(path.join(dir, '.gitignore'))) return false;
  }
  const start = path.join(root, rel);
  const files = [];
  let seen = 0;
  const stack = [start];
  while (stack.length) {
    const d = stack.pop();
    let entries;
    try { entries = fs.readdirSync(d, { withFileTypes: true }); } catch { continue; }
    for (const e of entries) {
      if (++seen > WALK_LIMIT) return false;
      if (e.name.startsWith('.') || e.isSymbolicLink()) return false;
      if (e.isDirectory()) stack.push(path.join(d, e.name));
      else if (show) files.push(path.relative(root, path.join(d, e.name)).split(path.sep).join('/'));
    }
  }
  // A file named as the path itself: its own entry was never listed.
  if (show && rel && files.length === 0 && !isDirectory(start)) files.push(rel);
  return !show || indexHolds(files);
}

// Does `rootTarget`, run from the root, name the path the grep clause's own
// operand names from `cwd` — the directory the clause ran in? The raw clause is
// parsed with the rewrite grammar; an absolute operand resolves the same from
// anywhere. A clause the grammar cannot read proves nothing.
function operandMatches(rawClause, rootTarget, root, cwd) {
  const raw = rewritePlan(rawClause, { isDir: (t) => isDirectory(path.resolve(cwd, t)) });
  if (!raw || raw.target === undefined || rootTarget === undefined) return false;
  return path.resolve(cwd, raw.target) === path.resolve(root, rootTarget);
}

// Commands that cannot move the shell: external programs run in a child, and
// these builtins leave the cwd alone. An allowlist, because the commands that
// DO move it are open-ended — `builtin cd`, `if cd`, `{ cd …; }`, `eval`,
// `source`, `popd`, any function (D#73 round 3 reproduced 19 forms).
const CWD_NEUTRAL = new Set([
  'echo', 'printf', 'grep', 'rg', 'ag', 'git', 'sed', 'awk', 'cat', 'head', 'tail', 'wc',
  'ls', 'find', 'sort', 'uniq', 'cut', 'tr', 'diff', 'cmp', 'test', '[', 'true', 'false',
  ':', 'node', 'python', 'python3', 'cargo', 'npm', 'npx', 'jq', 'xargs', 'tee', 'stat',
  'file', 'du', 'df', 'date', 'sleep', 'which', 'env', 'export', 'set', 'unset', 'nl',
  'rm', 'mkdir', 'cp', 'mv', 'touch', 'chmod', 'ln', 'readlink', 'realpath', 'basename',
  'dirname', 'perl', 'gh', 'curl', 'make', 'bash', 'sh', 'timeout', 'tar', 'sqlite3',
  'code-graph-mcp', 'nproc', 'rustc', 'go', 'wait', 'column', 'md5sum', 'sha256sum',
  'od', 'xxd', 'seq', 'paste', 'comm', 'uname', 'id', 'printenv',
  // Not `exit` or `return` (D#133 #10): if one before the grep ran, the grep
  // did not, and its output is empty exactly when the inject fires. `set` is
  // neutral only without errexit — see segmentCwd.
]);
// `set -e`, `set -euo pipefail`, `set -o errexit`: from here a failing command
// ends the script, so a later grep may not have run.
const SET_ERREXIT = /(?:^|\s)(?:-[a-zA-Z]*e[a-zA-Z]*|errexit)(?=\s|$)/;
// `X=1 Y=$(mktemp -d)`: a command substitution runs in a subshell.
const PURE_ASSIGNMENTS =
  /^\s*(?:[A-Za-z_][A-Za-z0-9_]*=(?:\$\([^()]*\)|"[^"]*"|'[^']*'|[^\s;|&<>()'"]*)\s*)+$/;
const CD_LITERAL = /^\s*(?:[A-Za-z_][A-Za-z0-9_]*=\S*\s+)*cd\s+('[^']*'|"[^"$`\\]*"|[^\s'"$`\\~*?[{|;&<>()]+)\s*$/;

/**
 * The directory `segments[idx]` runs in, or null when the segments before it
 * could have moved the shell somewhere we cannot name (D#76). Follows
 * cwd-neutral commands and `cd` to one literal path that exists; a relative
 * `cd` under a set CDPATH is not literal. A `cd` is followed only when it is
 * certain to have run if the grep did: it is unconditional (its `seps` entry,
 * from segmentSeparators, is '', `;` or a newline) or every separator from it
 * to the grep is `&&`, the one before it included: `true || cd x && grep` is
 * `(true || cd x) && grep`, which runs the grep without the cd. `false && cd x;
 * grep` and `cd x | cat` did not move the shell. Without `seps` every segment
 * counts as unconditional.
 */
function segmentCwd(segments, idx, shellCwd, { isDir = isDirectory, seps } = {}) {
  let cwd = shellCwd;
  const ran = (k) => !seps || SEP_RANK[seps[k]] <= 1
    || seps.slice(k, idx + 1).every((x) => x === '&&');
  for (let k = 0; k < idx; k++) {
    const seg = segments[k];
    if (/^\s*#/.test(seg) || PURE_ASSIGNMENTS.test(seg)) continue;
    const clause = firstShellClause(seg);
    const cd = CD_LITERAL.exec(seg);
    if (cd) {
      if (!ran(k)) return null;
      const arg = /^['"]/.test(cd[1]) ? cd[1].slice(1, -1) : cd[1];
      if (!arg || arg === '-' || (!path.isAbsolute(arg) && process.env.CDPATH)) return null;
      const next = path.resolve(cwd, arg);
      if (!isDir(next)) return null;  // `cd x; grep` runs the grep where it was
      try { cwd = fs.realpathSync(next); } catch { cwd = next; }
      continue;
    }
    const words = clauseWords(clause);
    if (!words) return null;
    const head = words.find((w) => !/^[A-Za-z_][A-Za-z0-9_]*=/.test(w.text));
    if (head && !(head.quotedFrom === -1 && CWD_NEUTRAL.has(head.text))) return null;
    if (head && head.text === 'set' && SET_ERREXIT.test(clause.replace(/^\s*set\b/, ''))) return null;
  }
  return cwd;
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
    // `show` has no file filter: `--include`/`-g`/`-t` excluded definitions it
    // would print (review of D#125).
    const all = extractCgFlags(cmd);
    if (all.includes('-g') || all.includes('-t')) return false;
    // show answers at most three symbols; a fourth would silently vanish.
    if (extractDeclSymbols(extractPatterns(cmd)).length > 3) return false;
    if (!showAnswersPattern(plan.pattern, cmd)) return false;
  }
  return true;
}

// One alternative `show` answers: a declaration keyword, the name, and an end
// of word (`\b`, or `\>` outside Perl) unless -w supplies it.
const SHOW_ALT = /^(fn|def|class|function|struct|trait) +([A-Za-z_][A-Za-z0-9_]*)(\\b|\\>)?$/;

/**
 * Does `show` print exactly the definitions this grep's pattern matches?
 * `show` answers whole names, case-sensitively, whatever surrounds them (0.161.0
 * Not covered, D#133): `fn foo` also matches `fn foobar`, `-i` matches `Foo`,
 * `pub fn foo` misses a private `fn foo`, `def foo(` misses a Ruby `def foo`,
 * and an alternative without a keyword matches lines `show` never prints. So
 * every alternative must be `KEYWORD NAME` ending the word, and nothing else.
 */
function showAnswersPattern(pattern, cmd) {
  if (typeof pattern !== 'string') return false;
  const flags = cgFlagSet(extractCgFlags(cmd));
  if (flags.has('-i')) return false;
  const fixed = flags.has('-F');
  const word = flags.has('-w');
  const dialect = patternDialect(cmd);
  if (dialect === 'conflict') return false;
  const alts = fixed ? [pattern] : pattern.split(dialect === 'basic' ? '\\|' : '|');
  return alts.every((alt) => {
    const m = SHOW_ALT.exec(alt);
    if (!m) return false;
    if (!m[3]) return word;
    // -F reads `\b` as two characters; PCRE reads `\>` as `>`.
    return !fixed && !(m[3] === '\\>' && dialect === 'perl');
  });
}

// v0.49 — plain `grep` speaks BRE: alternation/grouping arrive escaped
// (`a\|b`, `\(x\)`) and 0-hit against cg grep's rust-regex dialect, wasting
// the answer on the ALLOW fallthrough (2026-06-12: both answered:false denies
// were dialect/path-shape misses). Unescape for plain grep only; an -E, -P or
// ag pattern is passed on only when every dialect reads it alike; rg's is the
// answer's own. null: no equivalent pattern, the grep runs.
function translateBreToRg(cmd, pattern) {
  if (typeof pattern !== 'string' || !pattern) return pattern;
  const dialect = patternDialect(cmd);
  if (dialect === 'rust') return pattern;
  if (dialect === 'conflict') return null;
  // D#133 #5: an extended or Perl pattern was passed on unchecked, and the
  // dialects read `\d`, `[\(]`, `a{,2}`, `a+?b` and `(?i)` differently (GNU
  // grep 3.12, ugrep 7.8 and cg run on one fixture).
  const out = dialect === 'basic'
    ? breToRustRegex(pattern)
    : portableExtended(pattern, { perl: dialect === 'perl' });
  // ugrep, the `grep` Claude Code's shell runs, prints no line for an empty
  // match, where GNU grep and rg print every line (review of D#133, M-2).
  return out !== null && matchesEmpty(out) ? null : out;
}

/**
 * Can this pattern (rust syntax, as translateBreToRg returns it) match the
 * empty string? An assertion (`^`, `$`, `\b`, `\B`, `\<`, `\>`) matches
 * empty; so does an atom under `*`, `?` or a `{0…}` repeat, a sequence of such
 * atoms, and an alternation with one such branch (`a|`). Unreadable: true, so
 * the caller declines.
 */
function matchesEmpty(pattern) {
  let i = 0;
  let bad = false;
  const alt = () => {
    let any = seq();
    while (pattern[i] === '|') { i++; any = seq() || any; }
    return any;
  };
  const seq = () => {
    let all = true;
    while (i < pattern.length && pattern[i] !== '|' && pattern[i] !== ')') {
      let empty = atom();
      for (;;) {
        const c = pattern[i];
        if (c === '*' || c === '?') { empty = true; i++; continue; }
        if (c === '+') { i++; continue; }
        const m = c === '{' ? /^\{(\d+)(?:,\d*)?\}/.exec(pattern.slice(i)) : null;
        if (!m) break;
        if (Number(m[1]) === 0) empty = true;
        i += m[0].length;
      }
      all = all && empty;
    }
    return all;
  };
  const atom = () => {
    const c = pattern[i];
    if (c === '(') {
      i++;
      const inner = alt();
      if (pattern[i] !== ')') bad = true;
      i++;
      return inner;
    }
    if (c === '\\') {
      const n = pattern[i + 1];
      i += 2;
      return n === undefined || 'bB<>'.includes(n);
    }
    if (c === '[') {
      const end = bracketEnd(pattern, i);
      if (end === -1) { bad = true; i = pattern.length; return true; }
      i = end + 1;
      return false;
    }
    i++;
    return c === '^' || c === '$';
  };
  const empty = alt();
  return bad || i < pattern.length || empty;
}

/**
 * The regex dialect a grep clause's pattern is written in: 'rust' (rg, the
 * answer's own), 'perl' (ag, `-P`), 'extended' (`-E`), 'basic' (plain grep
 * and git grep), or 'conflict' (`-E` and `-P` together, an error to GNU
 * grep). `-F` is the callers' to read, from extractCgFlags, which
 * knows a flag's value slot (`--include -F`).
 *
 * The grep's OWN clause, not the whole command. This was the one flag check
 * in this module that v0.96 did not clause-scope, and round 4 of pre-ship
 * review found what it costs: `grep -rln "a\|b" src/ | xargs -P4 wc -l` read
 * the tail's `-P4` as "this grep speaks Perl regex", so the pattern was left
 * escaped. On its own that is a wrong dialect decision; it also desynchronised
 * the two hooks, because post-grep-inject passes a SEGMENT here while the deny
 * path passes the whole command — the same pattern then filed under two
 * spellings and the funnel scored a verbatim re-grep as neutral.
 * Read from the clause's words, like extractCgFlags: a pattern holding ` -E `
 * is not the flag (review of D#62). A word that is only quoted (`"-E"`) IS the
 * flag: the shell strips the quotes before grep reads its argv (D#133 M10).
 */
function patternDialect(cmd) {
  const verb = (String(cmd).match(GREP_HEAD) || [])[1];
  if (!verb) return 'basic';
  if (verb === 'rg') return 'rust';
  if (verb === 'ag') return 'perl';
  const words = clauseWords(firstShellClause(cmd).replace(VERB_STRIP, '')) || [];
  const has = (letter, long) => words.some((w) => w.text === `--${long}`
    || new RegExp(`^-[a-zA-Z]*${letter}[a-zA-Z]*(?:=|\\d|$)`).test(w.text));
  const perl = has('P', 'perl-regexp');
  const extended = has('E', 'extended-regexp');
  // Both: GNU grep reports "conflicting matchers specified" and exits 2
  // (review of D#133, L-4); there is no dialect to answer in.
  if (perl && extended) return 'conflict';
  if (perl) return 'perl';
  if (extended) return 'extended';
  return 'basic';
}

// An extended (ERE) or Perl pattern that GNU grep, ugrep and rust regex all
// read alike, returned as is; anything else is null and the grep runs. Only
// escapes of the word/space/boundary classes and of metacharacters pass, and
// `\<` `\>` only in ERE (PCRE reads them as the characters `<` `>`). A
// quantifier with nothing to repeat (so every `(?` group), after another
// quantifier (`+?` is lazy in rust, a repeated group in ERE) or after an
// assertion (`\b{2}`, `$+`: an error or a literal to GNU grep, every line to
// rust), a brace that is not `{n}`, `{n,}` or `{n,m}`, and a bracket holding an
// escape, a nested `[`, a set operator or a POSIX class are refused. Not
// `\W`: ugrep's matches a line break, so `\Wfoo` also printed the line before
// a match; not `\d` `\D` under Perl: ASCII to GNU grep, Unicode to rust, and
// ugrep's `\D` matches a line break (review of D#133, M-2).
const EXT_SAME_ESCAPE = 'wsSbB.[]^$*+?(){}|\\/';
function portableExtended(pattern, { perl = false } = {}) {
  let atomStart = true;    // a quantifier here would repeat nothing
  let quantified = false;  // the previous token was a quantifier
  let assertion = false;   // the previous token matches no character
  let altStart = true;     // an alternative starts here
  for (let i = 0; i < pattern.length; i++) {
    const c = pattern[i];
    const wasAltStart = altStart;
    altStart = false;
    if (c === '\\') {
      const n = pattern[i + 1];
      const same = n !== undefined && (EXT_SAME_ESCAPE.includes(n)
        || (!perl && (n === '<' || n === '>')));
      if (!same) return null;
      i++;
      atomStart = false;
      quantified = false;
      assertion = 'bB<>'.includes(n);
      continue;
    }
    if (c === '*' || c === '+' || c === '?' || c === '{') {
      if (atomStart || quantified || assertion) return null;
      if (c === '{') {
        const m = /^\{\d+(?:,\d*)?\}/.exec(pattern.slice(i));
        if (!m) return null;
        i += m[0].length - 1;
      }
      quantified = true;
      continue;
    }
    quantified = false;
    assertion = false;
    // `^` and `$` only where an alternative starts or ends (so never `$+`):
    // elsewhere ugrep reads them apart from GNU grep and rust (`\[*^[ab]`
    // matched `[a]`). A stray `}` is an error to ugrep -P (review of D#133,
    // M-2).
    if (c === '^' && !wasAltStart) return null;
    if (c === '$' && i + 1 < pattern.length && !')|'.includes(pattern[i + 1])) return null;
    if (c === '}') return null;
    // After `(`, `|` or `^` a quantifier repeats nothing; that also refuses
    // every `(?` group (`(?i)`, `(?:`, a lookaround).
    if (c === '(' || c === '|') altStart = true;
    if (c === '(' || c === '|' || c === '^') { atomStart = true; continue; }
    atomStart = false;
    if (c !== '[') continue;
    const end = bracketEnd(pattern, i);
    if (end === -1) return null;
    i = end;
  }
  return pattern;
}

// The index of the `]` closing the bracket expression opened at `i`, or -1
// when the bracket reads differently in POSIX and rust regex: POSIX has no
// escapes in a bracket, rust has escapes, `[` nesting and the `&&` `--` `~~`
// set operators; a leading `]` is a member in POSIX. A POSIX class
// (`[:alpha:]`) is ASCII to rust and follows the locale in GNU grep and ugrep
// (`é`, `٣`; review of D#133, M-2), so it counts as a nested `[`.
// Unterminated: -1 (grep reports an error).
function bracketEnd(pattern, i) {
  let j = i + 1;
  if (pattern[j] === '^') j++;
  if (pattern[j] === ']') return -1;
  for (; j < pattern.length && pattern[j] !== ']'; j++) {
    if (pattern[j] === '\\' || pattern[j] === '[') return -1;
    if ('&-~'.includes(pattern[j]) && pattern[j + 1] === pattern[j]) return -1;
  }
  return j >= pattern.length ? -1 : j;
}

// The two dialects swap these characters' roles: in a basic regex `( ) { } + ?
// |` are literals and `\(` … `\|` the operators, in rust regex the reverse. Only
// unescaping the operators (the translation until D#65) left the literals as
// operators, so `tombstoneActive()` searched `tombstoneActive` and the rewrite
// returned a superset of grep's lines. Checked against the grep Claude Code's
// shell runs (ugrep -G): `f()`, `a+b`, `a|b`, `{x}`, `a?` match only literally.
//
// A bracket expression is copied as written. Inside one, BRE has no escapes and
// rust has escapes, `[` nesting and the `&&` `--` `~~` set operators, so a
// bracket holding any of those means different things in the two; that pattern
// is untranslatable and the result is null — the caller lets the grep run.
// (`[:alpha:]` is a POSIX class in both and is kept.)
//
// Escapes: only the ones both read alike pass — the swapped operators, the
// word/space/boundary classes, and an escaped metacharacter. `\1`, `\d`, `\z`,
// `\_`, `\=` mean something else (or are an error) in one of the two, and so
// do an empty group `\(\)`, any `[:class:]` and a leading `]` in a bracket
// (review of D#65), `\W`, which matches a line break in ugrep, and a
// quantifier after an assertion (`\b*`, `\>\{2\}`; review of D#133, M-2).
const BRE_SWAPPED = '(){}+?|';
const BRE_SAME_ESCAPE = 'wsSbB<>.*[]^$\\/';
// An unescaped `^` is an anchor only where an alternative starts (the pattern's
// start, after `\(` or `\|`), and `$` only where one ends (the pattern's end,
// before `\)` or `\|`); elsewhere BRE reads them as literals and rust regex as
// anchors — `getUser\|$user_id` searched the text `$user_id` (D#125 #6). No
// translation: the grep runs.
function breToRustRegex(pattern) {
  let out = '';
  let altStart = true;
  let anchor = false;
  let assertion = false;
  for (let i = 0; i < pattern.length; i++) {
    const c = pattern[i];
    const wasAltStart = altStart;
    const wasAnchor = anchor;
    const wasAssertion = assertion;
    altStart = false;
    anchor = c === '^' && wasAltStart;
    assertion = anchor;
    if (c === '\\' && i + 1 < pattern.length) {
      const n = pattern[++i];
      if (n === '(' && pattern.startsWith('\\)', i + 1)) return null;
      if (wasAssertion && '+?{'.includes(n)) return null;
      if (BRE_SWAPPED.includes(n)) out += n;
      else if (BRE_SAME_ESCAPE.includes(n)) out += c + n;
      else return null;
      altStart = n === '(' || n === '|';
      assertion = 'bB<>'.includes(n);
      continue;
    }
    if (c === '*' && wasAssertion) return null;
    if (c === '^' && !wasAltStart) return null;
    // A `*` where an alternative starts, or right after its `^`, repeats
    // nothing: BRE reads it as a literal, rust regex as an error or a repeat of
    // the anchor (review of D#125).
    if (c === '*' && (wasAltStart || (wasAnchor && out.endsWith('^')))) return null;
    if (c === '$' && i + 1 < pattern.length
      && !pattern.startsWith('\\)', i + 1) && !pattern.startsWith('\\|', i + 1)) return null;
    if (BRE_SWAPPED.includes(c)) { out += '\\' + c; continue; }
    if (c !== '[') { out += c; continue; }
    const j = bracketEnd(pattern, i);
    if (j === -1) return null;
    out += pattern.slice(i, j + 1);
    i = j;
  }
  return out;
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

// Smallest per-dir share of the context cap worth spending on an overview answer.
const MIN_FANOUT_ANSWER_BYTES = 400;

/// Byte budget for each overview answer of the dirs that fire in one command.
/// A single dir is budgeted too: left at the answer's own 4,000-byte default,
/// its header pushed the hint past the envelope cap and the overview was
/// replaced by a bare "5+ Reads also into …" line (hook audit P2-5). Every hint first
/// pays for its own lines — an answer's header, footer and separator are about
/// 175 bytes plus the dir name twice, the one-line advice about 165 plus the
/// name three times — reserved as 200 plus the name three times, and the
/// answers share what is left of the cap. A share under MIN_FANOUT_ANSWER_BYTES
/// returns 0: every dir then gets the one-line advice instead.
function fanoutAnswerBudget(dirs) {
  if (dirs.length < 1) return undefined;
  const reserved = dirs.reduce((sum, d) => sum + 200 + 3 * Buffer.byteLength(d, 'utf8'), 0);
  const share = Math.floor((MAX_INJECTED_BYTES - reserved) / dirs.length);
  return share >= MIN_FANOUT_ANSWER_BYTES ? share : 0;
}

/// The fired dirs' hints as one context text. Every dir was marked delivered,
/// so each must be named in it: once the next hint would leave no room for a
/// line naming the dirs still to come, that line replaces the remaining hints.
function joinFanoutHints(dirs, hints) {
  const rest = (from) => `[code-graph] 5+ Reads also into ${dirs.slice(from).map((d) => `${d}/`).join(', ')} — \`code-graph-mcp overview <dir>/\` gives each dir's symbols+callers in one call.`;
  const out = [];
  let used = 0;
  for (let i = 0; i < hints.length; i++) {
    const need = Buffer.byteLength(hints[i], 'utf8') + (out.length ? 2 : 0);
    const tail = i + 1 < hints.length ? Buffer.byteLength(rest(i + 1), 'utf8') + 2 : 0;
    if (used + need + tail > MAX_INJECTED_BYTES) {
      out.push(rest(i));
      break;
    }
    out.push(hints[i]);
    used += need;
  }
  return out.join('\n\n');
}

function runMain() {
  if (isSilenced()) return;
  // v0.48 — process.cwd() follows the persistent shell; resolve the project
  // root by walking up so `cd backend/` no longer darkens the whole session.
  const shellCwd = process.cwd();
  const root = resolveProjectRoot(shellCwd);
  if (root === null) return;  // no index anywhere up to $HOME — no hint
  useSourceRoots(readSourceRoots(root));

  let input;
  try {
    // fd 0, not '/dev/stdin': the path form open(2)s the symlink target, which
    // fails with ENXIO when stdin is a socketpair (e.g. spawnSync {input}).
    // Reading the fd directly works for pipes, sockets, and files alike.
    input = JSON.parse(fs.readFileSync(0, 'utf8'));
  } catch { return; }

  const rawCmd = (input.tool_input && input.tool_input.command) || '';

  // Q1 — an in-place `sed`/`perl` is logged for the Stop check, then the
  // command goes on as it would have (nothing here answers or rewrites it).
  const inPlace = extractInPlaceEditTargets(rawCmd, shellCwd);
  if (inPlace.length > 0) {
    const sessionEdits = require('./session-edits');
    for (const abs of inPlace) {
      const rel = path.relative(root, abs);
      if (!rel || rel.startsWith('..') || path.isAbsolute(rel)) continue;
      sessionEdits.recordFileEdit(root, input.session_id, rel.split(path.sep).join('/'));
    }
  }

  // v0.49 — sed-range reads count toward the read-fanout state (the Read hook
  // never sees Bash-side file reads). A fired fanout hint already delivered an
  // overview — skip grep hinting for this command to avoid double output.
  //
  // Every dir that fired goes into ONE envelope. Claude Code parses a hook's
  // stdout as a single JSON value: two envelopes (two dirs crossing in one
  // compound command) were rejected whole, so the model saw neither hint while
  // the state recorded both as delivered. The shared context cap is split
  // between the answers, so a large first overview cannot truncate the second
  // dir out of the envelope.
  //
  // No permission decision: this is a Bash call, and `allow` would skip the
  // user's prompt for the WHOLE command (`sed -n 1,5p a.js; <anything>`). The
  // read hook's allow envelope used to leak here because pre-read-guide wrote
  // it from inside the shared tracker, out of hook-emit.test.js's allowlist view.
  const sedTargets = extractSedReadTargets(rawCmd);
  if (sedTargets.length > 0) {
    const readGuide = require('./pre-read-guide');
    const firedDirs = [];
    for (const t of sedTargets) {
      if (!readGuide.isSourceFile(t)) continue;
      const abs = path.isAbsolute(t) ? t : path.resolve(shellCwd, t);
      // D#560 — a linked worktree's copy counts toward the indexed directory.
      const dir = readGuide.trackRead(root, indexKey(root, path.relative(root, abs)));
      if (dir !== null) firedDirs.push(dir);  // a dir fires once: markHint starts its cooldown
    }
    if (firedDirs.length > 0) {
      const maxBytes = fanoutAnswerBudget(firedDirs);
      // A dir with no overview contributes nothing (buildFanoutHint → null);
      // it stays marked, so it does not fire again.
      const answered = firedDirs
        .map((dir) => ({ dir, hint: readGuide.buildFanoutHint(root, dir, { maxBytes }) }))
        .filter((h) => h.hint !== null);
      if (answered.length > 0) {
        process.stdout.write(emitPreToolContext(
          joinFanoutHints(answered.map((h) => h.dir), answered.map((h) => h.hint))) + '\n');
      }
      return;
    }
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
  if (!rootOnlyInSearchPath(rawCmd, root, extractSearchPath(cmd))) return;
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
  const plan = block && !isAnswerDisabled()
    ? rewritePlan(cmd, { isDir: (t) => isDirectory(path.resolve(root, t)) })
    : null;
  if (block && !isAnswerDisabled() && !rewriteMatchesBlock(plan, block, cmd, rawGrepPattern)) return;
  // A pattern the dialect bridge cannot translate (null) has no equivalent search.
  if (block && !isAnswerDisabled() && block.mode === 'grep' && grepPattern === null) return;
  // D#76 — the rewrite searches its root-relative target from the root; the
  // command searched its own operand from the shell's cwd. From a subdirectory
  // the rebase moves only operands that exist there, so an operand it left
  // alone (`src/` from `xtask/`, `tests/` from `src/`) names another directory.
  if (plan && relPrefix && !operandMatches(rawCmd, plan.target, root, shellCwd)) return;
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
        // Scoped to the grep's path: `show` alone answers the whole project
        // (D#125 #2 — `grep -A3 "fn f" lib/` was answered from src/).
        answer = runShowAnswer({
          cwd: root, symbols: block.symbols, within: searchPath ?? '',
          kinds: declKindsBySymbol(extractPatterns(cmd)),
        });
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

    // D#133 #8 — the answer searches tracked files plus ripgrep's walk; the
    // grep's own file set differs when the path holds untracked hidden, ignored
    // or (for git grep) untracked files. Checked only for an answer that would
    // replace the command: it runs git. The grep runs as typed.
    // A `show` answer reads the index instead, which skips more (review of
    // D#133, H-2).
    if (plan && !searchesSameFiles({
      root, target: extractSearchPath(cmd), verb: plan.verb, show: answeredMode === 'show',
      binaryCacheOk: !grepListsBinaryMatches(cmd),
    })) return;

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
    // stderr; the grep being replaced has no cap (round 3 M4). `-M 0`: cg cuts
    // each line at 512 characters, so a match past that column was not in the
    // answer at all (D#133 #12); 0 is unlimited, as grep prints. Only here, not
    // in the in-hook answer, which is a has-hits probe.
    const argvList = answeredMode === 'show'
      ? answer.argvs
      : [[...args.slice(0, 1), '-m', '0', '-M', '0', ...args.slice(1)]];
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
  grepListsBinaryMatches,
  useSourceRoots, readSourceRoots, MAX_SOURCE_ROOTS,
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
  operandMatches,        // D#76 — does a root-relative target name the path the command searched
  segmentCwd,            // D#76 — the directory a compound command's segment runs in
  segmentSeparators,     // D#76 — the separator before each top-level segment
  extractSedReadTargets, // v0.49 — sed-range reads feed the read-fanout state
  extractInPlaceEditTargets, // Q1 — `sed -i` / `perl -pi` targets feed the Stop check's edit log
  extractUnansweredTail, // v0.50 — compound-tail honesty in answered denies
  extractPatterns,    // v0.32.1 — exposed for tests
  countNamedPaths,    // v0.70 — multi-path deny→hint downgrade
  bareSourceTarget,      // D#73 — a bare source dir the rewrite grammar proves is the path operand
  isRevisionScopedGitGrep, // v0.71 — git grep --cached/treeish exclusion
  extractSearchPath,  // v0.47.0 — deny-with-answer
  normalizeCommandPaths, // v0.47.1 — abs-path matcher fix
  rootOnlyInSearchPath, // D#125 #1
  isDirectory,
  declKindsBySymbol, // D#125 #2 review
  searchesSameFiles, // D#133 #8
  indexReadsPath,    // review of D#133 H-2: the index's own skip rules, mirrored
  INDEX_EXCLUDED_DIRS,
  INDEX_EXTENSIONS,
  INDEX_DEFAULT_MAX_FILE_SIZE,
  showAnswersPattern, // D#133 — show answers whole names only
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
