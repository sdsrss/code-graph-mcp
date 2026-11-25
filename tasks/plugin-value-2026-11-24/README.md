# Does the plugin help Claude code? With vs without, on replayed daagu commits (2026-11-24)

Report (Chinese, local only, docs/ is gitignored): `docs/plugin-value-verdict-2026-11-24.md`.
Task prompts, commit ids, results, traces, diffs and replies: `private/` (git-excluded via
`.git/info/exclude`; this repo is public and daagu is not).

## Design

- Tasks: three daagu commits where a changed function's callers in other files must change too,
  each with tests in the same commit. `pick_commits.py` finds such commits (21 of 2,906).
  A: a reader method becomes async, two callers in other files must `await` it (one of them, a
  script, is reached by no test, so `run_coding.py` checks it statically). B: a scheduler method
  returns success, the worker in another file sets its exit code from it. C: a service method gains
  a flag, the API endpoints pass it and 404 on None.
- Base = the commit's parent (`setup.sh`: `git archive`, `git init`, plus a `--no-embed` index for
  the plugin arm, ~3 s each). Prompt = the user-style request in Chinese, interface named, callers not.
- Grading (`grade.py`): overlay the commit's test files (hidden during the session), run them and
  the full backend suite in the `daagu-backend:dev` image. Correct = hidden tests all pass, no
  full-suite failure outside the reference solution's own, static check clean. Reference check:
  hidden tests fail on the parent (16 / 6 / 6) and pass on the commit (99 / 82 / 35).
- Arms (`run_coding.py`): Opus 5.5, `claude -p`, isolated HOME and CLAUDE_CONFIG_DIR, tools
  Read/Edit/Write/Grep/Glob/Bash. Both get daagu's local CLAUDE.md minus the claude-mem-lite block;
  the no-plugin arm also loses the code-graph block. Plugin arm: installed 0.205.0 via
  `--plugin-dir`, binary in the session HOME, prebuilt index. The two arms of a (round, task) run
  concurrently. 3 rounds x 3 tasks x 2 arms = 18 sessions, $12.03.

## Result (`python3 analyze.py private`)

| | no plugin | plugin |
|---|---|---|
| correct | 9/9 | 9/9 |
| cost | $5.96 | $6.07 (+1.7%, paired 95% CI -13.1%..+16.5%) |
| turns | 231 | 233 (+0.9%, CI -16.9%..+18.6%) |
| wall time | 2,708 s | 2,570 s (-5.1%, CI -46.9%..+36.7%) |
| code-graph calls | 0 | 4 in 3/9 sessions (3 `affected`, 1 `callgraph`), none decisive |

- Plugin lower in 4/9 pairs on cost and turns (sign test p=1.0). Within-arm spread across rounds
  is wider than the between-arm difference (B/plugin turns 18-33).
- Hook output per plugin session: one stale-block notice (an artifact of a fresh HOME: real
  daagu sessions show it once per template, 0 in the last 15), plus 0-2 content injections, 9 in
  all: pre-edit impact 4 (caller lists correct), Stop "signatures changed" 2, grep AST view 1,
  definition pointer 1. The no-plugin arm found the same callers by grep (A: the script caller 3/3).
- Costs seen: the Stop notice listed only pytest functions (no callers) and made the model spend a
  closing turn on it in 2/3 A sessions; `affected` answered "309 test files" for two changed files
  in C and the model said the fan-out was too broad; first-request input 12,646 vs 8,298 tokens
  (7 MCP tool schemas + instructions + CLAUDE.md block; interactive sessions defer MCP schemas).

## Real-session scans run the same day

- `../../scripts/plugin_usage.py --since 2026-10-23T23:00 --split 2026-11-04T16:53=F --exclude dev/code-graph-mcp --exclude dev/claude-mem-lite`:
  era F neutral main 3 code-graph calls / 8,076 tools (0.4/1k), 3/28 sessions.
- `tool_mix.py 2026-11-04T16:53`: the official LSP tool (pyright, typescript, rust-analyzer plugins
  enabled) was called once in ~43k tool calls on this machine, and that once was this task's probe.
- `../usage-audit-2026-11-24/token_share.py 2026-11-04T16:53`: code-graph-mcp development = 39.9%
  of 9.83B input tokens.

## Harness notes

- Eval sessions have Bash and no filesystem sandbox: models built venvs in the scratchpad root and
  wrote two patch scripts to /tmp. Run a residue check after every batch (`find /tmp -maxdepth 1
  -newer <base>`, the scratchpad root, the source repo's `git status`).
- Models ignored CLAUDE.md's Docker test command in both arms and built host venvs instead.
- results.jsonl from this run counts each hook output twice; `injections.json` is the recount, and
  `count_injections` now dedupes by tool-use id.
