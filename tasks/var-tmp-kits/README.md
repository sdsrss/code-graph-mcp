# Old /var/tmp kits (deleted 2026-10-22)

The scripts, scenario files and keep-notes of fifteen /var/tmp work directories
from earlier sessions, copied here before the directories were deleted.
Binaries, indexes, target dirs, logs, source copies, corpora and node_modules
were not kept. Each subdirectory keeps the original layout; scripts still
hard-code `/var/tmp/<name>` (as do `tasks/d435-harness/`, `tasks/d520-kit/`
and several `tasks/specs/*.md`), so point their roots at a new directory first.
The `.keep` file in each says what the kit was for.

## Corpora (rebuild them)

| was | repo | commit |
|-----|------|--------|
| d446/corpora/flask | https://github.com/pallets/flask | d086db856be187255b8ec61ef409357393020f32 |
| d446/corpora/gin | https://github.com/gin-gonic/gin | 75ccf94d605a05fe24817fc2f166f6f2959d5cea |
| d446/corpora/gson | https://github.com/google/gson | 828a97be0f8d58108b140b77df8dc76b657f4a87 |
| d446/corpora/leveldb | https://github.com/google/leveldb | bb74ef739973a70ca9f0d90788a1da89b788c0ad |
| d446/corpora/express, d120/corpora/express | https://github.com/expressjs/express | 1faf228935aa0a13111f92c28ee795be64ce3f0f (4.21.2) |
| d446/corpora/hono, d120/corpora/hono | https://github.com/honojs/hono | 4eb101dd03e66e20dfda86e6e5ebce8045d61d05 (v4.6.14) |
| d446/corpora/cgm | this repository | a local one-commit copy (`git archive` of a commit, `git init`, one commit) |
| d132/tokio | https://github.com/tokio-rs/tokio | bb7ca7507b94d01ffe0e275ddc669734ab3bf783 (v1.41.1) |
| d370a/std, d370a/nx, d370a/fx | Python 3.14 stdlib, networkx, flask copies | stdlib recipe in the stdlib-oracle memory; one local commit each |

`scripts/scip_oracle/corpora.sh` clones hono, express, flask 3.1.0 and leveldb
1.23 at pinned commits (flask and leveldb there differ from the d446 copies above).

## SCIP tools (`scip-tools/`)

`package.json` / `package-lock.json` pin `@sourcegraph/scip-python` 0.6.6 and
`@sourcegraph/scip-typescript` 0.4.0: `npm ci` in a copy of `scip-tools/`.
`scip-clang` 0.4.0 was a downloaded binary (Sourcegraph's scip-clang releases).
Point `SCIP_TOOLS_BIN` at `node_modules/.bin` with `scip-clang` beside it.
