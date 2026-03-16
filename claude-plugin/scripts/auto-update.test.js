'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');

const {
  commandExists,
  fetchLatestRelease,
  getExtractedPluginVersion,
  parseLatestRelease,
  readBinaryVersion,
  promoteVerifiedBinary,
} = require('./auto-update');

function mkDir(prefix) {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix));
}

test('getExtractedPluginVersion reads extracted plugin manifest version', () => {
  const root = mkDir('code-graph-plugin-');
  const manifest = path.join(root, '.claude-plugin', 'plugin.json');
  fs.mkdirSync(path.dirname(manifest), { recursive: true });
  fs.writeFileSync(manifest, JSON.stringify({ version: '1.2.3' }, null, 2));
  assert.equal(getExtractedPluginVersion(root), '1.2.3');
});

function writeFakeBinary(filePath, version) {
  const script = [
    '#!/usr/bin/env bash',
    'if [ "$1" = "--version" ]; then',
    `  echo "code-graph-mcp ${version}"`,
    '  exit 0',
    'fi',
    'exit 0',
    `# ${'x'.repeat(1_100_000)}`,
    '',
  ].join('\n');
  fs.writeFileSync(filePath, script);
  fs.chmodSync(filePath, 0o755);
}

test('promoteVerifiedBinary accepts a runnable binary with the expected version', () => {
  const dir = mkDir('code-graph-bin-');
  const tmp = path.join(dir, 'code-graph-mcp.tmp');
  const dst = path.join(dir, 'code-graph-mcp');
  writeFakeBinary(tmp, '1.2.3');

  assert.equal(readBinaryVersion(tmp), '1.2.3');
  assert.equal(promoteVerifiedBinary(tmp, dst, '1.2.3'), true);
  assert.equal(fs.existsSync(tmp), false);
  assert.equal(fs.existsSync(dst), true);
});

test('promoteVerifiedBinary rejects binaries with mismatched version', () => {
  const dir = mkDir('code-graph-bin-');
  const tmp = path.join(dir, 'code-graph-mcp.tmp');
  const dst = path.join(dir, 'code-graph-mcp');
  writeFakeBinary(tmp, '1.2.2');

  assert.equal(promoteVerifiedBinary(tmp, dst, '1.2.3'), false);
  assert.equal(fs.existsSync(tmp), false);
  assert.equal(fs.existsSync(dst), false);
});

test('parseLatestRelease selects the matching platform asset', () => {
  const latest = parseLatestRelease({
    tag_name: 'v1.2.3',
    tarball_url: 'https://example.com/tarball.tgz',
    assets: [
      { name: 'code-graph-mcp-linux-x64', browser_download_url: 'https://example.com/linux-x64' },
      { name: 'other', browser_download_url: 'https://example.com/other' },
    ],
  }, 'code-graph-mcp-linux-x64');

  assert.deepEqual(latest, {
    version: '1.2.3',
    tarballUrl: 'https://example.com/tarball.tgz',
    binaryUrl: 'https://example.com/linux-x64',
  });
});

// ── commandExists ──────────────────────────────────────────

test('commandExists returns true for a known command (node)', () => {
  assert.equal(commandExists('node'), true);
});

test('commandExists returns false for a non-existent command', () => {
  assert.equal(commandExists('__nonexistent_cmd_xyz_12345__'), false);
});

test('fetchLatestRelease parses JSON without relying on global fetch', async () => {
  const latest = await fetchLatestRelease(async () => ({
    statusCode: 200,
    body: JSON.stringify({
      tag_name: 'v2.0.0',
      tarball_url: 'https://example.com/release.tgz',
      assets: [
        { name: 'code-graph-mcp-linux-x64', browser_download_url: 'https://example.com/bin' },
      ],
    }),
  }));

  assert.equal(latest.version, '2.0.0');
  assert.equal(latest.tarballUrl, 'https://example.com/release.tgz');
});