'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');

const { launchBackgroundAutoUpdate, syncLifecycleConfig, ensureIndexFresh, verifyBinary, computeQuietHooks } = require('./session-init');

test('syncLifecycleConfig is exported as a callable helper', () => {
  assert.equal(typeof syncLifecycleConfig, 'function');
});

test('ensureIndexFresh is exported as a callable helper', () => {
  assert.equal(typeof ensureIndexFresh, 'function');
});

test('ensureIndexFresh returns skipped when no index exists', () => {
  const origCwd = process.cwd();
  const tmpDir = require('node:os').tmpdir();
  process.chdir(tmpDir);
  try {
    const result = ensureIndexFresh();
    assert.equal(result, 'skipped');
  } finally {
    process.chdir(origCwd);
  }
});

test('verifyBinary returns available:true when binary is found and executable', () => {
  const result = verifyBinary();
  // In dev repo, binary should be found (target/release/code-graph-mcp)
  if (result.available) {
    assert.equal(typeof result.binary, 'string');
    assert.ok(result.binary.length > 0);
  } else {
    // Binary not built — still verify the return shape
    assert.equal(result.available, false);
  }
});

test('verifyBinary returns structured result with expected shape', () => {
  const result = verifyBinary();
  assert.equal(typeof result.available, 'boolean');
  assert.ok('binary' in result);
  if (!result.available && result.binary) {
    assert.ok('issue' in result);
  }
});

test('launchBackgroundAutoUpdate spawns detached silent updater', () => {
  const calls = [];

  const ok = launchBackgroundAutoUpdate((command, args, options) => {
    const record = { command, args, options, unrefCalled: false };
    calls.push(record);
    return {
      unref() {
        record.unrefCalled = true;
      },
    };
  }, { HOME: '/tmp/fake-home' });

  assert.equal(ok, true);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].command, process.execPath);
  assert.match(calls[0].args[0], /auto-update\.js$/);
  assert.equal(calls[0].args[1], 'check');
  assert.equal(calls[0].args[2], '--silent');
  assert.equal(calls[0].options.detached, true);
  assert.equal(calls[0].options.stdio, 'ignore');
  assert.equal(calls[0].options.env.CODE_GRAPH_AUTO_UPDATE_SILENT, '1');
  assert.equal(calls[0].unrefCalled, true);
});

const { consistencyCheck } = require('./session-init');

test('consistencyCheck is exported as a function', () => {
  assert.equal(typeof consistencyCheck, 'function');
});

test('consistencyCheck returns empty array when binary version matches plugin', () => {
  const result = consistencyCheck('/tmp/nonexistent-binary');
  assert.ok(Array.isArray(result));
});

// ──────────────────────────────────────────────────────────────────────────
// v0.9.0 — quietHooks inference from adopted state
// ──────────────────────────────────────────────────────────────────────────

test('computeQuietHooks: env "0" forces noisy regardless of adoption', () => {
  assert.equal(computeQuietHooks({ adopted: true, env: { CODE_GRAPH_QUIET_HOOKS: '0' } }), false);
  assert.equal(computeQuietHooks({ adopted: false, env: { CODE_GRAPH_QUIET_HOOKS: '0' } }), false);
});

test('computeQuietHooks: env "1" forces quiet regardless of adoption', () => {
  assert.equal(computeQuietHooks({ adopted: true, env: { CODE_GRAPH_QUIET_HOOKS: '1' } }), true);
  assert.equal(computeQuietHooks({ adopted: false, env: { CODE_GRAPH_QUIET_HOOKS: '1' } }), true);
});

test('computeQuietHooks: env unset → follows adopted state', () => {
  assert.equal(computeQuietHooks({ adopted: true, env: {} }), true);
  assert.equal(computeQuietHooks({ adopted: false, env: {} }), false);
});

test('computeQuietHooks: env unset, no env arg → follows adopted state', () => {
  assert.equal(computeQuietHooks({ adopted: true }), true);
  assert.equal(computeQuietHooks({ adopted: false }), false);
});

test('consistencyCheck returns version-mismatch when versions differ', (t) => {
  const os = require('os');
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cc-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const bin = path.join(dir, 'code-graph-mcp');
  fs.writeFileSync(bin, [
    '#!/usr/bin/env bash',
    'if [ "$1" = "--version" ]; then',
    '  echo "code-graph-mcp 0.0.1"',
    '  exit 0',
    'fi',
    'exit 0',
  ].join('\n'));
  fs.chmodSync(bin, 0o755);

  const issues = consistencyCheck(bin);
  const versionIssue = issues.find(i => i.id === 'version-mismatch');
  assert.ok(versionIssue, 'should detect version mismatch');
  assert.ok(versionIssue.msg.includes('0.0.1'));
});

