#!/usr/bin/env node
'use strict';
// PR impact review (v0.53.0). Renders a code-graph `affected` analysis of a
// pull request's changed files into a sticky PR comment: which test files to
// re-run, the blast radius, and changed production files that have NO covering
// test ("test gaps").
//
// Posture: this is a productization of the already-shipped `affected` command
// (reverse-dependency closure over imports∪calls∪references∪implements∪inherits).
// It introduces no new graph logic — it only shells out to the built binary and
// formats the result. The render path is pure (unit-tested with fixtures); the
// gh upsert + binary calls live in `computeReview` / `upsertComment` / `main`.
//
// Funnel-safe: every binary invocation carries CODE_GRAPH_INTERNAL=1 so CI
// analysis runs never inflate the deny→use conversion metrics (mirrors
// cg-answer.js).

const { spawnSync } = require('child_process');
const { hidden } = require('./proc-opts');

const MARKER = '<!-- code-graph-impact-review -->';
const SPAWN_TIMEOUT_MS = 60_000;
const TOP_AFFECTED = 15;

/// Constant lists mirroring `domain::PASCAL_TEST_*` / `INFIX_TEST_EXTS`. Both
/// sides are generated from their lists rather than transcribed, so adding an
/// ecosystem is one edit here and one in domain.rs — not a hunt through
/// hand-written boolean chains.
const PASCAL_TEST_EXTS = ['cs', 'vb', 'fs', 'java', 'kt', 'scala', 'swift', 'php'];
// `Spec` means TEST only in ScalaTest/Kotest. Elsewhere it is a production noun
// (OpenApiSpec.cs, WireSpec.java), so it gets its own narrower extension set.
const SPEC_TEST_EXTS = ['scala', 'kt'];
const PASCAL_TEST_STEM_EXTS = [
  ['Test', PASCAL_TEST_EXTS],
  ['Tests', PASCAL_TEST_EXTS],
  ['Spec', SPEC_TEST_EXTS],
];
const INFIX_TEST_EXTS = ['go', 'rs', 'py', 'dart'];

/// File-level test classifier — mirrors `domain::is_test_path` (Rust). This is
/// one of several deliberately-synchronized copies; `domain.rs` carries the
/// "Five sites must agree" note, and two of those sites are intentionally
/// divergent. Widening only the Rust side breaks the parity contract asserted
/// by `isTestPath mirrors domain::is_test_path patterns` in the test file, and
/// makes the "test gaps" section report every Java/C# test as uncovered
/// production code.
function isTestPath(p) {
  // Case-insensitive `test`/`tests` DIRECTORY segment at any depth: xUnit/NUnit
  // put suites under `src/Tests/<Project>/…` and Maven/Gradle under
  // `src/test/java/…` (issue #36). Note `toLowerCase()` is Unicode-aware where
  // Rust uses `to_ascii_lowercase()`; they agree on ASCII paths.
  const lower = p.toLowerCase();
  if (
    lower.startsWith('tests/') || lower.startsWith('test/') ||
    lower.includes('/tests/') || lower.includes('/test/')
  ) return true;
  // PascalCase test-class convention. Case-SENSITIVE and pinned to a known
  // extension so `src/latest.cs` and `src/mytests.rs` stay production.
  if (PASCAL_TEST_STEM_EXTS.some(([stem, exts]) => exts.some((ext) => p.endsWith(`${stem}.${ext}`)))) {
    return true;
  }
  if (INFIX_TEST_EXTS.some((ext) => p.endsWith(`_test.${ext}`))) return true;
  // pytest naming conventions. Case-SENSITIVE like the PascalCase leg above:
  // pytest matches `python_files` without normcase and finds conftest by the
  // literal basename, so `api/Test_Signup.py` is a production module. Keeps this
  // mirror identical to `is_test_node_sql`'s case-sensitive GLOB.
  if (p.endsWith('.py') &&
      (p.startsWith('test_') || p.includes('/test_') || p.endsWith('conftest.py'))) {
    return true;
  }
  return (
    p.startsWith('benches/') || p.startsWith('bench/') ||
    p.includes('__tests__/') ||
    p.endsWith('/tests.rs') ||
    p.endsWith('.test.ts') || p.endsWith('.test.js') ||
    p.endsWith('.test.tsx') || p.endsWith('.test.jsx') ||
    p.endsWith('.spec.ts') || p.endsWith('.spec.js') ||
    p.endsWith('.spec.tsx') || p.endsWith('.spec.jsx')
  );
}

