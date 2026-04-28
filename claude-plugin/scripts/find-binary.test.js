'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');

const { globalNodeModulesCandidates, findPlatformBinary, BINARY_NAME,
        compareVersions, getPackageVersion } = require('./find-binary');

function mkDir(t, prefix) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
}

test('globalNodeModulesCandidates includes derivation from process.execPath', () => {
  const candidates = globalNodeModulesCandidates();
  assert.ok(candidates.length > 0, 'at least one candidate path');

  const nodeBinDir = path.dirname(process.execPath);
  const expected = process.platform === 'win32'
    ? path.join(nodeBinDir, 'node_modules')
    : path.resolve(nodeBinDir, '..', 'lib', 'node_modules');
  assert.ok(candidates.includes(expected), `expected ${expected} in ${JSON.stringify(candidates)}`);
});

test('globalNodeModulesCandidates honors NPM_CONFIG_PREFIX', (t) => {
  const original = process.env.NPM_CONFIG_PREFIX;
  process.env.NPM_CONFIG_PREFIX = '/tmp/fake-npm-prefix';
  t.after(() => {
    if (original === undefined) delete process.env.NPM_CONFIG_PREFIX;
    else process.env.NPM_CONFIG_PREFIX = original;
  });

  const candidates = globalNodeModulesCandidates();
  const expected = process.platform === 'win32'
    ? path.join('/tmp/fake-npm-prefix', 'node_modules')
    : path.join('/tmp/fake-npm-prefix', 'lib', 'node_modules');
  assert.ok(candidates.includes(expected),
    `expected NPM_CONFIG_PREFIX-derived path in candidates: ${JSON.stringify(candidates)}`);
});

test('globalNodeModulesCandidates dedupes overlapping paths', (t) => {
  const original = process.env.NPM_CONFIG_PREFIX;
  // Force NPM_CONFIG_PREFIX to match the execPath-derived prefix
  const nodeBinDir = path.dirname(process.execPath);
  const matchedPrefix = process.platform === 'win32'
    ? nodeBinDir
    : path.resolve(nodeBinDir, '..');
  process.env.NPM_CONFIG_PREFIX = matchedPrefix;
  t.after(() => {
    if (original === undefined) delete process.env.NPM_CONFIG_PREFIX;
    else process.env.NPM_CONFIG_PREFIX = original;
  });

  const candidates = globalNodeModulesCandidates();
  const seen = new Set();
  for (const c of candidates) {
    assert.ok(!seen.has(c), `duplicate candidate: ${c}`);
    seen.add(c);
  }
});

test('findPlatformBinary locates platform pkg in NPM_CONFIG_PREFIX-derived global node_modules', (t) => {
  // Mirror what `npm install -g` produces for @sdsrs/code-graph-{platform}-{arch}.
  const fakePrefix = mkDir(t, 'find-binary-test-');
  const platDir = process.platform === 'win32'
    ? path.join(fakePrefix, 'node_modules', '@sdsrs', `code-graph-${process.platform}-${process.arch}`)
    : path.join(fakePrefix, 'lib', 'node_modules', '@sdsrs', `code-graph-${process.platform}-${process.arch}`);
  fs.mkdirSync(platDir, { recursive: true });

  // Copy node executable so realpathSync(candidate)'s basename === BINARY_NAME
  // (isNativeBinary check). Plain copy, not symlink, so basename matches.
  const fakeBinary = path.join(platDir, BINARY_NAME);
  fs.copyFileSync(process.execPath, fakeBinary);
  if (process.platform !== 'win32') fs.chmodSync(fakeBinary, 0o755);

  const original = process.env.NPM_CONFIG_PREFIX;
  process.env.NPM_CONFIG_PREFIX = fakePrefix;
  t.after(() => {
    if (original === undefined) delete process.env.NPM_CONFIG_PREFIX;
    else process.env.NPM_CONFIG_PREFIX = original;
  });

  const found = findPlatformBinary();
  assert.equal(found, fakeBinary, `expected ${fakeBinary}, got ${found}`);
});

test('findPlatformBinary returns null when no platform pkg installed anywhere reachable', (t) => {
  // Point NPM_CONFIG_PREFIX at an empty dir so global probe cannot match.
  const fakePrefix = mkDir(t, 'find-binary-empty-');
  const original = process.env.NPM_CONFIG_PREFIX;
  process.env.NPM_CONFIG_PREFIX = fakePrefix;
  t.after(() => {
    if (original === undefined) delete process.env.NPM_CONFIG_PREFIX;
    else process.env.NPM_CONFIG_PREFIX = original;
  });

  // Note: this test only proves the negative if no real install of the platform
  // package is reachable via require.resolve OR any other candidate path. On a
  // dev machine that has `@sdsrs/code-graph-linux-x64` installed globally, this
  // assertion will fail — that's not a defect of the helper but of test setup.
  // Skip if a real install is detected.
  const real = findPlatformBinary();
  if (real && !real.startsWith(fakePrefix)) {
    t.skip(`real platform pkg installed at ${real}, cannot test the null path here`);
    return;
  }
  assert.equal(real, null);
});

// ─── compareVersions (B fix: cache version invalidation helper) ───────────

test('compareVersions: equal', () => {
  assert.equal(compareVersions('1.2.3', '1.2.3'), 0);
});

test('compareVersions: cache older than pkg', () => {
  // After `npm update` to 0.16.8, an auto-update cache from 0.16.7 must NOT
  // shadow the freshly-installed platform-pkg binary. Returns -1 here so
  // findBinaryUncached falls through to platform-pkg.
  assert.equal(compareVersions('0.16.7', '0.16.8'), -1);
});

test('compareVersions: cache newer than pkg', () => {
  // Auto-update may legitimately be ahead of npm pkg (cache fetched 0.17.0
  // before npm shipped it). Returns 1 → cache wins.
  assert.equal(compareVersions('0.17.0', '0.16.8'), 1);
});

test('compareVersions: minor and patch boundaries', () => {
  assert.equal(compareVersions('1.0.0', '0.999.999'), 1);
  assert.equal(compareVersions('1.10.0', '1.9.99'), 1);  // numeric, not lexical
  assert.equal(compareVersions('1.0.10', '1.0.9'), 1);
});

test('compareVersions: tolerates non-numeric / short input', () => {
  // Non-numeric → treated as 0; shorter strings padded with 0.
  assert.equal(compareVersions('1.2', '1.2.0'), 0);
  assert.equal(compareVersions('foo', '0.0.0'), 0);
});

test('getPackageVersion reads root package.json', () => {
  const v = getPackageVersion();
  assert.match(v, /^\d+\.\d+\.\d+$/, `expected semver-ish, got: ${v}`);
});
