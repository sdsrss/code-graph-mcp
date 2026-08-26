#!/usr/bin/env node
'use strict';
const { test } = require('node:test');
const assert = require('node:assert');
const fs = require('node:fs');
const path = require('node:path');

// META③: under Claude Code, $TMPDIR is redirected to ~/.claude/tmp/ and writing
// there via a bare os.tmpdir() leaks/loops (memory: feedback_tmpdir_override_trap).
// All plugin scripts must route through cgTmpDir(). Only the file that DEFINES
// cgTmpDir (tmp-dir.js) may call os.tmpdir() directly.
//
// *.test.js files are excluded: their own os.tmpdir()-seeded mkdtempSync() fixture
// dirs are unique, self-cleaning, and not the shared-state leak this guard targets.
//
// One pre-existing, documented exception in production code: lifecycle.js's
// verifyHooksFire() builds a throwaway, self-cleaning mkdtempSync fixture directly
// under os.tmpdir() (NOT cgTmpDir()) on purpose — a concurrent process clearing
// <tmp>/code-graph-mcp mid-run would otherwise yank the fixture out from under an
// in-flight spawn (see the comment at that call site). Allowlisted by exact line
// content, not by file, so any OTHER new bare os.tmpdir() call added later in the
// same file still fails the guard.
const DEFINER = 'tmp-dir.js';
const ALLOWLIST = [{ file: 'lifecycle.js', contains: 'tmpBase || os.tmpdir()' }];

function listJsFiles(dir) {
  const out = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      out.push(...listJsFiles(full));
    } else if (entry.isFile() && entry.name.endsWith('.js') && !entry.name.endsWith('.test.js')) {
      out.push(full);
    }
  }
  return out;
}

