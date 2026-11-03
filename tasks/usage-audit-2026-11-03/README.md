# Usage and accuracy audit, 2026-11-03

Report: `docs/plugin-usage-audit-2026-11-03.md` (docs/ is gitignored, local only).
Era E = from 0.178.0 (2026-10-23T23:00Z) to 2026-11-03; era D = 0.170–0.177.

Transcript scans (read `~/.claude/projects`, write nothing):

| Script | What it answers |
|---|---|
| `../../scripts/plugin_usage.py --since 2026-10-16T12:01 --split 2026-10-23T23:00=E --exclude dev/code-graph-mcp --exclude dev/claude-mem-lite --detail` | calls / injections / coverage per 1k tool calls |
| `deep_usage.py --out FILE` | the 10-23 deep scan with era E and sgc/sec added |
| `cg_outcomes.py` | outcome of every explicit code-graph call (ok / no-index / empty) |
| `sig_edits.py` | signature edits with cross-file callers: searched? hook injected? (now split by era) |
| `ceiling.py` | savings ceiling, subagent types, assistant texts naming code-graph (era E) |
| `injections.py OUT.json` | every code-graph hook output in era E with the next 10 tool calls |

Tests run against copies (nothing writes into the user's repos):

| Script | What it tests |
|---|---|
| `oracle_user_projects.sh SCRATCH` | SCIP oracle on `git archive HEAD` copies of daagu / gsd-lite / claudemd / sgc / claude-mem-lite, indexed with `--no-embed`. Needs `npm install --prefix /var/tmp/scip-tools --save-exact @sourcegraph/scip-typescript@0.4.0 @sourcegraph/scip-python@0.6.6`. Delete `/var/tmp/cg-audit-1103` and `/var/tmp/scip-tools` afterwards. |
| `classify_wrong_edges.py JUDGED.json INDEX.db` | splits wrong edges (from `run.sh --dump-judged`) into cross-file / same-file name defined more than once |
| `startup_repair_probe.py A\|B PROJ HOME` | arm A sends Claude Code's handshake only, arm B adds one `tools/call`; prints vector slots before/after and whether `[startup-repair]` ran |

Read-fanout cross-session repro (TMPDIR keeps the hook state out of the shared `/tmp/code-graph-mcp`):

    R=<an indexed copy>; T=<scratch>/fanout-tmp; H=claude-plugin/scripts/pre-read-guide.js
    # reads 1-4 by sess-A, read 5 by sess-B, read 6 by sess-A, same directory
    printf '{"session_id":"%s","tool_input":{"file_path":"%s"}}' sess-B "$R/src/x/5.js" | (cd "$R" && TMPDIR="$T" node "$H")

Result on gsd-lite's `src/tools/orchestrator/`: sess-B's first read there got the overview, sess-A's 5th and 6th got nothing.
