#!/usr/bin/env node
'use strict';
// PreToolUse(Bash) hook: detect raw `grep`/`rg`/`ag` on the indexed source tree
// and either BLOCK with suggestion (v0.32+) or HINT (legacy path). Closes the
// "Bash comfort zone" leak — pre-training bias has Claude reach for `grep -rn`
// ~13× more than the indexed CLI on bash-heavy days (15-day baseline: 429 raw
// grep vs 191 functional CLI). v0.25.0 hint-only had ~0% transfer rate; v0.32.0
// upgrades the narrowest "I'm searching for a symbol" subset to block-with-reason.
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
const crypto = require('crypto');
const { cgTmpDir, cwdHash } = require('./tmp-dir');
const { recordRecommendation } = require('./recommendation-log');
const { runGrepAnswer, runShowAnswer, sanitizeSearchPath } = require('./cg-answer');

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
  if (!SRC_PATH.test(clause)) return false;         // not against indexed source tree
  // If a config file appears AND no source path remains after stripping it, skip.
  if (CONFIG_TARGET_ONLY.test(clause)) {
    const stripped = clause.replace(CONFIG_TARGET_STRIP, ' ');
    if (!SRC_PATH.test(stripped)) return false;
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
  const matches = [...stripped.matchAll(/"([^"]+)"|'([^']+)'/g)];
  return matches.map(m => m[1] !== undefined ? m[1] : m[2]);
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
    if (c === '"' || c === "'") { quote = c; continue; }
    if (c === ';' || (c === '&' && cmd[i + 1] === '&')) {
      const tail = cmd.slice(i + (c === ';' ? 1 : 2)).trim();
      return tail || null;
    }
  }
  return null;
}

// Shared deny-copy footer for the unanswered compound tail. Budget-conscious:
// two lines, verbatim command so the model can re-issue without thinking.
// Wording is path-neutral — it must stay true for BOTH the answered deny
// ("grep half answered, tail wasn't") and the static fallback (nothing was).
// Head-line marker paired with the NOTE below: the NOTE sits at the END of a
// long deny message and Claude Code's transcript view folds long tool errors,
// so a human reading the truncated view saw a clean "answered" deny with no
// clue the compound's tail was dropped (2026-07-18: two such denies were
// misdiagnosed as product bugs). The model gets the full reason either way.
function tailFlagSuffix(tail) {
  return tail ? ' (compound tail NOT run — see NOTE at end)' : '';
}

function appendUnansweredTailNote(lines, tail) {
  if (!tail) return;
  const shown = tail.length > 300 ? tail.slice(0, 300) + '…' : tail;
  lines.push(
    'NOTE: the rest of this compound command (after the grep) did NOT run — re-issue it separately:',
    `$ ${shown}`,
  );
}

