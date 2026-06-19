# Changelog

## v0.54.1 — edge confidence polish (post-review follow-ups)

Two non-blocking items from the v0.54.0 integration review. No schema change,
no `INDEX_VERSION` bump.

- **fix(cli)**: `refs` dedup keeps the **lowest** confidence among collapsed
  siblings. The dedup key is `(name, file_path, relation)` — two edges from one
  source to different same-name targets collapse to one row; the displayed
  confidence now shows the most conservative tier instead of an arbitrary
  first-wins value, so it can't understate a hidden sibling's ambiguity.
- **perf(indexer)**: Phase 2e (`classify_edge_confidence`) is now skipped when an
  index pass indexed and deleted nothing — the full-graph classification UPDATE
  was a guaranteed no-op there (e.g. query-time freshness checks where the file
  hash matched). It still runs globally whenever anything changed, since a
  duplicate-named node added/removed in one file flips cross-file edge ambiguity.

## v0.54.0 — edge confidence tiers

Edges now carry a **resolution confidence** so consumers can tell precise edges
from heuristic by-name guesses. **Schema migration** — `INDEX_VERSION 16→17`
forces a one-time index rebuild on first run.

### New: `edges.confidence` (extracted | inferred | ambiguous)

- A single post-resolution classification pass (`classify_edge_confidence`,
  Phase 2e) assigns confidence — **not** threaded through the ~10 insert sites.
  Purely additive metadata: **no edge is added or removed**, no default behavior
  change to `impact`/`dead-code`/`affected`.
  - `extracted`: same-file resolution, or a structural relation (imports /
    inherits / implements / routes_to / exports). Precise.
  - `inferred`: a cross-file `calls`/`references` edge resolved by bare name,
    target name unique among same-language nodes.
  - `ambiguous`: cross-file by-name where >1 same-language node shares the name
    — the class behind known false positives (bare-name calls, method-call drops,
    value-reference flood).
- The classification is idempotent and recomputed every index pass (an `inferred`
  edge becomes `ambiguous` when a duplicate-named sibling is later added, and back).

### `refs` exposes + filters by confidence

- `code-graph-mcp refs <symbol>` annotates non-`extracted` refs (`~inferred` /
  `~ambiguous`) and accepts `--min-confidence extracted|inferred|ambiguous` to
  hide lower-confidence edges (default: show all). JSON gains a `confidence` field.
- (MCP `find_references` output is unchanged in this release — CLI-first; a future
  release may surface confidence on the published tool schema.)

## v0.53.0 — `centrality`: architectural chokepoints

New CLI command that ranks functions by **betweenness centrality** over the call graph.
Read-only over the existing index — no `INDEX_VERSION` bump, no breaking changes.

### New: `code-graph-mcp centrality [--limit N] [--include-tests] [--json]`