test('no bare os.tmpdir() outside the cgTmpDir helper', () => {
  const root = __dirname;
  const offenders = [];
  for (const full of listJsFiles(root)) {
    const name = path.basename(full);
    if (name === DEFINER) continue;
    const src = fs.readFileSync(full, 'utf8');
    src.split('\n').forEach((line, i) => {
      const code = line.replace(/\/\/.*$/, '');
      if (!/\bos\.tmpdir\s*\(/.test(code)) return;
      const allowed = ALLOWLIST.some((a) => a.file === name && line.includes(a.contains));
      if (!allowed) offenders.push(`${path.relative(root, full)}:${i + 1}: ${line.trim()}`);
    });
  }
  assert.deepStrictEqual(
    offenders,
    [],
    `bare os.tmpdir() found outside cgTmpDir(); use cgTmpDir() from tmp-dir.js instead:\n${offenders.join('\n')}`
  );
});

// ── The three-name rule ─────────────────────────────────────────────────────
// A tmp redirect that names only TMPDIR is inert on Windows. node's os.tmpdir()
// reads TMPDIR first on POSIX; on Windows it reads TEMP then TMP and ignores
// TMPDIR entirely. A sandbox spelled with one name therefore holds on two
// platforms of a three-platform matrix and silently falls back to the INHERITED
// tmp on the third — which, for anything under `cgTmpDir()`, is the one shared
// machine-global directory the developer's live hooks are also using.
//
// This is not a style rule. `pre-edit-guide.test.js` spawned the real hook with
// `TMPDIR` alone, its `.cg-impact-<cwd>-<symbol>` cooldown flag landed in the
// real shared dir on windows-latest, and that reddened
// `js_test_suite_leaves_the_shared_tmp_dir_intact` on the v0.126.1 release
// commit. Every other redirect in the tree already spelled all three.
//
// Counted per file rather than matched within a window. The three names are
// written in three different shapes here — three consecutive module-scope
// assignments, all three inline in one spawn-env literal, and in two different
// orders — so any proximity rule is a magic number waiting to be wrong, and the
// repository has already paid for one of those (hardening.rs's env-literal
// window, three wrong versions). Counts are shape-blind: TMPDIR is never
// written without its two siblings, so the counts are equal or something has
// drifted. A second spawn that forgets them trips it just as a first one does.
//
// Equality is checked in BOTH directions on purpose. TMP/TEMP without TMPDIR is
// the mirror bug — inert on Linux and macOS instead of Windows — and it is the
// spelling someone reaches for after being burned by this one.
//
// Three limits of counting, all measured rather than guessed, all left in place:
//   * Prose inside a plain-indented block comment counts. See the stripping
//     note at the read site for why the fix for this is worse than the bug.
//   * It is TEXT, not a parser. An object key or a string literal that happens
//     to spell one of these names — `{ TEMP: 'temperature' }`, or a help string
//     reading `set TMPDIR: /path` — counts, and reddens this guard with a
//     message about tmp sandboxing that points at the wrong thing entirely.
//     Nothing in this tree does that today. If you land one, rename it or add
//     it here as a named exemption; do not delete the guard.
//   * Per-file totals cancel. One file holding a module-scope `TMPDIR` with no
//     siblings AND a spawn literal with `TMP`/`TEMP` and no `TMPDIR` counts
//     1/1/1 and passes while BOTH sites are broken. That is the price of being
//     shape-blind, and it is the right trade here: the window-based alternative
//     is the one that failed three times next door.
const TMP_NAME_FORMS = {
  TMPDIR: /process\.env\.TMPDIR\s*=|\bTMPDIR\s*:/g,
  TMP: /process\.env\.TMP\s*=|(?:^|[^A-Z_])TMP\s*:/g,
  TEMP: /process\.env\.TEMP\s*=|(?:^|[^A-Z_])TEMP\s*:/g,
};

test('every tmp redirect spells TMPDIR, TMP and TEMP together', () => {
  const dirs = [__dirname, path.resolve(__dirname, '..', '..', 'scripts')];
  // This file is excluded from its own scan: the regexes above are literal
  // occurrences of all three spellings, so it would be counting itself.
  // Compared by full path, not basename — a same-named file in `scripts/` is a
  // different file and must still be scanned.
  const SELF = __filename;

  const offenders = [];
  let scanned = 0;
  let sites = 0;
  for (const dir of dirs) {
    // Top level only, matching how the Rust guards in tests/hardening.rs
    // enumerate this same corpus. `scripts/` has non-JS subtrees
    // (nomic-bert-poc, embedding_benchmark) that hold nothing tmp-related.
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      if (!entry.isFile() || !entry.name.endsWith('.js')) continue;
      const full = path.join(dir, entry.name);
      if (full === SELF) continue;
      scanned++;
      // Comments are stripped before counting. This tree documents the rule in
      // prose above the code that follows it, so a `contains`-style count over
      // raw lines would be reading the explanation, not the behaviour.
      //
      // Line-prefix stripping ONLY, deliberately. The continuation lines of a
      // plain-indented `/* … */` block survive it, so a block comment writing
      // one of these names in prose false-positives — a review demonstrated it.
      // The obvious repair — removing `/* … */` spans whole with a non-greedy
      // regex — was written and then reverted, because a `/*` that is not a
      // comment opener deletes everything up to the next `*/`, and a genuine
      // one-name redirect inside that span then counts 0/0/0 and the guard goes
      // GREEN on a real offender.
      //
      // That is not a hypothetical about string literals. THIS TREE ALREADY
      // CONTAINS THE SHAPE: `pre-grep-guide.js:13` is a `//` line comment ending
      // `.../*.md/*.json)`, whose `/*` opens a span that closes 516 lines later
      // (lines 13-529) at a `/* ok */` — measured, 28271 of that file's 46349
      // bytes per `wc -c`, 61% of a production hook, deleted before counting.
      // It is count-neutral today only because that file holds no tmp redirect:
      // planting a `TMPDIR`-only one at line 300 scores 1/0/0 RED under the
      // line-prefix form and 0/0/0 PASS under the span form.
      //
      // Two honest qualifications, so this comment is not quoted back later in a
      // situation it does not cover. First, "loud beats silent" holds here
      // BECAUSE the false-positive rate is currently zero across 73 files — a
      // guard that cries wolf gets deleted, and a deleted guard detects nothing,
      // so the trade rests on that rate rather than on a law. Second, the span
      // form is not the only possible fix: stripping a block comment only when
      // its `/*` STARTS a line would handle the prose case and could not be
      // opened by a mid-line `/*.json` inside a `//` comment. It is unbuilt
      // because the problem it solves has zero instances, not because the class
      // is unfixable.
      //
      // So the trade is deliberate: this guard over-counts prose (loud, wrong,
      // and instantly obvious to whoever writes that shape) rather than
      // under-counting code (silent, and it would have let the very bug this
      // guard exists for walk straight through). Detection power first.
      const src = fs
        .readFileSync(full, 'utf8')
        .split('\n')
        .filter((l) => !/^\s*(?:\/\/|\*|\/\*)/.test(l))
        .map((l) => l.replace(/\/\/.*$/, ''))
        .join('\n');
      const counts = {};
      for (const [name, re] of Object.entries(TMP_NAME_FORMS)) {
        counts[name] = (src.match(re) || []).length;
      }
      sites += counts.TMPDIR;
      if (counts.TMPDIR === counts.TMP && counts.TMPDIR === counts.TEMP) continue;
      offenders.push(
        `${path.relative(path.resolve(__dirname, '..', '..'), full)}: ` +
          `TMPDIR×${counts.TMPDIR}, TMP×${counts.TMP}, TEMP×${counts.TEMP}`
      );
    }
  }

  // Vacuity: a layout change that moves these files, or a rename of the env
  // names, must fail loudly rather than scan an empty corpus and pass. Both
  // floors are well under the measured values (73 files, 14 redirect sites at
  // the time of writing) and are runaway detectors, not thresholds.
  assert.ok(scanned >= 25, `expected the JS corpus to be discovered, scanned ${scanned} files`);
  assert.ok(sites >= 8, `expected the tmp redirects to be discovered, found ${sites} TMPDIR sites`);

  assert.deepStrictEqual(
    offenders,
    [],
    'a tmp redirect names TMPDIR without TMP/TEMP (or the reverse), so it is ' +
      "inert on one of the three CI platforms and the child falls back to the caller's " +
      'shared tmp dir. Set all three names on every redirect:\n  ' +
      offenders.join('\n  ')
  );
});
