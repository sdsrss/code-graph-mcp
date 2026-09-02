'use strict';
// JS-14 (audit 2026-08-29): the dev-only copy step threw a raw stack on ENOSPC
// or an unwritable bin/, which reads as a bug in the script rather than as the
// environment problem it is. The missing-source path always printed a sentence
// and exited 1; these two now match it.
//
// Driven through a real spawn against a scripted layout, because the failure is
// a filesystem permission — a unit test that stubs `fs` would assert the shape
// of the catch block, not that the catch block is reached.
const test = require('node:test');
const assert = require('node:assert/strict');
const { spawnSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

function layout() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'copy-binary-'));
  const scripts = path.join(root, 'scripts');
  const release = path.join(root, 'target', 'release');
  fs.mkdirSync(scripts, { recursive: true });
  fs.mkdirSync(release, { recursive: true });
  fs.copyFileSync(path.join(__dirname, 'copy-binary.js'), path.join(scripts, 'copy-binary.js'));
  const name = os.platform() === 'win32' ? 'code-graph-mcp.exe' : 'code-graph-mcp';
  fs.writeFileSync(path.join(release, name), 'not really a binary\n');
  return { root, scripts, name };
}
const run = (scripts) =>
  spawnSync(process.execPath, [path.join(scripts, 'copy-binary.js')], { encoding: 'utf8' });

test('copy-binary: a writable layout still succeeds (control)', () => {
  const { root, scripts, name } = layout();
  try {
    const res = run(scripts);
    assert.equal(res.status, 0, `stderr=${res.stderr}`);
    assert.ok(fs.existsSync(path.join(root, 'bin', name)), 'the binary was installed');
    assert.match(res.stdout, /Copied binary to/);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test('copy-binary: an unwritable destination explains itself instead of throwing', (t) => {
  if (process.getuid && process.getuid() === 0) {
    t.skip('root ignores the mode bits this test relies on');
    return;
  }
  if (process.platform === 'win32') {
    // chmod does not make a directory unwritable there, so the copy would
    // succeed and this test would fail for a reason that is not the product's.
    // CI is ubuntu-only, but pre-commit runs this suite on the developer's box.
    t.skip('directory mode bits do not gate writes on Windows');
    return;
  }
  const { root, scripts } = layout();
  const bin = path.join(root, 'bin');
  fs.mkdirSync(bin);
  fs.chmodSync(bin, 0o555); // r-x: mkdir is a no-op, the copy is refused
  try {
    const res = run(scripts);
    assert.equal(res.status, 1, 'a refused install must exit 1, like the missing-source path');
    assert.match(res.stderr, /not writable by this user/,
      `the message must name the cause; got: ${res.stderr}`);
    assert.doesNotMatch(res.stderr, /at Object\.<anonymous>|node:internal/,
      `a stack trace is what this replaced; got: ${res.stderr}`);
    assert.match(res.stderr, /Source is intact at/,
      'and it must say the built binary is still there');
  } finally {
    fs.chmodSync(bin, 0o755);
    fs.rmSync(root, { recursive: true, force: true });
  }
});
