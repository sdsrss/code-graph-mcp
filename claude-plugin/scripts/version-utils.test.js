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

function makeFakePluginRoot(t, { withCargo = false, withTarget = false, asSymlink = false } = {}) {
  const parent = mkDir(t, 'vu-dev-');
  const pluginRoot = path.join(parent, 'claude-plugin');
  fs.mkdirSync(pluginRoot, { recursive: true });
  if (withCargo) fs.writeFileSync(path.join(parent, 'Cargo.toml'), '[package]\nname = "x"\n');
  if (withTarget) fs.mkdirSync(path.join(parent, 'target'), { recursive: true });

  if (asSymlink) {
    const real = path.join(parent, 'real-plugin');
    fs.mkdirSync(real, { recursive: true });
    fs.rmSync(pluginRoot, { recursive: true, force: true });
    fs.symlinkSync(real, pluginRoot);
  }
  return pluginRoot;
}

function withEnv(t, key, value) {
  const original = process.env[key];
  if (value === undefined) delete process.env[key]; else process.env[key] = value;
  t.after(() => {
    if (original === undefined) delete process.env[key]; else process.env[key] = original;
  });
}

test('isDevMode returns true in source repo when Cargo.toml AND target/ both exist', (t) => {
  const { isDevMode } = require('./version-utils');
  withEnv(t, 'CODE_GRAPH_DEV', undefined);
  const pluginRoot = makeFakePluginRoot(t, { withCargo: true, withTarget: true });
  assert.equal(isDevMode(pluginRoot), true);
});

test('isDevMode returns false for marketplace clone (Cargo.toml without target/)', (t) => {
  const { isDevMode } = require('./version-utils');
  withEnv(t, 'CODE_GRAPH_DEV', undefined);
  // Marketplace clones the full repo (Cargo.toml is git-tracked) but `target/`
  // is gitignored, so a fresh marketplace install has no target/. This is the
  // exact misclassification fixed for issue #12.
  const pluginRoot = makeFakePluginRoot(t, { withCargo: true, withTarget: false });
  assert.equal(isDevMode(pluginRoot), false);
});

test('isDevMode returns false for fully unrelated install (no Cargo.toml, no target/)', (t) => {
  const { isDevMode } = require('./version-utils');
  withEnv(t, 'CODE_GRAPH_DEV', undefined);
  const pluginRoot = makeFakePluginRoot(t);
  assert.equal(isDevMode(pluginRoot), false);
});

test('isDevMode honors CODE_GRAPH_DEV=1 even when neither Cargo.toml nor target/ exist', (t) => {
  const { isDevMode } = require('./version-utils');
  withEnv(t, 'CODE_GRAPH_DEV', '1');
  const pluginRoot = makeFakePluginRoot(t);
  assert.equal(isDevMode(pluginRoot), true);
});

test('isDevMode returns true when plugin root is a symlink', (t) => {
  const { isDevMode } = require('./version-utils');
  withEnv(t, 'CODE_GRAPH_DEV', undefined);
  const pluginRoot = makeFakePluginRoot(t, { asSymlink: true });
  assert.equal(isDevMode(pluginRoot), true);
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
