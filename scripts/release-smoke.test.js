'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');

const root = path.resolve(__dirname, '..');
const platformPackages = [
  'npm/linux-x64/package.json',
  'npm/linux-arm64/package.json',
  'npm/darwin-x64/package.json',
  'npm/darwin-arm64/package.json',
  'npm/win32-x64/package.json',
];

function readJson(relativePath) {
  return JSON.parse(fs.readFileSync(path.join(root, relativePath), 'utf8'));
}

function readCargoVersion() {
  const cargoToml = fs.readFileSync(path.join(root, 'Cargo.toml'), 'utf8');
  const match = cargoToml.match(/^version = "(\d+\.\d+\.\d+)"$/m);
  assert.ok(match, 'Cargo.toml must contain a package version');
  return match[1];
}

test('release artifacts keep versions in sync', () => {
  const rootPkg = readJson('package.json');
  const pluginManifest = readJson('claude-plugin/.claude-plugin/plugin.json');
  const marketplace = readJson('.claude-plugin/marketplace.json');
  const cargoVersion = readCargoVersion();
  const expectedVersion = rootPkg.version;

  assert.match(expectedVersion, /^\d+\.\d+\.\d+$/);
  assert.equal(cargoVersion, expectedVersion, 'Cargo.toml version should match package.json');
  assert.equal(pluginManifest.version, expectedVersion, 'plugin.json should match package.json');
  assert.equal(marketplace.metadata.version, expectedVersion, 'marketplace metadata version should match');
  assert.equal(marketplace.plugins[0].version, expectedVersion, 'marketplace plugin version should match');

  const optionalDeps = rootPkg.optionalDependencies || {};
  for (const packagePath of platformPackages) {
    const pkg = readJson(packagePath);
    assert.equal(pkg.version, expectedVersion, `${packagePath} version should match root package.json`);
    assert.equal(optionalDeps[pkg.name], expectedVersion, `${pkg.name} optionalDependency should match root version`);
  }
});

test('marketplace points at the plugin directory and matching plugin name', () => {
  const marketplace = readJson('.claude-plugin/marketplace.json');
  const pluginManifest = readJson('claude-plugin/.claude-plugin/plugin.json');

  assert.equal(marketplace.plugins.length, 1, 'marketplace should publish exactly one plugin entry');
  assert.equal(marketplace.plugins[0].source, './claude-plugin');
  assert.equal(marketplace.plugins[0].name, pluginManifest.name);
  assert.equal(marketplace.name, pluginManifest.name);
});

// D#279: the order and the waits live in scripts/npm-publish.js. A workflow
// step running `npm publish` itself would skip both, and a job bound shorter
// than the waits would cut the run off mid-wait.
test('npm packages are published only through scripts/npm-publish.js, inside a long enough job', () => {
  const workflows = path.join(root, '.github', 'workflows');
  for (const file of fs.readdirSync(workflows).filter((f) => /\.ya?ml$/.test(f))) {
    const code = fs
      .readFileSync(path.join(workflows, file), 'utf8')
      .replace(/\\\n\s*/g, ' ') // a shell line continuation joins its lines
      .split('\n')
      .filter((line) => !/^\s*#/.test(line));
    const direct = code.filter((line) => /\bnpm\s+publish\b/.test(line));
    assert.deepEqual(direct, [], `${file} runs npm publish directly`);
  }
  const release = fs.readFileSync(path.join(workflows, 'release.yml'), 'utf8');
  const calls = release.split('\n').filter((l) => l.includes('scripts/npm-publish.js') && !/^\s*#/.test(l));
  assert.deepEqual(calls.map((l) => l.trim()), ['run: node scripts/npm-publish.js "$VERSION"']);

  const job = release.match(/\n {2}publish:\n([\s\S]*?)\n {2}[a-z][\w-]*:\n/);
  assert.ok(job, 'release.yml has a `publish` job');
  // The step exactly: no continue-on-error, no budget override, no step bound
  // shorter than the waits, and the token npm authenticates with.
  const step = job[1].match(/\n( {6}- name: Publish to npm[^\n]*\n(?: {8}[^\n]*\n)*)/);
  assert.ok(step, 'the publish job has the "Publish to npm" step');
  assert.deepEqual(
    step[1].split('\n').filter((l) => l.trim() && !/^\s*#/.test(l)).map((l) => l.trim()),
    [
      '- name: Publish to npm (platform packages first, main package last)',
      'run: node scripts/npm-publish.js "$VERSION"',
      'env:',
      'NODE_AUTH_TOKEN: ${{ secrets.NPM_TOKEN }}',
    ],
  );
  assert.match(job[1], /\n {6}id-token: write\n/, '--provenance needs id-token: write');
  const bound = job[1].match(/^ {4}timeout-minutes: (\d+)$/m);
  assert.ok(bound, 'the publish job sets timeout-minutes');
  const { WAIT_SECONDS, MAIN_WAIT_SECONDS } = require('./npm-publish');
  const waits = (WAIT_SECONDS + MAIN_WAIT_SECONDS) / 60;
  // 10 minutes for the rest of the job (measured 162-219 s before the waits).
  assert.ok(Number(bound[1]) >= waits + 10, `publish timeout-minutes ${bound[1]} < ${waits} min of waits + 10`);
});

// Opt-in real-network smoke. Off by default (CI / local dev runs all mocked
// auto-update tests). Set CODE_GRAPH_AUTO_UPDATE_E2E=1 once per release to
// catch GitHub-API shape regressions that mocked tests will never see.
test('auto-update parses real GitHub releases/latest shape',
  { skip: process.env.CODE_GRAPH_AUTO_UPDATE_E2E !== '1' },
  async () => {
    const { fetchLatestRelease } = require('../claude-plugin/scripts/auto-update');
    // fetchLatestRelease returns null on rate-limit / network failure / parse
    // failure, so this test asserts the happy path: real shape from GH that
    // parseLatestRelease can lift into {version, tarballUrl, binaryUrl}.
    const parsed = await fetchLatestRelease();
    assert.ok(parsed, 'fetchLatestRelease returned null — likely rate-limited or GH API shape regressed');
    assert.match(parsed.version, /^\d+\.\d+\.\d+$/, `version must look semver-ish: got ${parsed.version}`);
    assert.ok(parsed.tarballUrl, 'expected tarballUrl in parsed release');
  }
);

