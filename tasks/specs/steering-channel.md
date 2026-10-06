---
status: approved
revision: 2
---
# Steering channel: stale-block notice once, and measure a git-excluded file channel

## goal
1. The out-of-date-block notice (0.164.0 known gap: "a project adopted by 0.163
   shows the out-of-date notice at every session start") is shown once per
   project for a given shipped template, not at every SessionStart.
2. Decide with measured numbers whether code-graph should deliver its steering
   through a git-excluded file loaded at launch, as claude-mem-lite 6.20.0 does
   with `CLAUDE.local.md`, instead of relying on the MCP `instructions` plus
   hooks (0.164.0, decision D4=C).

## non-goals
- The claude-mem-lite repository (its AGENTS.md interaction is reported, not fixed).
- Shipping a channel change in this task. A positive result produces a design
  for the user to approve (L3: LLM-visible steering + released default behavior).
- Writing `CLAUDE.md` from SessionStart again (D4 stands).

## constraints
- Candidate channels are measured as files present before the session starts
  (the real mechanism would inject once in the creating session; that is not
  what is being measured).
- `CLAUDE.local.md` makes Claude Code stop reading `AGENTS.md` by default
  (memory docs, "When Claude Code reads AGENTS.md"); `.claude/rules/*.md` does
  not. A shipped design must not use a channel with that effect, whatever the
  eval says.
- All arms in one window, one Claude Code version, `--model` pinned, same
  fixtures, same binary; `--ablation none` (only the plugin arm is compared).
- A probe must show the file reaches the model in each arm before the suite
  numbers count. `claude plugin eval` (and so `evals/run.sh`) cannot: it loads
  no project instruction file by design (r2).
- Budget: about $40 for the whole comparison, enforced with `--max-cost-usd`.

## success-criteria
- Notice: tests show first SessionStart → notice; second with the same shipped
  template → no notice; changed template → notice again; marker unwritable →
  notice (today's behavior, never silence by failure). JS suite green.
- Harness: `CG_EVAL_STEERING=none|local|rules` places the shipped block as
  nothing / `CLAUDE.local.md` / `.claude/rules/code-graph.md` (+ detail doc,
  git-excluded) in both the structural and the coding fixtures.
- Probe: the block's heading is quoted by the model in `local` and `rules`, and
  not in `none`.
- Results: per variant, code-graph use (runs with a code-graph call / runs),
  turns and cost per run, for the structural suite (10 cases × 3) and the
  coding suite (5 cases × 3); a recorded decision with the numbers.

## open-questions
- Does a launch-loaded file raise code-graph use over MCP instructions + hooks?
  Unanswered: needs a `claude -p` harness (isolated HOME + CLAUDE_CONFIG_DIR,
  credentials linked in, as claude-mem-lite's eval did). Waiting on the user:
  it runs on the user's login and costs money.
- If it does, is `.claude/rules/code-graph.md` as good as `CLAUDE.local.md`?

# Change log
- r1 (2026-10-06): created; user approved executing the recommendation
  ("按你的建议在我们仓库执行"), claude-mem-lite out of scope.
- r2 (2026-10-06): the probe showed plugin eval never loads CLAUDE.md,
  CLAUDE.local.md or .claude/rules (6/6 runs with a file placed did not quote
  the block; the 2 replies read in full said STEERING=no; 2 runs listed the
  file with ls; documented in the plugin-eval docs). Harness variants reverted; finding
  recorded in evals/README.md. Measurement method is an open question.
