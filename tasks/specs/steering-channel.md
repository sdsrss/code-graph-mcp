---
status: approved
revision: 6
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
- Answered (r3): a launch-loaded rules file raised code-graph use on structural
  questions 19/30 -> 26/30 (Fisher p=0.07), turns 149 -> 131, cost $5.54 ->
  $5.25, scores at ceiling both; coding tasks 0/15 -> 1/15, no change.
  `CLAUDE.local.md` was not measured (disqualified by the AGENTS.md rule).
- Decided (r6): not shipped. Implemented as 5279162 (24 shapes tested, end to
  end verified in a sandbox install incl. the uninstall sweep), withdrawn in
  21d7512 after review round 1 (3 high / 8 medium / 9 low). Reproduced: a
  `.gitignore` re-include (`!.claude/rules/*.md`) outranks `info/exclude`, so
  `git status` lists the file; a repo that gains `package.json` after the
  file exists ships it with `npm pack` (Docker and other packagers alike) with
  no SessionStart in between to take it out. Any file in the user's repository
  has the second property. Also found: auto-registration let the uninstall
  sweep strip a teammate's committed CLAUDE.md block (F6).
- Next candidate, unmeasured: a user-level `$CLAUDE_CONFIG_DIR/rules/code-graph.md`
  (one file, no repository writes, no git/npm/teammate exposure, one known path
  for the uninstall sweep). claude-mem-lite measured user-level rules close to
  CLAUDE.md in the main session (6/9/4/4 vs 7/5/4/5) and weaker with subagents.
  Needs its own A/B (`evals/steering/ab.py`, a `user` variant) and the user's
  call, since it writes into ~/.claude.
- Was (r3): ship a `.claude/rules/code-graph.md` channel? A
  design has to settle, before code: first-session delivery (the file is read
  before SessionStart writes it); when not to write (non-git, root at $HOME or
  /, file tracked, `.claude`/`rules` a symlink, an npm root without `files`
  that would publish it — `.git/info/exclude` is git-only); a file the user
  removed stays removed; worktrees share one exclude; refresh in place; and
  the file outliving `/plugin uninstall`, which runs no hook — it would keep
  steering every session toward a CLI that is gone.

# Change log
- r1 (2026-10-06): created; user approved executing the recommendation
  ("按你的建议在我们仓库执行"), claude-mem-lite out of scope.
- r2 (2026-10-06): the probe showed plugin eval never loads CLAUDE.md,
  CLAUDE.local.md or .claude/rules (6/6 runs with a file placed did not quote
  the block; the 2 replies read in full said STEERING=no; 2 runs listed the
  file with ls; documented in the plugin-eval docs). Harness variants reverted; finding
  recorded in evals/README.md. Measurement method is an open question.
- r3 (2026-10-06): measured in real `claude -p` sessions (evals/steering/ab.py,
  a8698b7): 90 sessions, $23.68; results in evals/README.md. Shipping is an
  L3 decision for the user.
- r4–r5 (2026-10-06): design, 24 shapes, implementation 5279162 (see git).
- r6 (2026-10-06): withdrawn after review round 1 (21d7512); see open-questions.
