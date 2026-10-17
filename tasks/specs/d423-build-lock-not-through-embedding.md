# D#423(d): the build lock does not cover the CLI's embedding

Status: implemented and verified (2026-10-17), for 0.171.2. Level L2 (CLI; restores
0.171.0's behaviour for the embedding phase, CHANGELOG fix). Follows
`d406-one-build-at-a-time.md`.

## Problem

0.171.1's `incremental-index` held `.code-graph/index-build.lock` until it
exited, embedding included. A starting MCP server takes that lock before its
startup index, and while its startup index has not run, `ensure_indexed`
answers every tool call `Indexing in progress` after a 2 s grace. 0.171.1's
own notes tell users to run `incremental-index` without `--no-embed` to get
vectors when no server runs, which is the shape that hits it.

Reproduced (`embed_wait_repro.py`, this repo as corpus via `git archive`,
7,157 nodes to embed, model from `CODE_GRAPH_MODEL_DIR`): CLI embedding
started, server started 3 s later, one `get_call_graph` call every ~5 s.

| Binary | Calls | Failed (`Indexing in progress`) | First answer | Server log |
|---|---|---|---|---|
| 0.171.0 (npm global) | 1 | 0 | 0.0 s | no wait |
| 0.171.1 (`cc5c859c…`) | 18 over 119 s | 18 | none | `Another index build is running; the startup index waits for it` |
| fix (`3ae9cbf0…`) | 12 over 55 s | 0 | 0.0 s | no wait |

The CLI embedded ~5.3 nodes/s (640 of 7,157 in ~120 s), so the 0.171.1 server
would have waited the whole 10 min `BUILD_WAIT`.

## Design

`incremental-index` drops the build lock after its index is written and
before `finish_embedding`, in both of its branches (a project's first index,
and an incremental run). The first-index branch called `build_full_index_at`,
which embeds inside; it is split into `index_full_at` (index only, returns the
connection) + `finish_embedding`, and `build_full_index_at` keeps both for
`rebuild-index`.

Why the embedding needs no lock: the server's own startup run already drops
the lock before its embedding backfill (`spawn_startup_indexing`'s inner
block), secondaries and the server's backfill embed beside other processes'
runs, and `insert_node_vectors_batch` skips a node that vanished between its
read and its store. The structural phase stays serialised, so D#406's
measured shapes are unchanged (their refresh runs `--no-embed`).

## Not done (re-ranked, with reasons)

The rest of D#423, judged by how a default install reaches it:

- (a) The server's other write paths (owed/backstop incremental, not-yet-indexed
  path, wipe-and-rebuild, a promoted secondary's first run) do not take the
  lock. In a default install the only automatic CLI writer is SessionStart's
  refresh, which yields to any `index.lock` holder. A new session's server
  takes `index.lock` at process start, so (a) needs another session's
  secondary to promote in the gap between the refresh's lock check and that
  server's start (sub-second). Other routes are a hand-run `incremental-index`
  or the opt-in per-edit hook beside a live server, both already warned.
  Fixing it touches the incremental path every tool call takes (four paths,
  each a skip-and-stay-owed arm and a test). Not observed in any E2E round.
  Deferred as D#425 (priority 1, low); revisit if an E2E round or a report shows it.
- (b) Start-up open/wipe before the lock: the version wipe is stamped by the
  first opener, the sub-header guard window is the measured ~ms 0-byte window
  (`325b1d09`), and snapshot install needs an https snapshot configured.
  Deferred, D#425.
- (c) `reindex --from-snapshot` under a build: the build's writes land in the
  replaced file, but the reindex's own pass leaves a complete index. Deferred,
  D#425 (the only harm is a success line from the replaced run).
- (e) D#406's unrelated remainder (C++ ctor, Rack::Protection, impact excluded
  count, ...): each needs its own corpus; one session each (D#426, priority 2).

## Acceptance

- RED then GREEN: `test_cli_incremental_index_embeds_without_the_build_lock`
  (embed-model, real weights; skips without them, as the server backfill tests
  do). Both branches, each pinned by its own mutation.
- Real binary: the repro above shows 0 failed calls while the CLI embeds.
- Both runs to the end (`embed_complete.py`): CLI exit 0, index healthy, every
  node embedded, 0 orphan vectors — fix and 0.171.0 alike.
- fmt, clippy both legs, full suite both legs, JS suite untouched.

## Decisions

- Probe for the test waits until the run's file is in the index before it
  tries the lock: a first version took the lock ahead of the run's own and the
  run waited the full 10 min (test passed in 607.84 s). The assertion is on
  vectors written by the time the lock was free, not on "still running": the
  pre-fix debug binary let the lock go at exit with the process still alive
  (`201 of 201 vectors (still running: true)`).
- `reader_nondestructive.rs`'s allow-list anchor moved from
  `build_full_index_at` to `index_full_at`, the function that now holds the
  destructive open (same list size; the comment already said the anchor tracks
  the holder).

## Verification record

- RED on the pre-fix code: `the first index: ... embedded 4 of 4 vectors`
  (3.88 s). GREEN after: 7.14 s.
- Mutations (exact replace, restore md5 `447ebee9…` checked): first-index
  `drop(build)` removed -> `the first index: 4 of 4`; incremental `drop(build)`
  removed -> `an incremental run: 8 of 8`.
- Guard: `reader_nondestructive` 7/7; a destructive open planted in
  `build_full_index_at` -> red (`index_ops.rs:171 (in fn build_full_index_at)`),
  restored md5 checked.
- Real binary (`cg-fix` = `target/release` md5 `3ae9cbf0…`, embed-model):
  12/12 calls answered over 55 s, first at 0.0 s, CLI still embedding.
- `embed_complete.py` on a 1,286-node corpus (`git archive HEAD src/cli
  src/indexer`): fix CLI exit 0 (316 s), healthy, 1162/1162 vectors, 0 orphan;
  0.171.0 CLI exit 0 (281 s), healthy, 1162/1162, 0 orphan. Both servers ran
  their backfill beside the CLI (0.171.0 log: 32 `[embed]` batches).
- fmt; clippy `--all-targets` both legs; `cargo test --features embed-model
  --no-fail-fast`: 2297 passed, 1 failed (the guard anchor above, fixed and
  re-run green); pre-commit default leg 2275 passed, 0 failed (31 binaries).
- Pre-tag review (round 1, fresh subagent, 302e030e): all seven claims held.
  MEDIUM, REPRODUCED: a node re-created under a reused id (re-indexed file whose
  nodes held the top ids) gets the vector of its old text from the unlocked
  embedding (fix = 0.171.0; 0.171.1 prevented it only between two CLI runs).
  Not fixed here: the root is the shared vector store
  (`insert_node_vectors_batch` checks existence, not content), every embed path
  goes through it, and a content guard needs its own audit of the order in
  which each path writes `context_string`. Disclosed in the 0.171.2 notes,
  comment corrected, deferred as D#429. LOW: the probe passed when its deadline
  ran out (`at_take = -1`) -> `Option`, asserted. LOW: no test pins the lock
  through the index (the probe cannot tell "freed during" from "freed after"
  the index without a seam) -> logged, not fixed. LOW: "1,162-node" -> 1,162 to
  embed, 1,286 in all. Note: 0.171.1's server also embedded beside the CLI ->
  "as 0.171.0's and 0.171.1's did".
