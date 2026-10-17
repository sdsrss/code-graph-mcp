# D#406(1): one index build at a time at session start

Status: implemented (2026-10-17), shipping in 0.171.1. Level L2 (CLI + MCP server + plugin hook).

## Problem

SessionStart spawns a detached `incremental-index` when git HEAD is newer than
`index.db` (or the index is version-stale / corrupt). The MCP server starts at
the same time and runs its own startup index. 0.171.0 stops SessionStart from
spawning while the server's build is *visible* (its status file), but not when
the CLI run starts first, or when the server starts after SessionStart looked.
The two runs then write one database at once.

Reproduced on django (3,453 files, 1,500 changed, `race_repro.py`: CLI first,
server 0.3 s / 1.5 s later), base = 0.171.0 (`394334b6`):

- 6/6 runs: `health-check` UNHEALTHY `reason: interrupted`;
- 6/6 runs: the CLI exits 1 (5x `database is locked`, 1x `database disk image
  is malformed` / `Could not find a row with rowid`);
- 4/6 runs: the server's startup index fails too;
- edges after the run: 171,053 .. 253,648 (a clean full index: 250,731).

Why the existing run lock does not help: `index-run.lock` is taken in
`in_flight::begin`, after the run has walked and diffed the tree (~1 s on
django). A server that looks during the CLI's walk sees no run.

## Design

A third lock, `.code-graph/index-build.lock`, held EXCLUSIVE by a whole index
build: the MCP server's startup run, and every CLI `incremental-index`. A build
that finds it held waits (polling, at most 10 min, then builds anyway with a
warning). Unopenable / non-Unix: no lock, the old behaviour.

SessionStart's refresh also yields to a live server: with
`CODE_GRAPH_YIELD_TO_SERVER=1` (set by `session-init.js`), `incremental-index`
takes the build lock, THEN checks `index.lock`; held → exit 0, nothing written.
An env var, not a flag: an older binary ignores it and refreshes as before; a
flag would make it exit 2 on every SessionStart.

Why this ordering closes the race (CLI = C, server = S):
C takes B (build lock), then checks L (index lock).
S takes L at process start, then (in the startup thread) takes B.
If C's check finds L free, S took L after that check, so S asks for B after C
holds it and waits. If C finds L held, C yields. Either way the two builds do
not overlap. Without the yield (a manual run), B alone still serialises C and
S's startup run.

The server recomputes "is there an index" after taking B: a CLI build it waited
for may have created the index, and a full build over it would redo it.

## Shapes (ordering x server state)

| # | Shape | 0.171.0 | After |
|---|---|---|---|
| 1 | CLI starts; server starts during the CLI's walk (before `begin`) | both write; UNHEALTHY (repro 3/3) | server's startup run waits for B, then indexes |
| 2 | CLI starts; server starts during the CLI's batches | same (repro 3/3) | same as 1 |
| 3 | Server alive (holds L); SessionStart spawns CLI | writes beside the server | CLI yields: exit 0, no write |
| 4 | Server building (status file) when SessionStart looks | no spawn (`1611eedb`) | unchanged |
| 5 | No server | CLI indexes | CLI indexes (B uncontended) |
| 6 | Manual `incremental-index` during the server's startup run | warns, writes beside it | waits for B (prints a line unless `--quiet`), then indexes |
| 7 | Manual `incremental-index`, server alive and idle | warns, indexes | unchanged |
| 8 | B holder killed | - | kernel drops B; the waiter proceeds; a dead run's token is recovered as before |
| 9 | B unopenable, non-Unix | - | no B: behaviour of 0.171.0 |
| 10 | B held past 10 min | - | the waiter builds anyway, with a warning |
| 11 | Secondary server | does not index | unchanged |
| 12 | Two CLI `incremental-index` at once | both write | serialised |
| 13 | `rebuild-index` holds L; SessionStart CLI | writes beside the rebuild | CLI yields |

Not covered (unchanged): the server's watcher / tool-call runs and its
embedding backfill do not take B, so a manual `incremental-index` while the
server is idle still writes beside them (row 7); query refreshes keep their
run-lock probe; `rebuild-index` does not take B.

## Acceptance

- RED then GREEN: server startup waits while B is held (unit test in
  `mcp::server`); CLI `incremental-index` waits while B is held (e2e); CLI
  yields under the env var while L is held, and indexes without it (e2e);
  `session-init.js` spawns with the env var (node test); B helper times out.
- `race_repro.py` on django with the fixed binary: 0 UNHEALTHY, CLI exit 0,
  server index not failed, edges equal to a clean index.
