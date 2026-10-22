# D#518 / D#520 / D#519 measurement kit

Scripts and scenario files copied on 2026-10-21 from three /var/tmp kits before
those directories were deleted (their bulky indexes, binaries and target dirs
were not kept). The scripts still hard-code their old roots; point `R` in
`d520-harness/corp.py` (and the `cd` lines of `tasks/d520-review-lows/run*.sh`)
at a new directory before reusing them.

| here | was |
|------|-----|
| d520-harness/ | /var/tmp/d520/harness (fuzz.py, parity.py, scen.py + scen_cases.py = the 1,789-step suite, s1..s8 scenario sets, fullcmp/fulldiff/cost2) |
| d520-scn518/ | /var/tmp/d520/scn518 |
| d518r2-tools/, d518r2-scn/ | /var/tmp/d518r2/{tools,scn} (par.py per-step incremental vs fresh; twofile2-4.py for D#542) |
| d520-review-tools/, d520-review-scn/ | /var/tmp/d520-review/{tools,scn} (round-1 review kit of 2f070d5a..33fbf845) |

Corpora (not kept, rebuild them):
- std: the Python 3.14 stdlib recipe in the stdlib-oracle memory (`tar -C /usr/lib/python3.14 --exclude=...`), `git init` + one commit.
- nx: a networkx source tree, `git init` + one commit.
- repo-now: `git archive 33fbf845 | tar -x -C <dir>`.

D#519's corrupt databases stay in /var/tmp/d516 (see its .keep).
