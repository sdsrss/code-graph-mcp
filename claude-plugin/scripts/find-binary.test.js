'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');

const { globalNodeModulesCandidates, nvmNodeModulesDirs, findPlatformBinary, createVersionGate,
        BINARY_NAME, compareVersions, getPackageVersion, isCachedBinaryFresh,
        unsupportedPlatformHint } = require('./find-binary');

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

test('NPM_CONFIG_PREFIX-derived path ranks BEFORE the execPath derivation', (t) => {
  // When the user sets NPM_CONFIG_PREFIX, `npm install -g` installs THERE — it
  // is the authoritative global root and must outrank the execPath-derived
  // (nvm) prefix. The old order let a stale relic in the nvm prefix shadow the
  // user's real prefix (and made this file's findPlatformBinary test flaky on
  // machines with an old global install).
  const original = process.env.NPM_CONFIG_PREFIX;
  process.env.NPM_CONFIG_PREFIX = '/tmp/fake-npm-prefix';
  t.after(() => {
    if (original === undefined) delete process.env.NPM_CONFIG_PREFIX;
    else process.env.NPM_CONFIG_PREFIX = original;
  });

  const candidates = globalNodeModulesCandidates();
  const envDerived = process.platform === 'win32'
    ? path.join('/tmp/fake-npm-prefix', 'node_modules')
    : path.join('/tmp/fake-npm-prefix', 'lib', 'node_modules');
  const nodeBinDir = path.dirname(process.execPath);
  const execDerived = process.platform === 'win32'
    ? path.join(nodeBinDir, 'node_modules')
    : path.resolve(nodeBinDir, '..', 'lib', 'node_modules');
  assert.ok(candidates.indexOf(envDerived) < candidates.indexOf(execDerived),
    `env-derived must precede execPath-derived: ${JSON.stringify(candidates)}`);
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

// ── nvmNodeModulesDirs: per-node global prefixes (relic detection) ──────────

test('nvmNodeModulesDirs enumerates existing per-node global module dirs (injected base)', (t) => {
  const base = mkDir(t, 'nvm-base-');
  fs.mkdirSync(path.join(base, 'v24.18.0', 'lib', 'node_modules'), { recursive: true });
  fs.mkdirSync(path.join(base, 'v24.11.1', 'lib', 'node_modules'), { recursive: true });
  fs.writeFileSync(path.join(base, 'alias-file'), 'x'); // non-dir entry → ignored
  const got = nvmNodeModulesDirs(base).sort();
  assert.deepEqual(got, [
    path.join(base, 'v24.11.1', 'lib', 'node_modules'),
    path.join(base, 'v24.18.0', 'lib', 'node_modules'),
  ].sort());
});

test('nvmNodeModulesDirs returns [] when the nvm base is absent (no nvm installed)', () => {
  assert.deepEqual(nvmNodeModulesDirs('/definitely/not/a/real/nvm/base'), []);
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

test('getPackageVersion falls back to .claude-plugin/plugin.json (marketplace layout)', (t) => {
  // The plugin cache ships only the claude-plugin subtree: no package.json two
  // levels above scripts/. Before the fallback, getPackageVersion() returned
  // null there and every version gate silently disarmed (first-candidate-wins),
  // re-opening the relic-shadowing incident for all marketplace installs.
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cgmcp-mkt-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(path.join(root, 'scripts'), { recursive: true });
  fs.mkdirSync(path.join(root, '.claude-plugin'), { recursive: true });
  // Copy find-binary.js plus whatever it actually requires, walking `./x`
  // requires transitively. The hand-written list this replaces went stale the
  // moment find-binary picked up a new sibling (proc-opts.js) and failed as
  // "Cannot find module" inside the fixture, far from the change — the same
  // shape as the copy-list in scripts/install-e2e.test.js.
  const copyWithLocalDeps = (entry, destDir, seen = new Set()) => {
    const name = path.basename(entry);
    if (seen.has(name)) return seen;
    seen.add(name);
    fs.copyFileSync(entry, path.join(destDir, name));
    for (const m of fs.readFileSync(entry, 'utf8')
      .matchAll(/require\(\s*['"]\.\/([\w.-]+?)(?:\.js)?['"]\s*\)/g)) {
      copyWithLocalDeps(path.join(path.dirname(entry), `${m[1]}.js`), destDir, seen);
    }
    return seen;
  };
  const copied = copyWithLocalDeps(path.join(__dirname, 'find-binary.js'), path.join(root, 'scripts'));
  assert.ok(copied.size >= 2, `fixture must carry find-binary.js plus its deps, got: ${[...copied].join(', ')}`);
  fs.writeFileSync(path.join(root, '.claude-plugin', 'plugin.json'),
    JSON.stringify({ name: 'code-graph-mcp', version: '9.8.7' }));
  const script = `process.stdout.write(String(require(${
    JSON.stringify(path.join(root, 'scripts', 'find-binary.js'))}).getPackageVersion()))`;
  const out = require('child_process').execFileSync(process.execPath, ['-e', script], { encoding: 'utf8' });
  assert.equal(out, '9.8.7');
});

// ─── isCachedBinaryFresh: disk cache version-check (mem #8454) ────────────
//
// Builds a fake binary that responds to `--version` with a controllable
// string. process.execPath (node itself) won't do — we need a binary
// whose --version line we control. Smallest approach: shell wrapper.

function buildFakeBinary(t, versionLine) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cgmcp-fake-bin-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const binPath = path.join(dir, BINARY_NAME);
  // readBinaryVersion parses "code-graph-mcp X.Y.Z" via the binary's first
  // stdout line on `--version`. Shell wrapper is simpler than compiling.
  const script = process.platform === 'win32'
    ? `@echo off\r\necho ${versionLine}\r\n`
    : `#!/bin/sh\necho '${versionLine}'\n`;
  fs.writeFileSync(binPath, script);
  if (process.platform !== 'win32') fs.chmodSync(binPath, 0o755);
  return binPath;
}

test('isCachedBinaryFresh: cache binary version >= pkg → fresh', (t) => {
  const bin = buildFakeBinary(t, 'code-graph-mcp 9.9.9');
  assert.equal(isCachedBinaryFresh(bin, '0.25.0'), true);
});

test('isCachedBinaryFresh: cache binary version equals pkg → fresh', (t) => {
  const bin = buildFakeBinary(t, 'code-graph-mcp 0.25.0');
  assert.equal(isCachedBinaryFresh(bin, '0.25.0'), true);
});

test('isCachedBinaryFresh: cache binary version < pkg → stale (THE BUG)', (t) => {
  // Reproduces mem #8454: cache pointed at bin/code-graph-mcp v0.5.28
  // while pkg was v0.25.0 → cache was returned silently with no
  // version-check, shadowing the installed 0.25.0 platform binary.
  // After this fix, returns false → caller clears cache + falls through.
  const bin = buildFakeBinary(t, 'code-graph-mcp 0.5.28');
  assert.equal(isCachedBinaryFresh(bin, '0.25.0'), false);
});

test('isCachedBinaryFresh: missing pkg version → permissive (trust cache)', (t) => {
  // Caller couldn't read package.json; refusing the cache would leave us
  // with nothing. Better to trust the one path we have.
  const bin = buildFakeBinary(t, 'code-graph-mcp 0.5.28');
  assert.equal(isCachedBinaryFresh(bin, null), true);
  assert.equal(isCachedBinaryFresh(bin, ''), true);
});

test('isCachedBinaryFresh: unreadable cache binary version → permissive', (t) => {
  // Old binary that doesn't support `--version`, or output we can't
  // parse. Same permissive path as missing pkg version.
  const bin = buildFakeBinary(t, 'whatever garbage no semver here');
  assert.equal(isCachedBinaryFresh(bin, '0.25.0'), true);
});

test('isCachedBinaryFresh: cache path does not exist → not fresh', () => {
  assert.equal(isCachedBinaryFresh('/nonexistent/path/code-graph-mcp', '0.25.0'), false);
});

test('isCachedBinaryFresh: empty/null cache path → not fresh', () => {
  assert.equal(isCachedBinaryFresh('', '0.25.0'), false);
  assert.equal(isCachedBinaryFresh(null, '0.25.0'), false);
  assert.equal(isCachedBinaryFresh(undefined, '0.25.0'), false);
});

test('isCachedBinaryFresh: file basename mismatch → not fresh', (t) => {
  // realpathSync.basename check inside isNativeBinary — wrong name = not ours.
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cgmcp-wrongname-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const wrongName = path.join(dir, 'other-tool');
  fs.writeFileSync(wrongName, '#!/bin/sh\necho wrong\n');
  if (process.platform !== 'win32') fs.chmodSync(wrongName, 0o755);
  assert.equal(isCachedBinaryFresh(wrongName, '0.25.0'), false);
});

// ── unsupportedPlatformHint (actionable message for tails with no prebuilt binary) ──

test('unsupportedPlatformHint flags Alpine/musl with a source/glibc-image hint', () => {
  const hint = unsupportedPlatformHint('linux', 'x64', 'musl');
  assert.ok(hint, 'musl should produce a hint');
  assert.match(hint, /musl|Alpine/);
  assert.match(hint, /cargo install/);
});

test('unsupportedPlatformHint flags native Windows-on-ARM with emulation/source hint', () => {
  const hint = unsupportedPlatformHint('win32', 'arm64', 'glibc');
  assert.ok(hint, 'win32-arm64 should produce a hint');
  assert.match(hint, /Windows on ARM|arm64/);
  assert.match(hint, /x64|cargo install/);
});

test('unsupportedPlatformHint returns null for supported platforms', () => {
  assert.equal(unsupportedPlatformHint('linux', 'x64', 'glibc'), null);
  assert.equal(unsupportedPlatformHint('linux', 'arm64', 'glibc'), null);
  assert.equal(unsupportedPlatformHint('darwin', 'arm64', 'glibc'), null);
  assert.equal(unsupportedPlatformHint('darwin', 'x64', 'glibc'), null);
  assert.equal(unsupportedPlatformHint('win32', 'x64', 'glibc'), null);
});

// --- createVersionGate: the discovery-chain version gate ---
// The incident it pins: a 0.16.6 `npm install -g` relic in the nvm global
// node_modules was returned VERBATIM whenever the auto-update cache was one
// release behind — an ancient server on a modern schema (MCP 30s timeout).

// Real file named code-graph-mcp so isNativeBinary passes; version is injected.
function mkGateBinary(t, version, versions) {
  const dir = mkDir(t, 'version-gate-');
  const bin = path.join(dir, BINARY_NAME);
  fs.writeFileSync(bin, 'stub');
  versions.set(bin, version);
  return bin;
}

function mkGate(t, pkgVersion) {
  const versions = new Map();
  const gate = createVersionGate(pkgVersion, { readVersion: (bin) => versions.get(bin) ?? null });
  return { gate, mk: (version) => mkGateBinary(t, version, versions) };
}

test('gate accepts a current-or-newer candidate on the spot', (t) => {
  const { gate, mk } = mkGate(t, '0.101.0');
  const current = mk('0.101.0');
  assert.equal(gate.consider(current), current);
  const newer = mk('0.102.0');
  assert.equal(gate.consider(newer), newer);
});

test('gate accepts an unverifiable candidate (no version readable / no pkg version)', (t) => {
  const { gate, mk } = mkGate(t, '0.101.0');
  const unreadable = mk(null);
  assert.equal(gate.consider(unreadable), unreadable,
    'a binary that will not report a version must not be refused — it may be the only path');

  const { gate: ungated, mk: mk2 } = mkGate(t, null);
  const anything = mk2('0.1.0');
  assert.equal(ungated.consider(anything), anything,
    'without a pkg version there is nothing to gate against');
});

test('gate holds back stale candidates; best() yields the NEWEST stale', (t) => {
  const { gate, mk } = mkGate(t, '0.101.0');
  const ancient = mk('0.16.6');   // the real-world relic
  const nearMiss = mk('0.100.0'); // one release behind
  assert.equal(gate.consider(nearMiss), null, 'stale must not be returned inline');
  assert.equal(gate.consider(ancient), null);
  assert.equal(gate.best(), nearMiss,
    'fallback must be the newest stale candidate, regardless of consideration order');
});

test('gate ignores non-binaries and best() is null when nothing was considered', (t) => {
  const { gate } = mkGate(t, '0.101.0');
  assert.equal(gate.consider(path.join(os.tmpdir(), 'does-not-exist', BINARY_NAME)), null);
  assert.equal(gate.best(), null);
});

test('compareVersions: pre-release sorts below its release (unified impl)', () => {
  // The two prior divergent copies disagreed here (NaN→0 vs parseInt
  // truncation); the canonical version-utils impl is pre-release-aware.
  assert.equal(compareVersions('1.2.3-rc1', '1.2.3'), -1);
  assert.equal(compareVersions('1.2.3', '1.2.3-rc1'), 1);
  assert.equal(compareVersions('1.2.3-rc1', '1.2.3-rc2'), -1);
  assert.equal(compareVersions('1.2.4-rc1', '1.2.3'), 1); // numeric triple still dominates
});

test('platformBinaryCandidates rejects truncated npm platform binaries (<1MB)', (t) => {
  // An interrupted npm install can leave a partial binary with the right
  // name; unlike the GitHub path (size+sha+exec gates) this tier had none.
  const prefix = fs.mkdtempSync(path.join(os.tmpdir(), 'cgmcp-sizegate-'));
  t.after(() => fs.rmSync(prefix, { recursive: true, force: true }));
  const pkgDir = path.join(prefix, 'lib', 'node_modules', '@sdsrs',
    `code-graph-${process.platform}-${process.arch}`);
  fs.mkdirSync(pkgDir, { recursive: true });
  const bin = path.join(pkgDir, BINARY_NAME);

  const withPrefix = (fn) => {
    const prev = process.env.NPM_CONFIG_PREFIX;
    process.env.NPM_CONFIG_PREFIX = prefix;
    try { return fn(); } finally {
      if (prev === undefined) delete process.env.NPM_CONFIG_PREFIX;
      else process.env.NPM_CONFIG_PREFIX = prev;
    }
  };

  fs.writeFileSync(bin, 'truncated');            // ~9 bytes — a torn install
  const { platformBinaryCandidates } = require('./find-binary');
  const rejected = withPrefix(() => platformBinaryCandidates());
  assert.ok(!rejected.includes(bin), 'truncated binary must not be a candidate');

  fs.writeFileSync(bin, Buffer.alloc(1_100_000)); // plausible release size
  const accepted = withPrefix(() => platformBinaryCandidates());
  assert.ok(accepted.includes(bin), 'plausibly-sized binary is a candidate');
});

test('platformBinaryCandidates finds the NESTED npm optionalDependency layout', (t) => {
  // `npm install -g @sdsrs/code-graph` does not always hoist the platform
  // optionalDependency to the global root: npm 12 leaves it under the shell
  // package's own node_modules. Probing only the hoisted spelling made a
  // SUCCESSFUL npm install read as "did not yield a binary", so the launcher
  // re-downloaded ~41MB from GitHub AND skipped recordGlobalInstall() — which
  // is the marker `uninstall` needs to know it owns those global packages.
  // Observed live 2026-08-17 with npm 12.0.1 in a sandboxed HOME.
  const prefix = fs.mkdtempSync(path.join(os.tmpdir(), 'cgmcp-nested-'));
  t.after(() => fs.rmSync(prefix, { recursive: true, force: true }));
  const nestedDir = path.join(prefix, 'lib', 'node_modules', '@sdsrs', 'code-graph',
    'node_modules', '@sdsrs', `code-graph-${process.platform}-${process.arch}`);
  fs.mkdirSync(nestedDir, { recursive: true });
  const bin = path.join(nestedDir, BINARY_NAME);
  fs.writeFileSync(bin, Buffer.alloc(1_100_000));

  const prev = process.env.NPM_CONFIG_PREFIX;
  process.env.NPM_CONFIG_PREFIX = prefix;
  let found;
  try {
    const { platformBinaryCandidates } = require('./find-binary');
    found = platformBinaryCandidates();
  } finally {
    if (prev === undefined) delete process.env.NPM_CONFIG_PREFIX;
    else process.env.NPM_CONFIG_PREFIX = prev;
  }
  assert.ok(found.includes(bin), `nested platform binary must be a candidate; got ${JSON.stringify(found)}`);
});
