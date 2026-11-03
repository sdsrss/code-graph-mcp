# D#370(g) kit (Python `C.m()` owner rule)

The scripts, parity scenarios and review case files of `/var/tmp/d370`, kept
for the redesign before that directory was deleted. The first design was
withdrawn after three review rounds (its code is on branch
`d370g-py-class-call`); the redesign (185f4ce8 and the 0.183.0 pre-tag review
repairs) was checked first against CPython with `enum/`, for release 0.183.0. The spec with every review round is
`tasks/specs/d370g-py-class-qualified-call.md`.

`enum/`: `gen.py SEED N OUT` generates one-call files, `truth.py` runs each with
CPython, `check.py TRUTH BASE_DIR HEAD_DIR` judges HEAD's edges against the
truth and BASE (`--attribute` groups failures by feature), `why.py` attributes
wrong and refused answers, `memrepro.py` reproduces the pre-tag review's
metaclass memory growth.

Scripts hard-code `/var/tmp/d370` (and the round-1/2 tools `/var/tmp/d370-review*`):
point their roots at a new directory first. Binaries, indexes, corpora copies,
oracle output, logs and `node_modules` were not kept.

## Layout

- `tools/`: `score.sh ARM BIN` indexes the corpora with BIN and scores them with
  the SCIP oracle (`CORPORA=` picks a subset); `edgediff.py BASE_DB HEAD_DB`
  (distinct edges added/removed); `classify.py`, `wrong.py`, `samples.py`,
  `clsrecv.py`, `modsplit.py`, `sim_member.py`, `sim2.py` (wrong-edge classes
  from judged dumps); `judgediff.py`; `go_check.py` (Go `pkg.F()` mechanical
  check, no oracle); `setup.sh`.
- `parity/`: `parity.py SCENARIO BIN...` (incremental run vs fresh index, every
  edge and pending row); `scn1`–`scn3` (D#370(a) and this task), `scn4` (review
  round 1), `scn5` (review round 2). Binaries go in `parity/bin/<name>`.
- `perf/`: `run5.sh` (full stdlib index, interleaved arms), `inc_toggle5.sh`
  (toggle tkinter `Widget.__init__`, time the incremental run).
- `review-cases/`: the reviewers' case files, one directory per case
  (`d370-review_cases/r*.py`, `d370-review2_cases/{bind,decl,body,c_nested,xpatch}`,
  `d370-review2_t1`, `d370-review3/cases` with the round-3 shapes `g1`–`g9`),
  plus their tools (`edges.py` dumps call edges of an index).
- `generators/`: `gen.py` / `gen2.py KIND N` (overlapping hierarchies `wide`,
  `fib`, and the cap-sized `capwide`); `cmp5.sh` (index one case dir with three
  binaries, dump edges).

## Corpora (rebuild them)

| name | repo | commit |
|------|------|--------|
| hono | https://github.com/honojs/hono | 4eb101dd03e66e20dfda86e6e5ebce8045d61d05 (v4.6.14) |
| flask | https://github.com/pallets/flask | ab8149664182b662453a563161aa89013c806dc9 (3.1.0) |
| leveldb | https://github.com/google/leveldb | 99b3c03b3284f5886f9ef9a4ef703d57373e61be (1.23) |
| cobra | https://github.com/spf13/cobra | e94f6d0dd9a5e5738dca6bce03c4b1207ffbc0ec (v1.8.1) |
| gin | https://github.com/gin-gonic/gin | 75ccf94d605a05fe24817fc2f166f6f2959d5cea (v1.10.0) |
| std | Python 3.14 standard library | recipe in the stdlib-oracle memory; one local commit |

SCIP tools: `npm ci` in a copy of `tasks/var-tmp-kits/scip-tools/`, with
scip-clang 0.4.0 beside it; `SCIP_TOOLS_BIN` points at `node_modules/.bin`.

## Numbers to beat (stdlib, default floor, judged edges)

0.182.0: 11,003/15,827 correct (69.5%), 4,824 wrong, recall 10,764/12,375.
Withdrawn 001b74a9: 10,993/13,905 (79.1%), 2,912 wrong, same recall, edge set
−1,923 / +0; flask, hono, leveldb, gin, cobra identical.
0.183.0 (redesign): 10,996/14,329 (76.7%), 3,333 wrong, same recall, edge set
−1,499 / +0; the same five identical.
