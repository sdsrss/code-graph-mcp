'use strict';
// Tests for install-lock.js — the inter-process gate that stops N concurrent
// sessions from running parallel `npm install -g` / binary downloads.
//
// Run: node --test claude-plugin/scripts/install-lock.test.js
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');

const { acquireLock, tryAcquireLock } = require('./install-lock');

function mkLockPath(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-lock-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return path.join(dir, 'install.lock');
}

test('acquire → contend → release → reacquire', (t) => {
  const lockPath = mkLockPath(t);
  const first = acquireLock(lockPath);
  assert.ok(first, 'first acquire succeeds');
  assert.equal(acquireLock(lockPath), null, 'second acquire fails while held (own pid is alive)');
  first.release();
  assert.equal(fs.existsSync(lockPath), false, 'release removes the lock file');
  const again = acquireLock(lockPath);
  assert.ok(again, 'reacquire after release succeeds');
  again.release();
});

test('stale lock from a dead pid is reclaimed regardless of age', (t) => {
  const lockPath = mkLockPath(t);
  // A pid that cannot exist (beyond kernel.pid_max defaults) → owner is dead.
  fs.writeFileSync(lockPath, JSON.stringify({ pid: 2 ** 30, at: 'x' }));
  const lock = acquireLock(lockPath);
  assert.ok(lock, 'dead-owner lock is reclaimed');
  lock.release();
});

test('over-age lock is reclaimed even without readable owner info', (t) => {
  const lockPath = mkLockPath(t);
  fs.writeFileSync(lockPath, 'not-json');
  const old = (Date.now() - 11 * 60 * 1000) / 1000;
  fs.utimesSync(lockPath, old, old);
  const lock = acquireLock(lockPath);
  assert.ok(lock, 'over-staleMs lock is reclaimed');
  lock.release();
});

test('fresh unreadable lock is respected (treated as held)', (t) => {
  const lockPath = mkLockPath(t);
  fs.writeFileSync(lockPath, 'not-json'); // fresh mtime, no pid to probe
  assert.equal(acquireLock(lockPath), null);
});

test('tryAcquireLock separates "a peer holds it" from "it could not be created"', (t) => {
  // `acquireLock`'s single `null` conflates the two, and a caller about to
  // destroy something needs opposite behaviour in each: a peer holding the lock
  // means the work is already being done and standing down is safe, while an
  // uncreatable lock means NO mutual exclusion exists here and a destructive
  // step must not assume it has any. removeCacheResidue() is that caller.
  const lockPath = mkLockPath(t);

  const held = tryAcquireLock(lockPath);
  assert.equal(held.ok, true, 'a free lock is taken');

  const contended = tryAcquireLock(lockPath);
  assert.equal(contended.ok, false);
  assert.equal(contended.reason, 'busy', 'a live holder reads as busy, not as a failure');
  held.release();

  // Unavailable: the lock's parent cannot be made a directory, because a
  // regular file already occupies that path. The reason has to come from the
  // syscall — an existsSync afterwards could not tell this apart from a peer
  // that created the file between our call and our check.
  const blocker = path.join(path.dirname(lockPath), 'blocker');
  fs.writeFileSync(blocker, 'not a directory');
  const broken = tryAcquireLock(path.join(blocker, 'nested', 'install.lock'));
  assert.equal(broken.ok, false);
  assert.equal(broken.reason, 'unavailable', 'a lock that cannot exist is not "busy"');
  assert.ok(broken.error, 'and it carries the errno that said so');
});

test('acquireLock keeps its contract on top of tryAcquireLock', (t) => {
  // One implementation, two entry points: the wrapper must not develop its own
  // idea of which errno counts as held.
  const lockPath = mkLockPath(t);
  const first = acquireLock(lockPath);
  assert.ok(first && typeof first.release === 'function');
  assert.equal(acquireLock(lockPath), null, 'busy still reads as null');
  first.release();
  const blocker = path.join(path.dirname(lockPath), 'blocker2');
  fs.writeFileSync(blocker, 'not a directory');
  assert.equal(
    acquireLock(path.join(blocker, 'nested', 'install.lock')), null,
    'and so does unavailable',
  );
});