function resolveBinary() {
  if (process.env._CG_REVIEW_BINARY) return process.env._CG_REVIEW_BINARY;
  try {
    return require('./find-binary').findBinary();
  } catch {
    return null;
  }
}

function runAffected(binary, args, cwd, stdin) {
  const res = spawnSync(binary, args, hidden({
    cwd,
    input: stdin,
    timeout: SPAWN_TIMEOUT_MS,
    encoding: 'utf8',
    maxBuffer: 16 * 1024 * 1024,
    stdio: ['pipe', 'pipe', 'ignore'],
    env: { ...process.env, CODE_GRAPH_INTERNAL: '1' },
  }));
  if (res.error || res.signal || res.status !== 0) return null;
  try {
    return JSON.parse((res.stdout || '').trim());
  } catch {
    return null;
  }
}

/// Build the review object by running `affected` over the changed files.
/// `changedFiles` is the raw list (newline-split, pre-filtered to non-empty).
/// Returns null only when the binary itself is unavailable — an empty diff still
/// yields a valid (empty) review so the comment path always has data.
function computeReview(binary, changedFiles, cwd) {
  const aggregate = runAffected(
    binary, ['affected', '--stdin', '--json'], cwd, changedFiles.join('\n') + '\n'
  );
  if (!aggregate) return null;

  const changed = aggregate.changed || [];
  const tests = aggregate.tests || [];
  const affectedFiles = aggregate.affected_files || [];
  const notIndexed = aggregate.not_indexed || [];

  // Per-file test-gap: a changed PRODUCTION (non-test) file is "uncovered" when
  // running `affected` on it alone surfaces zero test files. Run per-file so the
  // signal is attributable (the aggregate union can't be split back per file).
  //
  // `runAffected` returns null for BOTH "spawn failed / timed out / non-zero
  // exit" and "unparseable output" — none of which say anything about test
  // coverage. Those files go to `unanalyzed` and are disclosed. Folding them
  // into the same else-branch as "has tests" made a 60s timeout render as a
  // covered file: the most dangerous direction for a test-gap report to fail in.
  const uncovered = [];
  const unanalyzed = [];
  for (const f of changed) {
    if (isTestPath(f)) continue;
    const single = runAffected(binary, ['affected', f, '--json'], cwd, '');
    if (!single) {
      unanalyzed.push(f);
    } else if ((single.tests || []).length === 0) {
      uncovered.push(f);
    }
  }

  const topAffected = affectedFiles
    .slice()
    .sort((a, b) => (a.depth - b.depth) || a.path.localeCompare(b.path))
    .slice(0, TOP_AFFECTED);

  return {
    changed,
    not_indexed: notIndexed,
    tests: tests.slice().sort(),
    blast_radius: affectedFiles.length,
    top_affected: topAffected,
    uncovered: uncovered.sort(),
    unanalyzed: unanalyzed.sort(),
  };
}