- Surfaces the *structural bridges* — functions lying on the most shortest call paths
  between other functions. This is orthogonal to `map`'s `caller_count` "hot functions"
  (degree centrality): a chokepoint can have **few callers** yet route most cross-cluster
  traffic. (On this repo, `walk_for_relations` has 1 caller but ranks #3 by betweenness.)
- Exact **Brandes' algorithm** (O(V·E)) over the directed `calls` edge set; directed
  scores are not halved. Reports raw betweenness, a size-normalized `(n-1)(n-2)` figure,
  and `caller_count` side-by-side. Top `--limit` (default 15).
- Test symbols are excluded from the graph entirely (endpoints **and** intermediate hops)
  unless `--include-tests`, reusing the canonical `is_test_symbol` classifier rather than
  a parallel SQL heuristic.
- **CLI-only by design** — the model reaches it via Bash; no new MCP tool, keeping the
  LLM-visible instructions budget and tool-routing surface unchanged. The algorithm core
  lives in `src/graph/centrality.rs` (pure, unit-tested).

### New: PR impact review GitHub Action

- `.github/workflows/pr-impact-review.yml` posts a **sticky comment** on each PR with a
  code-graph `affected` analysis: test files to re-run, the blast radius, and changed
  **production files with no covering test** ("test gaps"). Productizes the already-shipped
  `affected` command — no new graph logic.
- Comment rendering + the gh sticky-upsert (find-by-marker → PATCH-or-POST) live in
  `claude-plugin/scripts/pr-impact-comment.js`; the render path is pure and unit-tested
  (`pr-impact-comment.test.js`, in the CI plugin-test set). Binary calls carry
  `CODE_GRAPH_INTERNAL=1` so CI runs never inflate the deny→use conversion funnel.
- **Opt-in merge gate**: repo variable `CODE_GRAPH_FAIL_ON_RISK=true` fails the job when a
  changed production file has no test in its reverse-dependency closure (default: off).
- Minimal permissions (`contents: read`, `pull-requests: write`); builds `--no-default-features`
  (impact analysis needs no embeddings), reusing the pinned `1.95.0` toolchain + rust-cache.

## v0.52.0 — `tour`: dependency-ordered reading order

New CLI command that answers "where do I start reading this repo?" deterministically.
Read-only over the existing index — no `INDEX_VERSION` bump, no breaking changes.

### New: `code-graph-mcp tour [PATH] [--json]`

- Lists modules in dependency order — prerequisites before the modules that build on
  them — via a Kahn topological sort over the project-map import edges. Each module is
  annotated with a role (`entry` / `foundational` / `core` / `mid`), its depended-on-by
  count, its in-scope imports, and key symbols. Optional `PATH` scopes the tour to a
  subtree; `--json` emits a `{"reading_order": [...]}` envelope.
- **Cycle-tolerant**: import cycles are broken deterministically (smallest unresolved
  in-degree, then lexical order) and flagged, so the output is stable for a fixed index.
- **CLI-only by design** — the model reaches it via Bash; no new MCP tool, keeping the
  LLM-visible instructions budget and tool-routing surface unchanged.
- Reuses the existing project-map graph (`get_project_map`); the ordering core lives in
  `src/graph/reading_order.rs` (pure, unit-tested).

## v0.51.1 — `affected` correctness fixes (post-release code review)

Fix-forward for defects found by a max-effort code review of v0.51.0's `affected`
command. No breaking changes; no `INDEX_VERSION` bump.

### `affected` correctness

- **fix(cli)**: the reverse-dependency closure now walks **all** dependency relations
  (imports ∪ calls ∪ references ∪ implements ∪ inherits) via a new
  `get_reverse_dependents`, not just imports ∪ calls. Test files that only `reference`
  (or implement/inherit) a changed symbol were silently dropped from the "tests to
  re-run" set — e.g. `affected src/domain.rs` went from 11 → 15 test files.
- **fix(cli)**: apply the cross-language compatibility filter (now shared by `affected`,
  `deps`, and the dependency-graph tool) so bare-name resolution false positives in a
  different language no longer leak into the blast radius.
- **fix(cli)**: `affected_files` is disjoint from `changed`; inputs normalizing to `""`
  (`.`/root) are skipped; a nonexistent test-path input is reported in `not_indexed`
  only (never both `not_indexed` and `tests`); `not_indexed` reports the raw input
  consistently; `--stdin` reads bytes lossily so a non-UTF-8 path can't break `--json`.
- **fix(domain)**: `is_test_path` now matches `.spec.tsx`/`.spec.jsx`.

### Honesty / consistency

- **fix(health)**: the text `health-check` prints the `Resolution:` line even on an
  unhealthy index, matching the unconditional `--json` block.
- **docs**: corrected the `is_test_path` "single source of truth" overclaim (the SQL
  filters and a `resolve.rs` closure are intentionally divergent copies) and the
  resolution-block relation list (it counts every relation, not just calls/imports/references).

### Known backlog (not in this release)

- `resolution_stats` runs a `GROUP BY` over edges on every `health-check` (perf on huge repos).
- a negative `--depth` clamps to 1 + exits 0 (consistent with `impact`'s flag form,
  inconsistent with the positional-numeric exit-2 audit convention).

## v0.51.0 — `affected` command (changed files → impacted tests) + resolution-coverage metric

Additive minor release. No breaking changes. No `INDEX_VERSION` bump (both features
read existing edge data; no extraction change). Adopted from a competitive scan of
[colbymchenry/codegraph](https://github.com/colbymchenry/codegraph).

### `affected` — reverse-impact test selection

- **feat(cli)**: new `code-graph-mcp affected [files...]`. Given changed files
  (positional or `--stdin` from `git diff --name-only`), it walks the reverse
  import ∪ call closure and reports the **test files to re-run** (primary) plus the
  full affected-file blast radius with depths (secondary). Flags: `--stdin`,
  `--depth N` (default 10), `--json`. Reuses the existing `get_import_tree(incoming)`
  traversal, inheriting its cycle-guarding and import-only-dependent coverage. Inputs
  go through `normalize_user_path`; unindexed inputs are reported in `not_indexed`
  (the `--json` envelope is same-shape on every path). Registered as an adoption-funnel
  conversion. Example: `git diff --name-only | code-graph-mcp affected --stdin`.

### Graph resolution-coverage metric

- **feat(health)**: `health-check --json` gains a `resolution` block —
  `pending_unresolved_calls` + per-language edge counts grouped by relation (calls,
  imports, references, implements, inherits, …), a pure aggregate over existing edges
  via a single `GROUP BY`. The text output gains
  a `Resolution:` summary line. Makes silent edge-resolution regressions observable
  instead of human-discovered after shipping.

### Internal

- **refactor(domain)**: extracted `is_test_path` (file-level test predicate) from
  `is_test_symbol`, which now delegates — single source of truth, behavior preserved.
- **test(coverage)**: new `tests/edge_coverage.rs` — multi-language (TS/Python/Rust)
  per-language edge-count baseline + a file-scoped method→sibling-method resolution
  invariant (guards the `method_call_edge_drops` class fixed at `INDEX_VERSION` 16).

## v0.50.0 — grep parity (BREAKING: exit codes) + auto-update chain audit

**Migration note (breaking)**: `code-graph-mcp grep` now uses grep-compatible exit
codes — **0 = matched, 1 = no match, 2 = error/usage** (previously: 0 on no-match,
1 on most errors). Scripts that treated any nonzero exit as failure must distinguish
1 (no match) from 2 (trouble). Revert path: pin the previous version
(`npm i @sdsrs/code-graph@0.49.0` / `cargo install code-graph-mcp --version 0.49.0`).
The bundled hook consumer (`cg-answer.js`) is updated in the same release and accepts
both old and new shapes.

### Auto-update chain audit (plugin shell / marketplace clone / binary / model)

- **fix(plugin)**: stale-relic downgrade war — a still-running Claude Code process
  fires SessionStart from the plugin-cache dir it loaded at startup; after
  auto-update installed vN+1, the vN scripts' `syncLifecycleConfig` saw
  `manifest.version !== currentVersion` and (direction-blind) called `update()`,
  dragging the install manifest and all six settings.json hook/statusline paths
  back to vN — upgrade↔downgrade ping-pong until the next auto-update re-ran
  (observed live 2026-06-12→13: manifest 0.49.0 → 0.48.0 fifteen minutes after a
  successful update; settings re-downgraded again after doctor re-registered).
  `syncLifecycleConfig` now defers to `installed_plugins.json` as the authority:
  a script running from a cache dir that is NOT the active installPath returns
  `deferred-to-active-install` and touches nothing. Deliberate downgrades via
  /plugin keep self-heal rights (installPath then IS the old dir); dev checkouts
  and npm installs are exempt. (`isStaleRelicContext` in lifecycle.js +
  install-e2e §1.11.) A relic SessionStart also skips the auto-adopt template
  refresh (it would "refresh" MEMORY.md back to the OLD shipped table), and
  doctor's settings-writing repairs (`hooks-invalid` / `missing-hooks-in-settings`)
  get the same guard with a redirect to the active copy (`relicRepairGuard`).
- **fix(plugin)**: marketplace clone staleness — auto-update wrote the plugin
  cache + installed_plugins.json but never touched
  `~/.claude/plugins/marketplaces/code-graph-mcp`, so its marketplace.json stayed
  at whatever version the last manual /plugin command saw (observed: 0.48.0 four
  days after 0.49.0 shipped), making the /plugin UI lie and letting Claude Code
  reinstall old plugin files from it. After a successful shell update, auto-update
  now fast-forwards the clone (`git pull --ff-only`, silent no-op on dirty/
  diverged/missing-git). State gains `marketplaceRefreshed`.
- **fix(embedding)**: model cache was existence-only and unverified — the
  embedding model (`models.tar.gz` → `~/.cache/code-graph/models/`) downloaded
  once and was never version-checked (same fault class as the v0.45.x native-
  binary pin) nor integrity-checked (the published `.sha256` only self-validates
  the bundle). The binary now pins the expected `model.safetensors` content
  (`MODEL_CONTENT_BLAKE3`); downloads verify-or-reject (mismatched weights are
  deleted, not loaded), the cache check is identity-aware (`.model-id` marker,
  one-time hash migration for pre-existing caches), and a future model change
  re-downloads automatically instead of pinning forever. Offline/stale caches
  still load (graceful degradation unchanged).
- **ci(release)**: the "Package model files" step now `sha256sum -c` pins all
  three HF files against known-good hashes of the pinned revision, so a
  compromised HF response or a revision bump without updating the client-side
  `MODEL_CONTENT_BLAKE3` fails the release instead of shipping mismatched weights.

- **fix**: leading-dash patterns work — `grep "--no-default-features"` no longer
  parses as a flag (clap `allow_hyphen_values` + `--` separator before the rg pattern).
- **fix**: per-file truncation is surfaced — files hitting the match cap are listed on
  stderr; new `--max-count N` flag controls the cap (`0` = unlimited, default 100).
- **fix**: repo-wide searches now include tracked-but-gitignored files (`git ls-files
  -ci` supplement) — `git grep` semantics; previously a `docs/` ignore rule silently
  hid git-tracked docs. Untracked ignored files stay skipped.
- **fix**: `... | head` no longer prints `Error: Broken pipe (os error 32)` — EPIPE
  ends output silently with exit 0, like grep.
- **feat**: `-i/--ignore-case`, `-w/--word-regexp`, `-F/--fixed-strings` (literal
  search — regex-hostile patterns like `res.json(` work with `-F`), multiple path
  arguments.
- **feat**: `-l/--files-with-matches` (bare paths; `--json` → array of strings) and
  `-A/-B/-C N` context lines (grep-style `:` match / `-` context separators with
  `--` between groups; AST annotation stays on match lines only; `--json` context
  entries carry `"context": true`).
- **perf**: `--json` mode reuses the per-file AST-node cache (was one DB query per
  match).
- **fix(plugin)**: grep denies on compound commands (`grep …; sed -n 1,60p f` /
  `grep … && wc`) now flag the unanswered `;`/`&&` tail with a verbatim re-issue
  line — on all three deny paths (grep-answer, show-answer, and the static
  fallback). Previously the whole command was blocked but the deny copy ("use
  these results directly instead of re-running") silently swallowed the tail's
  intent (2026-06-13 mem-project deny dropped a `sed` read of the file's first
  60 lines). `||` tails stay unflagged: with hits delivered, the on-failure
  branch would not have run anyway. Quote-aware — separators inside pattern
  quotes don't trigger. Deny records in `recommendations.jsonl` gain `tail: true`
  when a note was carried, so the funnel can segment re-issue behavior
  (`jq 'select(.tail)'` until `stats` breaks it out).
- **feat**: query-time freshness for AST annotations — before annotating, each
  matched file is hash-compared and lazily re-indexed when dirty (parity with the
  MCP tools' `ensure_file_indexed`), bounded by a sync budget
  (`CODE_GRAPH_GREP_SYNC_BUDGET`, default 8 files) and a 250ms SQLite busy
  timeout; beyond budget or under write contention annotations carry `[stale]`
  (JSON: `"stale": true`) with a stderr hint. Cost: +6.7ms on a repo-wide search
  (29.6→36.3ms avg).

## v0.49.0 — feat: answer-delivery upgrade — every guidance surface delivers results, and the funnel finally sees failures

First valid funnel reading (daagu, 6 sessions / ~3.5h real coding, 2026-06-12: 53 hook
events, 0 MCP calls, 3 post-deny CLI conversions) showed which levers work: delivered
answers satisfy in place (5/5), advice converts ~0 (0/40 hints), and the only conversions
were CLI invocations seconds after a deny. This release rebuilds around that evidence.

**What changes for users** (migration: nothing to do; opt-outs below):

- **Deny tier broadened, intent-aware** (`pre-grep-guide.js`): declaration-anchor greps
  with context flags (`rg "def X|class Y" -A 25`) are denied WITH the symbol bodies via
  `code-graph-mcp show`; `-l` / `--include` symbol greps are denied with the grep answer.
  `-L` / `-v` / `--exclude*` (intents the answer can't honor) stay hint. Replay on the
  daagu night: deny-class coverage 20 → 35 of 128 head-greps, hint 28 → 13.
- **Deny copy never mentions the escape env again** — the v0.48 "THIS command only"
  scoping was adopted as a permanent prefix within 8 seconds and reused 11× that night.
  `CODE_GRAPH_NO_BLOCK_GREP=1` still works; it is documented here, not taught in-band.
- **BRE→rust-regex bridge**: plain-grep patterns (`a\|b`) are unescaped before the
  answer runs — both `answered:false` denies that night were dialect/path-shape misses.
- **Read-fanout hint now DELIVERS the module overview** (`pre-read-guide.js`): on the
  5th same-dir read the hint embeds `code-graph-mcp overview <dir>` output. The read
  hook also gets the v0.48 subdir-cwd fix it missed (it had recorded ZERO events in
  daagu history) plus fd-0 stdin. `sed -n X,Yp` source reads now count toward fanout.
- **Edit→impact injection un-darkened** (`pre-edit-guide.js`): same subdir-cwd + stdin
  fixes (daagu: 115 edits, zero impact injections); impact CLI call uses the resolved
  binary; injections are recorded to the funnel.
- **Funnel instrument fixes (Rust)**: 0-tool-call sessions with in-window hook traffic
  now flush a usage record — previously the deny→use denominator could only contain
  converted sessions, making 0% conversion structurally unobservable. Model-initiated
  CLI queries record `{hook:"cli",action:"use"}` (hook-internal runs carry
  `CODE_GRAPH_INTERNAL=1` and are excluded). `stats` segments denies answered/static,
  reports CLI uses, and funnel conversion is any-use (mcp OR cli) with separable legs.
- **Guidance surfaces lead with the CLI** (MCP `instructions`, adopt index line,
  decision-table template): in Claude Code the MCP tools are deferred behind ToolSearch
  while Bash is always live. Adopted projects realign at next SessionStart.

**Opt-out / revert**: `CODE_GRAPH_QUIET_HOOKS=1` silences hooks; `CODE_GRAPH_NO_BLOCK_GREP=1`
downgrades block→hint per command (still measured); `CODE_GRAPH_NO_ANSWER_IN_DENY=1`
restores advice-only denies AND advice-only read hints; or pin `@sdsrs/code-graph@0.48.0`.
usage.jsonl / recommendations.jsonl shapes are additive — no schema bump, no index rebuild.

## v0.48.0 — fix: grep guard survives `cd` into subdirs; deny stops teaching its own bypass

Field reading of the v0.46 deny→use funnel on a real consumer project (daagu, 4 sessions /
~3h real coding, 2026-06-11) found the instrument itself mostly dark: of ~98 grep-bearing
commands the hook engaged with only 3. Four independent causes, all fixed:

- **Subdir-cwd dark (38/40 head-greps)**: the hook's `process.cwd()` follows the
  persistent shell, so after the model ran `cd backend/`, the `.code-graph/index.db`
  gate failed silently for the rest of the session. Now `resolveProjectRoot` walks up
  to the nearest ancestor holding the index (stops at `$HOME`), and bare
  subdir-relative path args (`app --include=*.py` from `backend/`) are rebased onto
  the project root with existence-verified probing. Recommendations are always
  recorded at the root; no `.code-graph` is ever created in subdirs.
- **Deny no longer advertises its own escape**: the answered deny used to end with
  "re-run with `CODE_GRAPH_NO_BLOCK_GREP=1` prepended" — one deny taught the model a
  permanent prefix within 5 seconds (14 subsequent greps that night, including symbol
  searches). The answered deny now carries no escape line (the results are already
  there); the static deny scopes it to "THIS command only — a per-command escape,
  not a default prefix".
- **Bypassed greps are now visible to the funnel**: `CODE_GRAPH_NO_BLOCK_GREP=1 grep …`
  (bare KEY=VALUE prefix, exactly what the deny taught) failed the command-head regex
  and was invisible — not even recorded. The head regex now accepts bare assignment
  prefixes and a bypassed source-grep records `{action:"bypass"}` then stays silent.
- **Glob search paths no longer break answer-in-the-deny**: the hook spawns the CLI
  without a shell, so a literal `backend/…/llm_engine/*.py` reached ripgrep as a
  nonexistent file → exit 1 → static deny with `answered:false` (the night's only
  deny failed exactly this way — on a query that should have been the honest
  no-hits→allow). Glob segments are now truncated (`…/llm_engine/*.py` →
  `…/llm_engine`) before both the run and the displayed command.

**Opt-out / revert**: unchanged — `CODE_GRAPH_QUIET_HOOKS=1` silences the hook,
`CODE_GRAPH_NO_BLOCK_GREP=1` downgrades block→hint (now per-command and measured),
`CODE_GRAPH_NO_ANSWER_IN_DENY=1` restores the static deny, or pin `@sdsrs/code-graph@0.47.1`.
Rust/SQLite surface untouched (the stats aggregator accepts the new `bypass` action
generically); no index rebuild needed.

## v0.47.1 — fix: grep guard now matches absolute paths (it was missing ~97% of real traffic)

The deny/hint tier of `pre-grep-guide.js` only matched **relative** source paths
(`grep -rn "X" backend/app/…`), but Claude Code's harness explicitly steers Bash toward
**absolute** paths — so `grep -rn "X" /abs/project/backend/app/…`, the dominant real-world
shape, never fired. Field replay (daagu, 3 real coding sessions, 2026-06-11): 42/42 raw
greps used absolute paths → 1 hint / 0 blocks as-is vs **30 hints / 16 blocks** after this
fix. The v0.47.0 answer-in-the-deny feature was unreachable on consumer projects until now.

- Fix: strip `<cwd>/` (the hook's cwd IS the project root) from the command before
  matching — absolute paths under the root now behave exactly like their relative
  spelling; paths outside the project still never fire (conservative edge preserved).
  The inline answer's CLI scope argument is passed in relative form.
- Replay methodology added to tests: real transcript commands asserted in both spellings.
- Known limitation (pre-existing, unchanged): the CLI `grep` shells to ripgrep, whose
  gitignore handling can diverge from git on `dir/` + `!negation` whitelists (observed:
  rg 14.1.0 prunes a git-whitelisted directory when walking from above). Worst case is
  the honest no-hits fallthrough: the raw grep is allowed through and finds the truth.

## v0.47.0 — feat: answer in the deny — denied greps now return the actual results

**What changes for users**: when the PreToolUse hook denies a symbol-shaped raw grep, the
deny message now CONTAINS the results of the AST-aware equivalent (`code-graph-mcp grep
"<pattern>" [path]`, run synchronously inside the hook, ~20ms warm / 2s timeout) instead of
only suggesting the command. Rationale: measured recommend→use transfer of suggestion-style
interventions is ~0% — the model rarely initiates a new tool call because a message told it
to, but it will use results already in front of it.
**Opt-out / revert**: `CODE_GRAPH_NO_ANSWER_IN_DENY=1` restores the v0.46 static deny;
`CODE_GRAPH_NO_BLOCK_GREP=1` still downgrades the whole block tier to hint.

- **Three deny outcomes** (new `cg-answer.js`, all failure modes degrade, never break the
  tool call): ≥1 hit → deny with embedded results (truncated at line boundary to ≤4KB);
  CLI missing/error/timeout → v0.46 static deny; **0 hits → the raw grep is ALLOWED** with a
  one-line FYI (regex-dialect differences — BRE `\|` vs ripgrep — mean 0 hits is not proof
  of absence, so a hard deny could mislead).
- **Funnel semantics**: deny records gain `answered: true|false`; no-hit fallthroughs record
  `{action:"hint", fallthrough:"no-hits"}`. Rust readers ignore the extra fields (verified:
  CLI `grep` does not write `usage.jsonl`, so hook-initiated runs cannot inflate deny→use).
  **Reading note**: an answered deny satisfies the need in-place, so `Deny→use` will read
  LOW even when this feature works — segment by `answered` when reading Piece 3.
- **Hook stdin hardening**: hooks now read fd 0 directly instead of `/dev/stdin` (the path
  form fails silently when stdin is a socketpair, e.g. under `spawnSync({input})` test
  harnesses; real Claude Code pipes were unaffected).

## v0.46.0 — feat: measure whether the DENY stick converts + honest conversion metric

The recommend→use conversion metric (v0.39.0) was producing **zero usable data** in this repo
and failing silently. This release makes it honest and adds the first real attribution of the
PreToolUse **DENY** intervention — without adding or broadening any intervention itself.

- **Honest, visible metric.** `stats` (text + JSON `recommendations.state`) and `health-check`
  now surface `absent | empty | live` instead of silently skipping the block — "hooks not
  recording here" can no longer be mistaken for "feature absent".
- **Test-leak fixed.** `resolve_project_root_from` walks up to the nearest ancestor `.git`;
  `plugin_e2e::spawn_server` wrote only a `Cargo.toml`, so a fixture nested under the repo
  flushed test metrics (`nonexistent_tool`, `dur_s:0`) into the **real** `usage.jsonl`,
  corrupting the conversion denominator. Fixed by test isolation (a `.git` marker) — prod
  resolution is unchanged (subdir-CLI behavior is intended). Regression-tested.
- **Per-session deny→use funnel.** `SessionMetrics` stamps a wallclock start; flush
  window-joins in-session `deny`/`hint` events from `recommendations.jsonl` into the usage
  record (`recs`, additive — no schema change). `stats` prints `Deny→use: M/N = X%` (+ hint);
  JSON gains `recommendations.funnel`.
- **doctor stale-path detection.** A registered-but-stale-version PreToolUse hook (e.g. an old
  plugin-cache path) fires but may run pre-`recordRecommendation` code, keeping the metric
  silently dark. `doctor` now flags such entries and routes to the existing re-register fix.

## v0.45.4 — fix: intra-class method calls were dropped from the graph in OO languages

The call graph silently omitted every **method → sibling-method** edge for class-based
languages (TypeScript, JavaScript, Python, Java, Ruby). Only Rust (impl methods carry a bare
scope) and Go (receiver methods carry a bare qualified name) were unaffected — which is why
self-dogfooding on this Rust codebase never surfaced it. Two independent root causes:

- **Qualified scope vs. bare source lookup.** A class method's enclosing scope is recorded as
  `Class.method`, but Phase-2 source resolution matched the relation's `source_name` only
  against each node's bare `name` (`method`), so the source never resolved and the edge was
  dropped. Resolution now also matches `qualified_name`.
- **Java calls were never extracted.** tree-sitter-java emits `method_invocation` (not
  `call_expression`), which had no dispatch arm — so *all* Java call edges were missing despite
  Java being a documented Full-tier language. Added the `method_invocation` arm.

Impact: `get_call_graph` / impact analysis / `find_references` undercounted, and
`find_dead_code` produced false positives (a method called only by its siblings looked like an
orphan). `INDEX_VERSION` is bumped 15 → 16, so existing indexes auto-rebuild on first use.

Also: a read-only **secondary** MCP instance (a second editor window on the same project) now
explains a stale "symbol not found" — it does not reindex on its own, so a just-edited symbol
may not be present yet — instead of a bare not-found that's indistinguishable from a typo.

## v0.45.3 — chore: regression-test the binary self-heal wiring (no behavior change)

Hardening follow-up to v0.45.1/v0.45.2. Those two patches both broke in the *orchestration*
that calls `downloadBinary` on a present-but-stale binary — the shell-matches-latest branch of
`checkForUpdate` — while the underlying decision predicate (`cachedBinaryNeedsUpdate`) was
correct and unit-tested the whole time. Predicate tests don't cover whether anything actually
*calls* the predicate, so the exact glue that regressed twice had no test. Extracted that glue
into `selfHealStaleBinary(latest, { needsUpdate, download })` with injected dependencies and a
wiring test (download-invoked when stale / no-op when current / returns false so the next
session retries when the download fails). No runtime behavior change — the self-heal path is
identical; it is now guarded against a third silent regression.

## v0.45.2 — fix: stale binary self-heals on the next session, not up to 6h later

Follow-up to v0.45.1. The binary self-heal added there only ran after the time-based
update throttle (`shouldCheck`, 6h) had elapsed, so a present-but-stale binary could stay
pinned for up to a full check interval after the plugin shell updated. The throttle now
also yields to a present-but-stale binary (compared against the last known release version
— no extra network call), so the self-heal runs on the next session instead of waiting out
the window. Binary still loads into the running MCP server on the next Claude Code restart.

## v0.45.1 — fix: plugin auto-update could pin the native binary at an old version

The plugin shell would update while the cached native binary (`~/.cache/code-graph/bin/`)
stayed stuck at whatever version was first installed, with the updater reporting "up to
date". Three compounding defects in `claude-plugin/scripts/auto-update.js`:

- **Binary download never succeeded.** `promoteVerifiedBinary` read the downloaded
  binary's version (which runs `<binary> --version`) *before* `chmod +x`. `curl -o` writes
  the temp file as `0644`, so the exec failed with `EACCES`, the version read as `null`, and
  verification rejected every download. The `chmod` now happens before the version read.
- **Self-heal was existence-only.** The "no update needed" path re-downloaded the binary
  only when the file was *missing*, never when it was present-but-stale. It is now
  version-aware (new `cachedBinaryNeedsUpdate`): a cached binary whose version differs from
  the latest release self-heals even when the plugin shell version already matches.
- Together these caused a permanent deadlock once the shell version caught up to the
  release while the binary lagged. Existing installs heal automatically on the next update
  check after upgrading to this version.

## v0.45.0 — value references Phase 3b: JSX, Go composites, tuples, primitive paths

More value-reference coverage and precision. The index format bumped (`INDEX_VERSION`
14 → 15), so the first run after upgrade rebuilds the index once.

- **JSX attribute callbacks (JS/TSX).** `<Button onClick={handleClick} />` now emits a
  `references` edge to `handleClick`. Inline-arrow attributes (`onClick={() => …}`) and
  JSX child expressions are not treated as bare references.
- **Go composite-literal field values.** `Handler{ OnEvent: handler }` (and positional
  `[]fn{a, b}`) now reference the field-value function; the field-name key does not.
- **Python tuple return / RHS.** `return f, g` and `a, b = f, g` now reference both `f`
  and `g` (previously only single-value forms were tracked).
- **Primitive-type-head paths suppressed.** `str::trim` / `u32::MAX` and similar no longer
  emit a `references` edge to the bare tail — like the existing PascalCase-type-head rule,
  these associated items can't be resolved and would bind a wrong same-named local.

## v0.44.0 — value references Phase 3a: C / C++ function pointers

Extends callback / function-pointer reference tracking to C and C++ — where function
pointers are the primary callback mechanism. The index format bumped (`INDEX_VERSION`
13 → 14), so the first run after upgrade rebuilds the index once.

- **C/C++ value references.** A function named by a bare identifier now emits a
  `references` edge when passed as a call argument (`qsort(a, n, s, compare)`), taken by
  address (`signal(2, &handler)`), used as a designated or positional initializer value —
  the vtable idiom `struct ops o = { .read = my_read }` — assigned (`ops->read = my_read`),
  bound (`fn_t cb = handler`), or returned.
- **Precision.** A C/C++ local — function parameters and body declarations — is excluded
  by resolving the declared name from the declarator chain (handling
  `int *x` / `void (*cb)(int)` / multi-declarator forms), never from an initializer value,
  so a local passed by name does not fabricate an edge to a same-named global function.
  References still resolve same-file or same-language only.

## v0.43.0 — value references Phase 2: more positions + Python/Go

Extends callback / function-pointer reference tracking (v0.41.0) to more syntactic
positions and two more languages. The index format bumped (`INDEX_VERSION` 11 → 13), so
the first run after upgrade rebuilds the index once.

- **More value positions (Rust + JS/TS).** Beyond call arguments, a function named by a
  bare identifier now emits a `references` edge when it is: a binding RHS (`let cb =
  handler` / `const cb = handler`), a return value (`return handler`, Rust tail
  expression `{ … handler }`, JS arrow body `() => handler`), or a struct / object field
  value (`Config { cb: handler }`, `{ onClick: handler }`).
- **Python value references.** Python now tracks callbacks passed by bare name in call
  arguments (`register(handler)`), keyword arguments (`sorted(xs, key=my_key)`),
  assignment RHS (`cb = handler`), `return`, and dict values — in addition to the
  existing type-annotation references.
- **Go value references.** Go now tracks callbacks in call arguments
  (`http.HandleFunc(p, handler)`), `:=` / `=` / `var` right-hand sides, and `return`.
- **Precision.** Each language excludes local bindings (parameters, `let`/`const`/`:=`/
  `var` declarations, and Rust `if let` / `match` / `for` patterns, Python assignment /
  `for` targets, Go `:=` / `range` targets) so a bare name that is a local — not a global
  function — does not emit a false reference. References still resolve same-file or
  same-language only.

## v0.42.0 — reference precision: drop macro-path and type-associated false positives

Follow-up to v0.41.0. The `references` edge extractor for Rust path-qualified values
(`extract_rust_path_reference`) emitted two classes of false positive that predate
v0.41.0; both are now suppressed. The index format bumped (`INDEX_VERSION` 10 → 11), so
the first run after upgrade rebuilds the index once.

- **Macro paths.** `tracing::error!(…)` / `serde_json::json!(…)` parse as a macro
  invocation whose macro name is a scoped path (`tracing::error`). The extractor treated
  the tail (`error`) as a value reference, colliding with same-named functions. Macro
  paths no longer emit a reference.
- **Type-associated paths.** `String::as_str` passed as a function pointer (e.g.
  `.map(String::as_str)`) emitted a reference to the bare tail `as_str`, which then
  bound to an unrelated local function — the associated item can't be resolved (std
  methods aren't indexed). A PascalCase path head (a type) now suppresses the reference;
  lowercase module heads (`crate::domain::SHARED`) still emit. Primitive-type heads
  (`str::trim`) are a documented residual.

On this repo these removed 14 phantom reference edges (504 → 490) with zero genuine
references lost.

## v0.41.0 — callback / function-pointer references (Phase 1)

A function passed as a *value* — a callback or function pointer — is referenced by a
bare identifier in a non-call position (`register(handler)`, `xs.iter().map(map_row)`,
`addEventListener('click', handler)`, `signal(&shutdown)`). The parser only emitted a
`calls` edge for actual call expressions, so these usages produced no edge:
`find_references`, `impact_analysis`, and dead-code all missed them. This release tracks
them as `references` edges (Rust + JS/TS/TSX). The index format bumped
(`INDEX_VERSION` 9 → 10), so the first run after upgrade rebuilds the index once.

- **Bare-identifier value references.** A bare function name in call-argument position
  (or Rust address-of `&fn`) now emits a `references` edge to the function, distinct
  from `calls` — a registered callback is coupling, not a synchronous call. They surface
  in `find_references` and as a new `value_references` count in `impact_analysis`, but
  stay out of the call graph and the calls-based hot-function / caller counts.
- **Precision gates.** Bare identifiers are only emitted as references when they are not
  a local binding: enclosing-function parameters, `let` bindings, and `if let` /
  `while let` / `match`-arm / `for` pattern bindings are all excluded (they shadow
  same-named accessor functions like `db` / `conn` / `node`). Resolution requires a
  same-file or same-language target — a reference is dropped rather than bound to a
  cross-language same-named function.
- **Cross-language resolution fix.** The Phase-2 edge resolver previously let non-`calls`
  relations fall through to a global, cross-language name pool. `references` now drops on
  no same-language match (like `calls`), so a Rust value-reference can never bind a
  same-named JavaScript function.

## v0.40.0 — dogfooding fixes: project_map accuracy, trace/grep, build-dir exclusion

End-to-end dogfooding pass. The index format bumped (`INDEX_VERSION` 8 → 9, for the
build-dir exclusion below), so the first run after upgrade rebuilds the index once
automatically.

Read-side query fixes for `map` / `project_map`. Dogfooding surfaced three
count/labeling defects in the project map:

- **Synthetic `<external>` bucket excluded.** Unresolved external-import targets
  (e.g. `os`, and external traits like `Drop` / `std::io::Write` / `Default`) live
  in a virtual `<external>` file. These leaked into the module map as a phantom
  `<root>` module ("0 symbols, external") and were counted as project symbols, and
  every external/builtin import surfaced as a misleading `→ <root>` dependency
  (dominating the Dependencies section). `<external>` is now excluded from the
  module list, symbol counts, key-symbols, and the cross-module dependency graph —
  Dependencies now shows genuine internal module coupling only.
- **Methods counted toward the symbol total.** The per-module "N symbols" count
  summed only functions + classes/structs/enums + interfaces/traits, silently
  dropping `method` nodes. For OO modules this undercounted, and contradicted the
  `key_symbols` list (which includes methods) — a class file could read "3 symbols"
  while listing 4. Methods now count, matching `overview`.
- **`deps` counts distinct symbols, not edges.** File-level dependencies labeled
  "N symbols" actually counted cross-file edges, so a symbol both imported and
  called inflated the count (2 symbols → "4 symbols"). Now counts distinct target
  symbols.
- **`trace` renders the route label, not raw JSON.** The CLI `trace` text output
  printed the raw `routes_to` metadata blob (`{"handler_end_line":10,...}`) as the
  route label instead of `GET /users` like the map already does. Now formatted as
  `METHOD path`.
- **`grep` exits non-zero on a ripgrep error.** An invalid regex (e.g. an
  unescaped `(` in `res.json(`) or unreadable path printed the error to stderr but
  still exited 0, hiding the failure from scripts. ripgrep's error exit code (2) is
  now surfaced as a non-zero CLI exit; a valid no-match still exits 0.
- **Build/dependency dirs excluded without a `.gitignore`.** Indexing relied on
  the `ignore` crate's gitignore rules, so a project with no `.gitignore` (or that
  isn't a git repo) indexed `node_modules/` (JS/TS source), `target/` (Rust/Maven
  build), and nested copies — bloating the index and polluting the graph with
  dependency/build code. These (plus `vendor/`, `bower_components/`) are now
  excluded by a hardcoded safety net, matched on whole path segments at any depth
  so a directory `target/` is skipped while a file `target.rs` is still indexed.
  Hidden dirs (`.git`, `.venv`, `.code-graph`, …) were already skipped.

## v0.39.0 — C++ method scope, real-session conversion metric, adopted-only map

Parser + metrics release. The index format bumped (`INDEX_VERSION` 7 → 8), so the
first run after upgrade rebuilds the index once automatically.

**C++ `Class::method` scope.** C/C++ method extraction was previously bare-name
only — in-class methods, out-of-class `Type::method` definitions, and qualified
`Foo::bar()` calls all lost their type scope, and qualified calls produced no
edge at all. Now:

- In-class methods (`class Foo { void bar(){} }`) and out-of-class definitions
  (`void Foo::bar(){}`) carry `node_type: "method"` + `qualified_name: "Foo.bar"`.
- Call sources inside C/C++ functions/methods now attribute to the function
  (`Foo.bar`) instead of `<module>` (`scope_name` had no `name` field for C/C++).
- Qualified calls `Foo::bar()` now produce a `calls` edge (resolved by rightmost
  name, same-language) — they were silently dropped before.

So call-graph / impact / dead-code for C and C++ are materially more accurate.

**Real-session conversion metric.** The PreToolUse hooks now record each
recommendation (raw-grep hint/deny, read-fanout hint) to
`.code-graph/recommendations.jsonl`, and `code-graph-mcp stats` reports the
field conversion signal — cg tool calls vs recommendations emitted — that the
synthetic routing benchmark can't see. `--json` gains a `recommendations` block
(`total` / `by_action` / `by_hook` / `cg_tool_calls` / `conversion_ratio`). The
recorder is append-only and never creates `.code-graph`, so non-project / tmp
cwds leave zero footprint.

**SessionStart map injection is adopted-only.** On top of the existing
quiet-by-default behavior (the map is opt-in via `CODE_GRAPH_VERBOSE_HOOKS=1`),
the project-map dump now also requires the project to be adopted into your
MEMORY.md workflow. Unadopted projects get no map even under verbose — the dump
was measured to be zero-referenced there.

## v0.38.0 — call-graph precision: prune import-contradicted edges

Graph-output release: `get_call_graph` / `get_ast_node include_impact` / dead-code
results change for repos with same-named functions across files. The index format
bumped (`INDEX_VERSION` 6 → 7), so the first run after upgrade rebuilds the index
once automatically.

**Call-graph false positives removed.** When a file makes a bare call `save()`
whose name matches several functions across files, the resolver used to fan the
edge out to every candidate (to protect dead-code precision when it had no way to
choose). When the caller's file has an explicit `imports` edge binding that name
to one node — e.g. Python `from db import save` — the bare call resolves to the
imported node, so the other same-name edges are false callers. Those are now
pruned: impact/call-graph no longer lists phantom callers, and the genuinely
uncalled sibling is correctly reported by dead-code. The prune is conservative —
it only touches bare-name edges contradicted by an import, never qualified calls
(`cache.save()`, `crate::x::foo()`), same-file targets, or the no-import tie case
(so Rust scoped-call dead-code precision is preserved). Languages whose imports
themselves fan out (JS/TS, Rust `use`) safely no-op.

**Other fixes:**

- **CLI `--json` empty/error paths** now always emit success-shaped JSON: `refs`
  (all three not-found branches), `similar` (no-embeddings / missing `--node-id`),
  and `search` no longer under-returns below `--limit` when the always-on
  test/module filter drops top rows.
- **`stats` version list** sorts numerically (major.minor.patch) instead of
  lexically, so `0.5.40` no longer sorts after `0.32.2`.
- **Index version-mismatch is now visible.** When two binaries of different
  `INDEX_VERSION` share one `.code-graph/index.db` and clear each other's data,
  the cause is printed to stderr (was a silent "index is empty").
- **Grammar.** Module-overview and dead-code entries print `1 file` / `1 line`
  instead of `1 files` / `1 lines`.

## v0.37.0 — CLI argument parsing migrated to clap-derive

All 22 `code-graph-mcp` CLI subcommands moved from a hand-rolled argv parser to
clap-derive; the hand parser is gone entirely. Every success path is preserved —
queries, filters, JSON envelopes, and the found/not-found exit codes are
unchanged. The changes are confined to argument-error handling and help, where
clap now owns parsing. **No MCP tool behavior changes** (the MCP server was never
hand-parsed).

**User-visible CLI changes** (see `code-graph-mcp <cmd> --help` for each surface):

- **Per-command help.** Every subcommand now has clap-generated `--help`/`-h`
  (exit 0). Top-level `--version`/`-V` unchanged.
- **Stricter argument errors → exit 2.** Unknown flags, extra positionals, a
  missing required argument, and non-numeric or negative-numeric flag values
  (e.g. `--depth -5`) now error with exit code 2. The old parser silently ignored
  unknown flags / extra args and clamped-or-defaulted bad numbers (exit 0/1).
- **`--flag=value` now honored.** `--limit=5`, `--direction=callees`,
  `--relation=calls`, etc. now parse correctly. The old exact-token parser
  silently ignored the `=value` form and fell back to defaults — sometimes
  reporting false success (e.g. `refs --relation=calls` returned *all* references).
- **`trace`.** The advertised-but-non-functional `--include-middleware` flag is
  removed; use `--no-middleware` to hide downstream middleware (shown by default),
  which is what the command always actually did.
- **`snapshot`** is now a real `create` / `inspect` subcommand pair; an unknown or
  missing subcommand errors (exit 2).

In-handler validation messages and exit codes for the enum flags (`--direction`,
`--change-type`, `--relation`) and the symbol-not-found / ambiguous-symbol guards
are unchanged. clap is added as a runtime dependency.

## v0.36.0 — Audit remediation: snapshot supply-chain integrity, CLI/MCP consistency, honest dead-code

Remediation of the 2026-06-03 architecture/security audit. Headline items: two
supply-chain hardenings on the snapshot install path, and a correctness fix that
makes the CLI and MCP give the **same** verdict on ambiguous symbols.

**Security — snapshot supply-chain integrity.** First-run snapshot install
downloads a `.db.zst` that becomes the entire code graph. It now verifies the
artifact against a published `<url>.blake3` sidecar **before** decompressing
(hard-fail on mismatch), caps the compressed side against a zip-bomb, and refuses
to honor a `.code-graph.toml [snapshot] url` override unless the developer opts in
out-of-band via `CODE_GRAPH_SNAPSHOT_TRUST_URL=1` — so a committed url in a
malicious repo/PR can no longer silently redirect the graph to an attacker-chosen
database. The artifact and sidecar fetches also reject HTTPS→HTTP redirect
downgrades, closing a plaintext-substitution path.

**Behavior change — `callgraph`/`impact` on same-file overloads.** A bare symbol
name resolving to ≥2 non-test definitions in the **same file** (e.g. two
`fn new()` in different impl blocks) is now reported as ambiguous instead of
silently merging their call graphs. Previously the CLI merged them (exit 0, a
conflated answer) while the MCP tool refused — same input, opposite answers; both
now refuse consistently via a shared resolver. **Migration:** disambiguate with
`--file` + `show --node-id <N>` (CLI) or `file_path` + `node_id` / `get_ast_node`
(MCP). Cross-file collisions still take `--file`/`file_path` as before.

**Output change — dead-code reports "candidates", not "results".** `find_dead_code`
(CLI + MCP) now frames output as candidates needing human verification and notes
that receiver-method calls (`obj.method()`) and cross-file const/type uses aren't
edge-tracked, so a flagged symbol may still be live. The MCP
`module_overview.include_dead` schema description carries the same caveat. Scripts
or agents that string-matched the old `results` wording should update.

**Fixes.**
- Vectors no longer go stale after an incremental edit: a cross-file dirty node
  whose context changed while the embedding model wasn't loaded now has its vector
  invalidated so the background embedder regenerates it.
- Enum arguments (`direction`, `change_type`, `relation`, `deps_direction`) are
  validated at the tool/command entry on both CLI and MCP, so a typo errors
  cleanly instead of surfacing as a confusing downstream/freshness error.
- CLI `--ignore` (dead-code) parses as a value flag, and a misspelled `--type` is
  rejected loudly instead of silently matching zero rows.

**Internal / hardening.**
- WAL bounded (`journal_size_limit` + checkpoint TRUNCATE in `run_optimize`).
- Rerank multipliers extracted to named `domain.rs` constants (value-preserving).
- `build.rs` pins the vendored `sqlite-vec` C source against a blake3 checksum and
  fails the build on tamper.
- Session metrics can tag dogfood traffic via `CODE_GRAPH_DOGFOOD=1`; the
  `TriggerRate` routing metric gains a soft floor.

## v0.35.0 — Reference edges + receiver-call resolution sharpen dead-code & find_references

**Feature (additive).** A new `references` graph relation captures edgeless
symbol *usages* — types used only in annotation/type position (`field: Foo`,
`func g() Bar`, `List<Foo>`) and Rust path-qualified const references
(`crate::a::FOO`). It is extracted for **Rust, TypeScript/TSX, Python, Go, and
Java**, each gated to that language's real type-position AST nodes and filtered
against per-language builtin/JDK noise sets so it points only at project
symbols. `find_dead_code` now counts incoming `references` edges as usage, and
`find_references` gains a `relation: "references"` filter (additive enum value
on the tool schema + CLI `--relation references`).

Why: a type/interface/enum/const that is defined and used **only** as a type
annotation or a path-qualified constant produced no `calls`/`imports` edge, so
`find_dead_code` reported it as dead (an agent acting on that could delete live
code) and `find_references`/`impact` were blind to it. Reference edges are
produced at parse time from the full source, so they are immune to the
`code_content` truncation that the same-file `instr` fallback suffered.

**Fix — receiver-method calls.** `obj.method()` calls whose receiver type can't
be statically inferred were dropped entirely, marking uniquely-named live
methods (e.g. `file_exists`, `validate`) as dead and hiding their callers from
`impact`/`callers`. They now resolve to a real `calls` edge **only when
unambiguous** — exactly one same-language method of that name (non-stdlib),
preferring a same-file match. Ambiguous or stdlib-noise names still drop, so
`impact` cannot fan out across unrelated modules.

**Fix — dead-code false positives.** The dead-code reference fallback now also
probes other files for edgeless node kinds (const/struct/enum/type/interface/
trait, name length ≥ 5, delimiter-aware) and scans same-file declaration bodies
(not just function/method bodies), rescuing cross-file path-qualified consts and
same-file struct-field type usages.

### Migration

- `INDEX_VERSION` is bumped 5 → 6: the server detects the mismatch on first run
  and automatically clears + rebuilds `.code-graph/index.db` so the new edges
  are present. No action needed.
- The `references` relation is additive; existing `find_references` calls
  (`calls`/`imports`/`inherits`/`implements`/`all`) are unchanged.

## v0.34.0 — Rust binary no-ops in non-project directories

**Behavior change (opt-out available).** `code-graph-mcp serve` now serves a
0-tool stub when launched directly in a non-project working directory (no
`.git`/manifest marker), mirroring the v0.33.0 MCP-launcher gate. It opens no
database, loads no embedding model, creates no `.code-graph/`, and emits no
tool-decision `instructions`.

Why: v0.33.0 gated the JS launcher path, but a binary invoked directly —
bypassing the launcher (a dev `.mcp.json`, or any MCP config pointing straight
at the binary) — still half-activated in a marker-less cwd. This closes that
parallel path so the activation boundary holds at the Rust layer regardless of
entry point. Detection is marker-based (`.git`, `package.json`, `Cargo.toml`,
`pyproject.toml`, `go.mod`, `pom.xml`, `build.gradle`), not a literal path
check — a real git repo under `/tmp` still activates.

### Migration / opt-out

- No action needed; real projects (with a project marker) serve the full tool
  catalog exactly as before.
- To force the full plugin MCP in a marker-less cwd, set
  `CODE_GRAPH_FORCE_PLUGIN_MCP=1` (same override as the launcher gate).

## v0.33.0 — plugin no-ops in non-project directories

**Behavior change (opt-out available).** The plugin now fully no-ops in working
directories that are not a project — i.e. that carry none of the recognized
project markers (`.git`, `package.json`, `Cargo.toml`, `pyproject.toml`,
`go.mod`, `pom.xml`, `build.gradle`). In such a cwd the MCP launcher serves a
0-tool stub instead of spawning the binary, and the SessionStart hook +
auto-adopt short-circuit.

Why: a cross-project audit found code-graph half-activating on the ~2035
headless `claude -p` calls claude-mem-lite spawns with `cwd=/tmp` ("Return ONLY
valid JSON") — none of which ever use code-graph. Each one paid an MCP-server
spin-up, a ~780-byte `instructions` block injected into a JSON-only task, a
SessionStart map probe, and an empty `/tmp/.code-graph/index.db`, plus an
adopted decision-table sentinel in `~/.claude/projects/-tmp/memory/MEMORY.md`.
Net waste, zero usage. A real git repo cloned under `/tmp` still activates —
detection is marker-based, not a literal path check.

### Migration / opt-out

- No action needed for normal use; real projects (with a project marker)
  activate exactly as before.
- To force the full plugin MCP in a marker-less cwd, set
  `CODE_GRAPH_FORCE_PLUGIN_MCP=1` (the same override that bypasses the
  tool-catalog dedup gate).
- `.code-graph` is no longer treated as a project marker for activation — a
  directory whose only marker is a previously-created `.code-graph` is now
  considered non-project (prevents a polluted `/tmp` from self-certifying).

### Changed

- New `claude-plugin/scripts/project-detect.js` centralizes project detection
  (`isProjectRoot` / `isNonProjectCwd`, marker set sans `.code-graph`); the MCP
  launcher, the SessionStart hook, and `adopt()` all gate on it.
- `adopt()` now refuses a non-project cwd **even when the memory dir already
  exists** — the prior guard sat inside `if (!fs.existsSync(dir))` and was
  bypassed because Claude Code pre-creates `~/.claude/projects/<slug>/memory`
  for every session (including the headless `/tmp` calls).

## v0.32.3 — CLI path normalization + enum-arg early validation

Three-fix patch bundling end-to-end dogfood findings. Two of the three
were *silent wrong-answer* bugs (worst kind: no error, no exit code,
just incorrect output). The third tightens MCP/CLI error attribution
so a single typo doesn't surface as two cascading errors.

### Fixed

- **CLI commands accept absolute paths under the project root.** The
  indexed `file_path` column is project-relative; users who pasted
  absolute paths from an IDE used to get:
  - `overview /abs/path` → "No symbols found" (silent wrong, exit 1)
  - `dead-code /abs/path` → exit-0 "No dead code found" (most dangerous —
    user trusts the wrong answer)
  - `deps /abs/path` → bogus `barrel_scan` fallback with empty
    `depended_by`/`depends_on` (user thinks file has no deps)
  - `callgraph X --file /abs/path` (also `impact`/`show`/`refs`) → empty
    filter, no edges, false "Symbol not found"
  v0.32.3 introduces `normalize_user_path(project_root, raw)` and
  routes all positional path args (`overview`, `deps`, `dead-code`)
  plus all `--file` flags through it. Behavior:
  - `.` → `""` (whole project, matches MCP `module_overview` semantics)
  - `./foo` → `foo`
  - absolute under root → relative portion (lexical strip; canonicalize
    fallback covers symlinks)
  - absolute outside root → explicit error, *not* silent wrong
  - relative path → unchanged
  Distinct from MCP-side `tool_module_overview` which *rejects* absolute
  paths outright — CLI is more lenient because real humans paste from
  IDE. Regression coverage in `tests/cli_e2e.rs::*absolute_path*`.

- **`get_call_graph` / `dependency_graph` / `module_overview` validate
  enum args at tool entry (MCP + CLI).** Previously a bogus
  `direction` / `deps_direction` value first hit the ambiguity check
  (which echoed the bogus value back in the JSON), and only after the
  user disambiguated with `file_path` would the underlying graph layer
  reject the enum — two errors for one mistake. Worse,
  `module_overview deps_direction=bogus include_deps=true` swallowed
  the downstream `tool_dependency_graph` error into a
  `dependencies_unavailable` text field and returned
  `isError=false`, so agents/scripts treated the call as successful.
  v0.32.3 gates each enum-valued arg at the tool/cmd entry with
  `matches!()`, matching the schema enum exactly and echoing the bad
  value in the error message. Affected:
  - MCP `get_call_graph` `direction`
  - MCP `dependency_graph` `direction`
  - MCP `module_overview` `deps_direction`
  - CLI `callgraph --direction`
  Regression coverage in
  `tests/mcp_stdio_integration.rs::mcp_enum_args_validated_at_tool_entry`
  and `tests/cli_e2e.rs::test_cli_callgraph_invalid_direction_errors_early`.

- **`show <Class.method>` falls back to base-name match when the DB
  doesn't store the qualified prefix.** Parsers populate
  `qualified_name` inconsistently (Rust `impl` blocks: yes; free
  functions: no). The old `show` fallback required an exact qualified
  match and silently returned "Symbol not found" when the DB only had
  the base name — even though `callgraph <Class.method>` resolved the
  same input fine via prefix-strip. v0.32.3: prefer exact qualified
  matches when any exist; otherwise fall back to all base-name nodes.
  No regression for the qualified case (`show Database.open` still
  uniquely matches the Rust `impl Database::open`). Regression
  coverage in
  `tests/cli_e2e.rs::test_cli_show_qualified_falls_back_to_base_name`.

- **`stats --last <non-integer>` now warns instead of silently
  showing all sessions.** Previously `--last abc` parsed through
  `.ok()` and fell through to "no filter" — exactly opposite of the
  user's stated intent. Aligned with `parse_flag_or`'s pattern
  (warn-and-default) used by other numeric flags.

### Tests

11 new regression tests across `cli_e2e` (+8: abs-path on
`overview`/`deps`/`dead-code`, callgraph invalid-direction,
show qualified fallback, plus 5 `normalize_user_path` unit tests in
`src/cli.rs`) and `mcp_stdio_integration` (+1 enum-arg validation).
Full suite: 595 passing.

## v0.32.2 — tmp-dir containment + healthCheck repair verification

Three-fix patch bundling reviewer follow-ups from v0.32.1 (M3/M4) and a
session-dogfood discovery (M6/tmp-dir). None changes the public contract
or schema; `healthCheck()` return shape gets one additive field
(`remaining`) and one tightened invariant (`repaired:true` requires a
post-install re-scan to be clean).

### Fixed

- **`healthCheck()` `repaired:true` is now verified by a post-install
  re-scan (M3)** — previously the function ran `install()` whenever a
  flagged path was found and unconditionally returned `repaired:true`,
  even if `install()` couldn't actually resolve the issue (binary
  permanently gone, third-party registry entry not ours to touch,
  etc.). `doctor.js` then printed "N issue(s) auto-repaired ✅" —
  Iron Law #2 style honesty violation. v0.32.2 extracts
  `scanForBrokenPaths()` as a pure function (also exported for direct
  unit testing); `healthCheck()` runs it once, then if issues found
  runs `install()` and re-runs the scan; `repaired:true` only when the
  post-repair scan is clean. Otherwise `repaired:false` plus a new
  `remaining[]` field listing what's still broken. `doctor.js` Hooks
  row consumes the new shape: `repaired:true` → `ok` /
  "N issue(s) auto-repaired"; `repaired:false` → `warn` /
  "N invalid path(s) — auto-repair did not resolve" + `fixId:'hooks-invalid'`
  so the user can still try the manual repair path.
- **`doctor.js` `hooks-invalid` repair output aligned with sibling
  message (M4)** — `runRepairs('hooks-invalid')` previously logged
  "✅ Hooks repaired" but the sibling `missing-hooks-in-settings` case
  already said "✅ Hooks repaired — restart Claude Code to apply".
  `settings.json` changes only land after Claude Code restarts the hook
  dispatcher, so both messages now say the same thing.
- **Hook + auto-update tmp artifacts no longer collide with Claude
  Code's transcript directory (M6)** — `pre-grep-guide.js` cooldown
  flags (`.code-graph-bash-<hash>`), `pre-edit-guide.js` impact cache
  (`.cg-impact-<symbol>`), `pre-read-guide.js` readfan state
  (`.code-graph-readfan-<hash>.json`), and `auto-update.js` staging dir
  (`code-graph-update-<ts>`) previously called `os.tmpdir()` directly.
  Claude Code overrides `$TMPDIR` to `~/.claude/tmp/` (to capture
  process output for transcript replay), so these artifacts landed
  alongside 9000+ transcript subdirs. Two failure modes:
  (a) **diagnostic blindness** — every doc / memory / debug query that
  checked `/tmp/.code-graph-bash-*` for hook firing returned empty even
  when the hook worked correctly (the v0.32.0 "PreToolUse dark under
  green health" investigation burned ~2 hours chasing this red
  herring); (b) **§8 SAFETY recursive-traversal trap** — scattering
  0-byte flag files alongside transcript subdirs amplifies the
  "`grep -r ~/.claude/tmp/`" footgun. Fix: new `tmp-dir.js` helper pins
  all hook + auto-update artifacts to `${tmpdir}/code-graph-mcp/`.
  Legacy orphans in the parent age out naturally (0-byte, no behavior
  impact). Diagnostic memory (`feedback_pretooluse_dark_under_green_health.md`)
  updated to point at the new location.

### Internal

- `lifecycle.js` now exports `scanForBrokenPaths()` as a pure function
  for unit testing.
- New `claude-plugin/scripts/tmp-dir.js` module — single source of
  truth for hook + auto-update tmp paths.

### Testing

- 448/448 plugin script tests pass (29 new on v0.32.0 baseline):
  - `healthCheck` clean-state → `healthy:true`, no `remaining` field
  - `healthCheck` post-repair re-scan clean → `repaired:true`,
    `remaining:[]`
  - `healthCheck` post-repair re-scan still broken → `repaired:false`,
    `remaining` lists what `install()` couldn't fix
  - `scanForBrokenPaths` exported and returns the expected issue
    structure
  - `update()` from v0.31.2 manifest + empty settings.json — reviewer
    Rec #2 covered the actual production migration path
  - `tmp-dir.js`: `CG_TMP_DIR` resolves to
    `${os.tmpdir()}/code-graph-mcp`, `cgTmpDir()` creates the dir on
    demand and is idempotent, regression guard that hook flag basenames
    never leak into the parent root (`os.tmpdir()` itself)

### Process

M3/M4 driven by `/superpowers:requesting-code-review` follow-ups on the
v0.32.1 patch. M6 surfaced by an end-of-cycle user dogfood pass —
direct invoke of `pre-grep-guide.js` showed HINT firing but zero
`/tmp/.code-graph-bash-*` flag files; `$TMPDIR` probe revealed the
override.

## v0.32.1 — block-tier false-positive fixes + foreign-strip risk reduction

Four-finding code-review patch on v0.32.0. All findings were `Important`
(not Critical, not data-loss), but they hit exactly the spec the v0.32.0
block tier was designed to satisfy ("narrowest case"), so they get fast
follow-up rather than a v0.33 bundle.

### Fixed

- **Block tier no longer false-positives on identifier-shaped PATH ARGUMENTS** —
  v0.32.0's `shouldBlock` ran `IDENTIFIER_LIKE.test(cmd)` against the full
  command, so `grep -rn "abc" src/EmbeddingModel.rs` blocked because the
  filename contains CamelCase. Same problem for snake_case directory names
  (`src/some_module/...`). v0.32.1 adds `extractPatterns(cmd)` that pulls
  quoted arguments out of the command and tests `IDENTIFIER_LIKE` against
  the patterns only. Unquoted-pattern usage (`grep foo src/`) falls back to
  hint behavior — conservative.
- **`type` dropped from declaration-anchor keyword list; remaining anchors
  pinned to pattern start** — `\btype\s+\w` matched "# type checking",
  "some type X", and other comment-string scans. v0.32.1 drops `type`
  (too common in English prose) and anchors `fn` / `def` / `class` /
  `function` / `struct` / `impl` / `trait` to `^\s*` so they only fire
  when the user is actually searching for a declaration.
- **`isOurHookEntry` script-name fallback tightened** — v0.32.0 used
  `cmd.includes('code-graph')` which would claim a user's own
  `~/code-graph/foo.js`. v0.32.1 uses `MARKETPLACE_NAME` (`code-graph-mcp`)
  instead. Foreign-entry strip risk eliminated.
- **`doctor.js` report no longer prints two `'Hooks'` rows** — check 6
  (healthCheck path validation) keeps `'Hooks'`; check 7 (settings.json
  coverage) renamed to `'Hook coverage'`.

### Internal

- Dropped a `pluginRootDir()` function that duplicated the module-level
  `PLUGIN_ROOT` constant.
- Cleaned up `doctor.js` unused imports (`removeHooksFromSettings`,
  `writeJsonAtomic`) that became dead when the v0.32.0 inversion replaced
  the legacy strip path with `install()`-as-repair.

### Testing

- 437/437 plugin script tests pass (18 new). New coverage:
  - `extractPatterns` matrix: single/multi quoted, env-prefixed verb,
    `rg` / `ag` heads, no-quote conservative fallback, empty/null inputs
  - I1 regressions: `"abc"` against CamelCase-named file, `"x"` against
    snake_case directory, English-prose pattern with identifier-name
    file path, unquoted-pattern hint fallback, inverse sanity (CamelCase
    pattern + plain path still blocks)
  - I4 regressions: `"# type checking"`, `"some type X"`, `"the def keyword"`
    all hint (not block); real `"def calc_total"` and `"fn render"` still
    block
- E2E smoke (three inputs through the actual hook script, sandboxed
  `.code-graph/index.db`): I1 case emits hint text, I4 case emits hint
  text, real CamelCase symbol search emits JSON block envelope with
  `permissionDecision: "deny"`. cargo check green.

### Process

Driven by `/superpowers:requesting-code-review` subagent on the
`b5c907f..a25bee9` (v0.31.2..v0.32.0) range. Verdict was "With fixes
— needs v0.32.1 patch"; this is that patch.

## v0.32.0 — re-route PreToolUse/PostToolUse/UserPromptSubmit via settings.json (root-cause fix for "hook never fired since v0.25.0")

Architecture-level fix for a silent failure that turned every v0.25.0+ release
into theater. The plugin's PreToolUse hooks (`pre-edit-guide.js`, `pre-grep-guide.js`,
`pre-read-guide.js`), the PostToolUse incremental indexer, and the UserPromptSubmit
context push **never actually fired** since v0.25.0. Only SessionStart worked.

### The bug

`claude-plugin/hooks/hooks.json` registered every event type (PreToolUse,
PostToolUse, UserPromptSubmit, SessionStart). Empirical 2026-05-24 diagnostic in
a real Claude Code session:

- 25+ Bash tool calls produced **zero** `PreToolUse:Bash` events in the session jsonl
- **Zero** `/tmp/.code-graph-bash-*` cooldown flags (the hook scripts were never spawned)
- `.code-graph/index.db` mtime was the SessionStart timestamp — `incremental-index.js`
  (PostToolUse:Edit handler) never ran despite 5+ Edits

Cross-validated by inspecting `~/.claude/plugins/cache/sdsrss/claude-mem-lite/*/hooks/hooks.json`
— claude-mem-lite ships `"hooks": {}` with the note "Hooks managed by install.mjs
in settings.json — this file cleared to prevent duplicates". The pattern is well-known
in the ecosystem; code-graph-mcp was the outlier.

The v0.31.1 "PreToolUse hooks never fired since v0.25.0" fix corrected the matcher
DSL (`tool == "Edit"` → `"Edit"`) — the matcher was right but the entries
themselves were never loaded by CC for non-SessionStart events.

### Fixed (root cause)

- `claude-plugin/scripts/lifecycle.js` — new `registerHooksToSettings(settings)`
  actively writes PreToolUse / PostToolUse / UserPromptSubmit entries into
  `~/.claude/settings.json` on `install()` and `update()`. Two-pass: evict our
  entries across every event (catches legacy v0.7/v0.8 entries with stale paths),
  then append fresh v0.32+ entries for the events we own. Description markers
  carry `[code-graph-mcp v0.32+] …` for cleanup. Hook command paths are
  absolute, derived from `__dirname` — no `${CLAUDE_PLUGIN_ROOT}` (immune to
  cross-plugin env leak per `feedback_plugin_env_isolation.md`).
- `claude-plugin/hooks/hooks.json` — SessionStart only. `_note` explicitly
  documents that other event types would be dead config if added back.
- `claude-plugin/scripts/doctor.js` — `legacy-hooks-in-settings` →
  `missing-hooks-in-settings`. Semantics inverted: presence in settings.json
  is now correct; missing is the bug. Repair runs `install()` instead of strip.
- `claude-plugin/scripts/session-init.js` — new self-heal path detects missing
  settings.json coverage and triggers `install()` (catches manual settings
  edits and third-party settings.json rewriters that don't preserve markers).

### Added

- **PreToolUse Bash block tier** in `pre-grep-guide.js`. Raw `grep -r{n}` /
  `rg` / `ag` against an indexed source tree with an identifier-shaped
  pattern is now blocked via `{ hookSpecificOutput: { hookEventName:
  "PreToolUse", permissionDecision: "deny", permissionDecisionReason:
  "<cg equivalent>" } }` (current Claude Code schema — the
  `hookEventName` discriminator is required for CC to recognize the
  permission verdict). Bash is the comfort-zone leak — 15d audit measured 429 raw
  grep vs 191 functional CLI (~13× preference). Block tier targets the
  narrowest "I'm searching for a symbol" subset: bare flags (no `-l`,
  `--include`, `-A`/`-B`/`-C`), identifier-shaped pattern (CamelCase ≥4ch,
  snake_case with `_`, or declaration anchor `fn X` / `class X` / `def X`),
  not a marker word (TODO/FIXME/XXX). Marker-only and precision-flag scans
  downgrade to the v0.25.0 informational hint. Escape hatch: prepend
  `CODE_GRAPH_NO_BLOCK_GREP=1` to fall back to hint behavior; independent
  of `CODE_GRAPH_QUIET_HOOKS=1` which silences entirely.

### Testing

- 419/419 `claude-plugin/scripts` tests pass — 165 net new across `hooks.test.js`,
  `lifecycle.test.js`, `pre-grep-guide.test.js`. New coverage includes:
  - settings.json install writes the expected matchers (`Edit`, `Bash`, `Read`
    for PreToolUse, `Write|Edit` for PostToolUse, UserPromptSubmit), every
    entry carries a description marker, paths are absolute (no
    `${CLAUDE_PLUGIN_ROOT}`)
  - install is byte-idempotent across re-runs
  - foreign plugins' settings.json entries survive install/uninstall (no
    collateral strip)
  - legacy v0.8.x SessionStart entries with stale paths get evicted on install
  - hooks.json contract: only `SessionStart` key present; cross-validates that
    `lifecycle.js` `buildSettingsHookEntries()` covers the matchers removed
    from hooks.json
  - `shouldBlock(cmd)` matrix: 22 cases covering CamelCase / snake_case /
    declaration anchors / precision-flag downgrades / marker-only downgrades

### Migration

Automatic via `lifecycle.js update()`. v0.31.x users upgrading to v0.32.0:
- `session-init.js syncLifecycleConfig` detects version mismatch → calls `update()`
- `update()` calls `registerHooksToSettings(settings)` → fresh entries land in
  `~/.claude/settings.json`
- Plugin-cache `hooks.json` arrives without the dead non-SessionStart entries
- No user action required

### Caveat

This fix has been verified at the unit + e2e level (fake-HOME install + JSON
shape inspection) but **not yet** in a real Claude Code session where the
upgrade path runs end-to-end. The bench evidence supporting the diagnostic
(zero PreToolUse:Bash events, zero cooldown flags, stale `.code-graph/index.db`
mtime) was collected before this fix — a successor session will be the first
real-world confirmation that the registered settings.json entries actually
trigger CC's hook dispatcher. The hook scripts themselves are unchanged from
the pre-fix versions verified in v0.25.0 through v0.31.2; only the
registration channel changed.

## v0.31.2 — dedup plugin MCP catalog when project provides its own + regression gate

Follow-up to v0.31.1's PreToolUse repair. Four small functional cleanups:

### Fixed
- `claude-plugin/scripts/mcp-launcher.js` — when the user's project has its
  own `.mcp.json` registering a `code-graph*` server (the recommended setup
  for dev work on this repo, so usage telemetry lands in the project's
  `.code-graph/usage.jsonl`), the plugin's MCP server now serves a minimal
  "0 tools" JSON-RPC stub instead of registering a duplicate 7-tool catalog.
  Saves ~4-8 KB of context per session and removes the AI's ambiguity
  between the two equivalent namespaces. Env override
  `CODE_GRAPH_FORCE_PLUGIN_MCP=1` bypasses the dedup gate.
- `claude-plugin/scripts/pre-edit-guide.js` — `code-graph-mcp` binary is now
  resolved via `findBinary()` (consistent with `mcp-launcher.js` and
  `incremental-index.js`) instead of a bare PATH lookup, so npm-global
  installs on systems where the global bin dir isn't on PATH for non-login
  shells (the failure mode behind mem #8187) no longer leave the hook
  silently inert.

### Added
- `claude-plugin/scripts/hooks.test.js` (5 tests) — regression gate that
  parses `hooks/hooks.json` and rejects matchers containing the expression
  DSL tokens (`==`, `tool `, `||`, `&&`, `"`) that caused the v0.25.0 →
  v0.31.0 silent breakage. Verified by a negative test (re-injecting the
  broken matcher makes the gate fail with a concrete diagnostic). Wired
  into `.github/workflows/ci.yml`'s `plugin-tests` job alongside
  `pre-edit-guide.test.js`, which was also missing from the CI matrix.

## v0.31.1 — fix: PreToolUse hooks never fired

Two compounding bugs caused `PreToolUse:Edit`/`Bash`/`Read` hooks and the
`PostToolUse:Write|Edit` incremental-index hook to be **registered but
silently inert** since v0.25.0. Replaying 147 historical `.rs` Edits across
5 sessions confirms the regression — none of them produced hook output,
even on functions with 30+ direct callers (`conn`, `lock_or_recover`).

### Fixed
- `claude-plugin/hooks/hooks.json` — matchers used the expression-style
  syntax `"tool == \"Edit\""`, but Claude Code's `matcher` field is a literal
  string / pipe-list / regex matched against the tool name. Switched to
  `"Edit"`, `"Bash"`, `"Read"`, and `"Write|Edit"`.
- `claude-plugin/scripts/pre-edit-guide.js` — `code-graph-mcp impact` was
  called without `--file`, so common short symbol names (`open`, `new`,
  `from`, `parse`, `init`) hit the ambiguous-symbol code path and returned
  `{"error": "..."}` instead of caller data. The hook then read
  `direct_callers || 0` from the error object and silently exited at the
  `< 1` gate. Now passes `--file <relative-path>` (the indexer stores
  repo-relative paths; absolute paths return 0 matches) and explicitly
  handles `error`-keyed responses.

Measured before/after on the same 147 historical Edits, impact-injection
fires went from **0 / 147 (0%)** to **37 / 147 (25%)**. Test-only sessions
(routing_bench.rs heavy) stay low by design — bench helpers have no
production callers, so silent-skip is correct there.

## v0.31.0 — Multi-account isolation: honor `CLAUDE_CONFIG_DIR`

Closes [#20](https://github.com/sdsrss/code-graph-mcp/issues/20). Users with
multiple Claude Code accounts (personal vs work) set `CLAUDE_CONFIG_DIR` to
keep their `settings.json`, `plugins/`, and `projects/` separate. The plugin
previously hardcoded `~/.claude/` across ~15 call sites, so for any account
running with the override:

- hook registrations were written to a file Claude Code did not read,
- adoption files / MEMORY.md sentinels landed in the wrong project dir,
- cache cleanup and `installed_plugins.json` writes pointed at the default
  install, not the configured one.

Net effect: the plugin was effectively broken under multi-account isolation.

### Fixed
- New shared helper `claude-plugin/scripts/claude-config.js` exposes
  `claudeHome()` (returns `process.env.CLAUDE_CONFIG_DIR || ~/.claude`,
  re-read on every call).
- `lifecycle.js`, `auto-update.js`, `doctor.js`, `session-init.js`,
  `adopt.js` now route all `~/.claude/...` paths through the helper.
- `adopt.js: memoryDir()` keeps its `(cwd, home)` signature for back-compat;
  `CLAUDE_CONFIG_DIR` simply overrides the `home + .claude` join.
- `adopt.js: isPluginModeInstall()` matches both the legacy
  `~/.claude/plugins/` marker and `CLAUDE_CONFIG_DIR/plugins/`.

### Tests
- New `claude-config.test.js`: env-var resolution + empty-string fallback.
- `adopt.test.js`: `memoryDir` + `isPluginModeInstall` honor the override.
- `lifecycle.e2e.test.js`: full install subprocess writes into
  `CLAUDE_CONFIG_DIR` and never touches default `~/.claude/`.

No default-behavior change: when `CLAUDE_CONFIG_DIR` is unset, every path
resolves exactly as before.

## v0.30.0 — UX pass: 16 silent-failure / misleading-feedback fixes

Four rounds of end-to-end dogfooding (fresh-project workflows, MCP stdio
fuzz, IO edge cases) surfaced 16 places where the tool silently swallowed
errors, gave misleading guidance, or returned empty results indistinguishable
from a successful no-op. All four commits are pure UX/correctness — no API
removals, no behavior changes for happy paths.

### Fixed — CLI feedback honesty
- **`incremental-index` now reports file deletions** (`src/indexer/pipeline/`):
  `IndexResult` gained `files_deleted`; the summary line reads "N files
  updated, M files removed, K nodes created" when M > 0. Previously a
  delete-only incremental said "0 files updated, 0 nodes created" — looked
  like a no-op even when nodes/edges were cascade-deleted.
- **`deps <file>` distinguishes missing-file from no-imports**
  (`src/cli.rs`): pre-check `project_root.join(file_path).is_file()` and
  report "File not found" before the barrel-scan fallback fires.
- **`deps <file>` surfaces unresolved imports** (`src/cli.rs`): when all
  edges point to `<external>` or cross-language targets and get filtered
  out, render "(no resolved deps; N unresolved outgoing/incoming)" instead
  of just printing the bare filename. JSON gets matching `unresolved_*`
  fields.
- **`callgraph` suppresses no-op fuzzy resolve** (`src/cli.rs`): no longer
  prints `[code-graph] Resolved 'X' → 'X'` when fuzzy matched the same
  input verbatim.
- **`incremental-index` in non-git dir prints why it skipped** (`src/main.rs`):
  the silent-bail guard for multi-repo workspace parents now emits "Skipping
  index: no .git anchor or existing .code-graph/ at …" in non-quiet mode.
  `--quiet` (hook path) remains silent — PostToolUse contract preserved.
- **`map` on empty project replaces dangling header** (`src/cli.rs`):
  "(empty project — no indexed source files)" instead of a lone "Modules:".

### Fixed — `health-check` / `doctor` contract
- **`health-check --json` emits valid JSON even with no index** (`src/cli.rs`):
  returns `{healthy:false, reason:"no_index", issue:...}` + exit 0 instead
  of bailing with stderr "No index found" + exit 1. Non-JSON mode keeps the
  stderr+exit-1 contract for interactive callers.
- **`doctor` routes "No index found" to the index-empty fix path**
  (`claude-plugin/scripts/doctor.js`): old behavior labeled it `binary-broken`
  (no fix handler) and reported "Fixing… 0/1 addressed". Now detects the
  `reason:"no_index"` JSON flag and runs `incremental-index` automatically.

### Fixed — MCP empty-string args
- **`get_call_graph` / `find_references` / `get_ast_node` / `find_similar_code`
  reject empty/whitespace `symbol_name`**: previously `symbol_name=""` fell
  through to fuzzy resolve and silently matched a random Unique() candidate
  (seen returning `function:"x"` from a DB with one fn called `x`).
- **`get_call_graph` / `find_references` treat empty `file_path` as absent**:
  `Some("")` used to filter to a nonexistent path, producing "Symbol 'x'
  not found in file ''" or silent empty-edge results.
- **`trace_http_chain(route_path="")` rejects upfront**: empty pattern used
  to substring-match every route, returning "no routes found" indistinguishable
  from a typo.
- **`dependency_graph(file_path="")` rejects upfront**: empty path used to
  trigger the "looks like a directory" hint at "", giving wrong guidance.

### Fixed — path & filesystem hardening
- **`module_overview` rejects absolute paths / `../` traversal / Windows
  drive letters** (`src/mcp/server/tools/overview.rs`): the index stores
  relative paths from project root, so `/etc`, `../foo`, `C:\Windows` will
  never match. Old behavior silently returned `0 files`; now errors with
  actionable message.
- **`snapshot create --out` pre-flights the target path** (`src/cli.rs`):
  rejects dir-as-out (`/tmp/`) and missing-parent (`/nonexistent/snap.db`)
  before SQLite VACUUM INTO leaks its raw "unable to open database file"
  error chain.
- **`scan_directory` tolerates per-entry walk errors**
  (`src/indexer/merkle.rs`): a single unreadable subdir (`chmod 000` build
  artifact, restricted mount, broken symlink target) used to abort the
  whole rebuild with `Permission denied (os error 13)`. Now skip-and-warn
  via tracing — readable files still get indexed.

### Notes
- 526 tests pass, `cargo +1.95.0 clippy --all-targets -- -D warnings` clean
  on both default and `--no-default-features`.
- No public API removals. New `IndexResult.files_deleted` field is purely
  additive. MCP tool schemas unchanged.

## v0.29.0 — edge resolution precision pass (12 silent-failure fixes)

Five rounds of end-to-end dogfooding surfaced 12 silent-failure / mis-attribution
bugs across the parser → resolver → MCP/CLI surface. Net effect on the project's
own index: **dead-code false positives dropped 21 → 6** (all remaining 6 are
documented design limitations: receiver `obj.method()` calls, type-as-field
references, cross-file constant access), **edges restored 3030 → 3266+** (+8%
real call relations recovered). The user-visible behavior shift is the
TypeScript `return_type` shape (no longer leaks `": "` prefix) — minor bump
flags that.

### Fixed — parser
- **TypeScript `return_type` strips leading `:`** (`src/parser/treesitter.rs`):
  `extract_signature_info` read the tree-sitter `return_type` field verbatim,
  which on TS/JS is a `type_annotation` node whose text starts with `:`. Stored
  values were `": string"` not `"string"`; signatures rendered `(name: string)
  -> : string`. Now trimmed at extraction — Python / Rust / Go produce clean
  values unchanged (no-op when first char isn't `:`).
- **Rust generic trait impl emits no method edges** (`src/parser/relations/rust.rs`,
  `src/parser/treesitter.rs`): `impl<'a, W: Write> Write for CapWriter<'a, W>`
  stored `source_name = "CapWriter<'a, W>"` from `type_node` verbatim; Phase 2
  source resolution exact-name-matched against `"CapWriter"` and dropped every
  method edge. Every generic trait impl's methods looked dead. Now strips
  generic params at both extraction sites (relations + treesitter qualified_name).
- **Method-level implements edges fan out within a file**
  (`src/parser/relations/mod.rs`, `src/indexer/pipeline/index_files.rs`): N
  structs each `impl Trait for StructN` in one file produced N×N×methods edge
  combinations because the resolver matched bare method names against every
  same-name node in the file. Parser now stamps `{"q":"impl_method","v":"<Type>"}`;
  resolver filters method candidates by `qualified_name LIKE "<Type>.%"` via
  the existing `self_filter_candidates`.

### Fixed — resolver
- **Same-file targets dropped under Path qualifier**
  (`src/indexer/pipeline/index_files.rs`): the `CalleeMeta::Path` branch
  excluded `local_ids` before applying the path filter, contradicting the
  spec's "same-file matches take precedence". `Foo::helper()` in the same
  file as `impl Foo { fn helper }` produced zero call edges. Now includes
  same-file candidates in the path-filtered pool.
- **`path_filter_candidates` misses single-file Rust mods**
  (`src/indexer/pipeline/resolve.rs`): only matched `/<seg>/` or `<seg>/`
  directory boundaries, so `crate::domain::foo()` resolving into `src/domain.rs`
  (single-file mod, no `domain/` directory) dropped. Now also accepts
  `path.ends_with("/<last_seg>.rs")`. **This single fix eliminated 14 of the
  20 dead-code false positives** (`normalize_type_filter`, all `migrate_v*_to_v*`,
  `create_tables_sql`, etc.).

### Fixed — MCP / CLI surface validation
- **`ast_search` / `semantic_code_search` invalid `type` filter silently empty**
  (`src/mcp/server/tools/ast_search.rs`, `src/mcp/server/tools/search.rs`,
  `src/cli.rs`): `normalize_type_filter("INVALID")` returns empty `Vec`;
  `.any()` on empty returns false → every node filtered out → "No results"
  with exit 0. Now bails up-front with the valid-values list.
- **`find_references` invalid `relation` silently falls back to `all`**
  (`src/mcp/server/tools/refs.rs`): `match relation { "calls"=>..., _=>None }`
  treated `"call"` (typo) identical to `"all"`. Now explicit `"all" => None`
  + Err on anything else.
- **`get_call_graph` `symbol_name` + `route_path` silently uses `route_path`**
  (`src/mcp/server/tools/callgraph.rs`): schema marks them mutually exclusive
  but impl preferred route_path silently. Now errors with the conflict.
- **`module_overview path=""` returns the whole project**
  (`src/mcp/server/tools/overview.rs`, `src/cli.rs`): empty string normalized
  to the same "match all" prefix as `"."`. Common variable-substitution bug
  (`process.env.X || ""` → dumps entire repo). Now errors; `"."` still works
  as the documented match-all alias.

### Fixed — FTS5 / snapshot / concurrency
- **FTS5 reserved-word queries leak raw syntax error**
  (`src/storage/queries/search.rs`): `semantic_code_search query="NOT"`
  returned `Error: fts5: syntax error near "NOT"`. Each sanitized token is
  now wrapped in `"…"` (FTS5 phrase syntax) — equivalent for normal tokens,
  defuses the NOT/AND/OR/NEAR keywords.
- **`snapshot inspect` silently succeeds on truncated SQLite**
  (`src/snapshot/mod.rs`): a 100-byte file starting with `"SQLite format 3\0"`
  magic passed the header check; `Database::open` initialized empty schema;
  every meta lookup returned None → defaults. Inspect returned a fake "empty
  valid snapshot" with zeroed fields. Now bails when all meta is missing.
- **Concurrent `incremental-index` shows cryptic `Error code 5: database is locked`**
  (`src/cli.rs`): two CLI processes racing on `.code-graph/index.db` got raw
  rusqlite SQLITE_BUSY. New `wrap_busy()` translates to "Another `code-graph-mcp`
  process is writing... Wait for it to finish, then retry." while keeping the
  original error for debug.

### Regression coverage
- `tests/cli_e2e.rs::test_cli_ast_search_invalid_type` /
  `test_cli_search_invalid_node_type` / `test_cli_overview_empty_path_errors`.
- `tests/integration.rs::test_module_overview_empty_path_errors` /
  `test_find_references_invalid_relation_errors` /
  `test_get_call_graph_symbol_and_route_mutually_exclusive` /
  `test_fts5_keyword_query_does_not_leak_syntax_error`.
- `tests/integration_call_qualifier.rs::path_qualifier_keeps_same_file_target` /
  `path_qualifier_resolves_single_file_rust_mod` /
  `same_file_generic_impl_method_edges_dont_fan_out`.
- `src/snapshot/tests.rs::inspect_rejects_truncated_sqlite_header`.
- `src/parser/relations/tests.rs::test_extract_rust_impl_trait_generic_type_strips_params`.

353+67+50+9+19 tests pass; `cargo +1.95.0 clippy --no-default-features` and
`--all-targets` both clean under `-D warnings`.

### Known limitations preserved
Six dead-code entries remain after this pass and are documented design gaps,
not bugs:
- Receiver method calls (`obj.method()` where receiver type isn't statically
  inferable): `validate`, `file_exists`, `db()`.
- Type-as-field references (`pub foo: SomeStruct`): `SnapshotConfig`.
- Cross-file constant access via Path qualifier (extractor doesn't emit edges
  for non-call identifier references): `PROD_SOURCE_FILTER_AND`,
  `TEST_SOURCE_FILTER_OR`.

Fixing these requires either a Rust type inferencer or extending edge
extraction to non-call identifier references — out of scope for this release.

## v0.28.0 — trigger-rate gate + hook coverage expansion

Data-driven release based on a 7-day usage audit (2026-05-12 → 2026-05-14,
141 main sessions): **1972 raw `bash grep` vs 47 code-graph calls** (the 47
all came from one dogfooding session — zero organic cg invocations across
20 sampled real sessions). Recall bench has stayed at P@1=100% throughout
2026, so the gap isn't "which tool" but "is the model reaching for a tool
at all". This release adds a measurement axis for that and widens three
hook-layer interventions that were missing real-world surface.

### Added
- **`BenchMode::TriggerRate`** (`tests/routing_bench.rs`): new bench mode with
  `tool_choice: auto` (recall benches use `required` / `any`). Surfaces the
  "model returned no tool / picked Bash" failure mode that forced-tool benches
  hide. 12-entry hard `TRIGGER_ORACLE` (10 `__CG__` + 2 `__DECOY__`) covering
  pure symptom prompts ("今天的报告数据不准"), misleading-grep framing
  ("用 grep 找一下…"), answer-flavored guesses ("应该是 cleanup 那段没等完吧"),
  and generic feature refs. New `bash_decoy()` (TriggerRate-only — kept out of
  ContextRich so v0.17.x baselines stay comparable), `compute_trigger_metrics`,
  `matches_trigger_class`, `DECOY_NAMES`. Baseline on Sonnet 4.5: **60.0%
  trigger (6/10) · 30.0% no-tool (3/10) · 100% decoy boundary · 0% leak**.
  Run with `ROUTING_BENCH_MODE=trigger-rate`.
- **PreToolUse:Read fanout detector** (`claude-plugin/scripts/pre-read-guide.js`):
  new hook fires on the 5th Read into the same source dir within a 30-min
  window. 7d audit found 16 sessions with 5+ Reads into one dir (top: 13
  reads into `backend/app/services/`) — Claude burns ~500-2000 tokens per
  Read when one `module_overview path=X/` returns symbols + caller counts in
  ~600. Per-cwd state in `/tmp/.code-graph-readfan-<hash>.json`, 5-min
  per-dir cooldown, 30-min state TTL. One-line hint suggests
  `code-graph-mcp overview <dir>/` or MCP `module_overview path=<dir>`. New
  `tool == "Read"` matcher in `claude-plugin/hooks/hooks.json` (3s timeout).
- **UserPromptSubmit symptom-hint fallback** (`claude-plugin/scripts/user-prompt-context.js`):
  24-entry `SYMPTOM_PATTERNS` (`/bug/i` · `/crash/i` · `/not work/i` ·
  `/why does/i` · `挂了` · `失败了` · `卡死` · `不准` · `缺失` · `又失败` ·
  `为什么` · `哪里[\s\S]{0,5}(?:错|有问题|不对)` · …). When the 4 existing
  channels (intent / qualified symbol / file path / any symbol) all return
  no actionable query AND the message has symptom phrasing AND a 10-min
  cooldown is cold, `determineQueryType` returns `{ type: 'symptom-hint' }`
  and `runMain` emits ONE LINE of prose (NO CLI execution — Phase A's lesson:
  heavy structured injection backfires on borderline prompts). Hint format:
  `[code-graph:hint] indexed repo — for vague-symptom prompts, try
  \`semantic_code_search "<symptom>"\` or \`module_overview <suspected-dir>\`
  to surface candidate code structurally. Skip if not searching code.`
  Actionable paths (impact / overview / callgraph / search) still take
  precedence; signature gains a 5th `message = ''` param keeping all legacy
  callers backward-compat.

### Changed
- **`pre-grep-guide.js` SRC_PATH expansion** (`claude-plugin/scripts/pre-grep-guide.js`):
  added 20 backend / DDD / web convention prefixes — `backend frontend services
  models domain controllers views handlers middleware routes repositories
  entities migrations tasks jobs workers features modules api web`. Root cause:
  the v0.21+ regex required prefix terms in `(src|tests|lib|…|app|server|
  client)/` to be preceded by whitespace / quote / start-of-string, so the
  dominant daagu-style miss `backend/app/services/…` never fired (`app/` sat
  after `backend/`). Audit shows 5 of the worst-offender sessions used exactly
  this layout. Generic terms (`core` / `utils` / `shared` / `common` / `types`)
  deliberately omitted — too many non-code contexts. 10 new positive regression
  tests pass; 3 precision guards (`web.config` / `node_modules/` / `docs/*.md`)
  stay false.

### Fixed
- `doc_lazy_continuation` lint on `tests/routing_bench.rs:704` —
  `Grep \n /// + Read decoys` wrapped such that the continuation line began
  with `+`, which clippy 1.95 reads as a markdown bullet. Reworded to
  `the Grep/Read decoys alone don't model…`. Both clippy passes
  (`--no-default-features` and `--all-targets`) clean.

### Failed experiment (reverted in-session, kept for lesson)
- **Phase A — "DO NOT use when X (Grep/Read/Bash)" steering in tool descriptions**:
  Hypothesis (mem #8234 / `feedback_negative_steering_backfire.md`): mem-style
  negative steering should lift trigger rate. **Bench falsified the hypothesis
  before commit**: TriggerRate baseline **60% → 40%** (3-run unanimous), two
  brand-new misses (`今天的报告数据不准 → None`, `test 又挂了 → Bash`), and the
  target miss (`用 grep 找一下…` → Grep) wasn't fixed. Root cause: clauses like
  "`INSTEAD OF Grep`" + "`DO NOT for: literals (Grep)`" are self-contradictory
  — when the user hints "grep" the second clause licenses the wrong tool. New
  rule in `feedback_negative_steering_backfire.md`: negative steering is safe
  only between cg-vs-cg, never pointing at native decoys. Revert via
  `git restore`, no commit hit `main`.

### Regression coverage
- `tests/routing_bench.rs::scoring_tests::compute_trigger_metrics_*` (4
  tests covering all-correct / mixed / empty-oracle / missing-pick), plus
  `decoy_tests::bash_decoy_has_required_fields_and_anchor`,
  `mode_tests::detect_mode_trigger_rate`, `trigger_oracle_well_formed`.
- `claude-plugin/scripts/pre-read-guide.test.js` — 34 tests covering source
  extension whitelist, dir extraction, cooldown / threshold logic, state
  load+save round-trip, TTL pruning, malformed-JSON tolerance, hint shape,
  silenced env, integrated 5-read flow.
- `claude-plugin/scripts/pre-grep-guide.test.js` — 10 new positive prefix
  regressions (`backend/app/services/`, `services/scheduler/`, `models/`,
  `controllers/`, `domain/`, `handlers/`, `migrations/`, `features/`,
  `api/`, `frontend/`) + 3 precision guards. 52/52 pass.
- `claude-plugin/scripts/user-prompt-context.test.js` — 18 new tests
  (`hasSymptom` positive + precision, `determineQueryType` symptom-fallback
  with cooldown + precedence + backward-compat, integration via `analyze`).
  99/99 pass.

### Rationale anchor
The strategy: don't try to argue Claude into picking better tools at the
description level — bench has already saturated there. Instead measure
"trigger rate" as a separate axis (Phase 0/F), widen the existing Bash and
Read hooks to fire on real-world layouts (Phases C/D), and add a low-noise
symptom-only fallback to the UPS hook for the bug-shaped prompts that
slipped through the existing 4 channels (Phase E). Phase A demonstrated
that re-trying the description angle backfires. Phase B (MCP `instructions`
symptom-mapping line) deliberately deferred until the C/D/E real-world
fire-rate data justifies it.

Surface: plugin-shipped behavior — users with adopted CG indexes will see
new `[code-graph]` hints on `bash grep -rn backend/services/...`, 5+ Reads
into the same source dir, and bug-flavored prompts that previously got
zero UPS injection. Escape hatch unchanged: `CODE_GRAPH_QUIET_HOOKS=1`
(env in `~/.claude/settings.json`).

Pre-push parity: `cargo +1.95.0 clippy --no-default-features --all-targets
-- -D warnings` clean on both targets; `node --test claude-plugin/scripts/
*.test.js` 251 tests pass (52 pre-grep + 34 pre-read + 99 user-prompt +
66 other); `cargo test` full suite passes.

## v0.27.0 — Python call relations + dead-code truncation guard

### Fixed
- **Python call-edge extraction (P0)**: `src/parser/relations/mod.rs` only
  matched the `call_expression` arm plus a Ruby-guarded `call` arm;
  tree-sitter-python emits `call` nodes that fell through to a no-op, so
  every `.py` file produced **0 call edges** despite README and `CLAUDE.md`
  documenting Python as Full tier. Knock-on effects: `module_overview`
  showed `caller_count=0` for every Python symbol, `find_dead_code`
  over-reported orphans, and `impact_analysis` / `get_call_graph` /
  `find_references` returned wrong results for any Python query. New
  `"call" if config.name == "python"` arm, plus `helpers::extract_callee_name`
  now treats Python `attribute` (field name `attribute`, not `property` /
  `field`) the same as JS `member_expression`. Reindexing this repo:
  **0 → 2969 total edges**; `scripts/analyze-search-queries.py` now
  produces non-zero caller counts.
- `cmd_overview` JSON empty contract (`src/cli.rs`): `overview . --json`
  returned `[]` on stdout but exited 1 with `Error: [code-graph] No symbols
  found under: .` smeared on stderr by `anyhow::bail!`, breaking log
  consumers piping stdout to `jq`. Now `.` normalizes to project root
  (mirroring MCP `tool_module_overview`); JSON-mode empty path emits a
  clean `eprintln!` + `exit(1)` so stderr stays free of the anyhow `Error:`
  prefix.
- `find_dead_code` truncation guard (`src/storage/queries/dead_code.rs`):
  when `CODE_GRAPH_MAX_CODE_LEN` caps a long function's stored body,
  references in the truncated tail were invisible to the SQL `instr`
  fallback, falsely flagging callback targets as dead. New OR clause
  detects truncated hosts via **two co-signals** — trailing `...` sentinel
  **and** declared-span > stored-newline-count by 5+ lines — and gives
  same-file names benefit of the doubt. Single signal alone is rejected
  (Python `def stub(): ...` has the sentinel without a span gap; compact
  test fixtures have a gap without the sentinel). Default 4 KB limit means
  real-world activation is rare.
- `snapshot::{mod,install}.rs`: three best-effort `git` invocations
  (`rev-parse HEAD` / `remote get-url origin` / `cat-file -e`) now redirect
  stderr to `Stdio::null()`. Previously `fatal: not a git repository`
  leaked into `cargo test` output and `snapshot create` runs on non-git
  roots.

### Removed
- `LanguageConfig.call_node_kind` field — defined but never read. The call
  dispatcher in `relations/mod.rs` uses hardcoded literal match arms because
  per-language call shapes diverge too far for one string to drive them
  (Ruby's `call` doubles as `require`; PHP splits into three kinds; C# uses
  `invocation_expression`; Bash uses `command`; Python's `call` carries an
  `attribute` field for method names). Keeping the field misled contributors
  into thinking new languages could be added by editing config alone — that
  was the Python regression's root cause. Field + assertions removed; the
  dispatcher entry now carries a comment enumerating every language's
  call-node kind so the trap is visible at the dispatch site.

### Regression coverage
- `src/parser/relations/tests.rs::test_extract_python_bare_call`,
  `::test_extract_python_method_call` — Python `call` arm + `attribute`
  callee.
- `src/storage/queries/dead_code.rs::tests::test_find_dead_code_skips_when_caller_content_truncated`
  — truncation guard.
- `tests/cli_e2e.rs::test_cli_overview_dot_means_project_root`,
  `::test_cli_overview_json_empty_no_anyhow_prefix` — overview path
  normalization + JSON empty-stderr cleanliness.

### Rationale anchor
Autonomous iteration loop (4 rounds): 1 P0 + 4 P1 + 1 P2 surfaced, all
fixed. Each change stays inside internal surface — no Δ-contract on MCP
tool schemas, CLI flags published to npm users, or SQLite schema.
Pre-push parity: `cargo +1.95.0 clippy --no-default-features --all-targets -- -D warnings`
clean on both targets; 467 tests pass.

## v0.26.0 — UserPromptSubmit context push default ON + trigger hints

### Changed
- `claude-plugin/scripts/user-prompt-context.js`: `computeQuietHooks` default
  flipped back to **noisy** (push ON). The v0.21 opt-in flip cited routing-bench
  P@1=100% as evidence the agent already picks tools correctly without push,
  but that bench measures *triage accuracy once the agent has decided to query
  a tool* — not the prior question of whether the agent reaches for a tool at
  all. The real counter-evidence is in `pre-grep-guide.js`'s 15-day baseline:
  **429 raw `grep` vs 191 functional CLI calls on the same indexed source tree
  (~13× pre-training bias toward grep)**. Push is the corrective. Per-type
  cooldowns (impact 30s / overview 5min / callgraph 60s / search 60s) cap
  frequency; the 8-char message floor + `shouldSkip` filter keep confirmation
  chatter silent. Escape hatch: `CODE_GRAPH_QUIET_HOOKS=1`.
- `claude-plugin/scripts/pre-edit-guide.js`: caller threshold lowered from
  `directCallers < 2` to `< 1`. Editing any function with one or more callers
  now surfaces the one-line impact summary; the per-symbol 2-minute cooldown
  is unchanged so the noise floor stays the same.
- SessionStart `project_map` injection (`session-init.js`) **stays default
  OFF** — that hook is a static dump duplicated by `MEMORY.md`'s decision
  table; this hook is a reactive trigger reminder. The two defaults are
  intentionally asymmetric.

### Added
- `src/mcp/server/mod.rs` MCP `instructions` field gains one line of explicit
  scenario triggers: `"who calls X?" → get_call_graph; "impact of X?" or
  before editing a fn → get_ast_node include_impact=true; concept search
  without an exact symbol → semantic_code_search`. Compile-time
  `assert!(NOISY.len() <= 1500)` budget guard unchanged (now 772 / 1500 bytes).
- Project `CLAUDE.md` "Code Graph Integration" section replaced with a 5-row
  trigger table (who calls / impact / module overview / concept search / HTTP
  route) — `CLAUDE.md` is loaded every session, higher priority than the
  invited-memory path in `MEMORY.md`.
- `claude-plugin/templates/plugin_code_graph_mcp.md` clarifies the asymmetric
  hook defaults and lists `CODE_GRAPH_QUIET_HOOKS=1` as the context-push
  escape hatch alongside the existing `VERBOSE_HOOKS` / `QUIET_HOOKS=0` flags.

### Rationale anchor
- mem #8234 documents that hook content has **bounded leverage** when the
  current bench corpus is saturated (Sonnet 4.5 hits P@1=100%); bench is the
  right oracle for tool-description boundary disambiguation, not for
  server-prelude / hook-content tuning. This release therefore lands without
  a fresh routing-bench cycle — the changes are all hook-content surface.

### Verification
- `cargo check`: clean (compile-time `assert!(len <= 1500)` on
  `NOISY` instructions string holds; final length ~772 bytes).
- `node --test claude-plugin/scripts/user-prompt-context.test.js`:
  77/77 pass — six `computeQuietHooks` priority-chain cases rewritten for
  the default-noisy invariant; one e2e check kept on the `=1` escape hatch.
- No change to `routing_bench.rs` corpus; intentionally skipped per mem #8234.

### Migration
- Existing users on default env will start seeing `[code-graph:impact|
  overview|callgraph|search]` push lines on intent-matching prompts. Set
  `CODE_GRAPH_QUIET_HOOKS=1` in `~/.claude/settings.json` env to opt out.
- Adopted projects: the `plugin_code_graph_mcp.md` template auto-refreshes on
  next SessionStart (unless `CODE_GRAPH_NO_TEMPLATE_REFRESH=1` is set).
- No data-migration, no schema change, no MCP tool API change.

## v0.25.1 — findBinary disk cache version-check

### Fixed
- `claude-plugin/scripts/find-binary.js`: disk cache (`~/.cache/code-graph/binary-path`)
  now validates the cached binary's `--version` against the package
  version before returning it. Previously the cache short-circuit at
  `findBinary()` entry only checked `isNativeBinary(cached)` (file
  exists + right basename) — once a stale path got written, it
  shadowed every newer binary on the system **forever**. Symptom on
  this dev machine: cache pinned `bin/code-graph-mcp` v0.5.28 (the
  un-tracked `scripts/copy-binary.js` artifact from March 17) while
  `~/.cargo/bin/code-graph-mcp` was the freshly installed v0.25.0,
  causing `incremental-index.test.js` to fail mid-pre-commit hook with
  the older binary's pre-v0.16.9 hard-bail behavior.

### How the bug bites end users
- Asymmetric version-check coverage. Auto-update cache at
  `find-binary.js:184-188` was already version-gated (mem #8187 fixed
  three install-chain bugs but landed only on the `~/.cache/.../bin/`
  branch). Disk cache `binary-path` — the entry-level fast-path that
  runs on **every** hook tick — had no equivalent gate. After
  `npm install -g` of an updated platform pkg, or any path drift in
  the platform-pkg layout, the disk cache would keep returning the
  pre-update binary until a user manually `rm`-ed the cache file.
- New `isCachedBinaryFresh(cachedPath, pkgVersion)` helper. Permissive
  on unknown values (missing pkg version, unreadable binary `--version`
  output) → trust the cache (don't refuse the only path we know
  about). Strict only when both versions parse and cached < pkg.

### Verification
- `node --test find-binary.test.js`: 19/19 pass — 11 existing +
  8 new covering THE BUG case (cached `0.5.28` vs pkg `0.25.0` →
  invalidate), equal versions, newer cache, missing pkg version
  permissive, unreadable binary permissive, non-existent path,
  null/undefined input, basename mismatch.
- `node --test lifecycle.test.js`: 12/12 — schema regression-clean.
- `cargo +1.95.0 clippy --no-default-features --all-targets -D warnings`: 0.

### Migration
- No user action needed. First findBinary call after upgrade detects
  stale cache (older than 0.25.1 cached binary) → invalidates →
  falls through to the rest of the discovery chain (target/release →
  auto-update cache → platform pkg → bundled → cargo install → PATH).
- For users on the dev branch with manually-recorded cache paths:
  `rm ~/.cache/code-graph/binary-path` triggers the same fresh walk.

## v0.25.0 — PreToolUse:Bash hint hook (raw-grep → cg CLI nudge)

### Added
- `claude-plugin/scripts/pre-grep-guide.js`: new PreToolUse:Bash hook that
  detects raw `grep`/`rg`/`ag` invocations on the indexed source tree
  (`src/`, `tests/`, `lib/`, `scripts/`, `claude-plugin/`, `tools/`, `pkg/`,
  `cmd/`, `internal/`, `app/`, `components/`, `server/`, `client/`,
  `crates/`, `packages/`) and emits a 6-line hint pointing at
  `code-graph-mcp grep / ast-search / callgraph / show`. Fires only on
  bare grep at command HEAD (pipe-greps like `cargo test | grep FAILED`
  are output filters and skipped). Per-command-hash cooldown 60s prevents
  repeat noise. Registered in `claude-plugin/hooks/hooks.json` with
  3s timeout.

### Motivation
- 15-day session telemetry (78 sessions / 13.5K assistant turns) showed
  429 raw `grep -rn` calls on source trees vs 437 `code-graph-mcp`
  invocations — ~1:1 overall but with severe variance (3 work days at
  10:0 or worse against `code-graph-mcp`, today's 05-11 at 39:10).
  Pre-training bias gives `grep -rn pattern src/` an enormous default
  weight; tool descriptions alone can route correctly (routing_bench
  Opus 4.7 P@1=95.5% in tool-only mode) but don't surface the indexed
  alternative when Claude isn't already deciding between tools. This
  hook closes the loop at the Bash entry point — same shape as the
  existing PreToolUse:Edit (`pre-edit-guide.js`) impact-summary hook.

### Verification
- `node --test claude-plugin/scripts/pre-grep-guide.test.js`: 35/35 pass.
  Covers fire cases (grep/rg/ag on src + tests + lib + claude-plugin,
  alternation patterns, env-prefixed, head/tail pipes downstream),
  skip cases (pipe-grep output filters, code-graph-mcp self-invocation,
  config-only targets like Cargo.toml/.gitignore/CHANGELOG.md, non-search
  tools like ls/cat/git/find), and 5 regression cases lifted verbatim
  from 2026-05-11 session telemetry.
- `node --test claude-plugin/scripts/lifecycle.test.js`: 12/12 pass —
  hooks.json schema change accepted by lifecycle's hook-identity matcher.
- E2E sanity: piping `{"tool_input":{"command":"grep -rn ... src/storage/"}}`
  through `pre-grep-guide.js` emits the 6-line hint on first invocation,
  silent on repeat (cooldown verified), silent on `cargo test | grep FAILED`
  (pipe-grep correctly skipped).
- Bench unaffected: routing_bench is tool-only mode (forced
  `tool_choice=any`), Bash hook injection happens outside that path —
  no P@1 regression possible.

### Migration
- Plugin SessionStart auto-updates the hook registration via
  `${CLAUDE_PLUGIN_ROOT}` path indirection. Disable per-session with
  `CODE_GRAPH_QUIET_HOOKS=1` (already gates the whole hook tier).
  No `.code-graph/index.db` in CWD → hook exits silently regardless.

## v0.24.1 — Adoption tag specificity fix

### Fixed
- adopt: MEMORY.md index-line tags renamed to MCP-tool-aligned multi-word
  form (`impact-analysis`, `find-references`, `module-overview`,
  `semantic-search`, `dependency-graph`, `trace-http-chain`, `http-route`,
  `find-similar-code`). Previous single-word tags (`impact`, `refs`,
  `overview`, `semantic`, `deps`, `trace`, `route`, `similar`) collided
  with release-notes and commit-message prose under the claudemd §11
  `read-the-file` hook's word-boundary + 0-2 char declension regex,
  producing false-positive denies on prose like "fail-open semantics" or
  "overview of changes". `callgraph`, `ast-search`, `dead-code` retained
  (already multi-word). Affects four index-line variants in
  `claude-plugin/scripts/adopt.js` (generic + web-* / frontend /
  rust-go-python-node) and the Rust drift mirror in
  `tests/routing_bench.rs`.

### Migration
- Existing adopted projects auto-refresh on next plugin SessionStart:
  `needsRefresh` does bytewise compare of MEMORY.md against the new
  `desiredBlock`; `stripSentinelBlock` cleans the old block (still v1
  sentinel — no version bump needed) and the new block is written in
  place. Lock manual edits with `CODE_GRAPH_NO_TEMPLATE_REFRESH=1`.

### Verification
- Hook-regex stress prose: OLD tags 3 FP (`impact`, `overview`,
  `semantics`) → NEW tags 0 FP; legitimate references still match.
- `adopt.test.js`: 66/66 pass. New regression case `stale INDEX_LINE →
  adopt rewrites in place without duplicating sentinel blocks` covers
  the bump-without-strip-extension failure mode (would otherwise leave
  orphan v1 + new v2 blocks).
- `routing_bench index_line_drift_check`: pass (Rust mirror byte-aligned
  with JS source).
- routing_bench context-rich (2026-05-11, OpenRouter sonnet-4.5,
  domain=all, 3-run majority vote, 382s): Recall 41/42 = 97.6%,
  FP-rate 0/10 = 0%, Overall 51/52 = 98.1% — **zero regression** vs
  v0.17.3+pm-desc-dedup baseline (Backend 22/22 = 100% kept; Frontend
  19/20 = 95% kept; same residual path-anchored `src/components/` miss
  unrelated to this change). Confirms tag-rename preserves routing
  signal.

## v0.24.0 — Bare-name call qualifier (Rust)

### Fixed
- callgraph: Rust qualified calls (`Type::method`, `crate::path::fn`,
  `self.method`, `Self::method`, builder chains like `OpenOptions::new().create()`)
  no longer route to unrelated project functions sharing the rightmost name.
  Eliminates phantom callers in `impact_analysis` and `find_dead_code` for
  short-named functions (`new`/`create`/`open`/`from`).
- parser: `impl crate::path::Type { ... }` impl-block type now strips the
  leading path so qualified_name and SelfRecv payloads match (was producing
  `crate::path::Type.method` qualified_names that broke same-type LIKE
  matching).

### Migration
- Existing `.code-graph/` databases keep working (qualifier-aware resolution
  is a no-op when `edges.metadata IS NULL`). Run `code-graph-mcp index --rebuild`
  to populate qualifier metadata on existing Rust files; incremental indexing
  picks it up automatically as files change.

### Verification
- `impact run_full_index`: 36 → 33 transitive callers; the 3 documented
  phantoms (decompress_with_cap, try_acquire_index_lock, from_project_root)
  no longer appear.
- routing_bench P@1: 22/22 (no regression).
- 558 tests pass with default + `--no-default-features`. Clippy clean with
  `--all-features`.

## v0.23.1 — snapshot UX + FTS garbage-query guard

Follow-up enhancements to v0.23.0 snapshot work plus an unrelated
search-quality fix.

`snapshot create --out <path>` now auto-zstd-compresses when `<path>`
ends in `.db.zst` (level 9, matching the producer workflow template).
Raw `.db` output unchanged — the existing two-step `--out foo.db &&
zstd -9 foo.db` flow still works.

`snapshot inspect <file>` now accepts both `.db` and `.db.zst` (format
detected from magic bytes, not extension), so first-time users who run
`snapshot create --out foo.db && snapshot inspect foo.db` get sensible
output instead of zstd's cryptic "Unknown frame descriptor". Garbage or
wrong-format files now produce: "X is not a code-graph snapshot —
expected zstd-compressed (.db.zst) or raw SQLite (.db)". `snapshot
inspect <typo>` also surfaces the file path in the error chain instead
of bare "No such file or directory (os error 2)".

Non-https `[snapshot] url` in `.code-graph.toml` now writes to stderr in
addition to `tracing::warn!`, so users see the rejection on CLI startup
paths that don't install a tracing subscriber.

`fts5_search` no longer OR-fallbacks when the user's single-word query
has zero AND-mode hits AND the original token doesn't appear anywhere
in the FTS index. This was returning noise via camelCase token splits —
a query like `ZzzzNoMatchXyzzz` matched any code containing the literal
`--no-default-features` (split on `-`) or the Rust `match` keyword.
Acronyms like `RRF` are unaffected: the original token *is* indexed, so
OR-fallback runs as before for legitimate recall expansion. Multi-word
queries are unchanged.

## v0.23.0 — shared graph snapshot

Team-shared graph artifact via GitHub Releases. New CLI subcommands
`snapshot create` and `snapshot inspect`. MCP server auto-fetches the
latest published snapshot on first start (when no local index exists) and
falls through to the existing full-index path on any failure — snapshot is
an optimization, not a dependency. Workflow template shipped at
`claude-plugin/templates/code-graph-snapshot.yml`. New CLI
`reindex --from-snapshot` forces a re-fetch. Snapshot status surfaces in
`health-check --json`. Snapshot file is symbols+edges+FTS5 only (no
`node_vectors`) to decouple from embedding model choice. Spec:
`docs/superpowers/specs/2026-05-10-shared-graph-snapshot-design.md`.

## v0.22.2 — index.db sub-header size guard

Defensive hardening for `Database::open` recovery. The existing
`is_corruption_error` retry branch covers files that error on open, but a
main DB file shorter than the SQLite header (100 bytes) can land in
SQLite-version-dependent territory — sometimes treated as fresh, sometimes
silently combined with stale `.wal/.shm` residue from a prior crashed
indexing pass.

The new `sub_header_size_guard` runs at the top of `open_impl` and wipes
the entire main+wal+shm triple whenever the main file exists but is < 100
bytes, so every recovery path starts from the same blank state.

### Why now

Round 2 of the v0.22.x dogfood loop surfaced `health-check` exit codes that
varied across repeated runs against an interrupted indexing state. The
existing recovery branch was deterministic-by-luck — relying on a
particular SQLite version's tolerance for sub-header files. The guard
makes recovery deterministic-by-design.

### Tests

Four new unit tests in `src/storage/db.rs::tests` document the safety
contract: 0-byte main alone, 0-byte main + stale wal/shm, partial-write
under header size, and the regression guard for valid databases. Full
suite: 303 lib + 198 integration = 501 passed, 0 failed.

### Also in this release

- `fix(cli): preserve user --depth in callgraph requested_max_depth`
  (`73cd954`) — CLI no longer clamps `--depth` before passing to the engine;
  the engine's own `CALL_GRAPH_MAX_DEPTH` cap and the `requested_max_depth`
  / `effective_max_depth` envelope fields surface truncation truthfully.

## v0.22.1 — dogfood loop fixes (test/prod boundary + truncation bias)

Five bug fixes from a 5-round structured dogfood pass. All fixes converge on
one root pattern: the test/prod source classification was implemented in five
sites independently, and result truncation in `centralized_compress` was
biased against production callers when source data ordering put tests at
the array head/tail.

### Fixes

- **`get_ast_node` `called_by` post-truncation bias** (`src/mcp/server/tools/ast_node.rs`)
  When `include_references=true include_tests=true`, SQL row order without
  `ORDER BY` clustered test callers at array start/end, and `centralized_compress`
  kept first 10 + last 5 — leaving zero production callers visible for
  test-heavy targets like `conn` (49 prod / 76 test). Stable-sort prod-first
  inside the tool before emitting.

- **`find_references` references post-truncation bias** (`src/mcp/server/tools/refs.rs`)
  Same pattern as above, but worse because `find_references` defaults
  `include_tests=true` (rename audits need test sites). 125-caller targets
  collapsed to a 10-prod-of-cli + 5-tests-of-tests/ window with all
  `src/indexer/`, `src/mcp/`, `src/storage/` prod callers silently dropped.
  Same prod-first stable sort inside the tool.

- **`module_overview` `caller_count` includes test sources** (`src/storage/queries/routes.rs`)
  `get_module_exports` `cc` LEFT JOIN counted every incoming `calls` edge —
  did not filter source-side `is_test`. `parse_code` showed `caller_count=39`
  while `find_references include_tests=false` / `get_ast_node impact` /
  `project_map hot_functions` all reported 0 prod. Aligned with the four
  other prod-only counts via the same source-side filter pattern.

- **`ast_search` ranking includes test sources** (`src/storage/queries/nodes.rs`)
  `get_nodes_with_files_by_filters` `ORDER BY (SELECT COUNT(*) FROM edges …)`
  ranked test-only utility wrappers (e.g. `extract_relations` 0 prod / 64 test)
  above genuinely hot prod symbols. Same source-side filter applied.

- **`find_references` "Symbol not found" for test/bench symbols** (`src/mcp/server/tools/refs.rs`)
  `resolve_fuzzy_name` filters test/bench candidates upstream; previous error
  said "not found" even when the symbol was present. Re-query without the
  filter to detect the "found-but-filtered" case and surface a bypass hint
  with the actual file paths. Unblocks the dead-code → find_references
  reverse-trace flow.

### Internal refactor

`src/storage/queries/{routes,nodes,project_map}.rs` now share a single
SQL filter via `src/domain.rs::prod_source_join_sql()` +
`PROD_SOURCE_FILTER_AND` / `TEST_SOURCE_FILTER_OR`. Five duplicate `LIKE`
chains collapsed to one canonical source. New test/harness directory
conventions only need a single edit going forward.

### Tests

- `tests/mcp_stdio_integration.rs` (new, 245 LOC) — three end-to-end JSON-RPC
  stdio tests against a real spawned `code-graph-mcp serve` subprocess.
  Covers prod-first sort survival across centralized_compress truncation,
  caller_count prod-only correctness, and the new explanatory error message.
  Caught a real gap in the error-message fix during authoring (the
  `FuzzyResolution::NotFound` branch needed the same treatment as
  `Unique`).
- `cargo test --release`: 299 lib + 3 new mcp_stdio_integration + ~194 other
  integration = 496 total, 0 failed (1 pre-existing `#[ignore]`).
- `cargo +1.95.0 clippy --no-default-features -- -D warnings` clean.
- `cargo +1.95.0 clippy --all-targets -- -D warnings` clean.

## v0.22.0 — 三巨头 source-file split (queries / relations / pipeline)

Pure refactor release — zero behavior change, public surface preserved across
all three splits. The three biggest source files (8049 lines as monoliths) are
now decomposed into 26 per-concern submodules so future edits don't need to
load 2000+ lines of context per touch.

### What moved

| Original | Lines | New tree | Files |
|---|---:|---|---:|
| `src/storage/queries.rs` | 2892 | `src/storage/queries/` | 10 |
| `src/parser/relations.rs` | 2783 | `src/parser/relations/` | 9 |
| `src/indexer/pipeline.rs` | 2374 | `src/indexer/pipeline/` | 7 |

Submodule items use `pub(super)`; mod.rs re-exports the items external callers
already depend on. External call sites in `cli.rs`, `mcp/server`, `tests/`,
`benches/`, and `claude-plugin/` need zero edits — paths like
`crate::storage::queries::upsert_file`, `crate::parser::relations::ParsedRelation`,
`crate::indexer::pipeline::run_full_index` continue to resolve.

The three orchestrator-style functions stay whole in their respective `mod.rs`
or `index_files.rs` — `walk_for_relations` (~650 lines) and the Phase-0..3
indexer dispatch (~770 lines) share local state across their match arms /
phases that splitting would either duplicate or thread back via large arg
lists. Splitting per-language inside `walk_for_relations` would lose the
shared `current_scope` / `current_class` propagation; splitting per-phase
inside `index_files` would break the shared `tx` / atomics / `batch_parsed`
/ `name_to_ids` / `global_name_map` state. Both are kept whole deliberately.

### Verification

- `cargo check` clean
- `cargo +1.95.0 clippy --no-default-features -- -D warnings` clean
- `cargo +1.95.0 clippy --all-targets -- -D warnings` clean
- `cargo test --release`: 292 lib + 129 integration = 421 tests, 0 failed
  (1 pre-existing `#[ignore]`)
- Pre-merge CI green on all three PRs (#15, #16, #17)
- Independent code-reviewer subagent passed each split with zero Critical /
  Important issues

### Commit references

- queries.rs: 657a1f9 (#15)
- relations.rs: 2dfbab9 (#16)
- pipeline.rs: aef55b2 (#17)

## v0.21.0 — Opt-in plugin hooks (token discipline) + callgraph caller_count ordering + multi-model routing bench

### Migration notes (read first)

**Two LLM-visible default behaviors flipped to opt-in.** Both have explicit env
opt-out paths; existing users who set the legacy `CODE_GRAPH_QUIET_HOOKS=1` see
no change. Users on default settings will feel the new behavior on next session.

- **`user-prompt-context.js` (UserPromptSubmit hook) — default-quiet.** Per-prompt
  CLI exec was costing 200–500 tokens/turn injecting outline/callgraph context
  the agent would have asked for via MCP itself. v0.20.0 routing-bench backend
  P@1 = 100% on Sonnet 4.5 proves the agent picks the right tool without
  push-injection. Restore the v0.20.0 noisy default: set
  `CODE_GRAPH_VERBOSE_HOOKS=1` in `~/.claude/settings.json` env block. Legacy
  `CODE_GRAPH_QUIET_HOOKS=0` still forces noisy for back-compat.
- **`incremental-index.js` (PostToolUse Edit/Write hook) — default-off.**
  v0.18.0 added query-time `ensure_file_indexed` (single-file hash + sync
  reindex) inside MCP tools that take `file_path`, so the PostToolUse hook
  spawning a fresh process per edit was redundant for the MCP-driven workflow
  and burnt ~80ms cold-start per edit. CLI-only workflows (running
  `code-graph-mcp search` after Bash-side edits without going through MCP)
  need the hook for freshness — opt back in with `CODE_GRAPH_HOOK_INDEX=on`.

The two knobs are independent: setting one does not affect the other. CLI-only
users typically want `CODE_GRAPH_HOOK_INDEX=on` only; users who relied on
per-prompt outline injection want `CODE_GRAPH_VERBOSE_HOOKS=1` only.

One internal-but-user-perceptible change: `get_call_graph` (and the underlying
`get_call_graph_query`) now orders results within each depth by `caller_count
DESC`. Previously ties broke by row order, which silently dropped the most-
relevant subtree under `CALL_GRAPH_ROW_LIMIT` truncation. Hot functions like
`conn` (51 callers + 72 test in this repo) are now guaranteed to surface
their high-connectivity subtrees first. No JSON shape change — only ordering.

### Plugin hook default flips (the headline)

`claude-plugin/scripts/user-prompt-context.js` — replaced 6 mixed-language
intent regex piles with per-keyword weighted patterns under `INTENT_PATTERNS`.
Each (regex, weight) row is testable in isolation; threshold 0.5 + uniform
weight 1.0 preserves the original OR-of-alternatives behavior 1:1. Future
tuning can downweight noisy short keywords (`bug`, `什么`) once false-positive
data accumulates. Maintenance cost: ~150 lines of table vs 6 × 200-char
regexes — the regex form had two prior silent-bug regressions (#5754, #7713).

`computeQuietHooks(env)` priority chain (high → low):

1. `CODE_GRAPH_QUIET_HOOKS=0` → forced noisy (legacy)
2. `CODE_GRAPH_QUIET_HOOKS=1` → forced quiet (legacy)
3. `CODE_GRAPH_VERBOSE_HOOKS=1` → opt-in noisy (new)
4. default → quiet (v0.21 flip)

`claude-plugin/scripts/incremental-index.js` — pure passthrough refactor
behind `shouldRun(env)` gate. `CODE_GRAPH_HOOK_INDEX=on|1|true` opts in;
default and any other value skip the binary exec. `module.exports =
{ shouldRun }` exposes the gate for the test file.

Both hook scripts gain dedicated `*.test.js` files: 91 new lines of tests
on user-prompt-context.js (covers the env-precedence chain + per-keyword
intent table) and 55 new lines on incremental-index.js (covers the env
gate + idempotent skip).

### Callgraph: caller_count DESC tie-breaker (`src/graph/query.rs`)

The recursive CTE in `query_direction` gained a `caller_counts` CTE
(non-correlated `GROUP BY target_id` over `edges WHERE relation = ?4`,
covered by `idx_edges_target_rel`) and a `LEFT JOIN` into the outer SELECT.
Final `ORDER BY` is now `depth ASC, caller_count DESC`. When the result set
saturates `CALL_GRAPH_ROW_LIMIT`, high-connectivity subtrees survive the
truncation instead of being silently dropped. Test:
`test_callees_ordered_by_caller_count` (3 callees, 5/1/0 external callers,
asserts the depth-1 ordering matches caller-count rank).

`caller_count` is computed for every node in the result, not just the
truncation boundary — small CPU overhead, big interpretability win for
`module_overview` and `find_references` consumers downstream that read
the same field for sort ordering.

### Routing-bench multi-model dispatch (`tests/routing_bench.rs`)

New `ROUTING_BENCH_MODELS` env var accepts a comma-separated model list
(`sonnet-4.5,sonnet-4.6,opus-4.7,haiku-4.5`) and dispatches one Backend
per name, sharing a single API key. Single-model `ROUTING_BENCH_MODEL`
still works (legacy callers unchanged). When more than one backend ran,
the bench prints a multi-model summary table:

```
=== Multi-model P@1 summary (threshold 70%) ===
  sonnet-4.5      backend  recall 22/22 (100.0%)  fp 0/10
  sonnet-4.6      backend  recall 22/22 (100.0%)  fp 0/10
  opus-4.7        backend  recall 21/22 ( 95.5%)  fp 0/10
  haiku-4.5       backend  recall 18/22 ( 81.8%)  fp 0/10
```

Use case: weekly CI cron walking the Anthropic family to catch routing
regression when Claude Code rotates the default model. v0.20.0 measured
100% P@1 on Sonnet 4.5 only — the rest of the family had no signal until
this hook existed.

`detect_backend()` (legacy single-model) is preserved and still backs
the default `ROUTING_BENCH_MODEL` path. New `detect_backends()` returns
a `Vec<Backend>`; pure helpers `parse_models_env(s)` and
`build_backends(models, anthropic_key, openrouter_key)` are unit-tested
without API keys (4 new tests under `multi_model_dispatch_tests`).

### Effectiveness benchmark harness (`tests/effectiveness_bench.rs`, new)

Turns the README's "40-60% session token savings" vibe-claim into a
regression-tracked number. For each navigation task in the corpus, runs
the equivalent `code-graph-mcp` CLI command on a fixture project and
compares the byte count of the response to a hardcoded `baseline_bytes`
representing the historical Grep+Read approach. Asserts the overall
ratio stays ≤ 0.60 (matches the headline claim's worst case).

Bytes are a token proxy; for English / TS source they correlate ~1:3
with BPE tokens, so a 50% byte reduction maps to a 50% token reduction
at the same ratio. The harness intentionally avoids a tokenizer
dependency — bytes-as-proxy is good enough for tracking trend over
releases. Run with:

```
cargo build --no-default-features
cargo test --test effectiveness_bench --no-default-features -- --ignored --nocapture
```

`#[ignore]`-gated like `routing_bench`, so it doesn't fire on default
`cargo test` — opt in with `--ignored`. New tasks added by hand-counting
(or by running grep/Read for the same intent and summing the bytes
touched), set `baseline_bytes` once, commit. Subsequent regressions move
the ratio without touching the baseline.

## v0.20.0 — Adversarial tool descriptions + single-file outline + project-typed memdir + 100% routing P@1

### Migration notes (read first)

**No breaking changes.** All edits are LLM-visible metadata, additive output
fields, or new feature gates that fall back to the v0.19.0 behavior when not
opted into. Three behaviors users feel automatically on next session:

- 7 MCP tool descriptions rewritten in adversarial style ("INSTEAD OF Grep",
  "Replaces N rounds of grep+Read") to compete with Claude Code's first-class
  Grep/Read/LSP tool prompts.
- `module_overview` (and CLI `overview`) on a single file path now emits an
  outline view: `L<start>-<end>  type  name (callers×)  signature` per symbol,
  sorted by line number. Replaces Read on 3000+ line source files.
- `code-graph-mcp adopt` now detects project type (Rust/web-rs/web-node/web-py/
  web-go/frontend/python/go/node/generic) and writes a per-type MEMORY.md
  index line — Web projects get HTTP-route-tracing priming, Rust CLIs get
  callgraph/impact priming, frontend projects get rename-audit priming.

To pin the generic INDEX_LINE behavior of v0.19.0, set
`CODE_GRAPH_PROJECT_TYPE=generic` in `~/.claude/settings.json` env block.
To pin tool descriptions, downgrade — there is no env opt-out by design,
since LLM-visible metadata changes are the headline feature here.

### LLM-visible metadata revamp (the headline)

`src/mcp/tools.rs` — all 7 visible tool descriptions rewritten following the
sdscc reference ("MCP tool description should compete with Grep/Read/LSP for
the same query"). Pattern: lead with the trigger phrase users actually type,
then state the alternative-tool replacement, then the boundary. Examples:

- `get_call_graph`: "Multi-hop call chain. Replaces N rounds of `grep \"X(\"` +
  Read. Pass route_path='GET /api/x' to trace HTTP handler → downstream."
- `module_overview`: "Symbols in a directory or file, grouped by type +
  caller count. Replaces Glob + Read×N for big dirs / huge files. Single
  file: include_deps=dep graph, include_dead=unreferenced."
- `find_references`: "Rename/remove audits — every site that imports/inherits/
  implements/calls a symbol. Repo-wide cross-language (LSP needs file open).
  Literals → Grep; 'who calls X?' → get_call_graph."
- `ast_search`: "Enumerate symbols by typed filters (type/returns/params)
  Grep can't express. Use for 'all fns returning Result<T>' / 'all structs
  implementing X'. ONE known symbol → get_ast_node."

Server `instructions` field gained one line: `"Repo-wide AST index (LSP only
handles open files; we don't). Replaces multi-round Grep+Read for structural
queries."` Compile-time `assert!(NOISY.len() <= 1500)` budget unchanged.

`test_descriptions_are_concise` (≤200 char) still passes for all 7 tools.

### Single-file outline format (cmd_overview / module_overview)

`ModuleExport` struct gained `start_line` + `end_line` fields, plumbed
through both SQL queries (sql_exports + sql_fallback). When `overview` /
`module_overview` resolves to exactly one file path, output switches from
"by-type compact list" to outline:

```
src/mcp/server/mod.rs
  L1213-1254  fn  handle_initialize  fn handle_initialize(&self, ...)
  L1256-1265  fn  handle_tools_list (3×)  fn handle_tools_list(&self, ...)
  ...
```

MCP `module_overview.active_exports[]` JSON gained `start_line` + `end_line`
(additive — existing clients ignore unknown keys).

### Project-typed memdir adoption (memdir L1 升格)

`claude-plugin/scripts/adopt.js` gains `detectProjectType(cwd, env)` and
`buildIndexLine(projectType)`. Detection state machine:

- **Cargo.toml**: strips `# ...` comments, scans only `[dependencies]`
  section (skips `[dev-dependencies]` / `[build-dependencies]` / target deps).
  Web frameworks: actix-web, axum, rocket, warp, poem, tide, salvo. (`hyper`
  excluded — too commonly a CLI HTTP client.)
- **package.json**: `JSON.parse` + checks only `dependencies` field (skips
  `devDependencies` to avoid false-promoting React component libraries).
  Frontend: next/react/vue/svelte/nuxt/astro/remix/solid-js. Web-node:
  express/fastify/koa/hono/@nestjs/core/@hapi/hapi.
- **pyproject.toml**: scans `[tool.poetry.dependencies]` + `[project.dependencies]`
  + `[project]` (PEP 621 inline). Web: django/flask/fastapi/starlette/sanic/
  tornado/quart.
- **requirements.txt fallback** with comment-strip.
- **go.mod**: skips `// indirect` deps and `//` comment lines. Web:
  gin-gonic/labstack-echo/gofiber/go-chi/gorilla-mux.

Per-type INDEX_LINE primes the right tools and demotes irrelevant ones —
e.g. a Rust CLI's INDEX_LINE no longer mentions `trace_http_chain`, freeing
attention budget for callgraph/impact/dead-code routing.

### CODE_GRAPH_PROJECT_TYPE env override

`detectProjectType(cwd, env)` honors `CODE_GRAPH_PROJECT_TYPE` env var when
set to a valid bucket name (`PROJECT_TYPES` Set is the allow-list).
Invalid/typo'd values silently fall through to file-based detection (so a
typo doesn't classify everything as `generic`). Use cases: power users who
want to pin a non-default classification, CI runs that want deterministic
typing across mixed repos, or opting out via `=generic`.

### routing-bench: oracle alternates (test infra)

`tests/routing_bench.rs` ORACLE entries can now express "either of these
tools is correct" via `|`-separated expected: e.g.
`("Who calls X?", "get_call_graph|find_references")`. New helper
`matches_expected(picked, expected)` splits on `|` and accepts membership.
Wired through `compute_recall`, `compute_overall`,
`assert_oracle_covers_registry`, and the main benchmark miss-detection.

Why: at depth=1, `find_references` with `relation=calls` returns the same
caller list as `get_call_graph`. Pinning a single answer over-fitted the
oracle to a stylistic preference rather than measuring real routing capability.

Result: routing-bench P@1 went from **95.5% (21/22)** → **100% (22/22)** on
the Backend oracle (OpenRouter Sonnet 4.5, ToolOnly mode).

### Test coverage

- `claude-plugin/scripts/adopt.test.js`: 43 → 65 tests (+22). Covers project-typed
  INDEX_LINE roundtrip + 12 detection-hardening tests (commented dep,
  dev-deps only, build-deps only, devDependencies, `// indirect`, malformed
  JSON, PEP 621, requirements.txt, env override valid/invalid/empty/forced-generic).
- `tests/routing_bench.rs` scoring tests: 40 → 43 (+3 alternates path coverage).
- Rust suite: 469 → 470 passed, 0 failed, 1 ignored (routing_bench API key gate).

### Internal: storage struct + clippy

- `ModuleExport` struct: 2 new fields (`start_line`, `end_line`). SQL touched
  in 2 places (sql_exports + sql_fallback). All 5 ModuleExport call sites
  in cli.rs / overview.rs read the new fields.
- One pre-existing clippy `iter_cloned_collect` lint cleaned up
  (`.iter().copied().collect()` → `.to_vec()` in the new outline branch).
- `cargo +1.95.0 clippy --all-targets -- -D warnings` clean on both
  `--no-default-features` and default builds.

## v0.19.0 — Tier-aware language support: bash/json + C/C++ #include/gtest + Dart top-level fix

### Migration notes (read first)

**No breaking changes.** All additions are backward-compatible. Existing indexes
pick up new edges and test markers on the next incremental update — no rebuild
required. Users feel three new behaviors automatically:

- New file extensions are now indexed: `.sh` / `.bash` (Bash), `.json` (JSON, file-FTS only).
- C/C++ `#include` directives now produce IMPORTS edges in the dependency graph.
- gtest macro invocations (`TEST` / `TEST_F` / `TEST_P` / `TEST_CASE` / `TYPED_TEST` / `TYPED_TEST_P`) are now marked `is_test=true` and named `Suite.Name`.

To revert any individual feature, pin to v0.18.4 (`cargo install code-graph-mcp@0.18.4`
or downgrade the npm-installed binary). No env-flag opt-out — the additions are
graph data shape, not behavior toggles.

### New language coverage

- **Bash** (`tree-sitter-bash 0.23.3`) — function definitions, command-style
  calls (with static-identifier filter rejecting `$VAR` / `$(...)` / shell
  built-ins like `[` and `:`), and IMPORTS edges from `source <file>` / `. <file>`
  (path prefix and `.sh` / `.bash` extension stripped; dynamic paths skipped).
- **JSON** (`tree-sitter-json 0.24.8`) — file-FTS indexing only. No AST symbols
  extracted by design (JSON has no function/class concepts); files are searchable
  via FTS5 like any other indexed text.

### C/C++ improvements

- `#include "foo/bar.h"` and `#include <stdio.h>` now emit IMPORTS edges from
  `<module>` to the bare module name. Path prefix and `.h` / `.hpp` / `.hxx` / `.hh`
  extensions stripped so cross-file resolution can match header file nodes.
  Closes a long-standing gap where C/C++ projects had near-empty import graphs.
- gtest macros parsed by tree-sitter as `function_definition` now extract
  `Suite.Name` (e.g. `MathSuite.Addition`) instead of colliding under the macro
  name (`TEST`), and force `is_test=true` on the resulting node. Six macros
  covered: `TEST`, `TEST_F`, `TEST_P`, `TEST_CASE`, `TYPED_TEST`, `TYPED_TEST_P`.

### Bugfixes

- **Dart top-level function scope** (silent call-graph hole): the `function_body`
  scope_name arm in `relations.rs` previously only matched `method_signature`
  prev-siblings. Top-level Dart functions wrap as `declaration > function_signature
  + function_body` — that AST path was silently dropped, so every call inside any
  top-level Dart function was missing from the call graph. Now both top-level
  and class-method shapes resolve correctly.

### Tier-aware language support docs

README and project `CLAUDE.md` previously claimed "16 languages" as a flat list.
Reality is a continuum of extraction depth. Updated to a 5-tier breakdown:

- **Full** (calls + imports + inheritance + HTTP routes + test markers):
  TS/TSX, JS, Go, Python, Rust, Java
- **Smoke-tested** (calls + imports + inheritance): C#, Kotlin, Ruby, PHP, Swift, Dart
- **Limited** (functions + calls + `#include` imports + gtest test markers;
  `Class::method` scope qualification still deferred): C, C++
- **Scripting**: Bash (with `source`/`.` imports), Markdown (headings)
- **File-FTS only** (no AST symbols extracted): HTML, CSS, JSON

### Test coverage

Parser test suite: 65 → 87 (+22). New tests:

- 6 inheritance smoke tests for C#/Kotlin/Ruby/PHP/Swift/Dart (audit confirmed
  baseline shapes work for `delegation_specifiers` / `inheritance_specifier` /
  `base_clause` / `class_interface_clause` / `superclass` / `base_list` with
  IFoo heuristic).
- 12 calls + imports smoke tests for the same 6 languages (Tier 2).
- 3 tests for C/C++ `#include` IMPORTS + gtest macro detection.
- Test infrastructure now provides regression protection for the 6 Tier 2
  languages that had zero specific tests before this release.

## v0.18.4 — Hidden-5 fold + tools.rs split + Cargo default lite + routing-bench CI

### Migration notes (read first)

**Cargo default features changed** (direct `cargo install code-graph-mcp` users):
the default build is now FTS5-only (~10 MB binary). Opt back into the full
hybrid (FTS5 + vector) with `cargo install code-graph-mcp --features
embed-model`. **npm/npx/plugin users see no change** — `release.yml` now passes
`--features embed-model` explicitly, so shipped binaries keep the same
capabilities they had in v0.18.3.

**MCP `instructions` shrunk from ~700 B to ~330 B** (visible in `initialize`
response). Removes the "5 advanced tools CLI-only" caveat that was the
v0.18.3 reality but is no longer true after the fold below. Compile-time
guard at 1500 B is unchanged.

**Hidden 5 names still callable** (`impact_analysis`, `find_similar_code`,
`dependency_graph`, `find_dead_code`, `trace_http_chain` + alias
`find_http_route`). Dispatcher entries kept for raw JSON-RPC / SDK clients
and existing integration tests. Claude Code is expected to use the new flag
forms below.

### Fold: hidden 5 → core 7 flags

The 5 niche tools that were registered-but-hidden from `tools/list` (and
therefore unreachable from Claude Code, which derives its callable set from
`tools/list`) are now reachable as flags on the core 7. Same backing logic;
new entry path:

| Old (hidden, still callable as alias) | New flag form (preferred) |
|---|---|
| `impact_analysis symbol_name=X` | `get_ast_node symbol_name=X include_impact=true` |
| `find_similar_code node_id=N` | `get_ast_node node_id=N include_similar=true` |
| `dependency_graph file_path=F` | `module_overview path=F include_deps=true` |
| `find_dead_code path=P` | `module_overview path=P include_dead=true` |
| `trace_http_chain route_path="GET /x"` | `get_call_graph route_path="GET /x"` |

`get_ast_node include_impact` was already in v0.18.3 — the other four flags
are new. CLI subcommands (`code-graph-mcp impact|similar|deps|dead-code|trace`)
are unchanged for Bash workflows.

### Refactor: `src/mcp/server/tools.rs` split (no behavior change)

The 2354-line tool dispatch file is now 9 focused modules under
`src/mcp/server/tools/`:

```
tools/
├── search.rs        — semantic_code_search
├── callgraph.rs     — get_call_graph + format helpers + truncation flags
├── ast_node.rs      — get_ast_node + ast_node_by_id + impact summary + similar attach
├── ast_search.rs    — ast_search
├── refs.rs          — find_references
├── overview.rs      — module_overview + compact + dep/dead fold
├── project_map.rs   — project_map
├── advanced.rs      — backing logic for the folded 5 (still pub(in server))
└── management.rs    — start/stop watch, get_index_status, rebuild_index
```

Visibility for handler methods is now `pub(in crate::mcp::server)` so the
dispatcher in `mod.rs` can still reach them across the new module boundary.
No public API change; the matching commit is the bisect target if you're
cherry-picking.

### CI: weekly routing-bench tracking

New `.github/workflows/routing-bench.yml` runs `tests/routing_bench.rs`
weekly (Sunday 03:17 UTC), on every release tag, and on manual dispatch.
Asserts P@1 ≥ 0.70 against the live 7-tool MCP schema using OpenRouter
(Claude Sonnet 4.5 default, override via workflow input). Cost ~$0.10 per
run. Requires `OPENROUTER_API_KEY` repo secret; without it the job no-ops
gracefully. Per-release P@1 lands in the GitHub Actions step summary +
artifact retention 90 days.

### Adoption template refresh

`claude-plugin/templates/plugin_code_graph_mcp.md` reflects the fold —
core-7 decision table now shows the new `include_*` and `route_path` flags
inline, and the legacy "进阶 5 走 CLI" section is rewritten as "old names
still work; prefer flag form in Claude Code". Adopted projects with
`CODE_GRAPH_NO_TEMPLATE_REFRESH` unset will pick this up on next
SessionStart.

## v0.18.3 — release-pipeline supply-chain hardening + pending-sweep narrow

Maintenance release. No public-API or schema changes; CLI flags, MCP tool
shapes, and SQLite schema all unchanged from v0.18.2. Output of comprehensive
gstack audit run (/cso, /review, /retro) on v0.18.2 — every finding actioned
or explicitly accepted with rationale.

**Release pipeline** — third-party action SHA pins, model revision pin

- `release.yml`: `dtolnay/rust-toolchain@stable` → `@e081816` (1.95.0 branch
  SHA-pinned). Closes the asymmetry where `release.yml` built shipped
  binaries with whatever the latest stable Rust was at build time, while
  `ci.yml` tested with `1.95.0`. Also closes the supply-chain window where
  a moved `@stable` tag would have silently affected every release.
- `release.yml`: `Swatinem/rust-cache@v2` → `@e18b497`,
  `softprops/action-gh-release@v3` → `@b430933`. Both third-party,
  in the release path; cache-action poisoning could exfiltrate `NPM_TOKEN`,
  release-action substitution could swap GH Release artifacts. Floating
  major-version tags are mutable; commit SHAs aren't.
- `release.yml` model-bundle step: HuggingFace `resolve/main/$f` →
  `resolve/c9745ed1d9f207416be6d2e6f8de32d1f16199bf/$f` for
  `sentence-transformers/all-MiniLM-L6-v2`. Plus `curl --fail` so a 404
  HTML page can no longer masquerade as `model.safetensors` (the bundle's
  downstream sha256 only validated the bundle against itself, not against
  a known-good upstream).

**Supply-chain CVE coverage** — new CI job + 6 RUSTSEC fixes

- New `audit` job in `ci.yml` runs `cargo audit` against `Cargo.lock` on
  every push/PR. cargo-audit pinned to `^0.22` because `0.21.x` panics on
  RustSec advisories using CVSS 4.0 (e.g. `RUSTSEC-2026-0066`) — fetch
  fails before any finding can be reported. Default behavior fails on
  vulnerabilities; informational `unmaintained`/`unsound` advisories print
  but don't block (most are transitive and not under our control until
  upstream replacements ship).
- `cargo update -p rustls-webpki -p tar` resolved the 6 advisories the
  new audit job surfaced on a v0.18.2 baseline:
  - `rustls-webpki 0.103.9 → 0.103.13` (RUSTSEC-2026-0099 wildcard cert
    name acceptance, RUSTSEC-2026-0098 URI name constraint, RUSTSEC-2026-0104
    CRL parsing panic, RUSTSEC-2026-0049 CRL distribution-point matching).
    Used transitively via `reqwest`/`quinn`.
  - `tar 0.4.44 → 0.4.45` (RUSTSEC-2026-0067 `unpack_in` symlink chmod,
    RUSTSEC-2026-0068 PAX size header). Direct dep behind `embed-model`
    feature, used to unpack the bundled HF model tarball.

**Indexer perf** — pending sweep no longer full-scans nodes

`resolve_pending_calls` in `src/indexer/pipeline.rs` previously did
`SELECT n.id, n.name, ... FROM nodes n JOIN files f ...` over the full
nodes table to build the in-memory `name → [(node_id, language)]` map for
resolution. Even a 1-row pending table triggered a full scan on every
incremental pass. Narrowed by adding
`AND n.name IN (SELECT DISTINCT target_name FROM pending_unresolved_calls)`
to the SELECT — scope drops to ≤ |pending| names per sweep. All 5
v0.18.2 regression tests still pass; resolution semantics unchanged.

## v0.18.2 — incremental dropped-edge root-cause fix (both directions)

Closes the bug documented in memory `feedback_incremental_edge_timing.md`:
incremental indexing silently dropped REL_CALLS edges in two symmetric
scenarios that only `rebuild-index` recovered. v0.18.1 query-time
freshness was a band-aid for the file_path-aware tools; this is the
underlying fix, in both directions.

**The bug, both directions**

*Direction A — callee added later*: file B has `caller_b() { foo(); }`.
At B's Phase 2, `foo` has no same-file or same-language target → REL_CALLS
dropped (memory `feedback_edge_resolution_same_language.md` correctly
forbids cross-language fallback for calls). Later, file A is added with
`function foo() {}`. Incremental index reindexes A only; B isn't in
`changed_paths`, so B's bare-name `foo()` is never re-resolved. Edge
`caller_b → foo` permanently missing until full rebuild.

*Direction B — callee removed*: same setup but A is *deleted*. Cascade
delete on `target_id` FK strips B's edge to A.foo automatically; B isn't
in `delete_paths`, so Phase 2 doesn't re-extract it. If A is then re-added
later, B has neither a stale edge nor a way to know it should re-resolve.

**The fix (schema v8)**

New `pending_unresolved_calls` table buffers REL_CALLS that Phase 2 can't
resolve at extraction time, plus inbound REL_CALLS edges Phase 0 is about
to cascade-strip. The post-Phase-2 sweep promotes pending rows to real
edges as soon as a same-language target appears.

- `(source_id REFERENCES nodes ON DELETE CASCADE, target_name,
  source_language, metadata)` with unique index on the triple — keeps
  inserts idempotent across repeated Phase 2 invocations.
- `ON DELETE CASCADE` makes the table self-cleaning: when caller B is
  reindexed (Phase 1 deletes its old nodes), pending rows for B's old
  source_ids drain automatically.
- Sweep scope is **same-language only** — cross-language is never
  promoted (the canonical false-positive class from
  `feedback_edge_resolution_same_language.md`). When multiple
  same-language candidates exist, the existing `refine_ambiguous_targets`
  applies (path-proximity + non-test preference), so dense-fanout cases
  don't regress dead-code precision.

**Direction A wiring (commit `d172cae`)**: at Phase 2's REL_CALLS drop
point in `pipeline.rs`, instead of silent `continue` we
`insert_pending_unresolved_call`. End of `index_files` runs
`resolve_pending_calls` which builds name → [(node_id, language)] and
node_id → path maps from current DB state (one indexed SELECT, not
per-row), iterates pending rows, applies `refine_ambiguous_targets`
where ambiguous, inserts edges, drops rows.

**Direction B wiring (commit `9c27739`)**: Phase 0 in `pipeline.rs` now
resolves file_ids before `delete_files_by_paths` drops them, calls new
`queries::get_inbound_calls_for_pending` to fetch inbound REL_CALLS
edges from non-deleted files, and writes pending rows for each before
letting cascade fire. Same post-Phase-2 sweep then handles the resolution.

**Migration**: SCHEMA_VERSION 7 → 8. INDEX_VERSION unchanged — existing
edges remain valid; the pending table starts empty and fills naturally
on next index pass. Migration is transactional (matches the pattern of
every prior migration). Existing v0.18.1 DBs auto-upgrade on first open.

**Test coverage** (5 new pending-resolution tests + 1 migration test):
- `test_pending_unresolved_call_resolves_when_callee_added_later`
  (direction A round-trip)
- `test_pending_buffers_on_callee_file_deletion` (direction B
  round-trip — edge → delete → buffered → re-add → edge restored)
- `test_pending_unresolved_call_does_not_cross_language` (TS pending
  vs Rust definition stays buffered; cross-language refused)
- `test_pending_resolves_multiple_calls_in_same_caller` (3 undefined
  calls in one caller → 3 pending rows → all drain on single sweep)
- `test_pending_cascade_deletes_when_caller_file_reindexed`
  (load-bearing schema FK behavior — explicit guard so a future
  migration weakening the FK fails loudly here)
- `test_v7_to_v8_migration_adds_pending_table` (asserts table + both
  indexes after v7-shape DB opened via Database::open)

**Bonus**: 2 new plugin-script test files
- `scripts/sync-versions.test.js` — 4 tests, fixture-copy strategy, locks
  release-tooling contract (`feedback_version_sync.md`). Includes
  `(9 files updated)` count assertion to catch silent target drops.
- `claude-plugin/scripts/mcp-launcher.test.js` — 3 tests, end-to-end MCP
  initialize via launcher + 2 static-grep checks for plugin-env
  isolation (`feedback_plugin_env_isolation.md`) and macOS quarantine
  hint surface.

**Test count**: 272 default-features (was 265 in v0.18.1), 265
no-default-features (was 261), 182 JS tests (was 175). All clippy 1.95
clean on both feature profiles.

**Compatibility**: All 16 MCP tool schemas unchanged. CLI flags
unchanged. Output JSON unchanged (no shape additions). Schema migration
is transparent to consumers — query results match v0.18.1 plus
previously-missing edges that should have been there all along.

## v0.18.1 — query-time freshness + call-graph truncation provenance

Three additive improvements to MCP tool surfaces, no breaking changes.
All output-shape changes are strictly additive — non-truncated /
non-edit-aware paths return the exact prior shape.

**1. Query-time freshness for file_path-aware tools** (commits `30678d6`,
`82f1526`).

When an MCP tool receives an explicit `file_path` argument, the agent is
signaling "I just edited this; please answer against the current bytes."
The 30s `last_incremental_check` debounce in the server is too coarse for
tight Edit→search loops — agents would see pre-edit call edges right after
saving a file.

- New `pipeline::ensure_file_indexed(db, root, rel_path, model)`: single-file
  hash-compare reindex, no-op when on-disk hash matches stored hash. Drops
  stale rows when the file is gone. Skips files we wouldn't index in the
  first place (binary / unrecognized language). Cross-file dirty-edge
  handling mirrors `run_incremental_index` (collect dirty node IDs BEFORE
  re-indexing so cascade delete doesn't strip the context-string
  regeneration target set).
- New `McpServer::ensure_file_fresh_opt(path)`: server-side wrapper that's
  a no-op on read-only secondaries, on missing/empty/directory paths, and
  when the embedding lock is contended. Invalidates `project_map` and
  `module_overview` caches only when a reindex actually fired.
- Wired into 6 file_path-aware tools: `get_call_graph`, `get_ast_node`,
  `module_overview`, `find_references`, `dependency_graph`,
  `impact_analysis`. Agents no longer have to remember which tools
  auto-refresh and which don't.

`test_ensure_file_indexed_picks_up_post_edit_changes` covers the no-op /
post-edit-pickup / repeat-no-op / file-deleted paths.

**2. Call-graph truncation provenance** (commit `fd168fd`).

The recursive CTE in `get_call_graph` silently caps at depth 10 and 200
rows. Agents reading partial results couldn't tell when truncation fired
vs. when the graph genuinely ended — a common failure mode for "who calls
X?" on hot functions where the real answer is "200+ across the codebase,
you're seeing a slice."

- `graph::query::CallGraphResult` wraps `Vec<CallGraphNode>` with
  `limit_hit` / `depth_capped` / `effective_max_depth` /
  `requested_max_depth` flags.
- `CALL_GRAPH_MAX_DEPTH` (10) and `CALL_GRAPH_ROW_LIMIT` (200) are now
  public constants — single source of truth (was a magic number in two
  places).
- `query_direction` returns `(nodes, limit_hit)` so the `direction="both"`
  merge can OR-combine saturation across both call directions.
- New JSON fields appear **only** when truncation fires: `limit_hit`,
  `depth_capped`, `effective_max_depth`, `requested_max_depth`,
  `truncation_warning`. The warning text gives the agent a recovery move
  ("pick a leaf node_id and re-query from there, or narrow with file_path").
- Wired into MCP `get_call_graph` (incl. rollup branch) +
  `trace_http_chain` (`call_chain_truncated` flag per handler), and CLI
  `code-graph-mcp callgraph` / `trace`.

`test_depth_capped_signal` verifies clamp + flag wiring with a depth=99
request.

**3. CHARS_PER_TOKEN clarified as bytes/token + CJK regression test**
(commit `6dc10ff`).

The constant has always been used with `s.len()` (UTF-8 byte count in
Rust), not Unicode char counts. The historical name suggested otherwise
and tempted "fixes" to char-count, which would silently halve the CJK
budget — one CJK char = 3 bytes ≈ 1 BPE token, so `bytes/3 ≈ chars ≈
tokens` (accidentally correct under the bytes interpretation).

- Doc on `CHARS_PER_TOKEN` now explains ASCII vs CJK behavior and the
  conservative-overestimation property that makes earlier-fire
  compression the safe error direction.
- `estimate_tokens` local rename `total_chars → total_bytes` to match.
- `test_estimate_tokens_cjk_byte_based`: 1000 CJK chars (3000 bytes) must
  estimate ~1000 tokens; ASCII 1000 chars (1000 bytes) must estimate
  lower, confirming the divisor is bytes-based. Regression guard against
  someone "fixing" the estimator to char-count.

No behavior change in this commit — doc + test only.

**Test count**: 265 default-features (was 264), 258 no-default-features
(was 257). 3 new tests across the three changes.

**Compatibility**: All 16 MCP tool schemas unchanged. CLI flags
unchanged. Output JSON additive only. Zero breaking changes for plugins
or downstream consumers.

## v0.18.0 — routing_bench frontend domain + project_map dedup hint

Two changes driven by the v0.17.3 30-day usage audit. The audit found
that **all 728 code-graph MCP/CLI calls in 30 days came from the
plugin's own repo** — frontend / non-Rust workflows had zero coverage
in the routing benchmark, so we couldn't tell whether tool descriptions
activated for them. It also found that `project_map` was being invoked
~11 times/30d via MCP **after** SessionStart had already injected the
same map at boot — pure redundancy.

**1. routing_bench frontend domain** (`tests/routing_bench.rs`).

Adds a second 20-query oracle (`FRONTEND_ORACLE`) covering the same 7
core tools with JS/TS/Vue/React phrasing (component / hook / Promise /
useEffect / Redux dispatch). Selectable via new env:

- `ROUTING_BENCH_DOMAIN=backend` (default — preserves v0.17.2/v0.17.3
  baselines comparable; runs only the original 22-query Rust pool).
- `ROUTING_BENCH_DOMAIN=frontend` — runs only `FRONTEND_ORACLE`.
- `ROUTING_BENCH_DOMAIN=all` — both pools (42 q), with separate
  `Backend recall` / `Frontend recall` buckets in the report so
  frontend regressions don't hide behind backend wins.

The bench helpers (`compute_recall`, `compute_overall`, `build_oracle`)
were refactored to accept an oracle slice instead of hardcoding `ORACLE`,
so the same scoring path covers both domains. `oracle_well_formed`
guards backend coverage; new `frontend_oracle_well_formed` and
`frontend_oracle_distinct_from_backend` guard the frontend pool.

15 new tests added (42 total, was 27); no API key required for any of
them. Test count: `cargo test --test routing_bench` 42 passed,
1 ignored (the API-gated `routing_recall_benchmark` itself).

**First frontend baseline** (sonnet-4.5, 3-run majority vote,
domain=all mode=context-rich, ~$0.80/run):

- Backend recall: **22/22 = 100%** (was 21/22 in v0.17.2 — the historic
  `EmbeddingModel struct definition` miss did not recur this run; v0.17.3's
  description tightening of `get_ast_node` and `semantic_code_search`
  appears to have stuck on sonnet-4.5).
- Frontend recall: **19/20 = 95.0%**.
- FP-rate: 0/10 = 0%.
- Overall: 51/52 = 98.1%.

The single frontend miss is `"List all React components in src/components/"` →
routes to `module_overview` (3-run unanimous), expected `ast_search`. This
is borderline-by-design: the query contains a module-path prefix
(`src/components/`) which triggers the v0.17.0 description rule "if module
path is known, prefer module_overview" — the same rule guarded by backend
ORACLE's `"How does the embedding pipeline work in src/embedding/?"`
regression case. Two valid routings; model picked the path-anchored one.
Future regression gate: frontend recall ≥ 19/20.

**Conclusion**: frontend pool achieves near-backend recall with vanilla
sonnet — tool descriptions already activate on JS/TS/Vue/React
vocabulary. The "frontend project shows zero MCP calls" observation
from the usage audit is workflow/install shortfall (the audited project
hadn't enabled the plugin in `.mcp.json`), not a routing-description
failure.

**2. project_map description: explicit dedup hint** (`src/mcp/tools.rs`).

Description rewritten from
`"Project architecture map. Use when: starting work on unfamiliar
code, finding which module owns functionality, or needing cross-module
dependency overview."`
to
`"Project architecture map. SessionStart hook already injects this at
boot. Call only if structure changed mid-session: major refactor,
rebuild-index, or many new modules."`

170 bytes, fits the 200-byte per-description cap asserted by
`mcp::tools::tests::test_descriptions_are_concise`.

**Re-bench (same methodology) post-description-change**: zero
regression. Backend 22/22, Frontend 19/20, FP 0/10, Overall 51/52
unchanged. Same single frontend miss. Conclusion: explicit
"do-not-call-redundantly" framing in tool descriptions is regression-
safe and reusable for any other MCP tool that already has SessionStart-
hook coverage.

**Expected impact**: ~11 redundant `project_map` MCP calls/month
eliminated (~33K tokens/month saved) without any routing precision
trade-off. Will be visible in the next 30-day window's
`code-graph-mcp stats tools.project_map.n` count.

## v0.17.3 — get_ast_node disambiguation (description tightening)

Bench-driven fix on **published tool descriptions** for the
"named-symbol queries leak to semantic_code_search" boundary.

**Background.** v0.17.2's context-rich bench (haiku-4.5 stress test)
surfaced 3 systematic misses: `EmbeddingModel struct definition`,
`weighted_rrf_fusion signature`, `format_call_graph_response
implementation` — all routing to `semantic_code_search` instead of
`get_ast_node`. The MEMORY.md hook already had `看 X 源码/签名 →
get_ast_node` but weak models ignored it at tool-selection.

**Diagnosis.** `semantic_code_search`'s description led with "Search
code by concept" with no explicit handoff to `get_ast_node` for
named-symbol queries. v0.17.0 added analogous redirects for
`semantic_code_search → module_overview` and `find_references → Grep`;
the named-symbol boundary was the missing one.

**Fix.** Two description edits in `src/mcp/tools.rs`:

- `semantic_code_search` (197 chars): rewritten to "Concept search
  when no symbol/module is named. If a symbol is named (e.g., 'show
  X struct'), use get_ast_node; if module path is known, use
  module_overview. Use when grep is noisy."
- `get_ast_node` (200 chars): "Inspect ONE named symbol: signature,
  full source, optional references/impact. Use when: query names a
  symbol asking for its definition/body/signature/implementation.
  PREFER over semantic_code_search."

Both fit the project's 200-char-per-description cap (asserted by
`mcp::tools::tests::test_descriptions_are_concise`). Tighter
example-list patterns were tested first but exceeded the cap.

**Bench results** (3-run majority vote on each model):

- **Sonnet 4.5 context-rich**: 22/22 / 0/10 / 32/32 = 100%
  pre-fix and post-fix. Zero regression.
- **Haiku 4.5 context-rich**: 19/22 → **20/22** (Recall 86.4% →
  90.9%, Overall 90.6% → 93.8%). `weighted_rrf_fusion signature`
  recovered to `get_ast_node`. Two queries still miss
  (`EmbeddingModel struct`, `format_call_graph_response
  implementation`) — they need stronger anchor patterns than fit in
  the 200-char budget; tracked as a follow-up.

**Iteration history (recorded for future tuning).** A higher-budget
description with three named example phrasings ('show X struct
definition', 'signature of Y', 'implementation of Z') recovered
`EmbeddingModel struct` on haiku but exceeded the 200-char cap and
caused a `List all structs in storage module → module_overview`
regression on the same model. Compressing to fit the cap dropped
the EmbeddingModel recovery but eliminated the regression. Net
haiku improvement: +1 query.

**Drift test still passes** — `INDEX_LINE_MIRROR` byte-equal to
`adopt.js` (no MEMORY.md hook change in this commit). Per-tool
descriptions are LLM-visible metadata (L3 published surface);
content is description-only with bench-verified outcomes on both
strong and weak models.

## v0.17.2 — routing_bench context-rich mode

Adds a measurement capability the existing bench architecture lacked:
grading the MEMORY.md hook line quality (added in v0.17.1). The
existing `tests/routing_bench.rs` only consumed tool descriptions;
it could not detect routing changes from MEMORY.md, the adoption-
memory file, or MCP `instructions`. Stage 3 hook content tuning
needed an oracle.

**`ROUTING_BENCH_MODE=context-rich` mode.** Adds:

- `INDEX_LINE_MIRROR` Rust constant + drift-detection test that
  spawns Node and asserts byte-equality with `adopt.js`'s
  `INDEX_LINE` export. Drift fails on every `cargo test`.
- Decoy `Grep` and `Read` tools added to the API call's `tools`
  array (descriptions calibrated with "Prefer over code-graph"
  anchors to be measurement-fair).
- 10-entry `FP_ORACLE` of strict-boundary queries (literal text,
  file reads by path, doc/config content) that should route to
  decoys, not code-graph.
- 3-run majority-vote aggregation per query (tie-break: first run);
  applied to both modes.
- Three reported metrics: Recall (out of 22 ORACLE), FP-rate (out
  of 10 FP_ORACLE), Overall (out of 32, loose summary).

**`temperature: 0` added to both backends** (Anthropic + OpenRouter)
in tool-only mode too. Pre-existing latent ±3-5pp single-run noise
masked Stage 3-level differences. Reproducible from this version on.

**First baselines** (2026-04-30, OpenRouter `anthropic/claude-sonnet-4.5`):

- Tool-only: **21/22 = 95.5%** (178s, 60 calls). Same residual miss
  as v0.16.7 (`Show me the EmbeddingModel struct definition` →
  `ast_search` instead of `get_ast_node` — pre-existing semantic
  borderline). Note: `feedback_routing_bench.md` had been tracking
  19/20 — that was against the 20-entry pre-v0.17.0 oracle; v0.17.0
  added 2 regression-guard queries, real total is 22.
- Context-rich: **Recall 22/22 = 100%** · **FP-rate 0/10 = 0%** ·
  **Overall 32/32 = 100%** (255s, 90 calls). The historically-stuck
  `EmbeddingModel struct` query routes correctly here — the MEMORY.md
  hook + Grep/Read decoys provide enough disambiguation context to
  flip it. Caveat: single bench run; Stage 3 will tell us how robust
  this is to hook-content variations.

**Default mode unchanged.** With `ROUTING_BENCH_MODE` unset or any
value other than `context-rich`, the bench behaves identically to
v0.17.1 except for `temperature: 0` and 3-run aggregation. The
`oracle_well_formed` and `index_line_drift_check` tests run on
every `cargo test`; the live benchmark stays `#[ignore]`'d.

## v0.17.1 — adoption-memory hook line: spec compliance

Single-file structural fix. `claude-plugin/scripts/adopt.js`
`INDEX_LINE` changes from an 11-line `array.join('\n')` block to a
single-line string, complying with the MEMORY.md spec ("each entry
should be one line"). The sentinel block written to
`~/.claude/projects/<slug>/memory/MEMORY.md` shrinks 11 lines → 1
line at next SessionStart per the v0.11.0 template-refresh
contract.

**No behavior change.** All 12 tool names (7 core + 5 hidden), all
6 中文 scene phrases (改 X 影响面 / 谁调用 X / X 被谁用 / 看 X 源码 /
Y 模块长啥样 / 概念查询), the `优先于 Grep` anchor, and the
`字面匹配走 Grep` reverse signal are kept verbatim. New:
spec-canonical tag syntax
`[impact, callgraph, refs, overview, semantic, ast-search, dead-code, similar, deps, trace]`
for explicit keyword matching. Reduces always-loaded MEMORY.md
context by ~366 chars per session.

The adoption-memory detail file (`plugin_code_graph_mcp.md`) is
unchanged — it already holds the full decision table the
multi-line block was duplicating.

**Bench scope.** `tests/routing_bench.rs` only consumes tool
`name + description + input_schema` (verified at
`tests/routing_bench.rs:224-233` + `:50-52`); it does not consume
MEMORY.md, the adoption-memory file, or MCP `instructions`. So it
cannot grade adoption-memory hook quality. A context-rich bench
(MEMORY.md in system prompt + Grep-decoy false-positive corpus) is
a separate change. Existing routing_bench is unaffected by this
PR.

**Tests.** `cargo test` 26/26, `node --test claude-plugin/scripts/*.test.js` 132/132,
`adopt.test.js` 43/43.

## v0.17.0 — quiet by default + tighter routing instructions

Two-part SessionStart context-cost reduction. The plugin used to inject
a ~2.3 KB `project_map` every session for un-adopted projects, plus a
1418-byte MCP `instructions` block packing 10 per-tool decision rules.
Both are redundant with what already exists: each tool's own
`description` carries its routing hint, and `MEMORY.md →
plugin_code_graph_mcp.md` already holds the full decision table for
adopted projects. v0.17.0 cuts both, and tightens the two tool
descriptions whose phrasing demonstrably mis-routed in benchmarks.

**1. SessionStart `project_map` injection: OFF by default.**
Old contract (v0.9.0): adopted → quiet, un-adopted → noisy. The
assumption was that adoption installed the MEMORY.md decision table
so the dump only became redundant *after* adopt.

New contract: quiet unconditionally. The decision table + the
on-demand `project_map` MCP tool + the per-tool descriptions cover
every workflow that the SessionStart map dump used to support, so
paying ~2.3 KB of context per session is wasteful — even pre-adopt.

- `CODE_GRAPH_VERBOSE_HOOKS=1` opts in to the dump (new).
- Legacy `CODE_GRAPH_QUIET_HOOKS=0` (force noisy) / `=1` (force quiet)
  still wins, preserving the v0.9.0 escape hatches.
- `computeQuietHooks({ adopted, env })` accepts but ignores `adopted`
  — kept only to avoid breaking call-sites.

**2. MCP `instructions` field trimmed 1418 → ~700 B.**
The old noisy block packed all 10 routing rules with CLI aliases
inline. Compile-time guard was 1500 B, against an observed Claude
Code truncation cutoff of ~2048 B. The 10 rules now live in
per-tool `description` strings (where clients actually read them to
pick a tool) and in the adopted-project decision table.

What remains in `instructions` is the boundary signal — which 5
advanced tools are CLI-only (MCP integration can't call them by
name), what to still use Grep / Read for, and where to find the
adopted decision table.

**3. Tool description tightening (LLM-visible).**

- `semantic_code_search` now adds *"If module path is known, prefer
  module_overview"* — closes the "I know the path AND a concept word"
  ambiguity that previously routed to semantic search and burned a
  vector lookup.
- `find_references` now adds *"For plain literals (string/regex),
  prefer Grep"* — `find_references` only tracks defined-symbol usage
  sites, not raw text. Before tightening it caught literal-string
  queries that should have gone to Grep.

**4. routing_bench: +2 regression guards.**
Two new oracle items in `tests/routing_bench.rs` directly probe the
tightened phrasings:

- *"How does the embedding pipeline work in src/embedding/?"* →
  expects `module_overview` (path > concept tie-breaker).
- *"I need to rename parse_tree to parse_ast — find every place I'd
  update."* → expects `find_references` (rename-audit intent
  preserved despite the new "prefer Grep for literals" line).

**Verification.** `OPENROUTER_API_KEY=… cargo test --release --test
routing_bench -- --ignored` against `anthropic/claude-sonnet-4.5`
returned **P@1 = 21/22 = 95.5%**, up from baseline v0.16.7's
19/20 = 95.0%. Both new guards passed. The single miss is *"Show me
the EmbeddingModel struct definition"* routing to `ast_search`
instead of `get_ast_node` — pre-existing oracle item, semantically
defensible (`ast_search` returns nodes by name+kind), not introduced
by this release.

## v0.16.9 — install/uninstall lifecycle hardening + MCP/CLI parity

Audit-driven fixes after sandboxed end-to-end testing of the install,
adopt, update, and uninstall flows. Three real bugs surfaced that the
existing 97-test suite couldn't see because none of them tested the
*real* user path: `npm uninstall`, post-upgrade binary resolution, and
adopt-from-fresh-clone. Plus a parity sweep on the MCP↔CLI surface.

**1. `npm uninstall` left dangling hooks in `~/.claude/settings.json`.**
The package shipped a full `lifecycle.js uninstall` that strips our
hook entries from settings.json — but nothing wired it to npm. After
`npm uninstall -g @sdsrs/code-graph` the package files were gone but
`settings.json` still pointed PostToolUse / SessionStart hooks at the
deleted scripts. Claude Code subsequently failed to fire those hooks
or surfaced ENOENT spam.

**Fix:** added `"preuninstall": "node claude-plugin/scripts/lifecycle.js
uninstall || true"` to `package.json`. npm now invokes the existing
uninstall path before removing files. The `|| true` ensures a
lifecycle failure never blocks the uninstall itself. Verified end-to-end
in a sandboxed HOME: settings.json hooks containing `code-graph` paths
get stripped; foreign hooks and `otherKey` configuration are preserved
byte-for-byte.

**2. `find-binary` cache shadowed fresh `npm update` binaries.** The
cache priority was: dev mode → auto-update cache (`~/.cache/code-graph/
bin/`) → platform npm pkg. After `npm update -g 0.16.7→0.16.8` the
platform-pkg binary was refreshed, but the auto-update cache still
held 0.16.7. find-binary returned the stale cache because it only
verified the binary was *executable*, never that the version matched.
Users kept running 0.16.7 until auto-update fired (up to 6h later).

**Fix:** when the auto-update cache hits, read its `--version` and
compare against the npm pkg version (`require('../../package.json').
version`). Cache wins when `cache.ver >= pkg.ver` (legitimate case:
auto-update fetched a newer release than npm has shipped). Cache loses
when older — find-binary falls through to platform-pkg. Includes a
3-digit semver compare helper that tolerates short / non-numeric input.

**3. `adopt` couldn't bootstrap a fresh clone.** The path required
`~/.claude/projects/<slug>/memory/` to already exist (created by
Claude Code on first session that writes memory). Fresh-cloned project
with no memory dir → `adopt` errored `no-memory-dir` and told the user
to "run claude at least once". CI / scripted setup / first-time users
on a new project all hit the wall.

**Fix:** introduced a project-marker check (`.git`, `Cargo.toml`,
`package.json`, `pyproject.toml`, `go.mod`, `pom.xml`, `build.gradle`,
`.code-graph`). Memory dir missing AND cwd has any marker → `mkdir -p`
and proceed. No marker → return `not-a-project` with a clearer error
("cd into a real project before running adopt"). The slug-pollution
guard remains in place for `/tmp` / `$HOME` accidents.

### Slug collision marker

Claude Code's slug encoding (`[^a-zA-Z0-9-]→'-'`) is lossy: `/foo/bar`
and `/foo bar` resolve to the same memory dir. Two projects can
silently share state with no signal. Added: `adopt` writes
`<!-- adopted-by: <abs-cwd> -->` as the first line of
`plugin_code_graph_mcp.md`. Re-adopt from a different cwd surfaces
`result.collisionWith` and a stderr warning. `needsRefresh`'s
bytewise compare strips the marker line first, so the marker doesn't
cause false-positive drift detection on every SessionStart.

### MCP↔CLI parity sweep

Drove every MCP tool against its CLI counterpart on the same query
and compared output. Three real divergences fixed:

- **`hot_functions`**: CLI used `callers` / `test_callers`, MCP used
  `caller_count` / `test_caller_count`; CLI cap=15, MCP cap=10. Both
  now use `caller_count` / `test_caller_count`. CLI honors `--compact`
  for top-10 cap (matching MCP `compact:true`); default returns top-15
  (the underlying SQL `LIMIT 15`).
- **`module_overview` compact**: MCP renamed `caller_count` → `callers`
  in compact mode but kept `caller_count` in full mode. Aligned both
  to `caller_count`.
- **`get_call_graph` self-edge**: CLI included the queried symbol
  itself with `direction=callers` AND `direction=callees` (count off
  by 2 for `direction=both`). MCP filtered `depth > 0`. CLI now
  filters seed in JSON output too. Human renderer keeps the seed for
  the tree root.
- **`project_map` compact `type` field**: MCP non-compact had `type`
  on each hot_function, compact dropped it. Both surfaces now keep
  `type` for parity.

### CLI accepts MCP tool names as aliases

Real-world friction observed in another project where Claude typed
`code-graph-mcp project_map --compact` (the MCP tool name) verbatim
into Bash and hit "Unknown subcommand: project_map". The MCP
`instructions` had `Start: project_map --compact` without the
parens-form CLI alias hint that the other 10 rules use. Two-layer fix:

- Fixed the instructions text: `Start: project_map (map --compact)`
  follows the existing `MCP-name (CLI-alias)` convention.
- Defense in depth: CLI dispatch now accepts MCP tool names directly.
  `project_map` / `module_overview` / `get_ast_node` / `find_references`
  / `get_call_graph` / `impact_analysis` / `find_similar_code` /
  `dependency_graph` / `trace_http_chain` / `find_dead_code` /
  `ast_search` / `semantic_code_search` all map to existing short-name
  handlers. `code-graph-mcp project_map --compact` now works. Typo
  suggester also learned the MCP names so `project_mapp` →
  `project_map`.

### Opt-in real-network auto-update test

`scripts/release-smoke.test.js` gained `auto-update parses real GitHub
releases/latest shape`, gated on `CODE_GRAPH_AUTO_UPDATE_E2E=1`. The
existing 10 auto-update unit tests are all mocked — there's no
guardrail against GitHub API shape regression. Run once per release
to validate `parseLatestRelease` against the real payload.

### Validation

- 165 node tests pass + 1 opt-in skip across 12 suites
- 391 cargo tests pass + 1 ignored (routing_bench needs API key)
- Sandbox lifecycle E2E: 16/16 pass with HOME-isolated mkdtemp
  (binary smoke / adopt / re-adopt / status / check / session-init /
  unadopt / residue audit, no orphan plugin file)
- A end-to-end: realistic settings.json with `code-graph` hook paths
  → `lifecycle uninstall || true` strips ours, preserves foreign
  hooks + `otherKey`

## v0.16.8 — callgraph tree, JSON contracts, dead-code defaults, E2E hardening

End-to-end usability pass: simulated a Claude Code session driving every
MCP tool and CLI subcommand on real symbols. Five independent fixes for
issues that surfaced — none blocking on their own, but each was eroding
the trust-layer agents need to act on tool output.

**1. `callgraph` rendered depth>1 nodes under the wrong parent.** The
recursive CTE was collapsing duplicates with `GROUP BY MIN(depth)`,
which lost the actual traversal parent and made every depth-N node
appear nested under the *last* depth-(N-1) sibling. So `A→B→C` plus
`D→B` printed as if `D` lived under `A` once `B` was already shown.

**Fix:** the CTE now tracks `parent_id` (the cg row that produced each
new node) on each inductive step, and dedup uses
`ROW_NUMBER() OVER (PARTITION BY node_id ORDER BY depth)` so the
shortest-path parent survives. CLI renderer builds a `parent_id →
children` map per direction and recurses, so callers/callees subtrees
stay separate under `--direction=both`. JSON output now includes
`parent_id` (null for the root) for any consumer that wants to rebuild
the tree.

**2. `similar` and `deps` violated the `--json` empty-result contract.**
Both subcommands had paths that wrote nothing to stdout and exited
with stderr only — breaking machine consumers per
`feedback_cli_json_empty_contract`. Added: `similar --json` writes
`[]` when vector search returns no neighbors; `deps --json` writes a
JSON error object `{"file":..., "depends_on":[], "depended_by":[],
"error":"..."}` when the file has no tracked imports. Two new
regression tests guard these paths.

Bonus: `similar 1010` (digits as positional) used to print the
unhelpful "Symbol not found: 1010". Now nudges toward
`similar --node-id 1010`. And `similar` with an existing symbol that
hasn't been embedded yet ("No embedding for node_id 342") explains
*why* (`(1033/1321 nodes embedded — embeddings still generating; try
again shortly or pick a node with --node-id from \`show X\`)`).

**3. MCP tool descriptions misled agents on subtle defaults.** Two
tools had descriptions that didn't match their actual behavior, so
agents made decisions on stale info:

- `module_overview` — caller counts include test callers, but the
  description didn't say so; agents reading "5 callers" couldn't tell
  if a function was prod-hot or only test-driven. Description now
  states "callers count includes tests" so the LLM picks a different
  tool when it actually needs prod-only callers.
- `find_references` — for constants, only `imports` edges are
  recorded; usage sites where the const is read don't appear because
  Rust grammar emits them as identifiers without an import-context.
  Description now says "consts: imports only, not value-uses" so the
  agent escalates to grep when auditing a const for rename.

Also added one line to the MCP `instructions` payload telling the
agent that `impact_analysis`/`find_dead_code`/`find_similar_code`/
`dependency_graph`/`trace_http_chain` are CLI-only after the v0.10.0
core/advanced split — Claude Code only sees the 7 core tools, so
agents trying to invoke the advanced 5 directly via MCP would 404.

**4. E2E suite was passing on dead queries.** `scripts/e2e-validate.js`
called `get_call_graph(handle_call_tool)`, `impact_analysis(
handle_call_tool)`, and `dependency_graph(src/mcp/server.rs)` —
all three symbols/paths had been renamed/moved sessions ago. The
assertions only checked "response contains non-empty text", so
"`[code-graph] Symbol not found: handle_call_tool`" passed as
success. 24/24 green, but actually testing zero-result paths. Real
response sizes told the story: get_call_graph 221 bytes (now 2628),
impact_analysis 220 bytes (now 498), dependency_graph 304 bytes
(now 2291).

**Fix:** swapped the queries to stable hot symbols (`handle_message`,
`conn`, `src/mcp/server/mod.rs`) and added two stricter assertions:
`assertNotEmptyResult(resp, label)` rejects 6 known empty-result
patterns ("Symbol not found", "No callers found", etc.); the MCP
`dependency_graph` returns JSON, not the human "Depends on" text, so
its assertion now `JSON.parse`s and checks `depends_on` is a non-empty
array.

**5. `dead-code` falsely flagged Criterion benchmarks as orphan.**
`benches/indexing.rs` defines three bench functions, all referenced
only via `criterion_group!(benches, bench_full_index, ...)`. The AST
relation extractor doesn't parse macro arguments as references, so
the benches showed up as ORPHAN every time — drowning out the four
real `EXPORTED-UNUSED` results worth attention.

**Fix:** added `benches/` to `domain::default_dead_code_ignores()`,
mirroring the existing `claude-plugin/` exclusion for shell-invoked
hook scripts. The rule generalizes: any directory whose entry points
are reached through tokens the AST can't resolve (macro arguments,
shell command strings, settings.json hook definitions) belongs in
the default ignore list. CLI `--no-ignore` still surfaces them. New
unit test pins the policy.

Together these don't change any external schema, but they materially
improve the signal an agent gets per tool call — fewer phantom
orphans, a callgraph tree that reads like one, and an E2E suite that
actually fails when a hot symbol moves.

## v0.16.7 — install reliability: 3 independent failure paths fixed

Reported on a fresh `/plugin install code-graph-mcp` on another
machine: MCP couldn't connect, the binary was nowhere to be found.
Triage found three independent breakages along the launcher chain;
each is fixed and tested separately so the chain is fault-tolerant
on first install.

**1. `find-binary.js`: didn't search npm global `node_modules`.**
`require.resolve('@sdsrs/code-graph-{platform}-{arch}/package.json')`
only walks the `node_modules` chain rooted at the requiring file —
it does NOT search global installs, because nvm and standard Unix
prefixes don't set `NODE_PATH`. So a working `npm install -g
@sdsrs/code-graph-linux-x64` was previously invisible to the
launcher even when the binary was sitting at
`~/.nvm/.../lib/node_modules/@sdsrs/code-graph-linux-x64/code-graph-mcp`.

**Fix:** new `globalNodeModulesCandidates()` probes 4 prefix
sources — `process.execPath`-derived (Linux/macOS:
`<prefix>/lib/node_modules`; Windows: next to `node.exe`),
`NPM_CONFIG_PREFIX` env, `~/.npm-global/lib/node_modules`, and
`npm root -g` (last resort, ~50-200ms). New `findPlatformBinary()`
combines fast-path (`require.resolve`) + slow-path (global probe).

**2. `auto-update.js`: trusted state file over filesystem.** When
`installedVersion === latestVersion`, `checkForUpdate` short-circuited
to the no-update branch without verifying that
`~/.cache/code-graph/bin/code-graph-mcp` actually exists. Once the
state file recorded "installed v0.16.6", a wiped cache or a
silently-failed prior download would never be repaired. Real-world
artifact: `update-state.json` says "Up to date" while the cache
directory is empty.

**Fix:** new `downloadBinary()` helper extracted from
`downloadAndInstall` so the binary download can run in either
context. Throttle bypassed when cache binary is missing (a hard
failure overrides the 6h check window). No-update branch
self-heals by calling `downloadBinary(latest)` when binary is
absent. `cachedBinaryPath()` exported for test harnesses.

**3. `mcp-launcher.js`: only one fallback strategy.** When
`findBinary()` returned null, the launcher tried `npm install -g
@sdsrs/code-graph` once and gave up if that didn't yield a binary.
But npm's `optionalDependencies` failure mode is to silently
accept partial installs (an OS-mismatch tolerance feature that
also masks transient registry/network errors), so the wrapper
package would install successfully while the platform binary
package was dropped.

**Fix:** second-stage fallback runs `auto-update.js --silent`
which downloads the platform binary directly from the GitHub
release into `~/.cache/code-graph/bin/`. Bypasses npm registry
entirely. Final error message also names the platform-specific
package (`@sdsrs/code-graph-{platform}-{arch}`) for manual
recovery.

**Tests:** 7 new (`find-binary.test.js` × 4 covering candidate
derivation + dedup + integration; `auto-update.test.js` × 3
covering `cachedBinaryPath` + `downloadBinary` null safety).
117 plugin JS + 385 Rust = 502 total green.

## v0.16.6 — semantic_code_search: doc demotion + find_references: include_tests

Two MCP tool UX bugs surfaced during a user-simulation pass
over the core 7 toolset on this very repo:

**semantic_code_search: README headings outranked code.** Query
`merkle tree change detection` returned `README.md` `License`
(h2, 0.45) / `Features` (h2, 0.44) / `Build` (h3, 0.42) ahead
of `DirectoryCache` struct in `src/indexer/merkle.rs` (0.37).
Root: markdown heading nodes get respectable vector-similarity
scores for unrelated queries (short heading text embeds close
to many concepts), and the re-ranker (`name_boost` /
`size_factor`) had no doc-tier preference. The tool is
`semantic_code_*search*`; for code-intent queries, prose should
not dominate.

**Fix (`src/mcp/server/tools.rs:193-209`):** `doc_penalty = 0.4`
multiplier applied when the candidate's language is `markdown`
AND the caller did not pass `language="markdown"`. Same query
after fix: TOP 6 all from `merkle.rs` / `watcher.rs`, first
result `DirectoryCache` rose to 0.60. Explicit
`language="markdown"` bypasses the penalty (verified
`Installation` h2 comes back at 0.59 for "installation
instructions" queries).

**find_references: no test-filter opt-out.** `upsert_file`
query returned 27 references, 24 of them `test_*` callers,
drowning the 3 production usage sites. Inconsistent with
`get_call_graph` and `get_ast_node include_impact=true`, which
already default to hiding test callers.

**Fix:** new `include_tests` boolean parameter (default `true`
to preserve rename-audit semantics — tests ARE usage sites),
plus `test_references_filtered` count in the response when
callers opt out. Schema published in `src/mcp/tools.rs:131`.
Call with `include_tests=false` to get production-only refs;
call without the flag (or `true`) for the pre-v0.16.6
behavior.

## v0.16.5 — impact_analysis: UNKNOWN risk for non-function symbols

Three impact-analysis paths (`cmd_impact`, `tool_impact_analysis`,
`append_impact_summary`) each maintained their own inline list of
"non-function" node types to flag as UNKNOWN. The lists had drifted:
two only matched `struct|class|enum|interface|type_alias` (missing
`constant` and `trait`), and `append_impact_summary` — the path
reached by the core-7 `get_ast_node include_impact=true` that Claude
Code actually uses — had no type check at all.

Symptom: `code-graph-mcp impact REL_CALLS` returned
`risk_level: LOW, 0 callers` even though 16 importers touch the
constant. An LLM acting on that signal would confidently change the
string and break every importer.

**Fix (`src/domain.rs`):** single source of truth
`is_function_node_type()` + `NON_FUNCTION_IMPACT_WARNING` constant.
All three paths share them. Non-function symbols with zero call-graph
callers now return `risk_level: UNKNOWN` plus an explicit warning
directing to `find_references` / `code-graph-mcp refs <symbol>`.
Function / method impact behavior is unchanged; `HIGH`/`MEDIUM`/`LOW`
still flow from `compute_risk_level` as before.

## v0.16.4 — watcher canonicalize: cfg-gate off Windows (UNC path trap)

v0.16.3 canonicalized the watcher root on every platform to fix
macOS FSEvents; on Windows that regressed the watcher because
`std::fs::canonicalize` there returns UNC paths (`\\?\C:\...`) while
the ReadDirectoryChangesW backend emits plain `C:\...` — the same
`strip_prefix` silently-drop-all-events failure as before, mirrored.
The canonicalize step is now cfg-gated to non-Windows only.

Windows Release workflow (build + npm publish + smoke test) was
always green because the watcher unit tests don't run there; this
only surfaced on the CI matrix.

## v0.16.3 — macOS FSEvents root canonicalization

Follow-up to v0.16.2. After the path-normalization fixes landed,
Windows CI turned green but the two macOS watcher tests still
timed out. Root cause: FSEvents emits every event path via realpath,
so a watch registered on a non-canonical root like
`/var/folders/xx/T/foo` (the `tempfile::TempDir` default on macOS)
could never produce a prefix match against realpath output
`/private/var/folders/...` — every event was silently dropped at
`strip_prefix`.

**Fix (`src/indexer/watcher.rs`):** `FileWatcher::start` canonicalizes
the root path before passing it to notify. No-op on systems without
symlinks in the path; unblocks macOS CI and also hardens production
against project roots with symlinked ancestors (home-dir on systems
where `/home` is a symlink to `/usr/home`, chrooted containers, etc.).

## v0.16.2 — cross-platform path normalization + watcher test stability

Follow-up to v0.16.1. That release fixed Clippy on the 1.95 toolchain,
which let the `Test` step run for the first time on macOS and Windows
in this repo's CI matrix — and immediately surfaced a set of
pre-existing cross-platform bugs the previous red baseline had been
hiding. v0.16.2 addresses them.

**Path normalization (fixes Windows runtime + tests):**
- `src/indexer/merkle.rs` — new internal `normalize_rel_path(&Path)`
  helper converts `\` to `/` on Windows. All relative paths that land
  in the DB, CLI/MCP output, and gitignore-prefix checks now use `/`
  on every platform. Without this, `starts_with(".git/")` style
  filters only fired when the OS used `/`, and Windows users saw
  `pkg\scripts\foo.js` in every tool response.
- `src/indexer/watcher.rs` — notify events go through the same
  normalizer before emission.
- Fixes 4 pipeline tests and 2 merkle tests that were red on
  `windows-latest` in v0.16.1 CI.

**macOS FSEvents flake:**
- `src/indexer/watcher.rs::tests::test_watcher_detects_file_changes`
  — recv_timeout raised from 5s to 15s. macOS FSEvents coalescing on
  loaded GH runners routinely exceeded 5s.
- `src/mcp/server/tests::test_watcher_detects_changes_and_reindexes`
  — replaced fixed 300ms sleep with bounded polling (40 × 200ms
  ≈ 8s total), which is correct on slow hosts and instant on fast.

**CI:**
- `.github/workflows/release.yml` — post-publish smoke now reads
  `map.json` via `fs.readFileSync('map.json',...)` instead of
  `require('$tmpdir/map.json')`. On Git Bash under Windows,
  `mktemp -d` returns a POSIX-looking `/tmp/tmp.XXXX` that Node.js
  on Win32 cannot resolve; the `require` was failing despite the
  file existing.

## v0.16.1 — JS edge resolution precision + CI clippy component fix

**Parser / indexer correctness (JS/TS):**
- `src/parser/relations.rs` — `walk_for_relations` no longer tags
  anonymous arrow functions (`test(() => {...})`, `[1,2].map(x => x)`)
  with the sentinel scope `<anonymous>`, which resolved to no source
  node and silently dropped every call inside such callbacks. Arrows
  without a `variable_declarator` parent now inherit the enclosing
  scope; JS/TS/TSX calls at module top-level fall back to `<module>`
  so they produce resolvable same-file edges. Test-file helpers like
  `writeJson`, `mkHome`, `readCargoVersion` that are referenced only
  from inside `test(...)` callbacks are no longer reported as orphan
  dead code.
- `src/indexer/pipeline.rs` — cross-file same-language resolution used
  to fan out an edge to every same-name target whenever no same-file
  match existed, turning a single `readJson()` call into N phantom
  edges across unrelated modules. New `refine_ambiguous_targets`
  prefers non-test candidates (when the caller is non-test code) and
  the candidate with the longest byte-common path prefix; keeps the
  remaining pool on true ties so Rust bare-name `crate::x::foo()`
  calls that always tie on prefix don't get dropped.

Before v0.16.1 this project indexed 28 cross-file JS `calls`
fan-out edges, all of them pointing at the wrong target in at least
one leg; after, 7 edges, each single-target and correct. `refs
writeJson` rose from 2 → 5 (the 3 real test-callback callers
previously lost).

**CI:**
- `.github/workflows/ci.yml` — `dtolnay/rust-toolchain@1.95.0` now
  installs the `clippy` component explicitly. Without this, the
  Clippy step failed with `'cargo-clippy' is not installed for the
  toolchain '1.95.0'` on every OS/feature-matrix cell in v0.16.0.

## v0.16.0 — production hardening pass (RRF math, schema v7 dim guard, readonly secondary, bounded watcher, CI matrix)

Architecture audit surfaced nine correctness / safety gaps — this
release addresses all of them plus four items flagged in a follow-up
code review. Schema bump auto-migrates; no user action required.

**Algorithmic correctness:**
- `src/search/fusion.rs` — `SCORE_BLEND_FACTOR = 0.1` silently dominated
  RRF by ~100× at k=30 (rank-0 RRF ≈ 0.0164 vs. max blend = 0.1),
  inverting the docstring's own "doesn't override rank ordering"
  contract and effectively converting RRF into per-source-raw-score
  ranking. Replaced with adaptive `blend_scale = 0.5 / ((k+1)(k+2))` —
  mathematically half the smallest adjacent-rank RRF gap. Semantic
  search results will shift (for the better) on queries where one
  source returns a high-raw-score item at a late rank.

**Data safety — schema v7 embedding-dim guard:**
- `SCHEMA_VERSION` 6 → 7. New `meta` table records embedding_dim. On
  open, mismatch → atomic DROP + rebuild `node_vectors` at current
  `EMBEDDING_DIM`. Prevents silent crash-on-INSERT when a user rebuilds
  the binary at a different dim (e.g., swaps embedding model).
- v6 → v7 upgrade path introspects the on-disk vec0 DDL via
  `sqlite_master.sql` (`float[N]` regex) and rebuilds if the existing
  table's dim ≠ current — the adversarial case where `meta` is empty
  but a pre-existing vec0 is present.

**Concurrency hardening:**
- `src/indexer/watcher.rs` — bounded `sync_channel(4096)` with
  overflow-drop policy (warn!). Unbounded channel had no cap on memory
  during bulk fs events (branch switches, IDE reformats). Merkle
  rescan is idempotent so dropped events don't lose data.
- `src/storage/db.rs` + `src/mcp/server/mod.rs` — secondary instances
  (flock denied) now open DB with `SQLITE_OPEN_READ_ONLY | query_only=ON`.
  Eliminates race where a secondary could run migrations +
  `INDEX_VERSION` DELETE sweep against the primary's DB. Secondary
  polls up to 3s for the primary's bootstrap then bails with a clear
  error rather than falling through to read-write.

**Contract strengthening:**
- `src/parser/relations.rs` — `ParsedRelation` carries `source_language`,
  stamped by `extract_relations_from_tree`. Resolver at
  `src/indexer/pipeline.rs` hard-errors on mismatch (bail!, not
  debug_assert!) so parser regressions fail in release builds too.
- `src/mcp/server/mod.rs` — `start_post_index_services` spawns a
  once-per-process Phase-3 repair thread before background embedding.
  README's "Startup repair for incomplete indexing" claim was
  documentation-only until now; `repair_null_context_strings` now
  actually fires on every session start (primary-only, idempotent).

**Documentation accuracy:**
- `README.md` — HTTP route tracing previously claimed Express, Flask/
  FastAPI, Go, ASP.NET, Rails, Laravel, Vapor (8 frameworks). Only 3
  are actually implemented in `extract_route_pattern`. Corrected.

**CI + release:**
- `.github/workflows/ci.yml` — matrix {ubuntu, macos, windows} × {no-embed,
  with-embed} (was ubuntu-only), toolchain pinned `@1.95.0`.
- `.github/workflows/release.yml` — new `smoke-verify` job runs after
  `publish` on all 3 OSes: npm install with retry-backoff, `--version`
  exact match, `incremental-index` + `map --json` on a tmp git repo.
  Catches missing platform binaries / `find-binary.js` regressions /
  version-sync drift before users hit them.

**Test delta:** +18 unit tests (RRF invariants ×4, schema v7 paths ×5,
readonly ×2, source_language stamp ×1, etc.). 250 unit + 56 integration
+ 44 hardening + 19 parser + 6 cli + 6 plugin + 1 routing = 382 tests
pass. Clippy 1.95 clean on both feature modes.

**Deferred to a later release (L3 refactor):** `tools.rs` (2236 LOC),
`relations.rs` (2174), `queries.rs` (2783) file splits — flagged in the
audit but require a dedicated session with plan-mode review.

## v0.15.2 — ast_search ranking + dead-code --json empty contract

User-driven QA pass exercising every MCP tool + CLI subcommand surfaced
two bugs whose contract violations were silent — both regressions guard
against recurrence.

Fixes:
- `src/storage/queries.rs` — `get_nodes_with_files_by_filters` (the SQL
  backing `ast_search` / `ast-search`) ordered by `f.path ASC` only, so
  the `LIMIT` clause silently truncated alphabetically-late files
  (`src/storage/queries.rs` itself, with 54 `Result`-returning fns) out
  of the top-N. New ordering is `caller_count DESC, path ASC, line ASC`
  so high-value symbols surface first regardless of file path.
- `src/cli.rs:2655` — `dead-code --json` returned only stderr (no stdout)
  when all results were filtered by `--ignore`, breaking JSON consumers
  piping stdout. Now emits `[]` to stdout before the human stderr
  message, matching the established empty-result contract used by
  `search` / `grep` / `callgraph` / `show` / `trace` / `overview`.

New regression tests:
- `test_get_nodes_with_files_by_filters_ranks_by_caller_count`
  (`src/storage/queries.rs`) — alphabetically-first low-caller fn must
  not outrank alphabetically-last high-caller fn at any `LIMIT`.
- `test_cli_json_empty_dead_code` (`tests/cli_e2e.rs`) — stdout must be
  `[]` and stderr must still surface "No dead code" when --ignore filters
  all results.

371 tests pass (was 369). Clippy 1.95 clean on both feature combos.

## v0.15.1 — TSX parity: LanguageConfig + require() + Express routes

v0.15.0 audit of JS/TS support surfaced a silent breakage for `.tsx`
files: `LanguageConfig::for_language("tsx")` hit the default arm where
`_ => "unknown"`, so every `config.name == "tsx"` branch was dead code.
Ripple effect: the describe/it `is_test` propagation added in v0.15.0
(scoped `matches!(config.name, ... | "tsx")`) silently skipped TSX.

Fixes:
- `src/parser/lang_config.rs` — add `"tsx" => "tsx"` to the static-name
  match so `config.name` is preserved through the default-config branch.
- `src/parser/relations.rs:101` — `require()` arm now matches
  `"javascript" | "typescript" | "tsx"` (was js/ts only).
- `src/parser/relations.rs:1172` — `extract_route_pattern` now routes
  `"tsx"` through `extract_express_route` alongside js/ts.

Two new regression tests: `test_extract_tsx_commonjs_require_and_route`
(parser) and `test_parse_tsx_describe_it_marks_nested_as_test`
(treesitter). 369 total tests pass.

C/C++ coverage audit surfaced three parallel gaps — `#include`
not extracted, GoogleTest `TEST`/`TEST_F`/`TEST_P` macros not
recognized, no scope qualification for `Class::method` / `obj.method` /
`obj->method`. Tracked for v0.16.0.

## v0.15.0 — same-language edge resolution, JS require() imports, markdown indexing, JS test-block detection

Multi-front accuracy pass motivated by user feedback that code-graph was
useful in Rust projects but under-utilized in JS / mixed / claudemd
projects. Traced to four compounding issues; all four fixed in this
release with regression tests.

### Same-language edge resolution — eliminates cross-language phantom edges

`src/indexer/pipeline.rs` resolved call/implements/imports target names
via a flat global bare-name lookup. In mixed-language projects this
produced catastrophic false positives: the Rust `hasher.update(&buf)`
call in `src/indexer/merkle.rs:hash_file` was resolving to the JS
`function update()` in `claude-plugin/scripts/lifecycle.js`, pulling
11 phantom Rust→JS edges into `callgraph hash_file` (verified via
dogfood before/after). Each same-named method (`update`, `open`,
`init`, `run`, `read`, `write`, etc.) was a collision vector.

Fix: edge resolution now uses a three-tier cascade — `same-file` →
`same-language` → (for calls: drop; for imports/implements: global
fallback to preserve the existing `<external>` sentinel path).
Non-call relations keep cross-language fallback because sentinel
nodes carry language `"external"` by design.

Mechanically, `get_all_node_names_with_ids` and the per-batch
`node_id_to_path` map now carry each node's `language`, enabling the
filter. Public type alias `NameEntry = (i64, String, Option<String>)`
added to keep clippy `type_complexity` happy.

Regression test `test_cross_language_bare_name_call_resolution`
plants an `update` collision across a Rust file and a JS file and
asserts that Rust `caller_rs` does not resolve any call edge to the
JS file.

### CommonJS `require()` — JS import edges appear for the first time

`src/parser/relations.rs` handled ES module `import` statements but
had no branch for `require(...)` calls, the canonical CommonJS form.
Consequence: Node.js code bases (including this repo's own
`claude-plugin/scripts/*.js`) had 3 total `imports` edges across 19
JS files before the fix. After the fix: 286 edges (path 27, fs 24,
child_process 18, os 17, plus local modules).

Require detection inserted into the existing `call_expression` arm;
handles `node:fs` scheme normalization and strips `.js`/`.ts`/`.mjs`/
`.cjs` suffixes so `require('./utils/version-utils.js')` resolves to
the same target as an ES `import` binding named `version-utils`.
Unresolved imports flow into the existing Phase 2b-ext external-
sentinel mechanism (previously only wired for implements), so
`<external>/fs` nodes now exist and are discoverable via `deps <file>`
dependency graphs.

Two new tests: `test_extract_js_commonjs_require` (parser level,
covers node scheme + extension stripping + relative paths) and
`test_js_require_creates_external_import_edges` (pipeline level,
end-to-end DB assertion).

### Markdown heading indexing — claudemd / docs projects become navigable

Added `tree-sitter-md = "0.3"` (pinned to 0.3 because 0.5.x ships
tree-sitter ABI 15 and this repo still runs tree-sitter 0.24 / ABI 14).
`detect_language` accepts `.md` / `.mdx`; `LanguageConfig` exposes
"markdown" for the default-config fallthrough; `extract_nodes` new arms
for `atx_heading` (walks marker children to infer level 1–6) and
`setext_heading` (paragraph + `setext_h{1,2}_underline`). Heading text
becomes the node name, `h1`..`h6` the node type. Searchable via FTS;
visible in `module_overview` and `project_map`.

Dogfood: this repo's README, CHANGELOG, and 4 plugin docs now yield
145 heading nodes. `code-graph-mcp search "Installation"` returns
`h2 Installation README.md:117` as the top hit.

Shell and JSON indexing deferred — tree-sitter-bash adds real value
for hook-script projects; JSON alone is low-yield because the useful
relations (hook → script name) cross file formats. Both tracked as
follow-up.

### JS `describe` / `it` / `test` AST blocks mark nested code as test

`LanguageConfig::has_test_attributes = false` for JS/TS because the
test framework is function-call-driven, not attribute-driven. The
existing `is_test_symbol` file-path heuristic caught `.test.js` /
`.spec.js` / `__tests__/` patterns but missed **in-source** test code
(Vitest in-source testing, Jest co-location without the suffix, or
any file that mixes prod + test definitions).

`extract_nodes` now intercepts `call_expression` nodes whose function
head is one of `describe`, `it`, `test`, `suite`, `context`,
`beforeEach`, `beforeAll`, `afterEach`, `afterAll`, `before`, `after`,
`fdescribe`, `xdescribe`, `fit`, `xit` (both bare and `.only` / `.skip`
/ `.each` member forms). Child argument nodes recurse with
`in_test_context = true` which flows into the existing `is_test` field
on every nested function / class / method.

Regression: `test_parse_js_describe_it_marks_nested_as_test` plants
6 definitions across `describe` / `it` / `it.skip` / `beforeEach`
nesting and asserts the `is_test` propagation is correct (plus a
top-level prod function stays `is_test=false`).

### Test + dogfood summary

367 total tests pass (+4 net new). `cargo +1.95.0 clippy --all-targets
-- -D warnings` clean. Full rebuild on this repo: 84 files → 1295
nodes → 2590 edges (was 1068 / 2300 pre-release). Net per-dimension:
- phantom Rust→JS call edges: 11 → 0
- JS imports edges: 3 → 286
- markdown heading nodes: 0 → 145
- indexed languages: 16 → 17

## v0.14.5 — FK-recovery fix, rebuild_index busy-timeout relief, error-kind telemetry

Patch release. Drops six observed bug classes surfaced by a full-fleet
error-rate audit over 156 MCP sessions + 55 Claude Code transcripts.

### Incremental-index FK recovery now truncates before rebuild

Historical transcripts showed 6 agent-side `FOREIGN KEY constraint failed`
errors on `project_map` (4), `module_overview` (1), and
`semantic_code_search` (1). Root cause: `run_incremental_with_cache_restore`
caught FK violations and fell back to `run_full_index`, but the latter
only does per-file upsert — orphan rows from the failed incremental
survived and re-triggered FK on the retry, bubbling the raw SQLite
error to tool handlers.

Fix (`src/mcp/server/mod.rs:987`): the FK branch now `DELETE FROM files`
in a transaction before re-running full_index. CASCADE chains nodes →
edges → node_vectors via the schema's existing `ON DELETE CASCADE`.
Pattern lifted verbatim from `tool_rebuild_index`.

Regression test (`test_fk_fallback_truncate_purges_stale_state_and_rebuild_recovers`)
injects a phantom file + node + edge via `PRAGMA foreign_keys = OFF`
and asserts truncate + full_index purge it while restoring on-disk
symbols. Guards against future removal of the truncate step.

### `rebuild_index` 10s "busy" cliff relaxed to 30s

`usage.jsonl` showed `rebuild_index` err-rate 5/9 = 55%, with all 5
failures hitting `max_ms ≈ 10009` — i.e. the `embedding_in_progress`
wait deadline, returning `{status:"busy"}` which session metrics count
as errors. Not a real failure mode; 30s accommodates larger projects
whose embedding pass exceeds 10s.

### `find_dead_code` excludes anonymous `_` constants

`const _: () = assert!(...)` and `let _ = ...` patterns are
compile-time-only bindings, never callable. They were being reported
as dead code. New filter in `find_dead_code` SQL: `n.name != '_'`.

### Canonical error-kind telemetry in `usage.jsonl`

`SessionMetrics::record_tool_call` now classifies failures into
`ErrKind { Timeout, NotFound, Ambiguous, FkConstraint, EmptyInput, Other }`
and emits per-tool breakdowns as `tools.<name>.err_kinds`:

```json
"get_ast_node": {"n": 69, "ms": 4630, "err": 12, "max_ms": 2003,
                 "err_kinds": {"timeout": 7, "ambiguous": 3, "not_found": 2}}
```

Additive — readers that only consume `n/ms/err/max_ms` are unaffected.
Success-only tools omit the `err_kinds` field entirely for compact
output. Unlocks post-hoc error analysis via `jq` instead of manual
transcript grep.

### Dev tooling: `scripts/analyze-search-queries.py`

Persistent sampler that classifies `code-graph-mcp search` queries
issued by the agent (extracted from Claude Code transcripts) into
keyword-like vs concept-like. Used to validate decisions about
MCP-vs-CLI routing trade-offs without needing a round-trip through
`routing_bench`.

---

## v0.14.4 — CLI `impact`/`callgraph` ambiguous-symbol guard (parity with MCP)

Patch release. Closes a CLI/MCP behavior gap discovered in the same
end-to-end audit that produced v0.14.3.

### Bare-name queries on overloaded symbols now prompt for disambiguation

MCP `get_call_graph` and `get_ast_node` already returned an
`Ambiguous symbol` error with suggestion list when a bare name
resolved to ≥2 non-test definitions in different files. The CLI
counterparts (`callgraph`, `impact`) did not — they silently merged
call graphs / caller lists across all same-named definitions,
misreporting risk_level and blast radius.

Example: this repo has two `open` functions (`Database::open` in
`src/storage/db.rs` and `CliContext::open` in `src/cli.rs`). Before
the fix:

```
$ code-graph-mcp impact open
Impact: open — Risk: HIGH
  26 direct callers, 31 total, 9 files ...
```

The 26 callers are a union of both `open`s. After the fix:

```
$ code-graph-mcp impact open
[code-graph] Ambiguous symbol 'open': 2 matches in different files.
Specify --file or --node-id:
  open (function) in src/storage/db.rs [node_id 5717]
  open (function) in src/cli.rs [node_id 7055]
```

Exit code 1 signals script-level callers that disambiguation is
required. Qualified names (`Database.open`), `--file`, and `--node-id`
paths still work unchanged.

### Implementation

New helper `detect_exact_ambiguity` in `src/cli.rs` queries
`get_nodes_with_files_by_name`, filters non-test definitions, and
returns `Some(candidates)` only when ≥2 distinct files are present
(multiple definitions in one file, e.g. overloads, stay
non-ambiguous). Shared `emit_exact_ambiguity` formatter handles both
`--json` and human modes.

Both `cmd_callgraph` and `cmd_impact` gain a `file_filter.is_none()`
guard that invokes the helper before the downstream query runs.

### Verified

`cargo test` 235/235, `cargo +1.95.0 clippy --all-targets` clean.

## v0.14.3 — module_overview compact truncation fields + CLI deps `<external>` parity

Patch release. Two UX bugs found during end-to-end tool audit.

### MCP `module_overview` compact mode — surface truncation metadata

Full mode already set `active_capped`/`showing`/`total_active`/`hint`
when a module had >30 active exports, but `compact_module_overview`
rebuilt the response by cherry-picking known fields and silently
dropped the conditional truncation fields. Users calling with
`compact=true` on a large module (e.g. `src/parser/` with 54 active
exports) saw `"summary": "54 active + 2 inactive"` and 30 items — no
signal that 24 were missing.

Fix: forward the four conditional fields at the end of
`compact_module_overview` with a `.get().cloned()` loop so any future
addition of a conditional field stays forwarded by default.

### CLI `deps` — filter synthetic `<external>` bucket like MCP does

`dependency_graph` in the MCP handler filters the `<external>` pseudo-
file (a container for unresolved third-party imports) from outgoing
deps. The CLI `deps` subcommand had the language-compat filter but not
the `<external>` guard, so CLI output at depth ≥2 could show
`<external>` as a fake file dependency.

Fix: add the one-line guard to `cmd_deps`'s `is_compatible_lang` so
both entry points apply the same filter.

### Verified

`cargo test` 235/235, `cargo +1.95.0 clippy --lib -- -D warnings`
clean. Before/after:

- `module_overview(path="src/parser/", compact=true)` now returns
  `active_capped: true, showing: 30, total_active: 54, hint: "..."`
- `deps src/mcp/server/tools.rs --json` depends_on no longer contains
  `{"file":"<external>","depth":2}`

## v0.14.2 — MCP init instructions fit Claude Code truncation budget

Patch release. Fixes observed silent truncation of the MCP `initialize`
response `instructions` field at Claude Code's ~2KB harness boundary — the
last 4 of 10 routing decision rules were being dropped, making Claude
fall back to Grep/Read where code-graph tools should have been invoked.

### MCP `instructions` — pack 10 decision rules under 1500-byte budget

Old noisy-mode instructions were ~2.5KB with three section headers and
verbose workflow tips. Claude Code's `initialize` handler truncated near
~2048 bytes, cutting `modifying a function signature`, `find_dead_code`,
`find_similar_code`, `dependency_graph`, and the `get_ast_node` row — all
critical routing signals.

Rewrite compresses to **1292 bytes** (~48% of original) while preserving
all 10 decision rules verbatim. Each rule now carries its CLI alias
inline (e.g. `get_call_graph (callgraph X)`), so the LLM learns the CLI
invocation from the same line it learns the routing intent — no separate
MEMORY.md cross-reference needed for the base case.

Also re-adds a `Prompts:` line enumerating the three registered MCP
prompts, and replaces the misleading `"5 CLI-only tools"` phrasing with
`"5 advanced tools"` — the hidden 5 are still callable via raw MCP
`tools/call`, they are just off `tools/list` by default to preserve
startup-token budget.

### Compile-time budget guard

`const _: () = assert!(NOISY.len() <= 1500, ...)` added in
`src/mcp/server/mod.rs`. Any future edit that blows the budget fails
`cargo check` with `rustc E0080: evaluation panicked` — catches the
regression at build time, not debug-build test time. Verified by
tightening the cap to 1000 and observing the compile break.

### CLI `search` — stderr hint directing concept queries to MCP

CLI `code-graph-mcp search <q>` is FTS5-only; the MCP
`semantic_code_search` tool adds vector similarity + RRF fusion. On
non-JSON success paths, a stderr tip now points concept-query users to
the MCP tool. `--json` mode is untouched so script consumers still see
clean stdout.

### Tests

366 tests pass across integration suites (v0.14.1 baseline + compile-time
assert test exercised via intentional budget-cap inversion). Clippy 1.95
clean on both `--no-default-features` and `--all-targets`. Routing bench
(`tests/routing_bench.rs` via OpenRouter `anthropic/claude-sonnet-4.5`):
**P@1 = 19/20 = 95.0%** — unchanged from the v0.14.1 baseline, confirming
the compression did not degrade routing quality. Single miss remains the
known-borderline `ast_search` vs `get_ast_node` on a struct-def lookup.

---

## v0.14.1 — semantic search UX + find_references type hint

Patch release. Six targeted accuracy/UX fixes to MCP tool responses surfaced by a
3-round smoke test. All changes are additive or remove false-positive warnings;
no schema changes, no behavior regressions.

### `semantic_code_search` — compression estimator aligned to actual output

The compression trigger estimated token cost from `context_string` (can exceed
2000 chars) but the actual result JSON only carries `code_content` capped at
`MAX_SEARCH_CODE_LEN = 500`. Small `top_k` queries (3, 5) were being forced into
`compressed_nodes` mode unnecessarily, losing `relevance` and `signature` fields.

Estimator now mirrors the output: it measures truncated `code_content` +
signature + name + path + ~80 chars JSON framing per result. Small `top_k`
responses return full arrays again.

### `semantic_code_search` — `match_confidence` + `low_confidence_warning`

Compressed responses (`compressed_nodes` / `compressed_files` /
`compressed_directories`) now include a rounded `match_confidence` float. When
`< 0.5`, a `low_confidence_warning` string explains that FTS found few matches
and results are likely vector-similarity noise, with advice to use concrete
identifiers or `ast_search`.

The FTS sparsity and source-intersection penalties used to over-fire on
precision queries (single-identifier FTS hits). The penalty now requires
`fts_search.len() >= 5`; below that, the query is treated as precision-mode
and not penalized.

Exact-name-match exemption: when any top-5 candidate's `name` or
`qualified_name` equals the query (case-insensitive), the warning is
suppressed. `match_confidence` is still returned so callers can judge.

### `find_references` — `type_definition_note` for type symbols

When the target is a `struct` / `enum` / `trait` / `type` / `interface` /
`class`, the response now includes a `type_definition_note` explaining that
the edge index captures explicit imports/inherits/implements and
struct-literal instantiation, but NOT method-qualified calls
(`Type::method()`), field access, or type annotations. Guides the caller to
query each method via `module_overview` for a complete rename audit.

### `get_index_status` — `embedding_coverage_pct` floor

When embedding is in progress with a small fraction done (e.g. 2/1052),
integer percent rounded to 0 and looked stuck. Now floors to 1 whenever
`vectors_done > 0`, so `embedding_status: in_progress` stays consistent with
the percentage.

### `get_ast_node(node_id)` — explanatory not-found error

`Node N not found` replaced with a message that explains node_ids are
rebuild-scoped and suggests re-resolving via `get_ast_node(symbol_name,
file_path)` or `semantic_code_search`.

### Tests

43 `mcp::server` unit tests remain green. Routing bench
(`tests/routing_bench.rs` via OpenRouter `anthropic/claude-sonnet-4.5`):
**P@1 = 19/20 = 95.0%** (threshold 70%). Single miss is a semantic-neighbor
pick (`ast_search` vs `get_ast_node` for a struct-def lookup) unrelated to
this release.

---

## v0.14.0 — durable statusline-provider chain + public register CLI

Minor release. Addresses a long-standing fragility in the composite statusline
integration: when the user cleaned `~/.cache/code-graph/`, the `_previous`
snapshot (pre-install statusline, e.g. GSD) was lost, leaving only code-graph
visible on the status bar.

### Durable backup for `statusline-registry.json`

`writeRegistry()` in `claude-plugin/scripts/lifecycle.js` now mirrors the
registry to `~/.claude/statusline-providers.json` on every write. This file
lives outside the `~/.cache/` hierarchy, so routine cache cleanup no longer
strands third-party provider entries.

`readRegistry()` self-heals: if the primary `~/.cache/code-graph/statusline-registry.json`
is missing or empty, it falls back to the durable backup and rewrites the
primary. No user action needed on upgrade — the first `writeRegistry()` call
after install writes both files; recovery from a prior cache wipe happens
automatically on next SessionStart.

Clearing the registry (e.g. during uninstall) clears both files.

### New public CLI: `statusline-chain.js`

`claude-plugin/scripts/statusline-chain.js` exposes a documented registration
surface for third-party plugins that want to coexist with code-graph's
composite statusline:

```
node <plugin-cache>/scripts/statusline-chain.js register <id> <command> [--stdin]
node <plugin-cache>/scripts/statusline-chain.js unregister <id>
node <plugin-cache>/scripts/statusline-chain.js list
```

Reserved ids (`_previous`, `code-graph`) are rejected with exit code 2. The
CLI uses existing `registerStatuslineProvider` / `unregisterStatuslineProvider`
so writes land in both primary + durable backup.

**Motivating use case:** GSD currently owns `settings.json.statusLine`
directly and is captured as `_previous` when code-graph installs. With this
CLI, GSD's install hook can instead call `statusline-chain.js register gsd
"<gsd-statusline-command>" --stdin` and become a first-class provider in the
composite, independent of install order. Fallback path (call without `--stdin`
if the command doesn't read stdin; skip call entirely if code-graph isn't
installed) keeps standalone operation working.

### Tests

Four new cases in `lifecycle.test.js`:

- `writeRegistry` mirrors to durable backup
- `readRegistry` self-heals primary from backup after simulated cache wipe
- `writeRegistry([])` clears both files
- `statusline-chain.js` CLI register/list/unregister + reserved-id guard

12/12 lifecycle tests pass; 228/228 Rust lib tests green; clippy 1.95 clean on
both `--no-default-features` and `--all-targets`.

## v0.13.0 — `stats` CLI + rebuild_index busy semantics + CLI/MCP search disambiguation

Minor release. Three changes driven by real-usage-data review:

### `stats` subcommand (new)

`code-graph-mcp stats` aggregates `.code-graph/usage.jsonl` across sessions
and prints per-tool counts (`n`, `avg_ms`, `err`, `max_ms`), search totals
(queries, zero-result ratio, hybrid/FTS split, avg quality), and index
activity (full vs incremental, avg full-rebuild time). Flags: `--last N`
limits to the most recent N sessions, `--json` emits structured output.

Motivation: the metrics module has been writing JSONL for months (1MB
rotation), but there was no reader. Running on this repo's own history
surfaced the `rebuild_index` error pattern that motivates change #2.

### `rebuild_index` MCP tool — busy signal is no longer an error

When the server rejects a rebuild request because background embedding is
still running, it now returns `Ok({status: "busy", retry_after_ms: 2000})`
instead of `Err("Background embedding still in progress")`. This matches
the precedent in `run_incremental_with_cache_restore` (which returns
`Ok(())` on the same condition) and keeps the usage-metrics `err` counter
from inflating on legitimate retry signals.

**Contract change** — SDK/script clients of the `rebuild_index` MCP tool
must now distinguish `status: "busy"` success payloads from actual errors.
JSON-RPC-level errors on `rebuild_index` now indicate real failures only
(missing `confirm`, no project root, DB error).

### CLI ↔ MCP search disambiguation

`plugin_code_graph_mcp.md` template previously listed `search "Z"` and
`semantic_code_search` as equivalent intents. They are not: the CLI
`search` command is **FTS5-only** (`src/cli.rs:710` → `fts5_search`), while
the MCP `semantic_code_search` tool performs **RRF fusion** of FTS5 + vector
similarity (`src/mcp/server/tools.rs:42 → 101`). The template now states
this explicitly in the core-7 decision table and the CLI cheat sheet.

Adopted memory files auto-refresh from the template on the next
SessionStart (v0.11.0+ behavior).

### Clippy 1.95 parity

Four `clippy::manual_checked_ops` and one `clippy::unnecessary_sort_by`
flagged by the 1.95 toolchain in the new `cmd_stats` code path are fixed
before push (local baseline: `cargo +1.95.0 clippy --no-default-features
-- -D warnings && cargo +1.95.0 clippy --all-targets -- -D warnings`,
both green).

## v0.12.1 — incremental-index skips non-project directories

Bugfix release: the PostToolUse `incremental-index` hook no longer creates
`.code-graph/` in directories that are not project roots. In multi-repo
workspace layouts (one parent dir containing N independent git repos, parent
not itself a repo), the hook previously materialized a stray 16 MB+ index at
the workspace parent, overlapping every child repo.

### What changes

`src/main.rs` incremental-index arm now bails silently when the resolved
project root has neither a `.git` anchor nor an existing
`.code-graph/index.db` (the index check preserves the explicit per-dir index
case where a user deliberately ran `incremental-index` in a non-git folder).

Silent-skip matches the prevailing hook-layer convention:
`incremental-index.js` swallows errors, `CliContext::try_open` returns `None`,
`session-init.js` returns `'skipped'`.

### Test coverage

`claude-plugin/scripts/incremental-index.test.js` — two cases:
- non-git tmpdir → exit 0, `.code-graph/` not created
- fake `.git/` tmpdir → exit 0, guard does not block

### Credits

Reported + fixed by @jgangemi (issue #8, PR #9). Re-landed on top of current
`resolve_project_root_from` helper with doc-comment scope creep removed.

## v0.12.0 — Scenario-keyed MEMORY.md index (auto-adopt template refresh)

Auto-adopt (`claude-plugin/scripts/adopt.js`) now seeds MEMORY.md's sentinel
block with a 5-row scenario→tool table in addition to the existing tool-name
list. The always-loaded context gap this closes: Claude Code knew the 7+5 tool
names but not the natural-language triggers ("who calls X?", "改 X 影响面")
that should route to them, so sessions silently slid to `Grep` / `Read` when a
code-graph tool would be more precise. The scenario phrases now live in the
200-line-capped MEMORY.md itself, not a second-hop `plugin_code_graph_mcp.md`.

### What changes

Sentinel `<!-- code-graph-mcp:begin v1 -->...<!-- code-graph-mcp:end -->` grows
from 3 lines to 9. Added block (nested under the existing index entry):

    - 场景速查（优先于 Grep）：
      - 改 X 影响面 → `get_ast_node symbol=X include_impact=true`（或 CLI `code-graph-mcp impact X`）
      - 谁调用 X / X 被谁用 → `get_call_graph X` 或 `find_references X`
      - 看 X 源码 / 签名 → `get_ast_node symbol=X`
      - Y 模块长啥样 → `module_overview` 或 CLI `code-graph-mcp overview Y/`
      - 概念查询（不知精确名）→ `semantic_code_search "Z"`；字面匹配用 Grep

### Migration — existing adopted projects

`needsRefresh()` detects INDEX_LINE drift automatically; the sentinel block
rewrites once on next SessionStart. No user action required.

### Opt-out

- Lock current MEMORY.md block against this refresh: `CODE_GRAPH_NO_TEMPLATE_REFRESH=1` (shipped in v0.11.0)
- Disable auto-adopt entirely for new projects: `CODE_GRAPH_NO_AUTO_ADOPT=1` (shipped in v0.9.0)
- Downgrade: reinstall `0.11.6` to restore the 3-line INDEX_LINE

### Verification

- `adopt.test.js`: 37/37 green — tests reference the `INDEX_LINE` constant, so the content extension is transparent.
- `routing_bench`: 19/20 = 95.0% on `anthropic/claude-sonnet-4.5` via OpenRouter — unchanged from v0.11.6. This release doesn't touch `ToolRegistry` descriptions, which is what the bench measures; the adopted MEMORY.md lives outside the oracle's prompt.

## v0.11.6 — Tool-description tightening (+5% routing P@1) + OpenRouter backend

First run of the routing-recall benchmark landed v0.11.4 at **P@1 = 18/20 = 90.0%**
(`anthropic/claude-sonnet-4.5` via OpenRouter). The two misses were both semantic
overlaps between adjacent tools. This release tightens 4 tool descriptions and
re-runs the bench: **P@1 = 19/20 = 95.0%**, a net +5.0 points with one miss
remaining (borderline — "show me the EmbeddingModel struct" routes to `ast_search`
with `type=struct`, which returns the right answer albeit via the "enumerate"
tool rather than the "inspect ONE" tool).

### Tool-description changes (`src/mcp/tools.rs`)

All stay under the 200-char registry limit.

- **`get_call_graph`** — leads with `"Who calls X, what X calls"` + `"Returns a
  graph (not a flat list)"`. Fixed routing for "Who calls ensure_indexed?"
  (was → `find_references`, now → `get_call_graph`).
- **`find_references`** — leads with `"Flat enumeration of all usage sites"` +
  explicit deflection: `"For 'who calls X?', use get_call_graph."`.
- **`get_ast_node`** — leads with `"Inspect ONE named symbol"` + `"you have a
  symbol name (or node_id) and want its definition/body"` to claim the
  "show me X / signature of Y" intent.
- **`ast_search`** — leads with `"Enumerate MULTIPLE symbols by structural
  criteria"` + deflection: `"For ONE known symbol, use get_ast_node."`.

Pattern: each description now leads with a shape verb (`who calls`, `flat
enumeration`, `inspect ONE`, `enumerate MULTIPLE`) and points at the
adjacent tool when a query drifts into overlap.

### Routing-bench OpenRouter backend (`tests/routing_bench.rs`)

Auto-detects `ANTHROPIC_API_KEY` (native Messages API) or `OPENROUTER_API_KEY`
(OpenAI-compatible `/chat/completions`). Tool schemas re-packaged as
`{type: "function", function: {...}}` for the OpenRouter path. Model default
`anthropic/claude-sonnet-4.5`; override with `ROUTING_BENCH_MODEL`. Anthropic
wins if both keys present.

### Baseline measurement (published)

| Run | Backend / Model | P@1 |
|-----|-----------------|-----|
| v0.11.4 baseline | openrouter / anthropic/claude-sonnet-4.5 | 18/20 (90.0%) |
| v0.11.6 post-tightening | openrouter / anthropic/claude-sonnet-4.5 | 19/20 (95.0%) |

Cost ≈ $0.10/run. Threshold stays at 0.70; consider raising to 0.85 after two
more releases confirm 95% as stable baseline (20-query sample is within model
stochasticity range).

## v0.11.5 — Hotfix: clippy 1.95 parity (`unnecessary_sort_by`)

`-D warnings` on stable clippy 1.95 flagged the two `sort_by(|a, b| b.0.cmp(&a.0))`
calls added in v0.11.4 rollup. Local clippy (0.1.91, ~4 months behind stable)
accepted them. Functional behavior unchanged.

### Fix

- `src/mcp/server/tools.rs:503-504`: `sort_by(|a, b| b.0.cmp(&a.0))` →
  `sort_by_key(|e| std::cmp::Reverse(e.0))` (applied exactly as clippy suggested).

### Why v0.11.4 shipped red

Local pre-push ran `cargo clippy --all-targets -- -D warnings` — passed on 0.1.91.
CI uses `dtolnay/rust-toolchain@stable` which pulls whatever's latest
(1.95.0 at ship time), catching `clippy::unnecessary_sort_by` which landed post-0.1.91.
Functional code from v0.11.4 is unaffected; only the `-D warnings` gate broke.
v0.11.4 tag + release left pointing at the failing commit as a historical artifact.

## v0.11.4 — Integration-friction fixes: ast_search hint + acronym expansion + call graph rollup

Integration-test pass against Claude Code found three specific friction points
where tool responses forced a second round-trip or missed relevant nodes.
All three fixed. Additive — no schema change, no re-index.

### Fixes

1. **`ast_search` generic-fallback hint.** When `returns="Vec<Relation>"` yields
   zero hits because the codebase uses `Vec<ParsedRelation>`, the response now
   carries `hint` + `suggested_query` instead of a bare `count: 0`. Example:
   `{ "count": 0, "hint": "No match for returns='Vec<Relation>'. Substring
   'Relation' has 7 matches — try that.", "suggested_query": {"returns":
   "Relation", "type": "fn"} }`. Strip rule: innermost `<…>` wins; multi-param
   types take the last comma-separated param. See
   `src/mcp/server/helpers.rs::strip_outer_generic`.

2. **Acronym query expansion.** `fts5_search` preprocessing now expands
   common CS/IR/DB acronyms into full-form terms alongside the original:
   `RRF` → `RRF` + `reciprocal` + `rank` + `fusion`; same for `BM25`, `FTS`,
   `AST`, `LSP`, `MCP`, `RPC`, `SQL`, `ORM`, `CTE`, `JWT`, `TTL`, `DAG`,
   `RBAC`, `CRUD`, `CORS`. Benchmark before/after on query `"RRF fusion BM25"`:
   `weighted_rrf_fusion` now appears at rank 3 (previously absent from top-5).
   New static dict in `src/search/acronyms.rs`; expansions deduped via the
   existing BTreeSet pass.

3. **`semantic_code_search` acronym-heavy FTS bias.** Queries that are entirely
   short uppercase tokens (≤3 tokens, each ≤5 chars, all `[A-Z0-9]`) now run
   with `fts_weight=2.0, vec_weight=0.8` instead of the default `1.0/1.2`.
   Rationale: embeddings handle letter-exact acronyms poorly while FTS5's
   token-exact match is reliable; shift the weight toward the precise channel.

4. **`get_call_graph` file-level rollup replaces `compressed_call_graph`.**
   When the flat node list exceeds `COMPRESSION_TOKEN_THRESHOLD` (previously
   this mode dumped the raw list anyway), group by `(file_path, direction)`
   and emit `{file, count, names[], node_ids[], min_depth, max_depth}` sorted
   by count desc. New mode string `"rollup_call_graph"`. Measured on
   `ensure_indexed` (86 nodes): previously 86 flat entries → now 2 caller
   rollups + 5 callee rollups, preserving `node_ids` for `get_ast_node`
   drill-down. Contract Δ: consumers matching on
   `mode == "compressed_call_graph"` must update to `"rollup_call_graph"`.

### Tests

- `strip_outer_generic` unit tests (4/4) cover `Vec<T>`, nested generics,
  multi-param (`Result<T, E>`), and no-bracket cases.
- `acronyms::expand_acronym` unit tests (4/4) cover case-insensitivity,
  unknown tokens, `BM25` numeric acronym, and an FTS-length-filter guardrail.
- 230 lib tests + 44 integration tests all green.

### Internal

New module `src/search/acronyms.rs`. `strip_outer_generic` in
`src/mcp/server/helpers.rs`. All other edits localized to `tool_ast_search`,
`tool_semantic_search`, and `format_call_graph_response` in
`src/mcp/server/tools.rs`, plus one flat_map augmentation in
`storage::queries::fts5_search_impl`.

### Routing-recall benchmark (new)

`tests/routing_bench.rs` — turns "does Claude Code naturally call our tools
for the right intents?" from vibe-check into a P@1 number. 20 oracle queries
(3 per tool for 6 tools + 2 for `find_references`), each sent to the Claude
API with the live 7-tool schemas from `ToolRegistry`; asserts the picked
tool matches the oracle expectation.

- `oracle_well_formed` runs in default `cargo test` and verifies every
  oracle entry references a real tool *and* every registered tool has at
  least one oracle query — catches drift when tools are renamed/added.
- `routing_recall_benchmark` is `#[ignore]` (requires `ANTHROPIC_API_KEY`).
  Run locally: `ANTHROPIC_API_KEY=sk-... cargo test --test routing_bench -- --ignored --nocapture`.
  Cost ≈ $0.10/run with `claude-sonnet-4-6` (20 queries × ~1.2K in + ~150 out).
  Threshold starts at P@1 ≥ 0.70; tighten as descriptions improve.
- New dev-dep `reqwest` (blocking + rustls-tls, no TLS-OpenSSL pulled in).
- CI wiring deliberately not added yet — run manually or add a gated step
  (`env: ANTHROPIC_API_KEY: ${{ secrets.ANTHROPIC_API_KEY }}`) when ready.

## v0.11.3 — Doc: "hidden but callable" clarified (Claude Code vs. raw MCP)

User-facing: no behavior change; corrects a misleading claim in the adopted
plugin memory after a 12-tool UX audit.

### Context

v0.10.0 trimmed `tools/list` to 7 core tools and documented the other 5
(`impact_analysis`, `trace_http_chain`, `dependency_graph`, `find_similar_code`,
`find_dead_code`) as "hidden but callable by name". UX audit found this holds
only for clients that invoke `tools/call` with a literal tool name (raw JSON-RPC,
MCP SDKs, CLI). **Claude Code's MCP integration derives its callable set from
`tools/list`** — `ToolSearch` returns `No matching deferred tools found` for the
hidden 5, and direct invocation errors with `No such tool available`.

### Fixes

1. **`claude-plugin/templates/plugin_code_graph_mcp.md` "进阶 5" table
   reworded**: CLI is now the primary column for Claude Code users; raw MCP
   name calls annotated as SDK/scripts-only. v0.11.0 template auto-refresh
   pushes this to previously-adopted projects on next SessionStart.
2. **`src/mcp/tools.rs` doc comment**: spells out which MCP clients can reach
   hidden tools and points to CLI fallback for Claude Code.

### Why this matters

Misleading docs caused agents to attempt `mcp__…__impact_analysis` /
`mcp__…__trace_http_chain` and hit a terminal "No such tool available" error
instead of routing to `code-graph-mcp impact|trace|deps|similar|dead-code`
via Bash.

## v0.11.2 — Post-audit follow-up: 4 residual precision fixes

Follow-up audit on top of v0.11.1. All additive/tightening — no schema breakage.

### Fixes

1. **`module_overview` no longer leaks inline `#[cfg(test)]` test fns.**
   Name-heuristic `is_test_symbol` couldn't catch `#[cfg(test)] mod tests { #[test] fn anything_goes }`
   whose names don't prefix `test_`. Root fix: `get_module_exports` SQL now
   `WHERE n.is_test = 0` on both the explicit-exports (JS/TS) path and the
   fallback (Rust / Go / Python) path — AST-level flag propagates through.

2. **Disambiguation suggestions carry `node_id` + `start_line`.**
   `resolve_fuzzy_name` and `disambiguate_symbol` suggestions now include
   both fields so callers can pick a specific definition when multiple
   same-name functions live in one file (e.g. two `fn new()` in different
   `impl` blocks of the same module). `disambiguate_symbol` also fires on
   same-file multi-def, not just cross-file collisions.

3. **`find_references` gains `node_id` parameter.** Lets callers pass the
   `node_id` from a suggestion directly, skipping the ambiguous name-lookup
   step. When a name is ambiguous within one file, the tool now returns
   a per-definition suggestion list (with `start_line`) instead of silently
   merging refs across defs.

4. **`find_dead_code` gets `ignore_paths` (MCP) / `--ignore` (CLI).**
   Shell-invoked plugin entry points (lifecycle/hook scripts in
   `claude-plugin/`) are not in the static AST call graph, so they surfaced
   as false-positive orphans. Added prefix-match exclusions with a sensible
   default (`["claude-plugin/"]`). Pass `ignore_paths: []` or
   `--no-ignore` to see the full list. Response carries `ignored_count`,
   `ignore_paths_applied`, `ignore_paths_defaulted` for transparency.

### Docs

- `plugin_code_graph_mcp.md`: hidden-5 tools now have an explicit
  required/optional parameter table (notably `trace_http_chain` takes
  `route_path`, not `route`) — users calling by name no longer need to
  trigger the error message to discover arg names.

### Tests

+4 new (+1 unit in `queries.rs`, +3 integration covering Bug #1 / Issue #3 /
Bug #2). Full suite: **347 passed / 0 failed** default features,
**340 passed / 0 failed** `--no-default-features`; clippy
`-D warnings` clean under both feature configs.

## v0.11.1 — 12-tool accuracy audit: 1 critical bugfix + 5 precision improvements

Post-audit fixes for tool output correctness. All changes are additive/tightening —
no consumer schema breakage.

### Fixes

1. **`find_references` — critical bugfix for exact-name resolution.**
   `resolve_fuzzy_name` was matching substrings before exact names, so
   `find_references("handle_tool")` falsely reported ambiguity with
   `handle_tools_list` / `handle_tools_call`. Now exact-name matches win first;
   same-name-in-multiple-files still produces `Ambiguous` but scoped to exact
   matches only. Same fix benefits `impact_analysis` and `get_call_graph`
   fuzzy-fallback paths.

2. **Centralized truncation keeps arrays homogeneous.** The
   `centralized_compress` pipeline used to splice a string sentinel
   (`"... [N items truncated]"`) into the middle of object arrays, breaking
   type consistency for strict JSON consumers and hiding how much was dropped.
   Arrays now truncate silently to `first-10 + last-5` (15 homogeneous items),
   and a new `_array_truncations: {<field>: {original, kept}}` sibling records
   the true pre-truncation length so callers can reconcile `count`/`total`
   siblings against what was actually returned.

3. **`project_map` schema sharpened.**
   - `hot_functions` SQL tightened to `n.type IN ('function','method')` so
     structs/classes no longer leak into the "hot functions" bucket.
   - `entry_points[].kind` added: `"main"` for program entry points, `"http_route"`
     for framework-registered handlers. Lets LLMs skip `main` when scanning the
     HTTP surface without sniffing the `route` string.

4. **`dependency_graph` filters the `<external>` sentinel.** The synthetic
   bucket for unresolved imports now no longer surfaces as a fake file dependency.

5. **`find_similar_code` reports cutoff-driven shortfalls.** When
   `max_distance` drops candidates below `top_k`, the response now carries
   `cutoff_applied: true`, `cutoff_dropped: N`, and a `hint` suggesting the
   user widen `max_distance`. Also echoes `top_k` and `max_distance` in every
   response for transparency.

6. **`impact_analysis` on types returns `risk_level: "UNKNOWN"`.** When the
   target is a struct/class/enum/interface/type_alias and the call graph finds
   zero callers, the risk level is now `UNKNOWN` instead of `LOW` — so LLMs
   don't mistake "call graph can't see type usage" for "no one uses this".
   The existing type_warning still explains why and points to
   `semantic_code_search` for broader coverage.

### Test coverage

- +2 unit tests in `src/mcp/server/helpers.rs` (truncation homogeneity,
  no-op when arrays < 20).
- +6 integration tests in `tests/integration.rs` covering each fix above.
- Full suite: lib 221 + integration 41 + cli_e2e 50 + parser 19 + plugin 6 +
  hardening 6 = 343 passed, clippy clean.

## v0.11.0 — auto-refresh stale decision table on plugin upgrade

### Migration note

v0.10.0 shipped the 7-core/5-hidden tool surface in the Rust binary **but left the adopted `plugin_code_graph_mcp.md` decision table file — and the `MEMORY.md` sentinel block — stuck at the v0.8.x/v0.9.x 12-tool content** for any project that had already auto-adopted. The plugin's `maybeAutoAdopt()` short-circuited on `isAdopted() == true` and never refreshed the template. Two related holes were also fixed:

1. The shipped source template (`claude-plugin/templates/plugin_code_graph_mcp.md`) was not updated in v0.10.0 — **new** `/plugin install` + first-adopt users were also getting the stale 12-tool table.
2. The `INDEX_LINE` constant in `adopt.js` (which drives the `MEMORY.md` sentinel block) was likewise still the v0.8.x 12-tool line.

### What changes on upgrade

- **Source template synced** to match the 7-core / 5-hidden surface. Fresh `/plugin install` gets the correct decision table on first adopt.
- **`INDEX_LINE` synced** to the v0.10.0 wording.
- **Auto-refresh on drift**: when a project is already adopted but the shipped template hash ≠ the project's copy (or the `MEMORY.md` sentinel block's content ≠ current `INDEX_LINE`), the next plugin SessionStart refreshes both silently. One-time stderr notice: `[code-graph] Refreshed decision table to latest shipped version.`
- Hand-edited decision tables are overwritten by default. To lock: `CODE_GRAPH_NO_TEMPLATE_REFRESH=1` in `~/.claude/settings.json` env.

### Opt-out

- `CODE_GRAPH_NO_TEMPLATE_REFRESH=1` — preserves your local edits of `plugin_code_graph_mcp.md`; also pins `MEMORY.md` sentinel to whatever it was. Does not affect first-adopt (only the refresh path).
- `CODE_GRAPH_NO_AUTO_ADOPT=1` — still gates the first-adopt path as in v0.9.0.
- `code-graph-mcp unadopt` — unchanged; strips sentinel + deletes target file.

### Why this matters

Without this fix, an already-adopted v0.8.x/v0.9.x user who upgrades to v0.10.x gets mixed state: the Rust binary serves 7 tools in `tools/list` but the MEMORY.md index + decision-table file still instruct the LLM to route through the full 12-tool surface as if they were peers. Functionally nothing breaks (hidden tools remain callable by name), but the decision guidance is misaligned. v0.11.0 closes the loop so the three surfaces — binary, index pointer, decision table — all move together on upgrade.

## v0.10.0 — tools/list surface trimmed to 7 core tools

### Migration note

MCP `tools/list` now advertises 7 tools instead of 12. The 5 hidden tools remain fully callable by name (aliases preserved) — only their visibility to the LLM at session start is removed, to shrink tools/list payload (~40% reduction) and cut decision fatigue in daily coding flows.

**Core 7 (exposed in tools/list)**:
`semantic_code_search`, `get_call_graph`, `get_ast_node`, `module_overview`, `project_map`, `find_references`, `ast_search`.

**Hidden but callable by name / CLI (backward-compatible aliases)**:
`impact_analysis`, `trace_http_chain`, `dependency_graph`, `find_similar_code`, `find_dead_code`.

**Rationale**: these 5 are niche (cleanup, duplicate detection, HTTP routing, file-level imports, blast-radius pre-check) — high value when needed, low daily frequency. For the primary blast-radius use case, prefer `get_ast_node symbol_name=X include_impact=true` which is in the core 7.

**Reverse / opt-out**: call any hidden tool by name via MCP `tools/call` or the matching `code-graph-mcp <subcommand>` CLI. All handlers, schemas, and CLI paths unchanged — only the tools/list catalog shrunk.

**Memory sync**: projects that auto-adopted v0.9.x will see updated `plugin_code_graph_mcp.md` decision tables on next session.

## v0.9.1 — Rust 1.95 clippy cleanup

CI-only cleanup; no runtime behavior changes, no user-visible differences. Fixes 9 clippy errors surfaced by Rust 1.95.0's stricter lints (pre-existing since ~v0.8.1, was shipping with red CI):

- `collapsible_match` (4): merge `match arm => if cond` into `match arm if cond =>` in `src/parser/relations.rs` C# arms + Python decorator scan.
- `unnecessary_sort_by` (4): `.sort_by(|a,b| b.x.cmp(&a.x))` → `.sort_by_key(|e| Reverse(e.x))` in `src/mcp/server/tools.rs` and `src/storage/queries.rs`.
- `useless_conversion` (1): drop redundant `.into_iter()` in a chained iterator in `src/graph/query.rs`.

Verified with `cargo +1.95.0 clippy -- -D warnings` on both `--no-default-features` and default feature sets.

## v0.9.0 — Context-aware auto-adopt (C')

### Migration note

Plugin-mode installs (`/plugin install` in Claude Code) now **auto-adopt** into the project's `MEMORY.md` on first `SessionStart`. Previously adoption required running the adopt script manually, which most users never discovered — so the tool-invocation contract never got loaded and MCP tools stayed underused.

**What changes on first upgrade (plugin mode)**:

1. `~/.claude/projects/<slug>/memory/plugin_code_graph_mcp.md` is written (tool-decision rules).
2. A sentinel-bracketed pointer line is appended to `MEMORY.md`.
3. `quietHooks` flips to `true` automatically — per-session `project_map` injection (~60 lines) is skipped; tools are loaded on-demand instead.
4. A single stderr notice fires on the first adoption showing how to opt out or reverse.

**Opt-outs** (in `~/.claude/settings.json` → `env`):

- `CODE_GRAPH_NO_AUTO_ADOPT=1` — prevents future auto-adoption; does not affect already-adopted projects.
- `CODE_GRAPH_QUIET_HOOKS=0` — forces `project_map` injection back on, even if adopted.
- `CODE_GRAPH_QUIET_HOOKS=1` — forces silent mode, even if not adopted.

**Reverse adoption**: `code-graph-mcp unadopt` (now a real CLI subcommand — see below).

**What does NOT auto-adopt**:

- npm global installs (`npm install -g @sdsrs/code-graph`)
- `npx ./tarball.tgz` invocations
- Bare dev checkouts / test fixtures
- CI / agent short-session contexts

Detection uses the script's `__dirname` (checks for `~/.claude/plugins/` prefix), not `CLAUDE_PLUGIN_ROOT` — the env var leaks across concurrent plugins.

### New

- **`code-graph-mcp adopt` / `unadopt` CLI subcommands**: previously only callable via `node claude-plugin/scripts/adopt.js`. Now uniform across plugin / npm / npx installs via `bin/cli.js` interception.
- **`CODE_GRAPH_NO_AUTO_ADOPT=1`**: explicit opt-out env for auto-adopt.

### CLI polish

- **`code-graph-mcp show <file-path>` nudge**: when the positional argument is an existing code file on disk, emit a clear pointer to `overview <file>` instead of silently returning no rows. `show` is for symbols; `overview` is for files.
- **`code-graph-mcp deps` barrel fallback**: files with no tracked dependency edges (Rust `mod.rs`, `index.ts` barrels, Python `__init__.py`) now scan source for language-appropriate re-export / import lines and surface them — previously a hard error.
- **Impact / references filter `<external>` placeholders**: stub nodes synthesized for unresolved external symbols no longer surface in `impact_analysis` / `find_references` results.

### Breaking (semantic default change)

The default meaning of "plugin installed but not adopted" changed from *"inject project_map every session, user must find /adopt to opt into the contract"* to *"adopted implicitly from the install action, quiet by default"*. Hence the minor bump. Users who preferred the v0.8.x noisy default can pin it with `CODE_GRAPH_QUIET_HOOKS=0`.

---

## v0.8.4 — `.code-graph` pollution + test leak cleanup

See [release notes](https://github.com/sdsrs/code-graph-mcp/releases/tag/v0.8.4).

## Older releases

See [GitHub Releases](https://github.com/sdsrs/code-graph-mcp/releases).
