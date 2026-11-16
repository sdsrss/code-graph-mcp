# 0.194.0 pre-tag review kits (R1, R2)

Fixtures and harnesses the two reviewers of 0.194.0 wrote, copied out of
/var/tmp before those directories were deleted (2026-11-16). Scripts hard-code
`/var/tmp/agent-tmp-r12-review*` and `/var/tmp/agent-tmp-converge-r12/bin/*`;
point them at new roots and rebuild the binaries before reuse.

- `r*/scripts/fx.py <case.py> [arm...]`: index a fixture (FILES, EDITS) per
  arm and print, per edit step, whether incremental == rebuild.
- `r1/scripts/gofuzz.py`: random 4-edit Go sequences, incremental vs rebuild
  (0.194.0: 14/400 divergent vs 0.193.0 0/400).
- Cases for the open P1 "an incremental run misses a definition changing
  kind": `r1/cases/fn2method.py`, `closer_swap.py`, `fn2method_js.py`,
  `fn2method_py.py`; an embedded type arriving later: `m1.py`, `m2.py`.
- Cross-package receiver and shadowed-package shapes: `r1/cases/xpkg*.py`,
  `shadowpkg*.py`, `r2/cases/q1_*.py`, `q2_*.py`, `c7_kill*.py`.

Corpora: gin 75ccf94d, hono 4eb101dd, cobra v1.8.1, BurntSushi/toml v1.4.0,
flask d086db85, express 1faf2289 (see `tasks/var-tmp-kits/README.md`), and
prometheus v2.53.0 (4c35b92).