/// Pure: render a review object to sticky-comment markdown. Always begins with
/// MARKER so the upsert can find and replace it.
function renderMarkdown(review) {
  const lines = [MARKER, '## 🔎 Code Graph impact review', ''];

  if (review.changed.length === 0) {
    lines.push(
      review.not_indexed.length > 0
        ? `No **indexed** code changed (${review.not_indexed.length} changed file(s) not in the graph — new/non-code files).`
        : 'No code changes detected in this PR.'
    );
    lines.push('', '<sub>code-graph-mcp `affected`</sub>');
    return lines.join('\n');
  }

  lines.push(
    `**${review.changed.length}** changed indexed file(s) · ` +
    `blast radius **${review.blast_radius}** file(s) · ` +
    `**${review.tests.length}** test file(s) to re-run`,
    ''
  );

  if (review.uncovered.length > 0) {
    lines.push(`### ⚠️ Test gaps (${review.uncovered.length})`);
    lines.push('Changed production files with no test in their reverse-dependency closure:');
    for (const p of review.uncovered) lines.push(`- \`${p}\``);
    lines.push('');
  }

  // Absence of a result is not a result. These files are listed apart from the
  // test gaps because the analysis never produced an answer for them.
  const unanalyzed = review.unanalyzed || [];
  if (unanalyzed.length > 0) {
    lines.push(`### ❔ Not analyzed (${unanalyzed.length})`);
    lines.push('The `affected` run for these files failed or timed out, so their test coverage is unknown:');
    for (const p of unanalyzed) lines.push(`- \`${p}\``);
    lines.push('');
  }

  if (review.tests.length > 0) {
    lines.push('<details><summary>Tests to re-run</summary>', '');
    for (const t of review.tests) lines.push(`- \`${t}\``);
    lines.push('', '</details>', '');
  }

  if (review.top_affected.length > 0) {
    const more = review.blast_radius - review.top_affected.length;
    const cap = more > 0 ? ` (top ${review.top_affected.length} of ${review.blast_radius})` : '';
    lines.push(`<details><summary>Blast radius${cap}</summary>`, '');
    for (const a of review.top_affected) lines.push(`- \`${a.path}\` (depth ${a.depth})`);
    if (more > 0) lines.push(`- …and ${more} more`);
    lines.push('', '</details>', '');
  }

  if (review.not_indexed.length > 0) {
    lines.push(`<sub>${review.not_indexed.length} changed file(s) not in index (new/non-code) — not analyzed.</sub>`);
  }
  lines.push('<sub>code-graph-mcp `affected` · reverse-dependency closure over imports∪calls∪references∪implements∪inherits</sub>');
  return lines.join('\n');
}

/// Split a stream of back-to-back JSON documents into their source texts.
///
/// `gh api --paginate` writes ONE document per page, concatenated with no
/// separator (`[...][...]`), which `JSON.parse` rejects outright. The caller's
/// catch then fell through to "no existing comment", so every CI run on a PR
/// past 100 comments POSTed a fresh sticky comment instead of patching the one
/// already there (audit 2026-08-22 P2-12).
///
/// Splits on TOP-LEVEL boundaries only, tracking string and escape state. The
/// obvious `][` → `],[` rewrite is wrong: `][` occurs inside ordinary comment
/// bodies (markdown reference links are literally `[text][ref]`), so that
/// repair would corrupt the very payload it is trying to read.
function splitJsonDocuments(text) {
  const out = [];
  let depth = 0;
  let inStr = false;
  let esc = false;
  let start = -1;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (inStr) {
      if (esc) esc = false;
      else if (c === '\\') esc = true;
      else if (c === '"') inStr = false;
      continue;
    }
    if (c === '"') { inStr = true; continue; }
    if (c === '[' || c === '{') {
      if (depth === 0) start = i;
      depth++;
    } else if (c === ']' || c === '}') {
      depth--;
      if (depth === 0 && start >= 0) {
        out.push(text.slice(start, i + 1));
        start = -1;
      }
    }
  }
  return out;
}

/// Flatten a paginated `gh api` array response into one array of items.
/// A single page parses as itself; multiple pages concatenate.
function parseGhPagedArray(stdout) {
  const items = [];
  for (const doc of splitJsonDocuments(String(stdout || ''))) {
    let value;
    try { value = JSON.parse(doc); } catch { continue; }
    if (Array.isArray(value)) items.push(...value);
    else items.push(value);
  }
  return items;
}

