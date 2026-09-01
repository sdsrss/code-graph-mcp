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
//
// ENG-04 (audit 2026-08-29), a fourth limit, now closed rather than documented:
// the forms below used to recognize `process.env.NAME =` and `NAME:` and nothing
// else, so the bracket spelling `process.env['TMPDIR'] = x` counted 0/0/0, and
// zero equals zero — a one-name redirect written that way passed silently while
// the behavioural guard it backs only reddens on Windows. Zero instances existed
// in the tree, which is exactly the state in which a scanning guard's blind spot
// is invisible; `countNames` is now exercised against every spelling directly
// (see the parity table below), because a corpus with no offender cannot tell a
// working matcher from a broken one.
function nameForms(name) {
  const bracket = `process\\.env\\[\\s*['"\`]${name}['"\`]\\s*\\]\\s*=`;
  const dot = `process\\.env\\.${name}\\s*=`;
  // `NAME:` — the object-literal spelling. The leading class keeps `TMP:` from
  // matching inside `TMPDIR:` and `TEMP:` from matching a longer name.
  const key = `(?:^|[^A-Za-z0-9_$])${name}\\s*:`;
  return new RegExp(`${dot}|${bracket}|${key}`, 'g');
}

const TMP_NAME_FORMS = {
  TMPDIR: nameForms('TMPDIR'),
  TMP: nameForms('TMP'),
  TEMP: nameForms('TEMP'),
};

// Comments are stripped before counting. This tree documents the rule in prose
// above the code that follows it, so a raw-line count would be reading the
// explanation, not the behaviour. See the long note at the call site for why the
// stripping is line-prefix only and deliberately over-counts prose.
function stripComments(text) {
  return text
    .split('\n')
    .filter((l) => !/^\s*(?:\/\/|\*|\/\*)/.test(l))
    .map((l) => l.replace(/\/\/.*$/, ''))
    .join('\n');
}

function countNames(src, forms) {
  const counts = {};
  for (const [name, re] of Object.entries(forms)) {
    counts[name] = (stripComments(src).match(re) || []).length;
  }
  return counts;
}