// v0.47.0 — pull the first source-tree path token out of the denied command so
// the inline answer can scope its search the same way the raw grep would have.
function extractSearchPath(cmd) {
  if (!cmd || typeof cmd !== 'string') return undefined;
  // v0.96 — scope to the grep's own clause so the answer is never scoped to a
  // path in a non-grep tail (the file the user actually grepped is the only one
  // the "already ran for you" answer may claim to have searched).
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

function commandHash(cmd) {
  return crypto.createHash('sha1').update(cmd).digest('hex').slice(0, 12);
}

// Project-scoped: the same `grep -rn "foo" src/` in two repos is two different
// questions and must not share one 60s cooldown (see cwdHash in tmp-dir.js).
function flagPath(cmd, cwd = process.cwd()) {
  return path.join(cgTmpDir(), `.code-graph-bash-${cwdHash(cwd)}-${commandHash(cmd)}`);
}

function isOnCooldown(cmd, now = Date.now(), windowMs = 60000, cwd = process.cwd()) {
  try {
    return now - fs.statSync(flagPath(cmd, cwd)).mtimeMs < windowMs;
  } catch { return false; }
}

function markCooldown(cmd, cwd = process.cwd()) {
  try { fs.writeFileSync(flagPath(cmd, cwd), ''); } catch { /* ok */ }
}

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

function buildBlockReason(unansweredTail) {
  // Shown to Claude via PreToolUse `decision: block` reason. Must give a
  // concrete alternate command Claude can re-issue without further thinking.
  // v0.49 — NO escape-hatch line anywhere in deny copy: the daagu 2026-06-12
  // night proved even the "THIS command only" scoping reads as a teachable
  // permanent prefix (adopted in 8s, reused 11×, incl. on the exact identifier
  // searches this hook targets). The env opt-out stays documented in README.
  const lines = [
    `[code-graph] Raw \`grep -rn\` on indexed source — denied by code-graph hook.${tailFlagSuffix(unansweredTail)}`,
    'Use the AST-aware equivalent (returns containing fn/module per hit, repo-wide):',
    '  code-graph-mcp grep "<pattern>" [paths...]      # AST context per hit; -F literal, -i, -w, -l, -C N, --max-count 0',
    '  code-graph-mcp ast-search "<pattern>" --type fn # filter by node type',
    '  code-graph-mcp callgraph SYMBOL                 # callers + callees',
  ];
  appendUnansweredTailNote(lines, unansweredTail);
  return lines.join('\n');
}

// v0.47.0 — deny WITH the answer inline. Hint-only had ~0% transfer and a bare
// deny still asks the model to initiate a new tool call; embedding the actual
// results removes that choice entirely.
// v0.48 — NO escape-hatch line here: the model already has the results, and
// advertising the bypass taught it a permanent prefix within 5 seconds (daagu
// 2026-06-11: one deny → 14 bypassed greps). The static deny keeps a scoped
// escape because there we give no answer.
function buildBlockReasonWithAnswer(pattern, searchPath, answer, unansweredTail) {
  const cmdShown = `code-graph-mcp grep "${pattern}"${searchPath ? ` ${searchPath}` : ''}`;
  const lines = [
    `[code-graph] Raw \`grep\` on indexed source — denied; the AST-aware equivalent already ran for you:${tailFlagSuffix(unansweredTail)}`,
    `$ ${cmdShown}`,
    answer.text,
  ];
  if (answer.truncated) {
    lines.push(`(truncated — run \`${cmdShown}\` yourself for the full list)`);
  }
  lines.push(
    'Each hit shows its containing fn/module — use these results directly instead of re-running the search.',
  );
  // NOTE (v0.63): a forced-ack salience line was trialed here and removed. The
  // answer is ALREADY in context, so demanding the model restate which hit it
  // will use risks performative compliance + friction with no measured gain
  // (deny-with-answer already satisfies in-place, 5/5 in the daagu replay). The
  // engagement nudge is kept only where it pays — the pre-edit impact summary,
  // a true before-you-edit reconciliation moment. Re-add here only behind an A/B.
  appendUnansweredTailNote(lines, unansweredTail);
  return lines.join('\n');
}

// v0.49 — show-mode deny: the model grepped for symbol DEFINITIONS with
// context flags (-A/-B/-C = "show me the body"); the answer IS the bodies,
// fetched via `code-graph-mcp show`. answer.text already carries per-symbol
// `$ code-graph-mcp show <sym>` headers.
function buildShowDenyReason(answer, unansweredTail) {
  const lines = [
    `[code-graph] Raw grep for symbol definitions — denied; here are the definitions from the AST index:${tailFlagSuffix(unansweredTail)}`,
    answer.text,
  ];
  if (answer.truncated) {
    lines.push('(truncated — re-run the `code-graph-mcp show <symbol>` command above for full source)');
  }
  lines.push('Use these directly instead of re-running the search.');
  // NOTE (v0.63): forced-ack salience trialed + removed here too — see
  // buildBlockReasonWithAnswer. The definitions are already delivered.
  appendUnansweredTailNote(lines, unansweredTail);
  return lines.join('\n');
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
  if (/(?:^|\s)-[a-zA-Z]*[EP][a-zA-Z]*(?:\s|=|\d|$)|--(?:extended-regexp|perl-regexp)\b/.test(cmd)) {
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
function buildUnavailableFyi(pattern, status) {
  const why = status === 'no-binary' ? 'binary not found' : 'ran but failed';
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
  if (!shouldHint(cmd)) return;

  // v0.64 — fingerprint the grep's pattern once, shared by the emit points below.
  // The funnel (aggregate_recommendations_jsonl) uses it to tell a verbatim re-grep
  // of an answered deny (inline answer ignored → fall-through) from a deeper
  // drill-down. undefined when there's no identifier-like pattern (unquoted / prose
  // grep) → omitted from the event, so the funnel stays back-compatible.
  const grepPattern = translateBreToRg(cmd, pickBlockPattern(cmd));

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

  const block = isBlockDisabled() ? null : classifyBlock(cmd);
  if (block) {
    // v0.47.0 — run the AST-aware equivalent inside the hook and embed the
    // results in the deny reason ("answer in the deny"). Degrades to the
    // v0.46 static deny on any failure; downgrades to allow+FYI on 0 hits
    // (regex-dialect differences mean 0 hits ≠ proof of absence).
    // v0.49 — intent-aware: declaration+context greps get `show` bodies,
    // falling back to the grep answer, then the static deny.
    let answer = { status: 'unavailable' };
    const pattern = grepPattern; // computed once above; reused for the answer + deny fingerprint
    // v0.48 — glob-truncated once, shared by the run and the deny message
    // (a literal `dir/*.py` argv made rg exit 1 → answered:false static deny).
    const searchPath = sanitizeSearchPath(extractSearchPath(cmd));
    let answeredMode = block.mode;
    if (!isAnswerDisabled()) {
      if (block.mode === 'show') {
        answer = runShowAnswer({ cwd: root, symbols: block.symbols });
        if (answer.status !== 'hits' && pattern) {
          answeredMode = 'grep';
          answer = runGrepAnswer({ cwd: root, pattern, searchPath });
        }
      } else if (pattern) {
        answer = runGrepAnswer({ cwd: root, pattern, searchPath });
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
      recordRecommendation(root, { hook: 'grep', action: 'hint', fallthrough: answer.status });
      process.stdout.write(
        (answer.status === 'no-hits'
          ? buildNoHitsFyi(pattern)
          : buildUnavailableFyi(pattern, answer.status)) + '\n');
      return;
    }

    // PreToolUse block via current CC schema (`hookSpecificOutput.permissionDecision`).
    // Verified empirically 2026-05-24: legacy `{decision:"block",reason}` was
    // ignored by Claude Code — the grep ran anyway. The hookSpecificOutput form
    // is the documented modern path. Exit 0 — this is a routing decision, not
    // a hook failure (exit 2 would mark the tool call as "hook errored").
    const answered = answer.status === 'hits';
    // v0.50 — flag the unanswered `;`/`&&` tail. Extracted from rawCmd: its
    // paths are valid in the model's shell cwd, where the re-issue will run.
    const unansweredTail = extractUnansweredTail(rawCmd);
    recordRecommendation(root, {
      hook: 'grep', action: 'deny', answered,
      // pattern fingerprints the denied search so the funnel can score a verbatim
      // re-grep of it (the inline answer was ignored) as fall-through, not a win.
      ...(pattern ? { pattern } : {}),
      // mode segments which answer type converts (show=bodies, grep=hits).
      ...(answered ? { mode: answeredMode } : {}),
      // reason segments WHY an unanswered deny fell back to the static copy:
      // 'no-binary' (flagship answer-in-deny dark — binary missing) vs
      // 'unavailable' (binary ran but failed/timed out). Without this the two
      // are indistinguishable in the funnel ("broken" looks like "no hits").
      ...(answered ? {} : { reason: answer.status }),
      // tail segments compound-command denies — lets the funnel compare
      // re-issue behavior for denies that carried a tail note.
      ...(unansweredTail ? { tail: true } : {}),
    });
    process.stdout.write(JSON.stringify({
      hookSpecificOutput: {
        hookEventName: 'PreToolUse',
        permissionDecision: 'deny',
        permissionDecisionReason: !answered
          ? buildBlockReason(unansweredTail)
          : answeredMode === 'show'
            ? buildShowDenyReason(answer, unansweredTail)
            : buildBlockReasonWithAnswer(pattern, searchPath, answer, unansweredTail),
      },
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
  splitTopLevelSegments, // compound-grep — quote-aware top-level segment splitter (PostToolUse reuse)
  firstShellClause,      // v0.96 — grep's own clause (up to first top-level separator)
  extractDeclSymbols,    // v0.49 — show-mode symbol extraction
  translateBreToRg,      // v0.49 — BRE→rust-regex dialect bridge
  buildShowDenyReason,   // v0.49 — show-mode deny copy
  extractSedReadTargets, // v0.49 — sed-range reads feed the read-fanout state
  extractUnansweredTail, // v0.50 — compound-tail honesty in answered denies
  extractPatterns,    // v0.32.1 — exposed for tests
  countNamedPaths,    // v0.70 — multi-path deny→hint downgrade
  isRevisionScopedGitGrep, // v0.71 — git grep --cached/treeish exclusion
  extractSearchPath,  // v0.47.0 — deny-with-answer
  normalizeCommandPaths, // v0.47.1 — abs-path matcher fix
  resolveProjectRoot,    // v0.48 — subdir-cwd dark fix
  rebaseRelativePaths,   // v0.48 — subdir-cwd dark fix
  commandHasBypass,      // v0.48 — bypass funnel visibility
  pickBlockPattern,
  buildHint,
  buildBlockReason,
  buildBlockReasonWithAnswer,
  buildNoHitsFyi,
  buildUnavailableFyi,   // v0.92 — allow-on-unavailable breadcrumb
  commandHash,
  isOnCooldown,
  markCooldown,
  isSilenced,
  isBlockDisabled,
  isAnswerDisabled,
};
