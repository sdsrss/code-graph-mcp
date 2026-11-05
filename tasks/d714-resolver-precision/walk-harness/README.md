# Re-export walk harness (0.185.0 pre-tag review, round 4)

Acceptance tools for the JS/TS re-export walk (`js_export_through_reexports`)
and its incremental fan-out. Written by the round-4 reviewer; paths made
relative. See `tasks/specs/d714-resolver-precision.md` (round 4) and the
deferred item on making the walk exact.

- `ivf.sh BIN CASEDIR` — `CASEDIR/before` indexed, `CASEDIR/after` applied
  incrementally, `after` rebuilt from scratch; prints `SAME (n edges)` or the
  `DIFF`. Writes `w/`, `f/`, `inc.txt`, `full.txt` into CASEDIR.
- `fuzz.py ROOT N SEED` — N random barrel graphs (6 barrels, 4 defining
  files) with a random edit, as `ROOT/z<SEED>_<i>/{before,after}`. Measured
  on seeds 7 and 11, N=200 each: 0.184.0 0/400 DIFF, 0.185.0 33/400.
- `shapes.py SPEC.py ARM[,ARM…]` + `sh/*.py` — hand-written valid and invalid
  ES-module programs; prints what `node app.js` binds next to each arm's call
  and import edges. Needs `CG_WALK_WORK` (binaries in
  `$CG_WALK_WORK/repro/bin/cg-new`, `cg-old`) and `node`. 0.185.0 matches
  Node on 44 of 62 valid programs.
- `real_inc.py BIN CORPUS RUNS SEED` — on a corpus copy (`$CG_CORPORA/<name>`,
  with `.git`), appends a comment to random barrels and importers and checks
  each incremental run ends with the full index's edges.
- `genperf.py ROOT cyclic|dag` — dense `export *` fixtures for cost.
- `cases.tar.gz` — the hand-written edit shapes as `kit38/<case>/{before,after}`
  (pre-tag reviews 1–2) and `r4sets61/<case>/{before,after}` (review 3),
  kept as an archive so this repo's own index does not take their JS files
  in. 0.185.0: kit38 38/38 SAME, r4sets61 57/61 (the 4 are D#739's shape).
  Run each case with `ivf.sh BIN <extracted>/<set>/<case>`.

Added for 0.186.0 (D#740 pre-tag reviews, 2026-11-05):

- `ivfm.sh BIN CASEDIR` — `ivf.sh` comparing edges WITH metadata and node
  types (`edgesm.sh`); `ivfr.sh` — the same, but the edit reaches the index
  through the query-time refresh (`deps` per deleted file, then one
  `affected` naming every changed file).
- `fuzz2.py ROOT N SEED` — barrel graphs whose edits also turn `function r`
  into `let r` and back and switch plain / renamed / default exports. 0.186.0:
  seeds 5–11 (1,050 graphs) 0 DIFF; 0.185.0 23.
- `chaingen.py ROOT N SEED STEPS` + `ivchain.sh BIN CHAINDIR` — N chains of
  STEPS successive trees around one name (renamed / direct / star / decoy
  files); each step is applied incrementally and compared, pending-call rows
  included, with a fresh index. Seeds 21 and 22, 300 chains of 6: 0.186.0 77
  DIFF (all D#741's shape: incremental keeps the call edge, the rebuild leaves
  the call pending), 0.185.0 14, 0.184.0 10 (seed 21).
