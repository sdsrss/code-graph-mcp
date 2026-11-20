# Pre-tag review of 0.204.0, round 3 (c52b115c..f06f5e96)

The reviewer's scripts, kept as they ran (paths point at the reviewer's
`/var/tmp/agent-tmp-d909-review3`, binaries at `bin/{v0.203.0,c52b115c,f06f5e96}`).
Findings and how they were handled: `tasks/specs/d909-python-reference-lexical-pick-2026-11-19.md`
(MEDIUM-1, the second scope pick's cost, was repaired in e5e00e79 by the
owner's choice, without a 4th round).

- `mkcost3.py <out>` (written by the author after the review): regenerates
  byte for byte the three cost inputs 0.204.0's notes cite, a1, a5 and a7.
  Time them with `../timeab.sh`.
- `gen_r3.py`: a fuzz generator (`initial`/`step`) of nested namesakes,
  header calls, `global`/`nonlocal` and classes; `parity_r3.py` runs edit
  sequences comparing incremental and fresh indexes.
- `dynjudge.py` / `dyn_run.py` / `gen_dyn.py`: run generated states under
  CPython and record the calls it makes, to judge removed call edges.
- `pairdiff.py` / `abcorp.py` / `symcmp.py`: edge diffs between two arms over
  generated states and corpora.
- `mutate.py` / `mutants.py`: the round's mutations (22 compiling; the three
  survivors are equivalent, see the spec).
- `timeab3.sh`, `timing.sh`: the timing harness and the runs behind MEDIUM-1.
