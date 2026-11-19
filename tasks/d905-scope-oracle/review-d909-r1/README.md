# 0.204.0 pre-tag review, round 1

Written by the blind reviewer of 341362e5..51db0ac3 (D#909). Paths inside the
scripts point at its work dir `/var/tmp/d909-review` (`bin/base`, `bin/head`,
`work/`); set them up again there, or edit, to rerun.

- `scripts/cases1.py` … `cases4.py`, `casesjs.py`: shape tables (`S..`, `J..`).
  `scripts/shapes.py <cases> <work> [keys]` prints base vs head edges per shape;
  `scripts/judgecases.py` adds what CPython does (`pysem/sem.py`, `sem2.py` run
  the shapes under Python 3.14 and print the definition each name reaches).
  - MEDIUM-1: S15 (a default `Row()` beside a nested `class Row`).
  - MEDIUM-2: S06–S10 (`global` / `nonlocal` rebinding).
  - LOW-1: S11 (a class body that binds the name reads the module, skipping
    the enclosing function: CPython's `LOAD_NAME`).
  - LOW-2: S22, S24, S24b (the header position landing outside the enclosing
    body when the `def` is that body's first statement).
- `scripts/mkcost.py`: the cost inputs `k1`…`k13` (LOW-3: `k1b`, `k1c`, `k2b`
  classes in deep blocks; `k1bd` the same with defs). `scripts/timeab.py`
  times them, arms alternating, a fresh copy per run.
- `scripts/mutate.py`, `mutants.json`: the 14 mutants (M03 and M14 survived
  round 1; the repair's tests kill both).
- `scripts/seq909.py`, `abfull.py`, `symjudge.py`: line-keyed edit sequences,
  a full-edge A/B, and an independent scope judge.

`../timeab.sh <base> <head> <work> <runs> <case-dir>...` is the author's timer
for the same purpose (D#905's inputs: 988 KB `c14`, 479 KB of generic
annotations nested 151 brackets deep, 119 KB of calls nested 150 deep as
arguments).
