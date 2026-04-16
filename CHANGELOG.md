# Changelog

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
