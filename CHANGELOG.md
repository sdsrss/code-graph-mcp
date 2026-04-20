# Changelog

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
