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
  // Terminate on ANY following heading, not just `### `: the next heading here
  // is `## Build from Source`, so a `### `-only cut ran 50 lines past the
  // section and let assertions be satisfied by unrelated text (pre-tag review).
  const end = rest.search(/\n#{2,3} /);
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
    for (const cmd of [['register', 'gsd', 'echo hi'], ['unregister', 'gsd'], ['list']]) {
      const refused = run(...cmd);
      assert.equal(refused.status, 2,
        `an unreadable registry must refuse \`${cmd[0]}\`; got ${refused.status}`);
      assert.doesNotMatch(refused.stdout, /\(empty\)/,
        `\`${cmd[0]}\` must not render a corrupt registry as a clean one`);
    }
  } finally {
    fs.rmSync(home, { recursive: true, force: true });
  }
});

test('the documented invocation path is one that resolves for a third party', () => {
  // Pre-tag review 2026-09-02: the first draft told third-party plugins to run
  // `$CLAUDE_PLUGIN_ROOT/scripts/statusline-chain.js`. Claude Code sets that
  // variable per-plugin, so inside THEIR hook it resolves to THEIR root, and in
  // a plain shell it is unset — the one fact most likely to be wrong was the
  // one this guard did not check.
  const doc = readmeSection();
  assert.doesNotMatch(doc, /CLAUDE_PLUGIN_ROOT[^\n]*statusline-chain/,
    'the registration command must not be rooted at $CLAUDE_PLUGIN_ROOT — it ' +
    'points at whichever plugin is running, which is never this one');

  // What it documents instead must be a path this package actually ships.
  const m = /\$\(npm root -g\)\/(\S+statusline-chain\.js)/.exec(doc);
  assert.ok(m, `the README no longer documents an npm-resolvable path: ${doc.slice(0, 400)}`);
  const relative = m[1].replace(/^@[^/]+\/[^/]+\//, ''); // strip the package name
  assert.ok(fs.existsSync(path.join(__dirname, '..', '..', relative)),
    `the documented path ${relative} does not exist in this repo`);
  // And the package must actually publish it — `files` is what npm ships.
  const pkg = JSON.parse(fs.readFileSync(path.join(__dirname, '..', '..', 'package.json'), 'utf8'));
  assert.ok(pkg.files.includes('claude-plugin'),
    `package.json files does not ship claude-plugin/: ${pkg.files}`);
  assert.match(m[0], new RegExp(`\\$\\(npm root -g\\)/${pkg.name.replace('/', '\\/')}/`),
    `the documented path names a different package than ${pkg.name}`);
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
