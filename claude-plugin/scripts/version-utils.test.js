'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');

function mkDir(t, prefix) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
}

// ── readBinaryVersion ──

test('readBinaryVersion returns version from valid binary', (t) => {
  const { readBinaryVersion } = require('./version-utils');
  const dir = mkDir(t, 'vu-');
  const bin = path.join(dir, 'code-graph-mcp');
  fs.writeFileSync(bin, [
    '#!/usr/bin/env bash',
    'if [ "$1" = "--version" ]; then',
    '  echo "code-graph-mcp 1.2.3"',
    '  exit 0',
    'fi',
    'exit 0',
  ].join('\n'));
  fs.chmodSync(bin, 0o755);
  assert.equal(readBinaryVersion(bin), '1.2.3');
});

test('readBinaryVersion returns null for non-existent binary', () => {
  const { readBinaryVersion } = require('./version-utils');
  assert.equal(readBinaryVersion('/tmp/does-not-exist-binary'), null);
});

test('readBinaryVersion returns null for binary with unexpected output', (t) => {
  const { readBinaryVersion } = require('./version-utils');
  const dir = mkDir(t, 'vu-');
  const bin = path.join(dir, 'code-graph-mcp');
  fs.writeFileSync(bin, '#!/usr/bin/env bash\necho "something else"');
  fs.chmodSync(bin, 0o755);
  assert.equal(readBinaryVersion(bin), null);
});

// ── isDevMode ──

test('isDevMode returns true in source repo (Cargo.toml nearby)', () => {
  const { isDevMode } = require('./version-utils');
  // Running from source repo: __dirname/../.. has Cargo.toml → true
  assert.equal(isDevMode(), true);
});

// ── getNewestMtime ──

test('getNewestMtime returns 0 for non-existent directory', () => {
  const { getNewestMtime } = require('./version-utils');
  assert.equal(getNewestMtime('/tmp/no-such-dir-xyz'), 0);
});

test('getNewestMtime finds newest .rs file mtime', (t) => {
  const { getNewestMtime } = require('./version-utils');
  const dir = mkDir(t, 'vu-mtime-');
  const sub = path.join(dir, 'sub');
  fs.mkdirSync(sub);

  const older = path.join(dir, 'old.rs');
  const newer = path.join(sub, 'new.rs');
  fs.writeFileSync(older, 'fn old() {}');

  fs.writeFileSync(newer, 'fn new() {}');
  const futureMs = Date.now() + 1000;
  fs.utimesSync(newer, futureMs / 1000, futureMs / 1000);

  const newerMtime = fs.statSync(newer).mtimeMs;
  const result = getNewestMtime(dir, '.rs');
  assert.equal(result, newerMtime, 'should return exactly the newest file mtime');
});

test('getNewestMtime ignores non-matching extensions', (t) => {
  const { getNewestMtime } = require('./version-utils');
  const dir = mkDir(t, 'vu-ext-');
  fs.writeFileSync(path.join(dir, 'file.js'), 'hello');
  assert.equal(getNewestMtime(dir, '.rs'), 0);
});
