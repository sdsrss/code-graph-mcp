---
status: draft
revision: 1
---

# Uninstall races an in-flight auto-update and leaves the cache behind

## Goal

`code-graph-mcp uninstall` must leave no `~/.cache/code-graph` behind, including
when an `auto-update.js` run spawned by the same session's SessionStart hook is
still in flight. Today the teardown and the updater share no coordination that
survives the teardown, so the updater re-creates the directory it was deleted
out from under — in the worst arm with a freshly downloaded 42 MB binary.

## Non-goals

- Stopping an updater started by a *different, still-live* Claude Code session.
  That session still has the plugin loaded; re-creating the cache is arguably
  correct there. Scope is the single-session teardown.
- Reworking `install-lock.js`. See the refuted approach below — the lock is fine
  for what it guards; it simply cannot guard its own deletion.

## The defect, reproduced

Measured 2026-09-13 in a `HOME`-isolated sandbox with a sandbox-local node
prefix (so `find-binary`'s `execPath`-derived global tier cannot reach the host
install). Two arms, distinguished by where the teardown lands relative to
`downloadBinary`'s `fs.mkdirSync(BINARY_CACHE_DIR)` (`auto-update.js:599`):

| Teardown lands | Mechanism | Residue | Runs |
|---|---|---|---|
| before the `mkdirSync` | whole chain proceeds on a re-created tree | `bin/code-graph-mcp` 42,847,128 B + `install-manifest.json` + `statusline-registry.json` + `update-state.json` — **41 MB** | 2/2 (t=0.2 s, t=2.0 s) |
| after curl created its tmp | tmp is unlinked with the directory; `statSync(binaryTmp)` throws ENOENT; `promoteVerifiedBinary` reports `binary-promote-failed` | `update-state.json` only (written by `noteUpdateFailure` → `saveState`) | 1/1 |

In both arms `CACHE_DIR` comes back. The pre-download window is wider than 2 s
(the GitHub version check runs first), which is why the cheap arm is the one
that reproduces on demand.

Real-world trigger: run the teardown within the first seconds of a session. A
teardown minutes into a session finds the updater long finished and leaves
nothing. Low frequency, silent, 41 MB.

`install-manifest.json` returning is its own small harm: `readManifest()`
afterwards reports an install that is not there.

## REFUTED — do not re-attempt

> "Have `uninstall` acquire `INSTALL_LOCK_FILE`, the lock that
> `auto-update.js:1300,1345` and `launcher-install.js:113` already respect and
> that `lifecycle.js` merely re-exports without ever taking."

Prototyped 2026-09-13. The lock **was** acquired (`acquireLock` returned truthy)
and **41 MB still came back, 2/2**. `INSTALL_LOCK_FILE` is
`CACHE_DIR/install.lock`; `removeCacheResidue()` deletes `CACHE_DIR`, so the lock
file goes with it and the updater acquires freely seconds later.

The general rule this is an instance of: **a mutual-exclusion token stored inside
the resource being destroyed cannot guard that destruction.** Any fix of this
shape fails the same way.

## Constraints

- The coordination record must live OUTSIDE `CACHE_DIR`.
- It must self-expire. A permanent tombstone silently disables auto-update for
  the life of the machine, which is a worse failure than 41 MB of residue.
- Must not penalise npm-only users, who have no plugin and no teardown.
- Must not make `uninstall` block on a 30–60 s download.

## Proposed shape

A short-TTL tombstone as a sibling of the cache directory, e.g.
`~/.cache/code-graph.uninstalled`, holding a timestamp.

- `uninstall` writes it immediately BEFORE `removeCacheResidue()`.
- Writers check it and abort when it exists and is younger than the TTL:
  - `downloadBinary`, before `fs.mkdirSync(BINARY_CACHE_DIR)` — covers arm 1.
  - `promoteVerifiedBinary`, before `fs.renameSync` — covers arm 2, and lets the
    `finally` block clean the tmp file it already owns.
- TTL ≈ 5 min: longer than any teardown, shorter than any interval over which a
  stale tombstone could matter. A reinstall inside the window merely skips one
  update check, and the install itself supplies the binary.
- A later `install`/`adopt` may clear it eagerly; the TTL is the backstop, not
  the primary mechanism.

## Success criteria

1. Arm 1 (teardown at t=0.2 s and t=2.0 s of an `auto-update check --force`)
   leaves no `~/.cache/code-graph`. Currently 41 MB, 2/2.
2. Arm 2 (teardown after curl has created its tmp) leaves no
   `~/.cache/code-graph`. Currently `update-state.json`, 1/1.
3. A tombstone older than the TTL does not suppress a legitimate update — assert
   with an injected clock, not a sleep.
4. With no tombstone present, `auto-update` behaviour is byte-identical to today.
5. Both arms get a test. Arm 2 is deterministic (create the tmp, then delete the
   directory) and needs no timing. Arm 1 needs a seam — inject the tombstone
   check, or drive `downloadBinary` directly — rather than a sleep race.

## Open questions

- Should `doctor` report an orphan `CACHE_DIR` with no plugin and no npm install,
  and offer to reclaim it? That closes the residual case (another live session's
  updater) without any cross-process protocol, and may be the better first move.
- Is `statusline-registry.json` returning a second, separate ordering problem, or
  purely collateral of the same re-creation? Not investigated.

# Change log

- r1 (2026-09-13) — filed from the full-lifecycle QA pass. Both arms measured;
  the install-lock approach prototyped and refuted before filing.
