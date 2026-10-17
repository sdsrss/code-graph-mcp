# D#423(d): the build lock does not cover the CLI's embedding

Status: implemented (2026-10-17), for 0.171.2. Level L2 (CLI; restores
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
  Deferred, P3; revisit if an E2E round or a report shows it.
- (b) Start-up open/wipe before the lock: the version wipe is stamped by the
  first opener, the sub-header guard window is the measured ~ms 0-byte window
  (`325b1d09`), and snapshot install needs an https snapshot configured.
  Deferred, P3.
- (c) `reindex --from-snapshot` under a build: the build's writes land in the
  replaced file, but the reindex's own pass leaves a complete index. Deferred,
  P3 (the only harm is a success line from the replaced run).
- (e) D#406's unrelated remainder (C++ ctor, Rack::Protection, impact excluded
  count, ...): each needs its own corpus; one session each.

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
  0.171.1 binary let the lock go at exit with the process still alive.

## Verification record

(filled in below as checks run)
