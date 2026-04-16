# Changelog

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
