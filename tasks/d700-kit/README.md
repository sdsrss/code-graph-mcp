# D#700(2b) kit (Python module-qualified call into the caller's own file)

Spec: `tasks/specs/d700-py-module-call-own-file.md`.

- `gen.py OUT N SEED`: one-call Python files (import form × call shape × 0–2
  extra occurrences of the imported name: bindings, attribute writes, reads,
  dynamic patches × module / `main` / helper placement) plus `ext.py`,
  `ext2.py`, `pkg/ext.py`. Running a case with CPython prints which `go` its
  one call ran (LOCAL = the file's own).
- `check.py CASES BASE_REPO HEAD_REPO`: copies of OUT indexed by the two
  binaries (`git init` + commit first). WRONG = HEAD stopped binding into the
  file while CPython ran the file's `go`; ADDED = a HEAD edge from `main` BASE
  lacks (`"loc"` is ignored when comparing metadata).

Corpora, oracle recipe and scoring: `tasks/d370g-kit/README.md` and the
stdlib-oracle memory. The `score.sh` used here is the d370g one with
`W=/var/tmp/d700`, an absolute binary path, and Python/JS scoring only.

- `parity/q01`–`q05`: incremental-vs-fresh scenarios for this rule, run with
  `tasks/d370g-kit/parity/parity.py` (point its `B`/`work`/`home` roots at a
  new directory first). `q03` shows a pre-existing imports gap (D#702),
  identical on 0.183.0.
- `perf/timeit.py ROUNDS BASE_BIN HEAD_BIN DIR...`: interleaved full index of
  each DIR with the two binaries, median wall/CPU and max RSS per child via
  `os.wait4`.

Seeds behind the numbers (the generator gained dimensions as the review went
on, so a seed reproduces only with the `gen.py` of its commit): seeds 1 and 2
(3,000 each) with `gen.py` at `3fc7cdee`; seed 3 (3,000, in-package cases) at
`2e427cd3`; seed 4 (2,000) and seed 5 (600, `alias`) at `d3add736`.
