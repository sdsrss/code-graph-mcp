# Pre-tag review of 0.204.0, round 2 (51db0ac3..c52b115c)

The reviewer's scripts, kept as they ran (paths point at the reviewer's
`/var/tmp/d909-review2`, binaries at `bin/{v0203,pre,rep}`). Findings and
how they were handled: `tasks/specs/d909-python-reference-lexical-pick-2026-11-19.md`.

- `sh3.py <cases.py> <work>`: index each case with every arm in `$ARMS`, print
  calls/references per arm. `cases_*.py` are the shape tables (`cases_v.py`
  and `cases_v5.py` were added by the author while checking the repair).
- `dyn.py`: runs a shape under CPython and traces the calls it makes.
- `gen_scope.py`: a fuzz generator (`initial`/`step`) of global/nonlocal
  lines, header defaults/annotations/decorators beside nested namesakes and
  nested classes; `fuzz_seqL.py` runs it line-keyed.
- `mutate.py` / `mutants.py`: the mutations of round 2.

The round-3 checks that came out of it live one level up: `subset_enum.py`
(every edge a head has that a base lacks, over generated states),
`removed_judge.py` with `linejudge.py` (every edge it lost, judged by line).
