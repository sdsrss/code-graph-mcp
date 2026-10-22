# D#543 / D#544 / D#545 measurement kit (0.177.0)

Ran from `/var/tmp/d543` (`R` in `harness/corp.py`, `PR` in `harness/parc.py`);
point them at a new directory before reusing. Binaries were `bin/cgm-<arm>`:
`base` = 9dbe8788 (0.176.0's code), `head` = the release, both
`cargo build --release` from `git archive` with their own `CARGO_TARGET_DIR`.
Corpora: `corpus/repo` (`git archive` of this repo + `git init` + one commit)
and the 0.174.0 corpora copied from `/var/tmp/d446/corpora` (flask, hono, gin, ...).

- `harness/dr.py <bin> <rel>`: index, delete `<rel>`, restore it, diff against a fresh index (D#543's 96 edges).
- `harness/fuzz.py <corpus> <seed> <nops> cgm-base,cgm-head` (`EXT=.rs|.py|.ts|.go`, `CTX=1` also compares context text):
  random touch/delete/restore/rename, incremental vs fresh after every op.
- `harness/parc.py <scenarios.py>` (`ARMS=base,head`): per-step incremental vs fresh, edges + pending rows + context text.
  `scn/ctx545.py` (D#545), `scn/newfile.py` (D#543 restore, the never-indexed-file gap D#547, the callee context case),
  `scn/m13.py` (D#544's M13 shapes).
- `harness/fullcmp.py <corpus> base head`: full index of one tree by two builds (symbols, edges, context text, pending rows).
- `harness/perf.py <corpus> <rel> <reps> base head`: delete / restore / one-file edit timings, interleaved.
