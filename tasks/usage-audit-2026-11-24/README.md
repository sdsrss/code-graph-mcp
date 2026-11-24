# Usage, outcome and cost audit, 2026-11-24

Report: `docs/plugin-usage-audit-2026-11-24.md` (docs/ is gitignored, local only).
Era F = from 0.185.0 (2026-11-04T16:53Z) to 2026-11-24; era E = 0.178.0–0.184.0
(only its second half is still on disk: transcripts are kept 30 days).

All scans read `~/.claude/projects` with fixed-depth globs and write nothing but
their output file. Outputs of this run were kept in the session scratchpad.

| Script | What it answers |
|---|---|
| `../../scripts/plugin_usage.py --since 2026-10-23T23:00 --split 2026-11-04T16:53=F --exclude dev/code-graph-mcp --exclude dev/claude-mem-lite --detail` | calls / injections / coverage per 1k tool calls, E vs F |
| `deep_usage.py --out FILE` | 11-03 deep scan with era F added (hook latency, result sizes, navigation runs) |
| `cg_outcomes.py` | outcome of every explicit code-graph call (ok / no-index / stale notice) |
| `sig_edits.py` | signature edits: searched before? hook injected? (cross-file is by NAME, so a common name like `measure` in one test file counts) |
| `ceiling.py` | savings ceiling for era F (decl grep then read; ref grep then 2+ reads) |
| `injections.py OUT.json` | every code-graph hook output since F, with the call it ran on and the next 10 |
| `inject_value.py OUT.json` | per injection kind: follow-through (a later call touches a file it names), duplicates, rewrite re-greps |
| `missed_callers.py OUT.json` | tool results carrying error shapes a stale call site produces; classify the hits by reading context (most are TDD red steps) |
| `token_share.py [SINCE]` | input/output tokens per project, main vs subagents (tokens, not dollars) |

Hook timing (silent path) was measured on a `git archive` copy of daagu indexed
with `code-graph-mcp incremental-index --no-embed` (4 s), running each hook
script 15 times with `TMPDIR` pointed at scratch so hook state stays out of
`/tmp/code-graph-mcp`.

Not rerun: the SCIP oracle on the user's projects. A fair comparison needs the
0.184.0 and 0.204.0 binaries on one snapshot (`feedback_oracle_arms_must_share_one_corpus`);
use `../usage-audit-2026-11-03/oracle_user_projects.sh` once per binary.
