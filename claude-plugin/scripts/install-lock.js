'use strict';
// Inter-process install lock. N concurrently-opened sessions each spawn a
// launcher / auto-update process; without a lock they ran parallel
// `npm install -g` against one global prefix (npm's staging dir is not
// concurrency-safe → EEXIST/ENOTEMPTY tree corruption) and clobbered each
// other's update-state counters. O_EXCL create is the atomic primitive; a lock
// whose owner pid is dead or whose file is older than staleMs is reclaimed
// (crashed installer must not wedge every future session).
const fs = require('fs');
const path = require('path');

const STALE_MS = 10 * 60 * 1000; // > the longest install step (npm 180s heal timeout)

function lockIsStale(lockPath, staleMs) {
  try {
    const age = Date.now() - fs.statSync(lockPath).mtimeMs;
    if (age > staleMs) return true;
    const info = JSON.parse(fs.readFileSync(lockPath, 'utf8'));
    if (!info || !Number.isInteger(info.pid)) return false; // unreadable → trust age only
    try { process.kill(info.pid, 0); return false; }        // owner alive
    catch (e) { return e.code !== 'EPERM'; }                // EPERM = alive, not ours
  } catch {
    return false; // raced away / unreadable — treat as held; age check re-runs next attempt
  }
}

/**
 * Try to take the lock, saying WHY when it fails.
 *
 * Returns `{ ok: true, release() }`, `{ ok: false, reason: 'busy' }` when a live
 * peer holds it, or `{ ok: false, reason: 'unavailable', error }` when the lock
 * could not be created at all (unwritable parent, EPERM, EROFS).
 *
 * The distinction is not cosmetic, and `acquireLock`'s single `null` is why this
 * exists. A caller that is about to destroy something needs opposite behaviour
 * in the two cases: `busy` means a peer is already doing this work, so standing
 * down is correct and safe; `unavailable` means NO mutual exclusion is possible
 * here, so a destructive step must not proceed on the assumption that it has
 * any. Both used to arrive as `null`. And the reason must come from the syscall
 * — an `existsSync` after the fact cannot tell "a peer created it between my
 * call and my check" from "my create failed for its own reasons".
 *
 * Never throws, never blocks.
 */
function tryAcquireLock(lockPath, { staleMs = STALE_MS } = {}) {
  try {
    fs.mkdirSync(path.dirname(lockPath), { recursive: true });
  } catch (error) {
    return { ok: false, reason: 'unavailable', error };
  }
  for (let attempt = 0; attempt < 2; attempt++) {
    try {
      const fd = fs.openSync(lockPath, 'wx');
      fs.writeSync(fd, JSON.stringify({ pid: process.pid, at: new Date().toISOString() }));
      fs.closeSync(fd);
      return { ok: true, release: () => { try { fs.unlinkSync(lockPath); } catch { /* ok */ } } };
    } catch (e) {
      if (!e || e.code !== 'EEXIST') return { ok: false, reason: 'unavailable', error: e };
      if (!lockIsStale(lockPath, staleMs)) return { ok: false, reason: 'busy' };
      try { fs.unlinkSync(lockPath); } catch { /* another reclaimer won — retry loop */ }
    }
  }
  // Both attempts lost the reclaim race: someone else is live and holding it.
  return { ok: false, reason: 'busy' };
}

/**
 * Try to take the lock. Returns `{ release() }` on success, null when another
 * live process holds it. Never throws, never blocks.
 *
 * Thin wrapper over `tryAcquireLock` — one implementation, so the two entry
 * points cannot drift on which errno counts as "held".
 */
function acquireLock(lockPath, opts = {}) {
  const r = tryAcquireLock(lockPath, opts);
  return r.ok ? { release: r.release } : null;
}

module.exports = { acquireLock, tryAcquireLock, STALE_MS };
