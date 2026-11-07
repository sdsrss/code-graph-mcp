# D#746 / D#747 / 0.189.0 measurement kit

Scripts and raw outputs behind `tasks/specs/d746-js-family-redo.md` and the
CHANGELOG 0.189.0 numbers. They hard-code `/var/tmp/agent-tmp-d746` (corpora
under `corpora/`, binaries under `bin/`: `base` = 0.188.0 source build, `h1`
e9b078c9, `h2` 55fb20a6, `h3` 21e167c5); point them at a new work directory.

- `setup.sh` — clones/copies the 13 corpora (hono, express, flask, tokio at
  the commits in `tasks/var-tmp-kits/README.md`; six `~/dev` projects and this
  repository as `git archive HEAD` copies; the Python 3.14 stdlib and nltk).
- `chains.sh ARM` — the chain sets under `sets/` (chaingen seeds 21, 22 x300;
  `tasks/d742-kit/famgen.py` seeds 1 (120) and 2 (200), 6 steps, generated with
  `PYTHONHASHSEED=0`) and the walk-harness kits, 8 in parallel.
- `steps.sh ARM SET...` — the same chains split into one-step chains (state
  k-1 indexed fresh, state k applied): a divergence at an early step no longer
  hides a later one. This is how fc1_109 step 4 (fixed in 21e167c5) showed.
- `timing2.sh A B` — full index and single-file edits, arms alternated.
- `mutate.py [NAME...]` — the mutations of the D#746/D#747 code run against
  the targeted tests (`cargo test --lib`); restores the file afterwards.
- `results.tar.gz` — corpus differentials (`diff-*.txt`), chain results per
  arm, timings, hono famchain/real_inc.
- `famgen2.py ROOT N SEED STEPS` (second pre-tag reviewer of 0.189.0) — chains
  aimed at the restore path: two nodes of a name, a node imported under two
  names, renamed exports, barrels, deletions, own-language twins. Seeds 101
  (200 chains), 102 and 103 (300 each), 6 steps; `whole3.sh ARM` runs 102/103
  as whole chains.
- `timing3.sh A B` — hono and the reviewer's adversarial trees (fan-in,
  sibling-only, member calls); `timing4.sh A B` — the fan-in tree's `.ts` file
  gaining a method of each of its 50 names (every caller re-extracted).
- `review-keep.tar.gz` — what the three pre-tag reviewers of 0.189.0 kept:
  round 1 `agent-tmp-review-0189/keep/` (H1 renamed-export cases r1–r5,
  M1/M2 shapes al*/nx*, gen2.py, steps.py, advgen.py adversarial trees),
  round 2 `agent-tmp-review-0189b/keep/` (famgen2.py, the alias-two-names
  repros r01*), round 3 `agent-tmp-review-0189c/keep/` (R1min / R2min, the
  swapped-name re-export through the scoped 2d-bind; P_age_toggle; S_amb_lose;
  famgen3.py; ivc.sh with the query-time refresh mode). Starting material for
  the next D#746 attempt.
