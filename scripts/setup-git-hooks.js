#!/usr/bin/env node
'use strict';
// npm `prepare`: point git at this repo's tracked hooks directory.
//
// Replaces the inline shell one-liner
//   git rev-parse --git-dir > /dev/null 2>&1 && git config core.hooksPath scripts/githooks || true
// which had two defects (2026-08-16 audit §四):
//
//   1. NOT IDEMPOTENT. `git config <k> <v>` writes `.git/config` unconditionally,
//      and a write takes a `config.lock`. `prepare` runs on far more than an
//      install — `npm pack --dry-run` triggers it — so a routine, read-only-looking
//      command mutated the repo. During this audit one such run left a 0-byte
//      `.git/config.lock` behind, which then blocked EVERY later `git config`
//      write in the checkout until it was removed by hand. Reading the current
//      value first makes the common case a pure read.
//
//   2. NOT PORTABLE. `> /dev/null 2>&1` and `|| true` are POSIX shell; npm runs
//      lifecycle scripts through cmd.exe on Windows, where `/dev/null` is not a
//      path and `||` is not that operator. A Windows contributor's `npm install`
//      ran a broken prepare.
//
// Everything here is best-effort: a checkout without git, a git that is not on
// PATH, or a read-only config is a reason to skip hook wiring, never a reason to
// fail an install.
const { execFileSync } = require('child_process');
const path = require('path');

const DESIRED = 'scripts/githooks';

function git(args) {
  return execFileSync('git', args, {
    cwd: path.join(__dirname, '..'),
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'ignore'],
    // Without this every `npm install` in a Windows checkout flashes a console
    // window — Node defaults `windowsHide` to FALSE. `windows-hide.test.js`
    // enforces it across the shipped JS dirs and caught this file the first time
    // it ran against it.
    windowsHide: true,
  }).trim();
}

try {
  git(['rev-parse', '--git-dir']); // throws outside a checkout — nothing to wire
} catch {
  process.exit(0);
}

let current = null;
try {
  current = git(['config', '--get', 'core.hooksPath']);
} catch {
  current = null; // unset: `git config --get` exits 1
}

// Already pointing somewhere deliberate — including this repo's own
// `.git/hooks`, which carries a symlink to the same hook. Overwriting a
// contributor's explicit choice on every `npm install` is not this script's job.
if (current) {
  process.exit(0);
}

try {
  git(['config', 'core.hooksPath', DESIRED]);
  console.log(`[code-graph] git core.hooksPath -> ${DESIRED}`);
} catch (e) {
  console.error(
    `[code-graph] could not set core.hooksPath (${e && e.message ? e.message : e}); ` +
    'pre-commit checks will not run locally. Set it by hand with: ' +
    `git config core.hooksPath ${DESIRED}`,
  );
}
