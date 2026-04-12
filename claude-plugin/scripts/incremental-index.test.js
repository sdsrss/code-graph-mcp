'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const os = require('os');
const { spawnSync } = require('child_process');

const { findBinary } = require('./find-binary');

test('incremental-index bails silently when cwd is not a git repo', () => {
  const bin = findBinary();
  if (!bin) {
    // Binary not built — skip rather than fail; matches session-init.test.js convention
    return;
  }
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'code-graph-no-git-'));
  try {
    const result = spawnSync(bin, ['incremental-index', '--quiet'], {
      cwd: tmpRoot,
      timeout: 8000,
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    assert.equal(result.status, 0, `expected exit 0, got ${result.status}; stderr: ${result.stderr}`);
    assert.equal(fs.existsSync(path.join(tmpRoot, '.code-graph')), false, '.code-graph should not be created outside a git repo');
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

test('incremental-index runs inside a git repo (creates or updates index)', () => {
  const bin = findBinary();
  if (!bin) return;
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'code-graph-git-'));
  try {
    fs.mkdirSync(path.join(tmpRoot, '.git'));
    const result = spawnSync(bin, ['incremental-index', '--quiet'], {
      cwd: tmpRoot,
      timeout: 8000,
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    assert.equal(result.status, 0, `expected exit 0, got ${result.status}; stderr: ${result.stderr}`);
    // Index may or may not materialize for an empty repo; the contract is just that the guard does NOT block this case
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});
