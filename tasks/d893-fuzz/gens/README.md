# Generators for `fuzz_seq.py --gen`

Written by the two pre-tag reviews of 0.201.0 (D#901). Each module defines
`initial(rng)` and `step(state, rng)` for one language; `gen_pymove.py`
extends `../gen_review.py` with file moves and swaps. Usage:

    python3 -I tasks/d893-fuzz/fuzz_seq.py <binary> 0 1500 out.jsonl --jobs 12 --gen tasks/d893-fuzz/gens/gen_go2.py
    python3 -I tasks/d893-fuzz/fuzzcmp.py base.jsonl head.jsonl

`fuzzcmp.py A B` prints each side's diverging seeds and rows, the seeds only
B diverges on, and B's rows that A lacks at the same step. Results for
0.200.0 and 0.201.0 are in `tasks/specs/d901-python-incremental-parity-2026-11-18.md`.
