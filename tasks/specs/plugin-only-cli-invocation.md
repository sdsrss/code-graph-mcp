---
status: draft
revision: 1
---

# Plugin-only users get a steering block they cannot execute (issue #41)

## Goal

Every command string this plugin puts in front of a user or an agent must be
runnable in the environment it is shown in. Today three surfaces hand a
plugin-only install the bare name `code-graph-mcp`, which for that install is
never on PATH.

## Evidence (verified at `c43a7a3`, v0.140.0)

- Issue #41 (opened 2026-08-27, `comments: 0`): "I use `code-graph-mcp`
  exclusively through claude plugin. The CLI is not installed or available in my
  environment." — the injected block promotes `code-graph-mcp callgraph <symbol>`
  and siblings.
- `claude-plugin/scripts/find-binary.js:456-510` `findBinaryUncached()` resolves,
  for a plugin-only install, to `~/.cache/code-graph/bin/code-graph-mcp`
  (`auto-update.js:39` `BINARY_CACHE_DIR = <cache>/bin`, `:496-499`
  `cachedBinaryPath()`). Nothing in the plugin path puts that name on PATH; the
  global-npm tiers below it exist only for users who ran `npm i -g` themselves.
- `claude-plugin/scripts/adopt.js:41-46` `buildTriggerRows()` emits six bare
  `code-graph-mcp …` rows; `:115` states `Fastest path = Bash CLI:`
  unconditionally. `buildBlock()` never consults binary reachability.
- `claude-plugin/scripts/session-init.js:806` the adoption announcement offers
  `Reverse:    code-graph-mcp unadopt`; `:824` the unrecorded-registry note
  offers `code-graph-mcp unadopt` again. **The escape hatch is as unrunnable as
  the feature.**
- `claude-plugin/templates/plugin_code_graph_mcp.md:47-52` repeats the CLI-first
  claim with bare names in the generated detail doc.

## Non-goals

- **Do not retreat from CLI-first.** The template's own record (`v0.49`) states
  the ranking is measured: MCP tools are deferred behind ToolSearch in Claude
  Code, Bash is always live, and every observed conversion on 2026-06-12 was a
  CLI call. The defect is the invocation string, not the routing.
- **Do not put a machine-resolved absolute path into the CLAUDE.md block or the
  detail doc.** Both are project files; `adopt.js:864` tells the user CLAUDE.md
  is git-tracked and to commit it. A `/home/<user>/.cache/…` path is wrong for
  every teammate, and `buildBlock()`'s byte-determinism (`:104-105`) is what
  `needsRefresh()` uses to detect drift — machine-varying content would make
  every session rewrite the block and churn the repo.
- ~~Not touching `formatResult()` (`adopt.js:866`): that text is only reached by
  someone who just ran `code-graph-mcp adopt`, so the bare name is correct
  there.~~ **Withdrawn 2026-09-08 — the premise was false.** A plugin-only user
  cannot reach `formatResult` through `code-graph-mcp adopt` at all: the cached
  binary has no `adopt.js` beside it to re-exec, so that command exits 1. The
  population that DOES reach this printer is the one running
  `node <plugin>/claude-plugin/scripts/adopt.js` — exactly the invocation
  SessionStart now hands them — and for them the bare name in the `Reverse:`
  line is the unrunnable spelling this whole spec exists to remove. Fixed in
  `2446874`; the two printers now share one `unadoptCommand()` in adopt.js.
- Not touching `doctor.js` / `README.md` `npm install -g` guidance: those are
  install instructions, not invocations of an assumed-present binary.

## Constraints

- The block line must be byte-identical across machines, users and platforms
  (see non-goal 2). It therefore names a **path shape**, not a resolved path.
- The block is always-loaded context in every session of every adopted project;
  the addition is capped at one line.
- The stderr announcement is ephemeral and per-machine, so it MAY name the
  resolved path — `runSessionInit()` already holds it as `binaryCheck.binary`
  (`session-init.js:747`), so no new probe and no new require.
- Changing the block's bytes makes every adopted project refresh on its next
  SessionStart. That is the designed drift-refresh path
  (`maybeAutoAdopt → needsRefresh → adopt`), and it must stay a single
  idempotent rewrite, not a loop.
- L3 by core §2 (LLM-visible metadata: adoption-memory / shipped prompt
  template), regardless of LOC.

## Success criteria

1. A plugin-only user (no `code-graph-mcp` on PATH) can copy a runnable
   invocation out of the CLAUDE.md block without leaving the block.
2. The `unadopt` escape hatch printed at SessionStart is runnable in the same
   environment that printed it.
3. `buildBlock(type)` remains byte-deterministic per project type: two calls on
   different machines produce identical bytes. Guarded by a test.
4. `needsRefresh()` reports true exactly once for an already-adopted project on
   the version that introduces the new line, and false on the run after.
5. The detail doc carries the same fallback fact, once.
6. No new subprocess, no new `require` on the hook path, no measurable addition
   to SessionStart wall-clock.

## Open questions

- None blocking. One judgment recorded: the block names
  `~/.cache/code-graph/bin/code-graph-mcp` rather than teaching `npm i -g`
  first, because the former is already true for the reporter's install while the
  latter asks them to install something they deliberately did not install.

# Change log

- r1 (2026-09-07): initial draft from issue #41 triage at `c43a7a3`.
