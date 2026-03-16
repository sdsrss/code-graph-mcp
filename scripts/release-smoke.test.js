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