// ENG-06 (audit 2026-08-29): recursive, not top-level-only. The three JS hygiene
// guards all assumed a flat directory while the CI / pre-commit / release
// discovery chains have been recursive for some time (pinned by
// `test-discovery-drift-guard.test.js`), so the first nested test file would have
// RUN in CI while escaping every guard that reads the corpus. Zero nested files
// exist today; that is what makes the assumption cheap to fix and impossible to
// notice. `node_modules` is skipped because it is not this repo's code.
function listJsFilesRecursive(dir, { includeTests }) {
  const out = [];
  let entries;
  try {
    entries = fs.readdirSync(dir, { withFileTypes: true });
  } catch {
    return out; // an optional directory that does not exist here
  }
  for (const entry of entries) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === 'node_modules' || entry.name.startsWith('.')) continue;
      out.push(...listJsFilesRecursive(full, { includeTests }));
    } else if (entry.isFile() && entry.name.endsWith('.js')) {
      if (!includeTests && entry.name.endsWith('.test.js')) continue;
      out.push(full);
    }
  }
  return out;
}

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
    // Recursive (ENG-06): the discovery chains that decide what CI RUNS are
    // recursive, so a guard that reads only the top level grades a smaller corpus
    // than the one that executes.
    for (const full of listJsFilesRecursive(dir, { includeTests: true })) {
      if (full === SELF) continue;
      scanned++;
      // NOTE (kept at the read site because it explains `stripComments`):
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
      // BECAUSE the false-positive rate is currently zero across 74 files — a
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
      const counts = countNames(fs.readFileSync(full, 'utf8'), TMP_NAME_FORMS);
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
  // floors are well under the measured values (74 files, 16 redirect sites at
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

// ── The two-name rule (HOME / USERPROFILE) ──────────────────────────────────
// ENG-05 (audit 2026-08-29). Exactly the axis above, one directory over: a test
// that redirects HOME into a sandbox is inert on Windows, where `os.homedir()`
// reads USERPROFILE and ignores HOME. It had no counting guard at all, and the
// corpus showed it — 12 files spelled `HOME:` and 3 of them spelled USERPROFILE.
//
// Two mitigations were in place and neither makes the guard redundant, which is
// why this is worth a rule rather than a comment: the Windows CI job sets a
// suite-level USERPROFILE (`tests/hardening.rs`), and most `claudeHome()` paths
// are pinned by an explicit CLAUDE_CONFIG_DIR. Both stop at the same edge — a
// developer running `node --test` on a Windows machine gets neither, and every
// sandbox in that run reads and writes the real home directory.
//
// Same shape-blind counting, same both-directions equality, and the same three
// limits as the tmp rule (prose in block comments counts; it is text and not a
// parser; per-file totals can cancel). The trade is unchanged: over-count loudly
// rather than under-count silently.
const HOME_NAME_FORMS = {
  HOME: nameForms('HOME'),
  USERPROFILE: nameForms('USERPROFILE'),
};

test('every HOME redirect also spells USERPROFILE', () => {
  const dirs = [__dirname, path.resolve(__dirname, '..', '..', 'scripts')];
  const SELF = __filename;

  const offenders = [];
  let scanned = 0;
  let sites = 0;
  for (const dir of dirs) {
    for (const full of listJsFilesRecursive(dir, { includeTests: true })) {
      if (full === SELF) continue;
      scanned++;
      const counts = countNames(fs.readFileSync(full, 'utf8'), HOME_NAME_FORMS);
      sites += counts.HOME;
      if (counts.HOME === counts.USERPROFILE) continue;
      offenders.push(
        `${path.relative(path.resolve(__dirname, '..', '..'), full)}: ` +
          `HOME×${counts.HOME}, USERPROFILE×${counts.USERPROFILE}`
      );
    }
  }

  assert.ok(scanned >= 25, `expected the JS corpus to be discovered, scanned ${scanned} files`);
  assert.ok(sites >= 40, `expected the HOME redirects to be discovered, found ${sites} HOME sites`);

  assert.deepStrictEqual(
    offenders,
    [],
    'a HOME redirect does not spell USERPROFILE (or the reverse), so it is inert on ' +
      "Windows and the child reads the developer's real home directory. Set both names " +
      'on every redirect:\n  ' +
      offenders.join('\n  ')
  );
});

// The guard above these two is a TEXT MATCHER, and a text matcher's blind spots
// are invisible in a corpus that contains no offender — which is precisely the
// corpus this repo has (ENG-04 was found by reading the regexes, not by a red
// test). So the matcher is exercised directly against every spelling that means
// "this process redirects an env var", including the bracket form that used to
// count zero. A row here failing means the scan above is blind to that shape,
// not that some file is wrong.
test('countNames recognizes every spelling of an env-var redirect', () => {
  const forms = { TMPDIR: nameForms('TMPDIR') };
  const cases = [
    ['process.env.TMPDIR = x;', 1, 'dot assignment'],
    ["process.env['TMPDIR'] = x;", 1, 'bracket assignment, single quotes'],
    ['process.env["TMPDIR"] = x;', 1, 'bracket assignment, double quotes'],
    ['process.env[`TMPDIR`] = x;', 1, 'bracket assignment, backticks'],
    ["process.env[ 'TMPDIR' ] = x;", 1, 'bracket assignment, padded'],
    ['env: { ...process.env, TMPDIR: t },', 1, 'object key'],
    ['{TMPDIR: t}', 1, 'object key, no leading space'],
    ['// process.env.TMPDIR = x;', 0, 'line comment is not code'],
    ['const s = readTMPDIR;', 0, 'a longer identifier is not this name'],
  ];
  for (const [src, want, why] of cases) {
    assert.equal(countNames(src, forms).TMPDIR, want, `${why}: ${JSON.stringify(src)}`);
  }

  // Negative control for the counting itself: a file that redirects only TMPDIR
  // must come out UNEQUAL, or every equality check above passes vacuously.
  const oneName = countNames("process.env['TMPDIR'] = t;", TMP_NAME_FORMS);
  assert.deepStrictEqual(
    [oneName.TMPDIR, oneName.TMP, oneName.TEMP],
    [1, 0, 0],
    'a one-name bracket redirect must count as an imbalance, not as 0/0/0'
  );

  // The fixture spells CLAUDE_CONFIG_DIR because `js_test_files_neutralize_
  // claude_config_dir` (tests/hardening.rs) scans this corpus as TEXT and cannot
  // tell a sample string from a real spawn. Keeping the key makes the sample a
  // correctly-sandboxed env rather than a silenced one; it is not counted here.
  const homeOnly = countNames(
    "env: { ...process.env, HOME: h, CLAUDE_CONFIG_DIR: c },",
    HOME_NAME_FORMS
  );
  assert.deepStrictEqual(
    [homeOnly.HOME, homeOnly.USERPROFILE],
    [1, 0],
    'a HOME-only redirect must count as an imbalance'
  );
});
