# Changelog

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
