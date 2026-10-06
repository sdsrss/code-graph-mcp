---
status: implemented
revision: 5
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

## design (r4): `.claude/rules/code-graph.md`

One plugin-owned file, git-excluded, refreshed in place, swept at uninstall.

- **Content.** First line `<!-- managed-by: code-graph-mcp -->` (ownership
  marker, as the detail doc). Then the block's heading, text and table, minus
  the detail-doc pointer (0 of 45 measured sessions opened that doc, so no
  detail doc is written), plus one line: these commands come from the
  code-graph-mcp plugin; if `code-graph-mcp` is not found, ignore this file.
- **Where.** SessionStart, plugin mode only, when cwd is the git top-level and
  holds `.code-graph/`. A session in a subdirectory writes nothing.
- **First session.** The file is written after Claude Code read its rules, so
  the session that creates it gets the same text once as `additionalContext`.
- **Exclude.** `/.claude/rules/code-graph.md` is appended to
  `git rev-parse --git-path info/exclude` (a worktree's resolves to the
  common dir) unless `git check-ignore` already ignores it. If that write
  fails, the file is not written.
- **Uninstall.** The project is recorded in the adopted-projects registry;
  `unadopt` removes the file (marker-guarded) and an emptied `rules/` and
  `.claude/`. The post-uninstall statusline sweep runs `unadopt` over the
  registry: verified 2026-10-06 in a sandbox — after `claude plugin
  uninstall` the cache dir stays (marked `.orphaned_at`), the statusline still
  points at it, and one render removed a registered project's CLAUDE.md block,
  the cache and the statusline entry. Holes: a statusline the user replaced,
  or no interactive session before Claude Code reaps the cache; then the
  file's own last line applies.
- **Opt-out.** `CODE_GRAPH_NO_AUTO_ADOPT=1` (its existing meaning: no automatic
  adoption surface).

### shapes (each one a test)

| # | Shape | Result |
|---|---|---|
| 1 | git top-level with `.code-graph/`, nothing at the path | created, exclude line, registry, injected once |
| 2 | our file, same content | unchanged, nothing written |
| 3 | our file, older content | rewritten |
| 4 | a file at the path without our first line | left alone |
| 5 | `.claude` is a symlink | refused |
| 6 | `.claude/rules` is a symlink | refused |
| 7 | the file is a symlink | refused |
| 8 | the path is tracked by git | refused |
| 9 | not a git work tree | nothing |
| 10 | git top-level is `$HOME` or `/` | nothing |
| 11 | cwd is a subdirectory of the top-level | nothing |
| 12 | top-level `package.json` without `"private": true` and without a `files` array | refused (npm would publish it; `info/exclude` is git-only) |
| 13 | same, `"private": true` | created |
| 14 | `files` array with an entry starting with `.claude`, `*`, `.` alone or empty | refused; other `files` arrays: created |
| 15 | CLAUDE.md already holds our block | nothing (no duplicate) |
| 16 | we created it, the user deleted it | not re-created (marker `.code-graph/rules-file`) |
| 17 | exclude not writable | not written |
| 18 | path already ignored | created, exclude untouched |
| 19 | `CODE_GRAPH_NO_AUTO_ADOPT=1` | nothing |
| 20 | not plugin mode | nothing |
| 21 | linked worktree | file in the worktree, exclude line in the common dir |
| 22 | `unadopt` / uninstall sweep | our file removed, emptied dirs removed, registry entry dropped |
| 23 | `unadopt` with a user file at the path | left alone |
| 24 | no `.code-graph/` at the top-level | nothing |

## open-questions
- Answered (r3): a launch-loaded rules file raised code-graph use on structural
  questions 19/30 -> 26/30 (Fisher p=0.07), turns 149 -> 131, cost $5.54 ->
  $5.25, scores at ceiling both; coding tasks 0/15 -> 1/15, no change.
  `CLAUDE.local.md` was not measured (disqualified by the AGENTS.md rule).
- Decided (r4): the user approved "design first, implement only if the
  uninstall residue is solved"; the sweep was verified, so it is implemented on
  this branch, not released. Was: ship a `.claude/rules/code-graph.md` channel? A
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
- r4 (2026-10-06): design + shapes; uninstall sweep verified in a sandbox;
  implementation approved under the condition above.
- r5 (2026-10-06): implemented (rules-file.js + adopt.js unadopt/helpers,
  session-init injection, lifecycle/cli-entry shared predicate). Shapes 1–24
  covered by rules-file.test.js (20 tests); 11 guard mutations each caught.
  End-to-end in a sandbox install (plugin-mode path): session 1 created the
  file and the model quoted the block "from context added by a hook"; session
  2 quoted it "from .claude/rules/code-graph.md"; git status empty both times;
  `claude plugin uninstall` + one statusline render removed the file and its
  `.claude/`. Trap: a local directory marketplace runs the plugin from its
  source dir, which is not plugin mode unless it sits under .claude/plugins/.
