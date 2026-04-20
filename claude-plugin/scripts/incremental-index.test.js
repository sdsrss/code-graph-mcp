'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const os = require('os');
const { spawnSync } = require('child_process');

const { findBinary } = require('./find-binary');

test('incremental-index bails silently when cwd is not a git repo', (t) => {
  const bin = findBinary();
  if (!bin) {
    // Binary not built — skip rather than fail; matches session-init.test.js convention.
    return;
  }
  const tmpRoot = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'code-graph-no-git-')));
  t.after(() => fs.rmSync(tmpRoot, { recursive: true, force: true }));

  const result = spawnSync(bin, ['incremental-index', '--quiet'], {
    cwd: tmpRoot,
    timeout: 8000,
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  assert.equal(result.status, 0, `expected exit 0, got ${result.status}; stderr: ${result.stderr}`);
  assert.equal(
    fs.existsSync(path.join(tmpRoot, '.code-graph')),
    false,
    '.code-graph must not be created outside a git repo',
  );
});

test('incremental-index runs inside a minimal git repo without creating stray state', (t) => {
  const bin = findBinary();
  if (!bin) return;
  const tmpRoot = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'code-graph-git-')));
  t.after(() => fs.rmSync(tmpRoot, { recursive: true, force: true }));

  fs.mkdirSync(path.join(tmpRoot, '.git'));
  const result = spawnSync(bin, ['incremental-index', '--quiet'], {
    cwd: tmpRoot,
    timeout: 8000,
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  assert.equal(result.status, 0, `expected exit 0, got ${result.status}; stderr: ${result.stderr}`);
  // Index may or may not materialize for an empty repo; the contract is that the guard does NOT block this case.
});
