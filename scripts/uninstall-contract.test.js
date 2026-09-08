#!/usr/bin/env node
'use strict';
// Dropped before anything runs: the teardown probe below redirects HOME, and
// `claudeHome()` is `CLAUDE_CONFIG_DIR || homedir/.claude` — for a developer who
// exports it (the documented multi-profile setup) the variable would WIN over
// the redirect and the probe would act on the live config. Pinned by
// `js_test_files_neutralize_claude_config_dir` in tests/hardening.rs.
delete process.env.CLAUDE_CONFIG_DIR;

const { test } = require('node:test');
const assert = require('node:assert');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { execFileSync } = require('node:child_process');

// npm 7 REMOVED the `preuninstall` / `postuninstall` lifecycle scripts. This
// package declared `preuninstall` anyway, and `engines.node >= 16` means npm 8
// at the oldest — so it could never fire for any supported install. Measured on
// npm 11: `npm uninstall -g` left the hook entries in ~/.claude/settings.json
// and ~40 MB of cached binary behind, silently (the POSIX hooks are wrapped in
// `[ -f ]`, so they no-op instead of erroring and the user never notices).
//
// The teardown is a user-run command instead, and the order matters: after npm
// removes the package there is no CLI left to run. These guards pin both halves
// — no dead lifecycle script, and docs that put the teardown first.

const ROOT = path.resolve(__dirname, '..');
const PKG = JSON.parse(fs.readFileSync(path.join(ROOT, 'package.json'), 'utf8'));
const README = fs.readFileSync(path.join(ROOT, 'README.md'), 'utf8');

test('no uninstall lifecycle script — npm 7+ never runs them', () => {
  for (const hook of ['preuninstall', 'postuninstall']) {
    assert.strictEqual(
      PKG.scripts?.[hook],
      undefined,
      `package.json declares "${hook}", which npm has not run since v7. Under ` +
        `engines.node ">=16" (npm 8+) it can never fire, so it is a promise of ` +
        `cleanup that never happens. Document "code-graph-mcp uninstall" instead.`
    );
  }
});

test('engines.node still implies an npm with no uninstall hooks', () => {
  // The guard above is only correct while the declared floor is npm 7+. If
  // someone lowers engines.node to 14 (npm 6), preuninstall WOULD fire again and
  // this reasoning needs revisiting rather than silently holding.
  const min = /(\d+)/.exec(PKG.engines?.node ?? '');
  assert.ok(min, 'engines.node must declare a minimum version');
  assert.ok(
    Number(min[1]) >= 16,
    `engines.node floor is ${min[1]}; below 16 the bundled npm may still honour ` +
      `preuninstall, so the "dead script" guard above no longer holds.`
  );
});

test('README tells npm users to tear down before uninstalling', () => {
  const section = README.slice(README.indexOf('### npm (Global)'));
  const teardown = section.indexOf('code-graph-mcp uninstall');
  const npmRemove = section.indexOf('npm uninstall -g');
  assert.ok(teardown !== -1, 'README npm section must document `code-graph-mcp uninstall`');
  assert.ok(npmRemove !== -1, 'README npm section must still document `npm uninstall -g`');
  assert.ok(
    teardown < npmRemove,
    'README must run `code-graph-mcp uninstall` BEFORE `npm uninstall -g` — after npm ' +
      'removes the package the teardown command no longer exists on disk.'
  );
});

test('the teardown command the docs name actually exists', (t) => {
  // Negative control against documenting a command that was renamed away: the
  // CLI must really dispatch an `uninstall` subcommand.
  //
  // Asserted by RUNNING it, not by grepping the entry point for the shape of its
  // `if`. This guard read `bin/cli.js` for `sub === "uninstall"` and went red when
  // the dispatcher moved to claude-plugin/scripts/cli-entry.js with the behaviour
  // intact — a guard that a refactor can break while the contract holds was
  // testing the wrong thing. `--help` returns before any destructive work; a
  // subcommand that stopped being intercepted would be forwarded to the binary,
  // which has no `uninstall`, so this exits non-zero instead.
  //
  // Sandboxed cwd AND home, though `--help` returns first (pre-ship review): the
  // command one line below this comment is the real teardown dispatcher, and it
  // deletes `~/.cache/code-graph`, strips code-graph hooks from
  // `~/.claude/settings.json` and unadopts the project in cwd. That it is
  // harmless today rests entirely on the `--help` guard being the FIRST
  // statement in cli-entry.js's `uninstall` arm — one refactor away from this
  // suite tearing down the developer's own machine. The predecessor grep could
  // never do that; §8.V3 says a session-modified destructive path is sandboxed,
  // not reasoned about. Both HOME and USERPROFILE, because `os.homedir()` reads
  // the latter on Windows.
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'uninstall-contract-'));
  t.after(() => { try { fs.rmSync(home, { recursive: true, force: true }); } catch { /* gone */ } });
  const out = execFileSync(process.execPath, [path.join(ROOT, 'bin/cli.js'), 'uninstall', '--help'],
    {
      cwd: home, encoding: 'utf8', stdio: ['pipe', 'pipe', 'pipe'],
      env: { ...process.env, HOME: home, USERPROFILE: home },
    });
  assert.match(
    out,
    /USAGE:\n\s+code-graph-mcp uninstall/,
    'the CLI no longer intercepts an `uninstall` subcommand, so the README instruction ' +
      'and this whole teardown path are broken.'
  );
});
