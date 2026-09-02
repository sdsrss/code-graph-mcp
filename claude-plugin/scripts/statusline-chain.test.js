'use strict';
// JS-10 (audit 2026-08-29): `statusline-chain.js` is the public registration
// API for third-party plugins — register / unregister / list — and it had no
// user-facing documentation at all. A repo-wide grep for it hit source comments
// only, so the only way to find it was to read the plugin's source.
//
// The README now documents it. This test is what keeps that documentation
// honest: every fact it states is re-derived here from the running CLI and from
// lifecycle's own constants, not transcribed.
const test = require('node:test');
const assert = require('node:assert/strict');
const { spawnSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

const CLI = path.join(__dirname, 'statusline-chain.js');
const README = path.join(__dirname, '..', '..', 'README.md');

function readmeSection() {
  const src = fs.readFileSync(README, 'utf8');
  const start = src.indexOf('### Sharing the statusline with another plugin');
  assert.ok(start > 0, 'the README section moved or was removed — update this guard');
  const rest = src.slice(start + 1);
  const end = rest.indexOf('\n### ');
  return end === -1 ? rest : rest.slice(0, end);
}

test('the documented exit codes are the ones the CLI returns', () => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-chain-doc-'));
  const run = (...args) => spawnSync(process.execPath, [CLI, ...args], {
    encoding: 'utf8',
    env: { ...process.env, HOME: home, USERPROFILE: home, CLAUDE_CONFIG_DIR: path.join(home, '.claude') },
  });
  try {
    const doc = readmeSection();
    assert.match(doc, /Reserved ids \| `code-graph`, `_previous`/);

    // 2 — a reserved id, for BOTH reserved names.
    for (const id of ['code-graph', '_previous']) {
      const res = run('register', id, 'echo hi');
      assert.equal(res.status, 2, `${id} must be refused with exit 2; got ${res.status}`);
      assert.match(res.stderr, /reserved/);
    }
    // 1 — usage.
    assert.equal(run('register').status, 1, 'a missing argument is a usage error');
    assert.equal(run('nonsense').status, 1);
    // 0 — the happy paths, including the already-in-that-state ones the README
    // groups with them.
    assert.equal(run('register', 'gsd', 'echo hi', '--stdin').status, 0);
    assert.equal(run('register', 'gsd', 'echo hi', '--stdin').status, 0, 'idempotent re-register');
    assert.equal(run('list').status, 0);
    assert.match(run('list').stdout, /gsd \[stdin\]: echo hi/,
      '--stdin is what the README says it is: the provider gets the status JSON');
    assert.equal(run('unregister', 'gsd').status, 0);
    assert.equal(run('unregister', 'gsd').status, 0, 'unregistering twice is not an error');

    // 2 — a registry that exists and cannot be read. The README calls this out
    // because an installer reading exit codes must not treat it as success.
    const registry = path.join(home, '.cache', 'code-graph', 'statusline-registry.json');
    fs.mkdirSync(path.dirname(registry), { recursive: true });
    fs.writeFileSync(registry, '{ not json');
    const refused = run('register', 'gsd', 'echo hi');
    assert.equal(refused.status, 2, `an unreadable registry must refuse; got ${refused.status}`);
  } finally {
    fs.rmSync(home, { recursive: true, force: true });
  }
});

test('the documented registry paths are the ones lifecycle writes', () => {
  const doc = readmeSection();
  const src = fs.readFileSync(path.join(__dirname, 'lifecycle.js'), 'utf8');
  // Derived from the source, so a move breaks the test rather than the docs.
  assert.ok(src.includes("'statusline-registry.json'"), 'the working-copy filename moved');
  assert.ok(src.includes("'statusline-providers.json'"), 'the durable-mirror filename moved');
  assert.ok(src.includes("path.join(os.homedir(), '.cache', 'code-graph')"),
    'the cache directory moved — the README names it');
  assert.match(doc, /~\/\.cache\/code-graph\/statusline-registry\.json/);
  assert.match(doc, /~\/\.claude\/statusline-providers\.json/);
});
