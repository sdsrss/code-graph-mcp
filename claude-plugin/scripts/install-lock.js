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
 * Try to take the lock. Returns `{ release() }` on success, null when another
 * live process holds it. Never throws, never blocks.
 */
function acquireLock(lockPath, { staleMs = STALE_MS } = {}) {
  try { fs.mkdirSync(path.dirname(lockPath), { recursive: true }); } catch { return null; }
  for (let attempt = 0; attempt < 2; attempt++) {
    try {
      const fd = fs.openSync(lockPath, 'wx');
      fs.writeSync(fd, JSON.stringify({ pid: process.pid, at: new Date().toISOString() }));
      fs.closeSync(fd);
      return { release: () => { try { fs.unlinkSync(lockPath); } catch { /* ok */ } } };
    } catch (e) {
      if (!e || e.code !== 'EEXIST') return null;
      if (!lockIsStale(lockPath, staleMs)) return null;
      try { fs.unlinkSync(lockPath); } catch { /* another reclaimer won — retry loop */ }
    }
  }
  return null;
}

module.exports = { acquireLock, STALE_MS };