/// Upsert a sticky comment: find an existing comment containing MARKER and PATCH
/// it, else POST a new one. Uses `gh api` (preinstalled on GitHub runners).
function upsertComment(repo, prNumber, body) {
  const gh = (args, input) => spawnSync('gh', args, hidden({
    encoding: 'utf8', input, timeout: SPAWN_TIMEOUT_MS,
    env: { ...process.env },
  }));

  const list = gh(['api', '--paginate', `repos/${repo}/issues/${prNumber}/comments`]);
  let existingId = null;
  if (list.status === 0) {
    const comments = parseGhPagedArray(list.stdout);
    const hit = comments.find((c) => c && (c.body || '').includes(MARKER));
    if (hit) existingId = hit.id;
  }

  if (existingId) {
    const res = gh(
      ['api', '--method', 'PATCH', `repos/${repo}/issues/comments/${existingId}`, '-F', 'body=@-'],
      body
    );
    return res.status === 0;
  }
  const res = gh(
    ['api', '--method', 'POST', `repos/${repo}/issues/${prNumber}/comments`, '-F', 'body=@-'],
    body
  );
  return res.status === 0;
}

function main(argv) {
  // argv: [changedFilesPath]. Env: GH_REPO, PR_NUMBER, CODE_GRAPH_FAIL_ON_RISK.
  const fs = require('fs');
  const changedPath = argv[0];
  if (!changedPath) {
    console.error('usage: pr-impact-comment.js <changed-files.txt>');
    process.exit(2);
  }
  const binary = resolveBinary();
  if (!binary) {
    console.error('[pr-impact] code-graph-mcp binary not found; skipping review.');
    return; // best-effort: never fail the PR over a missing analyzer
  }

  const changedFiles = fs.readFileSync(changedPath, 'utf8')
    .split('\n').map((s) => s.trim()).filter(Boolean);

  const review = computeReview(binary, changedFiles, process.cwd());
  if (!review) {
    console.error('[pr-impact] affected analysis failed; skipping comment.');
    return;
  }

  const body = renderMarkdown(review);
  const repo = process.env.GH_REPO;
  const prNumber = process.env.PR_NUMBER;
  if (repo && prNumber) {
    const ok = upsertComment(repo, prNumber, body);
    if (!ok) console.error('[pr-impact] failed to upsert PR comment.');
  } else {
    // No PR context (e.g. local run) — print to stdout for inspection.
    process.stdout.write(body + '\n');
  }

  const unanalyzed = review.unanalyzed || [];
  if (unanalyzed.length > 0) {
    console.error(`[pr-impact] ${unanalyzed.length} changed file(s) could not be analyzed: ${unanalyzed.join(', ')}`);
  }

  const failOnRisk = /^(1|true|yes)$/i.test(process.env.CODE_GRAPH_FAIL_ON_RISK || '');
  if (failOnRisk && review.uncovered.length > 0) {
    console.error(`[pr-impact] fail-on-risk: ${review.uncovered.length} changed file(s) have no covering test.`);
    process.exit(1);
  }
  // A file the analyzer never answered for is unmeasured risk, not cleared
  // risk: under an explicit fail-on-risk gate it blocks like a test gap does,
  // with its own message so the two causes stay distinguishable in CI logs.
  if (failOnRisk && unanalyzed.length > 0) {
    console.error(`[pr-impact] fail-on-risk: ${unanalyzed.length} changed file(s) could not be analyzed.`);
    process.exit(1);
  }
}

if (require.main === module) {
  main(process.argv.slice(2));
}

module.exports = {
  isTestPath, renderMarkdown, computeReview, upsertComment, MARKER,
  parseGhPagedArray, splitJsonDocuments,
};
