---
status: implemented
revision: 3
---

# Statusline: what code-graph did in this session

## Goal

The user cannot see code-graph working. Its contributions reach the model
silently (a grep rewritten, an edit-time impact line, a post-grep answer, a
subagent's index facts), while claude-mem-lite's show up in replies and
dashboards. Add a per-session count to code-graph's statusline segment:

    code-graph: ✓ 41,230 nodes | 1,204 files | 5 answers · 2 calls

- **answers**: code-graph content delivered to the model by a hook in this
  session: `grep`/`deny` answered, `grep`/`inject` answered, `edit`/`hint`
  answered, `read`/`hint` answered, `subagent`/`subagent_context`,
  `stop`/`stop_check`.
- **calls**: code-graph commands the model itself ran in this session
  (`cli`/`use`, recorded by the Rust CLI; hook-internal runs are already
  excluded by `CODE_GRAPH_INTERNAL=1`).

## Evidence

- `daagu/.code-graph/recommendations.jsonl` (2026-10-16, 4,023 rows): 174
  `cli`/`use`, 53 + 38 answered `grep`/`inject`, 39 `edit`/`hint`, 33
  answered `read`/`hint`, 30 answered `grep`/`deny`, 15 `subagent_context`;
  no row carries a session, so a per-session count is impossible today.
- Claude Code 2.1.301 builds every child's environment with
  `CLAUDE_CODE_SESSION_ID: e.sessionId` beside `CLAUDE_CODE_SESSION_ATTENDED`
  (binary, function building `{CLAUDECODE:"1", CLAUDE_CODE_SESSION_ID, …}`),
  and the Bash tool's child sees the same id as the transcript.

## Non-goals

- MCP tool calls (`usage.jsonl`) are not counted: 0 explicit MCP calls in
  neutral projects across 09-20..10-16 (`scripts/plugin_usage.py`).
- No new file under `.code-graph/` or the temp dir; no change to what the
  funnel (`code-graph-mcp stats`) reports.
- Prompt-hook injections are not recorded today and are not counted.

## Constraints

- Records gain `session`: the first 8 hex digits of a UUID-shaped
  `CLAUDE_CODE_SESSION_ID`; absent otherwise (JS `recordRecommendation`
  and Rust `record_cli_use`, same rule, each pinned by a test).
- The composite passes the stdin `session_id` to the code-graph provider as
  `CODE_GRAPH_STATUSLINE_SESSION`, the way it passes `CODE_GRAPH_STATUSLINE_CWD`.
- The statusline reads `recommendations.jsonl` through
  `isOwnedPath`/`openOwned` (symlink, hard link, FIFO refused), at most its
  last 512 KB (the rotation keep size), filters on the session substring
  before parsing, and adds no visible text when both counts are 0, when the
  session is unknown, or when the read fails.
- `CODE_GRAPH_STATUSLINE_COUNTS=0` turns the segment off.
- The render budget is unchanged (the health check keeps its 1.5 s).

## Success criteria

- RED-first tests: recommendation-log stamps / omits `session` for a UUID,
  a non-UUID and an unset variable; Rust `record_cli_use` the same; the
  statusline counts each listed kind, ignores another session's rows and
  every non-listed row, renders nothing new at 0, refuses a symlinked log,
  honors the off switch; the composite forwards a valid session id and drops
  an invalid one.
- Full JS suite, `cargo test` for the touched Rust test module, and the
  hardening/doc guards green.
- Replay on a copy of `daagu`'s real log with synthetic session stamps: the
  counts match a Python count of the same rows.

## Open questions

None.

## Plan

1. `recommendation-log.js`: `sessionTag(env)`; stamp in `recordRecommendation`.
2. `src/cli/usage.rs`: same tag in `record_cli_use`; test in `src/cli/tests.rs`.
3. `statusline-composite.js`: forward the session; `statusline.js`:
   `sessionCounts(root, tag)` + render suffix + off switch.
4. README statusline section + CHANGELOG; fresh-subagent review with the
   owned-path shapes (symlink, hard link, FIFO, dangling, loop) in the brief.

# Change log

- r1 (2026-10-16): initial.
- r2 (2026-10-16): review round 1 (0 HIGH, 1 MEDIUM, 7 LOW). M1: the Rust
  stamp test read the env, so CI (no session id) stayed green with the stamp
  deleted; `record_cli_use_in_session` now takes the id, tested for UUID /
  non-UUID / none. New tests: a symlinked `.code-graph`, the tag inside another
  session's row, a FIFO test that times out instead of hanging (each pinned by
  a mutation). Docs: calls = query subcommands; prompt/SessionStart injections
  are not counted; the 512 KB window can lower the count. Left: counts also on
  an unhealthy (✗) report; a frame read during rotation shows none; a rewrite
  the user rejects at the permission prompt still counts (unverified).
- r3 (2026-10-16): D#352 first item. `record_cli_use_with_env` takes the env
  as a lookup, so tests pin both names it reads (`CLAUDE_CODE_SESSION_ID`,
  `CODE_GRAPH_INTERNAL`; the latter had no test at all); each misspelling
  mutation turns its test red. D#351 in the same function: a symlinked
  `.code-graph` is refused (`reject_symlinked_dir`), RED first (the row
  landed in the link's target). Measured with a copy of the release build
  in a Claude Code Bash child: `callgraph --help` and a failed query each
  record a call, stamped with this session's tag; CHANGELOG says so.
  Tests: a hard-linked log is not read (red with openOwned's `nlink` check
  removed); the FIFO test's timeout is 2.5 s, under the composite's 3 s kill.
  O_NONBLOCK alone stays unpinned: the file's lstat check refuses a FIFO
  first, so only the lstat-to-open race reaches it.