- Full suite + clippy both legs + JS suite.

## Decisions

- Chose a new lock file over reusing `index-run.lock`: an early shared hold on
  the run lock makes the holder's own `dead_runs` probe read "in flight", so a
  CLI run would never recover a dead run. Reversible (cost: one file).
- Chose an env var over a hidden flag (see Design).
- Cap 10 min: a live holder that never finishes is a hung process; past the cap
  writing beside it is the lesser harm than never indexing.
- Pre-tag review (round 1, fresh subagent): HIGH non-Unix dead code
  (`BUILD_LOCK`) -> `cfg_attr(not(unix), allow(dead_code))`; MEDIUM first index
  had no `.code-graph/` to lock in -> `ensure_owned_dir` before the lock; MEDIUM
  the refresh held the lock through embedding -> SessionStart passes
  `--no-embed` (the server embeds; the per-edit hook already does this); LOW
  audit table pins candle-core's version; LOW C++ constructor control added
  (`c` cannot have a case: no class bodies, `.h` reads as C++).
- Not fixed, disclosed: a promoted secondary's first index (and the server's
  other runs) do not take the build lock. Chose disclosure over extending the
  lock to every server write path: four paths (not-yet-indexed, owed
  incremental, backstop, wipe-and-rebuild) would each need a skip-and-stay-owed
  arm, the behaviour is 0.171.0's, and the review stop line applies. Deferred.
  Also disclosed: start-up open/wipe before the lock; `reindex --from-snapshot`
  unlinks under a build.

## Verification record

- RED before the fix: `test_startup_run_waits_for_another_build` (indexed 1
  file while B held), `test_cli_incremental_index_waits_for_another_build`
  (exited 0 at once), `test_cli_incremental_index_yields_to_a_server_when_asked`
  (`mode: incremental`), session-init node test (no env var). All green after.
- Mutations (exact restore, md5-checked): check-L-before-B -> the yield test's
  wait-then-server block red; dropping the post-wait recompute ->
  `test_startup_run_after_a_build_that_created_the_index_is_incremental` red.
- `race_repro.py` django, cli-first 0.3/1.5 s x3: base 6/6 UNHEALTHY, CLI exit
  1 x6, server failed 4/6, edges 171,053..253,648; fix 0/6, CLI exit 0, server
  0 failed, edges 250,731 x6. server-first 0.5/3.0 s x2: base 3/4 UNHEALTHY;
  fix 0/4, CLI waited then `skipped`, edges 250,731 x4.
- `--quiet` suppresses the wait line (django, B held 1 s): stderr empty.
- fmt, clippy `--all-targets` both legs: pass. JS suite 1726 passed, 0 failed (2 skipped).
- Also in this change: session-init's test covers a dead writer's status file
  (the refresh starts); red when `writerAlive` is skipped.
- Review repairs: emulated non-Unix `cargo check --lib` (cfg arms flipped in a
  `git archive` copy): HEAD exit 101 `BUILD_LOCK is never used`, repaired exit
  0. django first index (`FRESH=1`, server 1.5 s later) x3: base 3/3
  UNHEALTHY 163,381 edges; repaired 0/3, 250,731. Regression x2 each ordering:
  healthy. New e2e `test_cli_a_first_index_takes_the_build_lock` red before.
  Audit mutations 5/5 red (incl. candle-core 0.10.0); constructor mutations
  cpp/csharp/dart 3/3 red. First-index x3 repaired: CLI exit 0 and the
  server's index did not fail in all 3.
- Delta review (round 2, fresh subagent) of 69e5d71c..7265a076: D1-D4 held
  (9 refusal shapes identical to v0.171.0; vectors 2/91 -> 91/91 via the
  server; flipped-cfg check --all-targets both legs). Repaired: the
  constructor comment was wrong (`detect_language` reads `.h` as C, so the
  rule's `c` arm covers a C++ class in a header; `c` removed kept the test
  green) -> `.h` case added; the first-index test cited tokio numbers ->
  django; the audit test now pins the dependent's crates.io source (a git
  fork of candle-core 0.9.2 was green). Mutations: c/cpp/csharp/dart 4/4 red;
  git-fork candle-core red (compiled binary run against an edited Cargo.lock,
  restored and md5-checked). session-init.test.js 1-in-8 failure the reviewer
  saw: 0/10 at HEAD and 0/10 at v0.171.0 here. Closed without a third
  reviewer: test/comment/docs only, each mutation-verified (depth limit).
