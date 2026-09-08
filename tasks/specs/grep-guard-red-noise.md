---
status: draft
revision: 1
---

# The grep guard's red denies: one that shouldn't fire, and two that lie

## Goal

A `grep` the guard folds should either be denied with an answer that is
genuinely equivalent, or not denied at all. No red error whose only content is
"I threw away half your command", and no "the AST-aware equivalent already ran
for you" above a command that is not the equivalent.

## Evidence (verified at `9567b09`)

Field report, three red blocks. Two are the same defect in
`claude-plugin/scripts/pre-grep-guide.js`; the third belongs to another plugin.

### 1. The deny drops the compound's tail — and never had to

`grep -n "setup_saturating_pool_project" tests/cli_e2e.rs | head; echo "---";
sed -n '1190,1250p' tests/cli_e2e.rs` is denied whole. The answer covers the
grep; the `sed` range read — the thing the model actually wanted next — is
discarded, and the deny says so in a NOTE.

- `extractUnansweredTail()` (`:373`) already detects the tail. It is spent only
  on decorating the deny (`:405 tailFlagSuffix`, `:409
  appendUnansweredTailNote`). The deny still fires.
- The module already holds the rule that settles this. `:260`: *"only DENY when
  the inline grep answer can cover the SAME scope"* — a grep naming ≥2 paths is
  downgraded to hint because a first-path-only answer is "an incomplete
  substitute that rationally teaches CODE_GRAPH_NO_BLOCK_GREP bypass". A dropped
  `; sed …` is the same incompleteness, and was never put under the same rule.
- `:773-779` records the cost in the field: *"the only 2 non-converting denies
  were `unavailable`, one a `def render` compound cmd that then half-ran — grep
  blocked, `; python3 …` tail dropped, no result"*. The response was a NOTE.
- **The permission-neutral route already exists and already handles this
  shape.** `post-grep-inject.js` is a PostToolUse hook whose entire purpose is
  compound greps; its `findFoldableGrepSegment()` walks every top-level segment
  and has **no head-is-grep exclusion**. Head-grep compounds never reach it only
  because PreToolUse denies them first. It also carries the redundancy gate
  (`grepFoundPattern`) that suppresses the inject when the model's own grep
  already surfaced the symbol, and a DISTINCT cooldown prefix (`postinject`), so
  the PreToolUse cooldown cannot suppress it.

The result is an asymmetry with no user-visible logic:

| command | today |
|---|---|
| `echo x && grep Sym tests/` | runs whole, answer injected, no red |
| `grep Sym tests/ && echo x` | denied red, `echo x` dropped |

### 2. The "equivalent" command drops the grep's flags

`grep -rln "applyTierFilter\|tier:" tests/*.mjs` — the reporter wanted a file
list. The deny printed `code-graph-mcp grep "applyTierFilter|tier:" tests` and
delivered 25+ hit lines.

`buildBlockReasonWithAnswer` (`:576`) renders `grep "<pattern>" <path>` with no
flags, and `runGrepAnswer` (`cg-answer.js:204`) builds argv `['grep', pattern,
scope]`. Every flag is dropped. `code-graph-mcp grep` supports `-i -w -F -l -c`
(this file's own help text lists them at `:560`), verified against the built
binary: `-l` prints the 3 paths, `-c` prints `path:count` exactly as `grep -c`
does.

### 3. The path glob is silently widened

`sanitizeSearchPath` (`cg-answer.js:91`) truncates at the first glob segment, so
`tests/*.mjs` becomes `tests`. The reason is real — a literal glob in argv made
rg exit 1 — but the deny then prints a command claiming a scope the user did not
ask for. `code-graph-mcp grep -g '*.mjs'` is the faithful mapping, verified:
`-g '*.js'` matches nested paths basename-style.

### Not this repository

The `rm -rf "$SB"` deny is claudemd's §8 hook (source at `~/dev/claudemd`), and
it fires on the exemption its own message advertises (`D=$(mktemp -d) … rm -rf
"$D"`), which the reporter had used. Diagnosed separately; no code change here.

## Approach

1. `classifyDeny(cmd)` — the deny gate as `runMain` applies it: `classifyBlock`
   says the answer covers the grep, AND no top-level `;`/`&&` tail would be
   discarded. Null on a tail → `runMain` emits nothing, the command runs whole
   under the normal permission flow, and post-grep-inject delivers the answer.
   `classifyBlock` itself is untouched, so post-grep-inject (which calls it per
   segment, where no tail can exist) keeps its exact behaviour.
2. Carry the grep's own flags into the answer: `-i -w -F -l -c` and
   `--include=GLOB` → `-g GLOB`, short clusters (`-rln`) decomposed.
3. Split a globbed path into scope + `-g`: `tests/*.mjs` → `tests` plus
   `-g '*.mjs'`.
4. The argv that runs and the command string the deny prints come from ONE
   builder. Two renderings of the same command drifting apart is this
   repository's recurring defect (v0.142.0 shipped a fix for exactly that).

## Non-goals

- **Not emitting `permissionDecision: 'allow'`** for the compound case. That
  would skip the user's normal Bash permission prompt, which post-grep-inject's
  header comment already names as the thing not to do. The hook stays silent
  and the default flow decides.
- **Not widening to `|` or `||` tails.** A pipe is the same pipeline (the answer
  replaces the whole of it), and the `||` branch would not have run given the
  answer delivered hits. Both keep denying.
- **Not touching the `-A/-B/-C` context path.** Those already route to show-mode
  or to hint; they never reach the grep answer's flag set.
- Not changing `sanitizeSearchPath`'s signature — it is the defensive
  re-sanitize on four call sites. The split is a new function beside it.

## Constraints

- Every flag mapped must be one `code-graph-mcp grep` actually accepts, verified
  against the binary rather than against the help text.
- `-F` means the pattern is literal, so the BRE→rust-regex unescape
  (`translateBreToRg`) must not also run on it.
- Deny copy and executed argv must not be able to disagree.
- L3 by core §2: released-artifact user-visible default behaviour change, on the
  hook that gates every Bash grep.

## Success criteria

1. A head-grep compound with a `;` or `&&` tail produces no hook output at all;
   the same command with `|` or `||`, and the same command without a tail, still
   deny. Asserted end to end through the existing stdin-spawn harness.
2. `grep -rln "Sym" src/` runs the answer with `-l` in argv and prints `-l` in
   the deny's command line, from one builder.
3. `grep -rn "Sym" tests/*.mjs` runs with scope `tests` and `-g '*.mjs'`, both
   shown.
4. No deny message can carry a compound-tail NOTE, because no deny can carry a
   tail. The tail-note apparatus goes with it.

## Open questions

- None blocking. One judgment recorded: compound greps lose the deny's
  in-context answer whenever post-grep-inject's redundancy gate suppresses the
  inject. That is the intended trade — the gate suppresses precisely when the
  model's own grep already printed the hits, which it now actually gets to run.

# Change log

- r1 (2026-09-08): drafted from the three-red-block field report at `9567b09`.
