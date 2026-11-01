# Grep rewrite rate after 0.178.0 (D#626)

## Question

The D#574 pass (2026-11-01) found no `pre-grep-guide.js` rewrite in any
transcript after 2026-10-23T18:55Z, when 0.178.0 was installed. The count was
0 of 1,525 greps in that era, against 33 of 11,448 before it. At a constant
rate about 4.4 would be expected, and P(0) ≈ 0.012. Was this a regression?

## Answer: no

The hook's grep path did not change, and the drop comes from the commands.

1. **Code.** `git diff v0.177.0 HEAD -- claude-plugin/scripts/pre-grep-guide.js`
   changes two lines, both in the sed-range read branch (D#560's `indexKey`).
   `cg-answer.js`, `hook-emit.js`, `recommendation-log.js`, `tmp-dir.js`,
   `find-binary.js` and `hook-fail-open.js` are unchanged. `project-root.js`
   only gains functions, and `resolveProjectRoot` is unchanged.
2. **It still fires.** A fixture repo with `grep -rn "ConfigLoader" src/` is
   rewritten by v0.177.0, v0.178.0 and HEAD alike. The installed 0.180.0
   scripts, with `recordRecommendation` stubbed, rewrite 3 of 3 qualifying
   greps typed against this repo, using the installed 0.180.0 binary. The hook
   also still runs in live sessions: all 25 of its non-empty transcript outputs
   after the cut are sed-range overviews (16 of them on 2026-11-01).
3. **Replay over the real commands.** `funnel.js` replays runMain's decision
   chain (from 0.180.0, which matches 0.177.0 on this path) over every
   transcript Bash command since 2026-10-01 that starts with grep/egrep/rg.
   Cut: 2026-10-23T19:00Z.

   | era, cwd | commands | would be answered | rewritten in session |
   |---|---|---|---|
   | before, main checkout | 4,334 | 39 | 33 |
   | after, main checkout | 533 | 1 | 0 |
   | before, worktree | 1,330 | (cwd deleted) | 0 |
   | after, worktree | 357 | 0 of the 4 still present | 0 |

   All 33 real rewrites fall inside the replay's 39, so the mirror misses no
   path that rewrites. In the after era, one command would be answered: `grep
   -n "yieldLoop\|…" race.mjs` in `claude-mem-lite/tasks/d593`. Run through
   the full installed hook it stays silent, correctly: `tasks/` is gitignored
   there, so `searchesSameFiles` declines it.

Why fewer greps qualify:

- **Worktrees.** 23% of grep-first commands ran in a linked worktree before the
  cut and 40% after. A grep in a worktree with no index of its own runs as
  typed in every version (`operandMatches` declines it, because the answer
  would search the main checkout). Worktree greps got 0 rewrites in both eras,
  0 of 1,687.
- **Shapes.** In the main checkout the shapes match across eras: 81% / 80%
  pipe into `head` and 25% / 24% carry `-A/-B/-C`. All 33 real rewrites were a
  lone grep (`-n`, `-nE`, `-n -E` or `-rln`) with no pipe, separator,
  redirect or context flag. The replay would answer 39/4,334 before
  and 1/533 after; at the earlier rate that is 4.8 expected and 1 seen. Those
  39 sit in 25 sessions, one of them with 8, so they are not independent
  draws. Since the replay runs the same code over both eras, this gap is in
  what was typed, not in the hook.

The P(0) ≈ 0.012 figure assumed one constant rate per grep. The replay,
running the same code over the after-era commands, expects about 1 rewrite
before the answer-stage losses (33 of 39 survived those before the cut).

## Caveats

- The replay reads today's filesystem: current `source-roots.json`, current
  index and current directories. Worktree cwds are mostly deleted, so they show
  as `cwd-gone`.
- Transcripts age out, so this corpus cannot be rebuilt later. The numbers
  above are from 2026-11-01.

## Re-run

    python3 extract.py 2026-10-01 /path/greps.jsonl
    TMPDIR=<scratch> node funnel.js <repo-or-tree> /path/greps.jsonl 2026-10-23T19

`funnel.js` writes no cooldown flags, no recommendation rows and no answer
child. It mirrors runMain as of 0.180.0, so re-read runMain before using it on
a later tree.
