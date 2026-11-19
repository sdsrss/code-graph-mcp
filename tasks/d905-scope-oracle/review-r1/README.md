# 0.203.0 pre-tag review, round 1

Written by the blind reviewer of 0580ad88..455f3cb0 (D#905). Paths inside
the scripts point at its work dir `/var/tmp/d905-review` (`base-bin`,
`head-bin`, `fuzzwork/`); set them up again there, or edit, to rerun.

- `tools/seq.py <bin> <seqfile>`: runs a sequence of trees (`STATES`) and
  compares the incremental index with a fresh one after each step (uses
  `tasks/d893-fuzz/fuzz_seq.py`).
- Sequences for finding F1:
  - `seqs/s5492min.py`: head misses `a5 -> lib/other.py:KT.T` where 0.202.0
    matched.
  - `seqs/nh_v_*`: the same without the closure local; 0.202.0 misses the
    edge too (D#906).
  - `seqs/v_*`: the variants with the closure local.
- `tools/gen905.py`: the D#905 sequence generator that found F1 (seed 5492).
- `tools/ab.py <case>` and `cases/`: base vs head edges per shape. Finding F2
  (alias forms) is in `alias` and `alias2`, F3 (depth cap) in `deepimport`
  and `deepstar`, F6 in `stubs`.
- `tools/mutants.py`, `tools/runmut.py`: the 35 mutants and their runner.
- `tools/mkcost.py`, `tools/timeit.sh`: the cost inputs (F4: c14).
