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

const { acquireLock } = require('./install-lock');

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
