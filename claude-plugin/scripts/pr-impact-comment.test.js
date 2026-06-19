'use strict';
const { test } = require('node:test');
const assert = require('node:assert');
const fs = require('fs');
const os = require('os');
const path = require('path');

const { isTestPath, renderMarkdown, computeReview, MARKER } = require('./pr-impact-comment');

test('isTestPath mirrors domain::is_test_path patterns', () => {
  for (const p of [
    'tests/integration.rs', 'test/foo.js', 'benches/indexing.rs', 'bench/x.rs',
    'src/__tests__/a.ts', 'src/foo/tests.rs', 'pkg/x_test.go', 'src/y_test.rs',
    'a.test.ts', 'a.test.js', 'a.test.tsx', 'a.test.jsx',
    'a.spec.ts', 'a.spec.js', 'a.spec.tsx', 'a.spec.jsx',
  ]) {
    assert.ok(isTestPath(p), `${p} should be a test path`);
  }
  for (const p of ['src/lib.rs', 'src/graph/centrality.rs', 'README.md', 'src/testing.rs']) {
    assert.ok(!isTestPath(p), `${p} should NOT be a test path`);
  }
});

test('renderMarkdown: empty diff', () => {
  const md = renderMarkdown({ changed: [], not_indexed: [], tests: [], blast_radius: 0, top_affected: [], uncovered: [] });
  assert.ok(md.startsWith(MARKER), 'must start with marker');
  assert.match(md, /No code changes detected/);
});

test('renderMarkdown: only non-indexed changes', () => {
  const md = renderMarkdown({ changed: [], not_indexed: ['docs/x.md', 'new.rs'], tests: [], blast_radius: 0, top_affected: [], uncovered: [] });
  assert.match(md, /No \*\*indexed\*\* code changed \(2 changed file/);
});

test('renderMarkdown: full review with test gaps', () => {
  const md = renderMarkdown({
    changed: ['src/a.rs', 'src/b.rs'],
    not_indexed: ['NEW.md'],
    tests: ['tests/a_test.rs'],
    blast_radius: 20,
    top_affected: [{ path: 'src/c.rs', depth: 1 }, { path: 'src/d.rs', depth: 2 }],
    uncovered: ['src/b.rs'],
  });
  assert.ok(md.startsWith(MARKER));
  assert.match(md, /2\*\* changed indexed file/);
  assert.match(md, /blast radius \*\*20\*\*/);
  assert.match(md, /Test gaps \(1\)/);
  assert.match(md, /- `src\/b\.rs`/);
  assert.match(md, /Tests to re-run/);
  assert.match(md, /- `tests\/a_test\.rs`/);
  // 20 blast radius but only 2 shown → "top 2 of 20" + "…and 18 more"
  assert.match(md, /top 2 of 20/);
  assert.match(md, /…and 18 more/);
  assert.match(md, /1 changed file\(s\) not in index/);
});

// Stub binary: a node script that emulates `code-graph-mcp affected`.
// `affected --stdin --json` → aggregate; `affected <file> --json` → per-file.
function writeStubBinary(dir) {
  const stub = path.join(dir, 'stub-cg.js');
  fs.writeFileSync(stub, `#!/usr/bin/env node
'use strict';
const args = process.argv.slice(2);
// args[0] === 'affected'
if (args.includes('--stdin')) {
  process.stdout.write(JSON.stringify({
    changed: ['src/a.rs', 'src/b.rs', 'tests/a_test.rs'],
    tests: ['tests/a_test.rs'],
    affected_files: [{path:'tests/a_test.rs',depth:1,is_test:true},{path:'src/x.rs',depth:1,is_test:false}],
    not_indexed: ['NEW.md'],
  }));
  process.exit(0);
}
const file = args[1];
// src/a.rs is covered (has a test); src/b.rs is uncovered (no tests).
if (file === 'src/a.rs') {
  process.stdout.write(JSON.stringify({ changed:[file], tests:['tests/a_test.rs'], affected_files:[], not_indexed:[] }));
} else {
  process.stdout.write(JSON.stringify({ changed:[file], tests:[], affected_files:[], not_indexed:[] }));
}
process.exit(0);
`);
  fs.chmodSync(stub, 0o755);
  // Wrap so it's executable as a single binary path: use `node stub.js` via a shell shim.
  const shim = path.join(dir, 'cg');
  fs.writeFileSync(shim, `#!/usr/bin/env bash\nexec node "${stub}" "$@"\n`);
  fs.chmodSync(shim, 0o755);
  return shim;
}

test('computeReview: aggregate + per-file test-gap detection', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-prreview-'));
  try {
    const binary = writeStubBinary(dir);
    const review = computeReview(binary, ['src/a.rs', 'src/b.rs', 'tests/a_test.rs', 'NEW.md'], dir);
    assert.ok(review, 'review computed');
    assert.deepStrictEqual(review.tests, ['tests/a_test.rs']);
    assert.strictEqual(review.blast_radius, 2);
    assert.deepStrictEqual(review.not_indexed, ['NEW.md']);
    // src/b.rs has no covering test → uncovered; src/a.rs covered; test file skipped.
    assert.deepStrictEqual(review.uncovered, ['src/b.rs']);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('computeReview: returns null when binary unavailable', () => {
  const review = computeReview('/nonexistent/cg-binary-xyz', ['src/a.rs'], os.tmpdir());
  assert.strictEqual(review, null);
});
