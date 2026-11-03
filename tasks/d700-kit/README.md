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
  lacks (`"rb"` is ignored when comparing metadata).

Corpora, oracle recipe and scoring: `tasks/d370g-kit/README.md` and the
stdlib-oracle memory. The `score.sh` used here is the d370g one with
`W=/var/tmp/d700`, an absolute binary path, and Python/JS scoring only.
