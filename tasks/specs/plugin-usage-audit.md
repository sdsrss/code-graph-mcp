---
status: implemented
revision: 1
---

# Plugin usage audit from real session transcripts

## Goal

A repeatable measurement of how code-graph is used in real Claude Code
sessions, so a hook or steering change is judged on its effect on usage as
well as on UX. 0.154.0 (deny → rewrite) and 0.164.0 (hook de-noise) each cut
explicit use without anyone seeing it: the coding eval had 0/15 code-graph
calls in both arms, so it could not register a drop.

`scripts/plugin_usage.py` reads the transcripts Claude Code keeps under
`<config dir>/projects/` and reports, per period (and per project on request),
per 1,000 tool calls:

- explicit code-graph calls (Bash command word `code-graph-mcp`, MCP tool,
  code-graph subagent or skill) — classified by `subagent_share.classify_bash`;
- hook injections by hook (`[code-graph…` context), and the same for
  claude-mem-lite (`[mem…` / `[claude-mem-lite…`) as a reference arm;
- interceptions: PreToolUse denies (an error tool result carrying the
  `[code-graph` reason) and rewrites (`updatedInput` from a code-graph hook);
- pointer follow-through: an injection that names a `code-graph-mcp <cmd>`
  command, and whether a code-graph call follows within the next N tool calls.

## Non-goals

- No value claim: call counts are not outcomes. Outcome evals stay in
  `evals/` and `evals/steering/`.
- No change to the plugin or to what it records.

## Constraints

- Read-only; fixed-depth globs (`*/*.jsonl`, `*/*/subagents/agent-*.jsonl`),
  never a recursive walk of the config dir.
- Interactive sessions only by default (`entrypoint == "cli"`): `claude -p`
  eval runs would otherwise dominate.
- A hook's output is counted once: JSON stdout produces both a
  `hook_success` and a `hook_additional_context` attachment; plain-text
  stdout (UserPromptSubmit) produces only `hook_success`.

## Success criteria

- `python3 -m unittest scripts/test_plugin_usage.py` green, with synthetic
  transcripts covering each counter, the single-count rule, the period
  split, project exclusion, the entrypoint filter, the follow-through window
  and the classifier's false-positive shapes (a repo path or commit message
  naming a plugin).
- Run on this machine's transcripts, it reproduces the shape of the
  2026-10-16 manual audit (neutral projects, main sessions): explicit
  code-graph use ≥20 per 1k tool calls before 2026-09-25T11:00 and ≤2 per 1k
  from 2026-09-30.

## Open questions

None.

# Change log

- r1 (2026-10-16): initial.
