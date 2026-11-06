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
