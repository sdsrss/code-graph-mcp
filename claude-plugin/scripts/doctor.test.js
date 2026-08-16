'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');

const fs = require('fs');
const os = require('os');
const path = require('path');

const { runDiagnostics, formatReport, surveyHookCoverage } = require('./doctor');
const { buildSettingsHookEntries } = require('./lifecycle');

// Build a settings.json whose hooks exactly mirror what we'd register now.
function settingsWithCurrentHooks() {
  const desired = buildSettingsHookEntries();
  const hooks = {};
  for (const [event, entries] of Object.entries(desired)) {
    hooks[event] = entries.map(e => JSON.parse(JSON.stringify(e)));
  }
  return { hooks };
}

test('runDiagnostics returns an array of check results', () => {
  const results = runDiagnostics();
  assert.ok(Array.isArray(results));
  assert.ok(results.length > 0, 'should have at least one check result');
  for (const r of results) {
    assert.equal(typeof r.name, 'string');
    assert.ok(['ok', 'warn', 'error', 'skip'].includes(r.status));
    assert.equal(typeof r.detail, 'string');
  }
});

test('formatReport produces readable output', () => {
  const results = [
    { name: 'Binary version', status: 'ok', detail: 'v0.7.16' },
    { name: 'Source fresh', status: 'warn', detail: 'src/ modified 3min after binary', fixId: 'binary-stale' },
    { name: 'Schema', status: 'ok', detail: 'v6' },
  ];
  const output = formatReport(results);
  assert.ok(output.includes('Binary version'));
  assert.ok(output.includes('v0.7.16'));
  assert.ok(output.includes('Source fresh'));
  assert.ok(output.includes('3min'));
});

test('formatReport shows issue count when problems exist', () => {
  const results = [
    { name: 'Test', status: 'warn', detail: 'problem', fixId: 'test-fix' },
  ];
  const output = formatReport(results);
  assert.ok(output.includes('1'));
  assert.ok(output.includes('issue'));
});

test('formatReport: --check-only never says "Fixing..." (it does not repair)', () => {
  const results = [
    { name: 'Hook coverage', status: 'warn', detail: 'missing', fixId: 'hooks' },
  ];
  // Default (repair mode) announces the fix.
  assert.ok(formatReport(results).includes('Fixing...'),
    'repair mode should announce Fixing...');
  // --check-only is read-only: it must NOT claim to fix, and should point the
  // user at the repair command instead.
  const checkOnly = formatReport(results, { checkOnly: true });
  assert.ok(!checkOnly.includes('Fixing...'),
    `--check-only must not say "Fixing..."; got: ${checkOnly}`);
  assert.ok(checkOnly.includes('--check-only'),
    `--check-only should hint how to fix; got: ${checkOnly}`);
});

test('formatReport shows all-clear when no problems', () => {
  const results = [
    { name: 'Binary version', status: 'ok', detail: 'v0.7.16' },
    { name: 'Schema', status: 'ok', detail: 'v6' },
  ];
  const output = formatReport(results);
  assert.ok(output.includes('All checks passed') || output.includes('0 issues'));
});

test('surveyHookCoverage reports clean when all entries are current', () => {
  const cov = surveyHookCoverage(settingsWithCurrentHooks());
  assert.equal(cov.missing.length, 0, 'no missing entries');
  assert.equal(cov.stale.length, 0, 'no stale entries');
});

test('surveyHookCoverage flags a present-but-stale hook path', () => {
  const settings = settingsWithCurrentHooks();
  // Repoint one PreToolUse entry at an old, now-pruned plugin-cache version dir —
  // present, recognized as ours (description unchanged), but the script path no
  // longer exists on disk. replaceAll (not replace) so BOTH the `if [ -f "…" ]`
  // guard and the `node "…"` exec path move to the dead dir — a realistic stale
  // entry (the executed path is what staleness keys off; a half-mutated command
  // whose exec path stayed current is, correctly, not stale).
  const bash = settings.hooks.PreToolUse.find(e => e.matcher === 'Bash');
  bash.hooks[0].command = bash.hooks[0].command.replaceAll('/scripts/', '/0.0.1-old/scripts/');
  const cov = surveyHookCoverage(settings);
  assert.equal(cov.missing.length, 0, 'entry is present, not missing');
  assert.ok(cov.stale.includes('PreToolUse:Bash'),
    `stale Bash path should be flagged; got stale=${JSON.stringify(cov.stale)}`);
});

test('surveyHookCoverage flags missing entries when settings empty', () => {
  const cov = surveyHookCoverage({});
  assert.ok(cov.missing.length === cov.expected.length, 'all expected entries missing');
  assert.equal(cov.stale.length, 0, 'nothing present to be stale');
});

// ── relicRepairGuard (v0.50.0 — doctor twin of the session-init relic guard) ──

test('relicRepairGuard blocks settings repair from a relic copy and redirects', () => {
  const { relicRepairGuard } = require('./doctor');
  const lines = [];
  // Relic context → guard fires, prints the redirect, returns true (skip install).
  assert.equal(relicRepairGuard({ relic: true, log: (s) => lines.push(s) }), true);
  assert.ok(lines.some(l => l.includes('not the active install')),
    `guard must explain why repair is skipped, got: ${lines.join(' | ')}`);
  // Active (or dev/npm) context → repair proceeds.
  assert.equal(relicRepairGuard({ relic: false, log: () => {} }), false);
});

// ── classifyEmbeddings (vector-availability — warns on silent FTS5-only) ──

test('classifyEmbeddings WARNS when embed-capable but nothing embedded (vector inactive)', () => {
  const { classifyEmbeddings } = require('./doctor');
  // The exact silent-FTS5 gap: model_available compile-flag true, real embeddable
  // nodes exist, but 0 embedded (model never downloaded/loaded).
  const r = classifyEmbeddings({ model_available: true, embedding_progress: '0/2745',
    embedding_status: 'pending', search_mode: 'fts_only' });
  assert.equal(r.status, 'warn', 'must not false-green a vector-inactive index');
  assert.match(r.detail, /FTS5-only|vector INACTIVE/);
});

test('classifyEmbeddings reports the real download outcome, not "retry shortly" (issue #35)', () => {
  const { classifyEmbeddings } = require('./doctor');
  const base = { model_available: true, embedding_progress: '0/27201',
    embedding_status: 'pending', search_mode: 'fts_only' };

  // A download that failed must SAY so, with its cause. Advising the user to
  // wait is what made a permanently-broken install look like a slow one.
  const failed = classifyEmbeddings({ ...base,
    model_download: 'download FAILED after 3 attempt(s): tls handshake rejected' });
  assert.equal(failed.status, 'warn');
  assert.match(failed.detail, /FAILED after 3 attempt\(s\): tls handshake rejected/);
  assert.doesNotMatch(failed.detail, /retry shortly/);

  // No record at all is a DIFFERENT diagnosis: the download never started.
  const never = classifyEmbeddings(base);
  assert.equal(never.status, 'warn');
  assert.match(never.detail, /NO download has ever been attempted/);
  assert.match(never.detail, /CODE_GRAPH_MODEL_DIR/);

  // In-flight is the one state where waiting IS the right advice.
  const inflight = classifyEmbeddings({ ...base, model_download: 'download in flight (attempt 1)' });
  assert.match(inflight.detail, /in flight/);

  // Weights already on disk (npm plugin installs them without the download
  // marker): "NO download has ever been attempted" would contradict the
  // filesystem — the missing step is only a server session to load them.
  const present = classifyEmbeddings({ ...base, model_files_present: true });
  assert.equal(present.status, 'warn');
  assert.match(present.detail, /model files present/);
  assert.doesNotMatch(present.detail, /NO download has ever been attempted/);

  // ≥0.116 splits that bool. `ready` keeps the restart advice…
  const ready = classifyEmbeddings({ ...base, model_files_present: true, model_files_state: 'ready' });
  assert.match(ready.detail, /restart the MCP server/);

  // …but `unverified` (weights hand-placed in the platform CACHE dir, no current
  // .model-id) must NOT advise a restart: the server re-downloads instead, so the
  // restart cannot help the offline user who put them there.
  const unverified = classifyEmbeddings({ ...base, model_files_present: true, model_files_state: 'unverified' });
  assert.equal(unverified.status, 'warn');
  assert.match(unverified.detail, /re-downloads on next start/);
  assert.match(unverified.detail, /CODE_GRAPH_MODEL_DIR/);
  assert.doesNotMatch(unverified.detail, /restart the MCP server/);

  // Absent state must still reach the download-history diagnosis, not the
  // files-on-disk branch.
  const absent = classifyEmbeddings({ ...base, model_files_present: false, model_files_state: 'absent' });
  assert.match(absent.detail, /NO download has ever been attempted/);
});

test('classifyEmbeddings WARNS when binary lacks embed-model feature', () => {
  const { classifyEmbeddings } = require('./doctor');
  const r = classifyEmbeddings({ model_available: false, embedding_progress: '0/0' });
  assert.equal(r.status, 'warn');
  assert.match(r.detail, /without embed-model/);
});

test('classifyEmbeddings OK for hybrid (partial + complete) and no-embeddable', () => {
  const { classifyEmbeddings } = require('./doctor');
  assert.equal(classifyEmbeddings({ model_available: true, embedding_progress: '900/2745' }).status, 'ok');
  assert.equal(classifyEmbeddings({ model_available: true, embedding_progress: '2745/2745' }).status, 'ok');
  // total === 0 is a non-code index, genuinely nothing to embed → ok, not a false warn.
  const none = classifyEmbeddings({ model_available: true, embedding_progress: '0/0' });
  assert.equal(none.status, 'ok');
  assert.match(none.detail, /no embeddable nodes/);
});

// ── dev-rebuild feature preservation (no silent hybrid→FTS5 downgrade / ping-pong) ──
test('devBuildCommand preserves feature set: hybrid → --features embed-model, fts → --no-default-features', () => {
  const { devBuildCommand } = require('./doctor');
  assert.match(devBuildCommand(true), /--features embed-model/);
  assert.doesNotMatch(devBuildCommand(true), /--no-default-features/);
  assert.match(devBuildCommand(false), /--no-default-features/);
  assert.doesNotMatch(devBuildCommand(false), /--features embed-model/);
});

test('detectEmbedModel reads model_available from `health-check --json`; probe failure → null (never a false downgrade signal)', () => {
  const { detectEmbedModel } = require('./doctor');
  // hybrid binary
  const hybridStub = (_bin, args) => {
    assert.deepEqual(args, ['health-check', '--json']);
    return JSON.stringify({ model_available: true });
  };
  assert.equal(detectEmbedModel('/bin/cg', hybridStub), true);
  // FTS5-only binary
  assert.equal(detectEmbedModel('/bin/cg', () => JSON.stringify({ model_available: false })), false);
  // probe throws (binary broken) → null (caller defaults to FTS5 + note, not a downgrade claim)
  assert.equal(detectEmbedModel('/bin/cg', () => { throw new Error('boom'); }), null);
  // unparseable output → null
  assert.equal(detectEmbedModel('/bin/cg', () => 'not json'), null);
  // no binary → null
  assert.equal(detectEmbedModel(null), null);
});

test('unresolvedCount: repair mode exits 0 iff every found issue was fixed', () => {
  const { unresolvedCount } = require('./doctor');
  // Clean run — nothing found.
  assert.equal(unresolvedCount({ checkOnly: false, issueCount: 0, fixed: 0 }), 0);
  // Repair fixed everything ("N/N addressed") → 0, so `doctor && …` and
  // self-heal automation don't read a successful repair as failure. This is
  // the regression this contract guards: previously exited 1 on any issue found.
  assert.equal(unresolvedCount({ checkOnly: false, issueCount: 3, fixed: 3 }), 0);
  // Partial repair → the remainder is unresolved (nonzero → exit 1).
  assert.equal(unresolvedCount({ checkOnly: false, issueCount: 3, fixed: 1 }), 2);
  // Advisory-only issue with no working auto-repair (fixed stays 0) → unresolved.
  assert.equal(unresolvedCount({ checkOnly: false, issueCount: 1, fixed: 0 }), 1);
});

test('unresolvedCount: --check-only reports every found issue (never repairs)', () => {
  const { unresolvedCount } = require('./doctor');
  // check-only performs no repair, so fixed is 0; a found issue must still
  // surface as unresolved (exit 1) — check mode reports cleanliness.
  assert.equal(unresolvedCount({ checkOnly: true, issueCount: 2, fixed: 0 }), 2);
  assert.equal(unresolvedCount({ checkOnly: true, issueCount: 0, fixed: 0 }), 0);
});

// ── Integrity (audit DB-1) ─────────────────────────────────────────────────
//
// The `integrity` block shipped in v0.113.0 and nothing here consumed it: when
// the index is unhealthy the binary prints the full report to stdout and THEN
// exits 1, execFileSync throws on the exit code, and the catch arm reported
// `Schema: error … fixId:'binary-broken'` — a fixId runRepairs has no case for.
// A genuinely corrupt index was blamed on the binary, with no repair offered.

test('parseHealthPayload recovers the report from a nonzero-exit stdout, and only a report', () => {
  const { parseHealthPayload } = require('./doctor');
  const report = JSON.stringify({ healthy: false, schema_version: 10, nodes: 5 });
  assert.equal(parseHealthPayload(Buffer.from(report)).schema_version, 10,
    'a real report on stdout must be recovered even though the process exited 1');
  assert.equal(parseHealthPayload(`  ${report}\n`).nodes, 5, 'string input + surrounding whitespace');
  assert.equal(parseHealthPayload(JSON.stringify({ reason: 'no_index' })).reason, 'no_index');

  // Anything that is not this command's payload must NOT be read as one — a
  // panic or a wrapper's noise becoming "a clean bill of health" is worse than
  // the bug being fixed.
  assert.equal(parseHealthPayload(null), null);
  assert.equal(parseHealthPayload(''), null);
  assert.equal(parseHealthPayload('thread panicked at src/cli.rs:1'), null);
  assert.equal(parseHealthPayload('{"ok":true}'), null, 'JSON without a report key is not a report');
  assert.equal(parseHealthPayload('[1,2,3]'), null, 'an array is not a report');
  assert.equal(parseHealthPayload('null'), null);
});

test('the corrupt-index payload the BINARY emits is routed, not rejected', () => {
  // Cross-language contract. The Rust side gained a `reason: "corrupt"` payload
  // for an index it cannot open; `parseHealthPayload` keyed only on
  // `schema_version`, which that payload does not carry, so the one case where
  // recovering the report matters most fell through to `binary-broken` — the
  // fixId with no repair. Neither side's unit tests could see it: each was
  // asserted against its own fixture. Caught only by running the real binary
  // against a real clobbered index.
  //
  // These are the ACTUAL keys `health-check --json` emits on that path (checked
  // against the binary), so drifting either side reds this.
  const { parseHealthPayload, classifyHealthReport } = require('./doctor');
  const payload = {
    healthy: false,
    reason: 'corrupt',
    schema_version: null,
    issue: 'index database is corrupt: file is not a database (/p/.code-graph/index.db). '
      + 'The index is a rebuildable cache — run: code-graph-mcp rebuild-index --confirm',
    integrity: {
      quick_check: 'index database is corrupt: file is not a database (/p/.code-graph/index.db).',
      fts_drift: null,
      orphan_vectors: null,
    },
    nodes: 0, edges: 0, files: 0, watching: false, db_size_bytes: 0,
    search_mode: 'fts_only', embedding_progress: '0/0', embedding_coverage_pct: 0,
    embedding_status: 'unavailable', model_available: false,
    snapshot: { status: 'absent' },
  };

  assert.ok(parseHealthPayload(JSON.stringify(payload)),
    'the binary emits this on stdout before exiting 1 — it must be recognised as a report');

  const byName = Object.fromEntries(classifyHealthReport(payload).map((r) => [r.name, r]));
  assert.equal(byName.Integrity.status, 'error');
  assert.equal(byName.Integrity.fixId, 'index-corrupt', 'must route to a repair that exists');
  // The zeroed counters are "unmeasurable", not "measured as zero". Reporting
  // `Schema: ok vnull` or `Index: warn empty -> index-empty` off them would
  // fabricate a verdict and send the repair down the wrong path.
  assert.equal(byName.Schema.status, 'skip');
  assert.equal(byName.Index.status, 'skip');
  assert.notEqual(byName.Index.fixId, 'index-empty',
    'a corrupt index must not be repaired as an empty one');
});

test('classifyIntegrity: severity differs per probe, and an absent block is not a pass', () => {
  const { classifyIntegrity } = require('./doctor');
  const of = (integrity) => classifyIntegrity({ integrity });

  // quick_check complaining = pages do not read back.
  const corrupt = of({ quick_check: 'row 12 missing from index nodes_fts', fts_drift: 0, orphan_vectors: 0 });
  assert.equal(corrupt.status, 'error');
  assert.equal(corrupt.fixId, 'index-corrupt', 'must route to a repair that exists');
  assert.match(corrupt.detail, /CORRUPT/);

  // FTS drift = wrong search answers, no crash.
  const drift = of({ quick_check: 'ok', fts_drift: -3, orphan_vectors: 0 });
  assert.equal(drift.status, 'warn');
  assert.equal(drift.fixId, 'index-corrupt');
  assert.match(drift.detail, /drifted from nodes by -3/);

  // Orphan vectors alone: disclosed, but NOT an issue. Answers stay correct and
  // the only repair is a full rebuild — a permanent warn with a disproportionate
  // fix is how doctor ends up exiting 1 forever on an install that is fine.
  const orphans = of({ quick_check: 'ok', fts_drift: 0, orphan_vectors: 7 });
  assert.equal(orphans.status, 'ok', 'orphan vectors must not raise an issue on their own');
  assert.match(orphans.detail, /7 orphan vector\(s\)/, '...but the count must still be visible');

  // Deliberate skips and unmeasurable probes are not verdicts either way.
  assert.equal(of({ quick_check: 'skipped_large', fts_drift: 0, orphan_vectors: 0 }).status, 'skip');
  assert.equal(of({ quick_check: null, fts_drift: null, orphan_vectors: null }).status, 'skip');
  assert.equal(classifyIntegrity({}).status, 'skip', 'binary too old to report = skip, never ok');
  assert.match(classifyIntegrity({}).detail, /not reported by this binary version/);

  // Healthy.
  assert.equal(of({ quick_check: 'ok', fts_drift: 0, orphan_vectors: 0 }).status, 'ok');
});

test('healthRows: a nonzero exit carrying a report is a DIAGNOSIS, not a broken binary', () => {
  // The seam the bug actually lived in. `health-check --json` prints the report
  // and then exits 1 when unhealthy; execFileSync throws on the exit code.
  const { healthRows } = require('./doctor');
  const exitOne = (stdout, stderr = '') => () => {
    const err = new Error('Command failed');
    err.status = 1; err.stdout = stdout; err.stderr = stderr;
    throw err;
  };
  const report = JSON.stringify({
    healthy: false, schema_version: 10, nodes: 4422, edges: 9001, files: 232,
    embedding_progress: '0/0', model_available: true,
    integrity: { quick_check: 'database disk image is malformed', fts_drift: 0, orphan_vectors: 0 },
  });

  const rows = healthRows('/bin/cg', { runHealthCheck: exitOne(report) });
  const byName = Object.fromEntries(rows.map((r) => [r.name, r]));
  assert.equal(byName.Integrity.status, 'error');
  assert.equal(byName.Integrity.fixId, 'index-corrupt');
  assert.notEqual(byName.Schema.fixId, 'binary-broken',
    'the binary ran fine and told us exactly what is wrong — do not blame it');
  assert.equal(byName.Index.status, 'ok', 'Index must not be `skip: health-check failed`');

  // A genuine binary failure (no payload on stdout) must STILL report broken.
  const broken = healthRows('/bin/cg', { runHealthCheck: exitOne('', 'Segmentation fault') });
  assert.equal(broken.find((r) => r.name === 'Schema').fixId, 'binary-broken',
    'negative control: without a recoverable report the old diagnosis is still right');

  // And "no index" still routes to index-empty rather than binary-broken.
  const noIndex = healthRows('/bin/cg', { runHealthCheck: exitOne('', 'No index found at .code-graph') });
  assert.equal(noIndex.find((r) => r.name === 'Index').fixId, 'index-empty');

  // Healthy path: exit 0, plain payload.
  const ok = healthRows('/bin/cg', {
    runHealthCheck: () => JSON.stringify({
      healthy: true, schema_version: 10, nodes: 10, edges: 20, files: 3,
      embedding_progress: '0/0', model_available: true,
      integrity: { quick_check: 'ok', fts_drift: 0, orphan_vectors: 0 },
    }),
  });
  assert.equal(ok.find((r) => r.name === 'Integrity').status, 'ok');
});

test('classifyHealthReport routes a corrupt index to index-corrupt, not binary-broken', () => {
  const { classifyHealthReport } = require('./doctor');
  const rows = classifyHealthReport({
    healthy: false, schema_version: 10, nodes: 4422, edges: 9001, files: 232,
    embedding_progress: '0/0', model_available: true,
    integrity: { quick_check: 'database disk image is malformed', fts_drift: 0, orphan_vectors: 0 },
    issue: 'database integrity check failed: database disk image is malformed. …',
  });
  const byName = Object.fromEntries(rows.map((r) => [r.name, r]));
  assert.equal(byName.Integrity.fixId, 'index-corrupt');
  assert.equal(byName.Schema.status, 'ok',
    'schema is fine — the OLD code reported `Schema: error … binary-broken` for exactly this payload');
  assert.equal(byName.Index.status, 'ok', 'the index has rows; it is the pages that are bad');
});

test('runRepairs: index-corrupt counts fixed only when the post-rebuild probe is clean', () => {
  // Same discipline as the hooks-invalid arm: `rebuild-index` exiting 0 says the
  // command ran, not that the corruption cleared (failing hardware reproduces it
  // immediately). Both the spawn and the re-probe are injected — doctor.js
  // destructures execFileSync at load, so patching child_process afterwards
  // stubs nothing and would run the real rebuild against this repo's index.
  const { runRepairs } = require('./doctor');
  const corrupt = [{ name: 'Integrity', status: 'error', fixId: 'index-corrupt' }];

  let rebuilds = 0;
  const ran = () => { rebuilds++; return true; };

  assert.equal(
    runRepairs(corrupt, { rebuildIndex: () => { throw new Error('index is locked by another process'); },
                          integrityOk: () => true }),
    0, 'a rebuild that threw must not count as fixed even when the probe would pass');

  assert.equal(runRepairs(corrupt, { rebuildIndex: ran, integrityOk: () => false }), 0,
    'rebuild ran but integrity still fails → not fixed (exit 0 is not evidence)');

  assert.equal(runRepairs(corrupt, { rebuildIndex: ran, integrityOk: () => true }), 1,
    'rebuild ran and the re-probe is clean → fixed');

  assert.equal(runRepairs(corrupt, { rebuildIndex: () => false, integrityOk: () => true }), 0,
    'no binary to rebuild with → not fixed, and the probe must not be consulted');

  assert.equal(rebuilds, 2, 'precondition: the spawn injection is live, not inert');
});

// ── P1-13: every emitted fixId must have a repair arm ───────────────────────
//
// `binary-broken` was emitted from two places and handled by none, so runRepairs
// fell through `default: break` and doctor printed "1 issue(s) found. Fixing..."
// followed by "0/1 addressed" with nothing at all in between — and exit 1. The
// per-arm tests below cover the new arm; THIS test is the meta-guard the audit
// asked for, so the next fixId cannot ship orphaned.
//
// Source-derived on purpose: exported sets would have to be kept in sync by
// hand, which is the same failure mode one level up.
function doctorSource() {
  return fs.readFileSync(path.join(__dirname, 'doctor.js'), 'utf8');
}

function emittedFixIds(src) {
  return new Set([...src.matchAll(/fixId:\s*'([a-z0-9-]+)'/g)].map((m) => m[1]));
}

function handledFixIds(src) {
  const start = src.indexOf('function runRepairs(');
  assert.ok(start > 0, 'runRepairs must still be a top-level function for this guard to see it');
  // Its body ends at the first line that is exactly `}`. Not `\n}`: runRepairs'
  // own destructured options default closes with `} = {}) {` at column 0, so the
  // looser marker cut the body off at the signature and the scanner reported
  // zero repair cases — which would have passed as "no orphans" if the positive
  // control below had not caught it.
  const end = src.indexOf('\n}\n', start);
  assert.ok(end > start, 'could not find the end of runRepairs');
  const body = src.slice(start, end);
  return new Set([...body.matchAll(/case\s+'([a-z0-9-]+)':/g)].map((m) => m[1]));
}

test('every fixId doctor emits has a runRepairs case (meta-guard)', () => {
  const src = doctorSource();
  const emitted = emittedFixIds(src);
  const handled = handledFixIds(src);

  // Positive control: the scanner must actually be seeing both sides. A regex
  // that silently matched nothing would report a clean sweep forever.
  assert.ok(emitted.size >= 8, `expected the scanner to find the emitted fixIds, found ${emitted.size}`);
  assert.ok(handled.size >= 6, `expected the scanner to find the repair cases, found ${handled.size}`);
  assert.ok(emitted.has('binary-broken') && handled.has('index-empty'), 'sanity: known ids on both sides');

  const orphans = [...emitted].filter((id) => !handled.has(id)).sort();
  assert.deepEqual(orphans, [],
    `these fixIds route to \`default: break\` — doctor promises "Fixing..." and does nothing: ${orphans.join(', ')}`);
});

test('runRepairs: binary-broken re-downloads and counts fixed only on a verified re-diagnosis', () => {
  const { runRepairs } = require('./doctor');
  const broken = [{ name: 'Binary version', status: 'error', detail: 'failed to read version', fixId: 'binary-broken' }];

  let updates = 0;
  const update = () => { updates++; };

  assert.equal(runRepairs(broken, { devMode: () => false, runAutoUpdate: update, binaryUsable: () => true }), 1,
    'update ran and the binary now answers --version + health-check → fixed');

  assert.equal(runRepairs(broken, { devMode: () => false, runAutoUpdate: update, binaryUsable: () => false }), 0,
    'the updater exits 0 for a dozen reasons — only the re-diagnosis may count a fix');

  assert.equal(
    runRepairs(broken, {
      devMode: () => false,
      runAutoUpdate: () => { throw new Error('no network'); },
      binaryUsable: () => true,
    }),
    0, 'an update that threw must not count as fixed even when the probe would pass');

  assert.equal(updates, 2, 'precondition: the update injection is live, not inert');
});

test('runRepairs: binary-broken in dev mode rebuilds from source instead of downloading', () => {
  const { runRepairs } = require('./doctor');
  const broken = [{ name: 'Schema', status: 'error', detail: 'health-check failed: Segmentation fault', fixId: 'binary-broken' }];

  let downloaded = 0;
  let built = null;
  const fixed = runRepairs(broken, {
    devMode: () => true,
    runAutoUpdate: () => { downloaded++; },
    buildBinary: (cmd) => { built = cmd; return true; },
    binaryUsable: () => true,
  });

  assert.equal(downloaded, 0, 'a source checkout must not be "repaired" by downloading a release binary');
  assert.match(String(built), /^cargo build --release/);
  assert.equal(fixed, 1);
});

test('integrityResolved re-classifies with the SAME function the diagnosis used', () => {
  // If "resolved" were its own predicate it could drift from "not raised", and
  // doctor would count a repair that left the issue standing.
  const { integrityResolved } = require('./doctor');
  const probe = (integrity) => () => ({ schema_version: 10, integrity });

  assert.equal(integrityResolved({ probe: probe({ quick_check: 'ok', fts_drift: 0, orphan_vectors: 0 }) }), true);
  assert.equal(integrityResolved({ probe: probe({ quick_check: 'malformed', fts_drift: 0, orphan_vectors: 0 }) }), false);
  assert.equal(integrityResolved({ probe: probe({ quick_check: 'ok', fts_drift: 5, orphan_vectors: 0 }) }), false,
    'a warn-level finding is still unresolved');
  assert.equal(integrityResolved({ probe: probe({ quick_check: 'ok', fts_drift: 0, orphan_vectors: 9 }) }), true,
    'orphan vectors alone never raised an issue, so they cannot block resolution either');
  assert.equal(integrityResolved({ probe: () => null }), false,
    'no payload at all is not evidence of repair');
});

test('runRepairs: hooks-invalid counts fixed only when the post-install re-scan is clean', () => {
  // hooks-invalid is raised only after diagnosis already ran install()+re-scan
  // and paths were STILL broken. The repair arm must re-verify, else it reports
  // a false exit 0 ("healthy") while the hooks stay broken. Stub the lifecycle
  // deps runRepairs pulls via require('./lifecycle') on the shared cached export
  // object; restore in finally so no other test sees the stubs.
  const { runRepairs } = require('./doctor');
  const lc = require('./lifecycle');
  const orig = { install: lc.install, scan: lc.scanForBrokenPaths, relic: lc.isStaleRelicContext };
  const hooksInvalid = [{ name: 'Hooks', status: 'warn', fixId: 'hooks-invalid' }];
  try {
    lc.isStaleRelicContext = () => false;   // not a relic → repair proceeds
    lc.install = () => {};                    // install() that cannot restore the paths
    // Re-scan still broken → must NOT count as fixed (old code did fixed++ blindly).
    lc.scanForBrokenPaths = () => [{ type: 'hook', event: 'PreToolUse:Edit', path: '/gone.js' }];
    assert.equal(runRepairs(hooksInvalid), 0, 'still-broken after install must not count as fixed');
    // Re-scan clean → the repair took effect → counts as fixed.
    lc.scanForBrokenPaths = () => [];
    assert.equal(runRepairs(hooksInvalid), 1, 'verified-clean after install counts as fixed');
  } finally {
    lc.install = orig.install;
    lc.scanForBrokenPaths = orig.scan;
    lc.isStaleRelicContext = orig.relic;
  }
});

// ── CLI argument handling ──────────────────────────────────────────────────
//
// Contract audit follow-up: `args.includes('--check-only')` ignored every other
// argument, so a typo'd flag ran the FULL repair pass — writing settings.json and
// MEMORY.md — while the user believed they had asked for the read-only mode. A
// typo silently inverting a read-only contract is the worst shape this flag can
// have, so an unrecognized argument now stops before any diagnosis runs.

const { execFileSync } = require('child_process');

// BOTH entry points. `node lifecycle.js doctor …` carried its own copy of the
// flag parsing, so the first version of this guard fixed doctor.js and left the
// sibling running the repair pass on a typo. They now share `runDoctorCli` from
// doctor.js, and every case below is asserted against both so they cannot drift.
const ENTRY_POINTS = [
  { label: 'doctor.js', argv: (args) => [path.join(__dirname, 'doctor.js'), ...args] },
  { label: 'lifecycle.js doctor', argv: (args) => [path.join(__dirname, 'lifecycle.js'), 'doctor', ...args] },
];

function runDoctorCli(homeDir, args, entry = ENTRY_POINTS[0]) {
  try {
    const stdout = execFileSync(process.execPath, entry.argv(args), {
      // CLAUDE_CONFIG_DIR as well as HOME: claude-config.js returns
      // `process.env.CLAUDE_CONFIG_DIR || path.join(os.homedir(), '.claude')`,
      // so the env var WINS and redirecting HOME alone leaves the sandbox open
      // for any developer who exports it. The `no arguments still repairs` case
      // below runs the full repair pass, which is what would land in their real
      // config. Same fix as tests/cli_e2e.rs; this JS sibling was missed.
      env: { ...process.env, HOME: homeDir, CLAUDE_CONFIG_DIR: path.join(homeDir, '.claude') },
      stdio: ['pipe', 'pipe', 'pipe'],
    }).toString();
    return { code: 0, stdout, stderr: '' };
  } catch (err) {
    return {
      code: err.status,
      stdout: err.stdout ? err.stdout.toString() : '',
      stderr: err.stderr ? err.stderr.toString() : '',
    };
  }
}

function freshHome(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-doctor-cli-'));
  fs.mkdirSync(path.join(dir, '.claude'), { recursive: true });
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
}

for (const entry of ENTRY_POINTS) {
  test(`${entry.label}: refuses an unknown argument instead of silently repairing`, (t) => {
    // Every near-miss spelling of the read-only flag.
    for (const typo of ['--check-onlyy', '--checkonly', '--check_only', '-check-only', '--dry-run']) {
      const home = freshHome(t);
      const r = runDoctorCli(home, [typo], entry);
      assert.equal(r.code, 2, `${typo} must be rejected, not ignored`);
      assert.match(r.stderr, /unknown argument/, `${typo} must say why`);
      assert.equal(r.stdout, '', `${typo} must not emit a diagnostic report`);
      assert.equal(
        fs.existsSync(path.join(home, '.claude', 'settings.json')), false,
        `${typo} must not have run the repair pass — that is the read-only contract ` +
        'the user thought they were invoking');
    }
  });

  test(`${entry.label}: --check-only still reports and still writes nothing`, (t) => {
    const home = freshHome(t);
    const r = runDoctorCli(home, ['--check-only'], entry);
    assert.ok(r.stdout.length > 0, 'the real flag still produces a report');
    assert.equal(fs.existsSync(path.join(home, '.claude', 'settings.json')), false,
      'read-only');
  });

  test(`${entry.label}: no arguments still repairs (the guard must not make it inert)`, (t) => {
    // Negative control for the two above.
    const home = freshHome(t);
    const r = runDoctorCli(home, [], entry);
    assert.ok(r.stdout.length > 0);
    assert.equal(fs.existsSync(path.join(home, '.claude', 'settings.json')), true,
      'the default mode must still perform repairs');
  });

  test(`${entry.label}: --help exits 0 without running diagnostics`, (t) => {
    const home = freshHome(t);
    const r = runDoctorCli(home, ['--help'], entry);
    assert.equal(r.code, 0);
    // Matches src/main.rs's help too — the two texts are kept in sync, and the
    // e2e test test_cli_js_subcommands_help_is_side_effect_free asserts the same
    // USAGE marker on the binary side.
    assert.match(r.stdout, /USAGE:\n\s+code-graph-mcp doctor/);
    assert.match(r.stdout, /--check-only/);
    assert.equal(fs.existsSync(path.join(home, '.claude', 'settings.json')), false,
      '--help must not run the repair pass — a help flag that acts is its own bug class');
  });
}

// ── an unwritable ~/.claude must be diagnosed as such by EVERY repair arm ────
//
// Round-6 F2/F4: the `settingsUnwritable` state was taught to
// `missing-hooks-in-settings` but not to `hooks-invalid`, and neither arm had a
// test. The consequence was a chmod being reported as "plugin scripts may be
// missing — reinstall the npm package": a diagnosis that sends the user to fix
// something that is not broken, from the tool whose job is to say what is.

function unwritableHome(t, seed) {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-doctor-ro-'));
  const claudeDir = path.join(home, '.claude');
  fs.mkdirSync(claudeDir, { recursive: true });
  fs.writeFileSync(path.join(claudeDir, 'settings.json'), JSON.stringify(seed, null, 2) + '\n');
  fs.chmodSync(claudeDir, 0o555);
  t.after(() => {
    try { fs.chmodSync(claudeDir, 0o755); } catch { /* already restored */ }
    fs.rmSync(home, { recursive: true, force: true });
  });
  return home;
}

test('a read-only ~/.claude is reported as a permissions problem, not a missing package', (t) => {
  // The entry has to be recognizable as OURS, or the coverage survey reports it
  // as missing and the *other* arm answers — which is how the first version of
  // this test passed while the arm it names stayed unguarded. Build the real
  // entries, then repoint every path at a dead directory (same technique as the
  // stale-path survey test above) so `hooks-invalid` is what gets raised.
  const desired = buildSettingsHookEntries();
  const hooks = {};
  for (const [event, entries] of Object.entries(desired)) {
    hooks[event] = entries.map((e) => {
      const copy = JSON.parse(JSON.stringify(e));
      copy.hooks = copy.hooks.map((h) => ({
        ...h,
        command: h.command.replaceAll('/scripts/', '/0.0.1-gone/scripts/'),
      }));
      return copy;
    });
  }
  const home = unwritableHome(t, { hooks });

  const r = runDoctorCli(home, []);
  const all = r.stdout + r.stderr;

  assert.match(all, /not writable/,
    'the real cause must appear in the report');
  // Scope the negative to THIS arm's own sentence. A bare /npm install -g/ scan
  // also matches the binary-not-found row, which is legitimate advice on a
  // machine without the binary — true on CI's plugin-tests job, which builds no
  // Rust, and never true locally. That made this assertion pass everywhere I ran
  // it and fail on the first CI run.
  assert.doesNotMatch(all, /hook path\(s\) still invalid/,
    'must NOT tell the user their plugin scripts are missing over a chmod — that ' +
    'is the misdiagnosis this arm exists to prevent');
  // The file really was left alone.
  const settings = JSON.parse(fs.readFileSync(path.join(home, '.claude', 'settings.json'), 'utf8'));
  assert.ok(settings.hooks.PreToolUse, 'settings untouched');
});

test('a read-only ~/.claude is reported by the missing-hooks arm too', (t) => {
  // No code-graph entries at all → `missing-hooks-in-settings` rather than
  // `hooks-invalid`. Both arms print about the same failed install() call and
  // have to agree about why it failed.
  const home = unwritableHome(t, { model: 'opus' });

  const r = runDoctorCli(home, []);
  const all = r.stdout + r.stderr;

  assert.match(all, /not writable/, 'the real cause must appear here as well');
  assert.doesNotMatch(all, /already had entries/,
    'must not claim the hooks were already registered');
  assert.notEqual(r.code, 0, 'nothing was fixed, so this is not a clean run');
});

// ── the exit code must reflect what doctor could NOT fix — on every entry point ─
//
// `unresolvedCount` has a unit test above, but a predicate test does not cover
// whether anything calls the predicate — this repo learned that in v0.45.3, on a
// self-heal glue that regressed twice while its predicate stayed green. The exit
// code is now produced by three entry points (doctor.js, `lifecycle.js doctor`,
// and the Rust binary, which used to filter argv before dispatch), so the wiring
// is exactly the part that can drift.
//
// The invariant asserted here is the CHANGELOG v0.85.4 promise in its own terms:
// exit 0 when every found issue was resolved, 1 when something was left, and
// --check-only nonzero whenever any issue exists at all.
//
// HONEST SCOPE — what this can and cannot catch. It compares the exit code
// against doctor's OWN "N/M addressed" line, so it only distinguishes
// "unresolved" from "found" in an environment where they differ, i.e. where
// every found issue was fixable. On a checkout whose src/ is newer than the
// built binary, the unfixable "Source fresh" issue makes remaining>0 always, and
// then `found>0` and `remaining>0` agree — reverting `unresolvedCount` to the
// pre-v0.85.4 `return issueCount` leaves these two tests GREEN (measured; only
// the predicate test above reddens). Treat the 0-branch as pinned by the
// predicate test plus the report-vs-code consistency here, NOT by these alone.
for (const entry of ENTRY_POINTS) {
  test(`${entry.label}: exit code equals "did anything remain unfixed"`, (t) => {
    const home = freshHome(t);
    const r = runDoctorCli(home, [], entry);
    const addressed = /(\d+)\/(\d+) issue\(s\) addressed/.exec(r.stdout);
    const found = /(\d+) issue\(s\) found/.exec(r.stdout);

    if (!found) {
      // A perfectly clean sandbox: nothing found, nothing to leave unfixed.
      assert.equal(r.code, 0, `no issues found must exit 0; got ${r.code}\n${r.stdout}`);
      return;
    }
    assert.ok(addressed, `a repair run that found issues must report N/M addressed:\n${r.stdout}`);
    const fixed = Number(addressed[1]);
    const total = Number(addressed[2]);
    const remaining = total - fixed;
    assert.equal(
      r.code, remaining > 0 ? 1 : 0,
      `exit code must key off issues left UNRESOLVED (${remaining} of ${total}), not issues found. ` +
      `A run that fixed everything and still exited 1 is what broke \`doctor && …\` ` +
      `and every self-heal caller.\n${r.stdout}`
    );
  });

  test(`${entry.label}: --check-only exits nonzero while any issue exists`, (t) => {
    const home = freshHome(t);
    const r = runDoctorCli(home, ['--check-only'], entry);
    if (/issue\(s\) found/.test(r.stdout)) {
      assert.notEqual(r.code, 0,
        `--check-only must stay nonzero while issues exist — it repairs nothing, ` +
        `so "all resolved" can never be true for it.\n${r.stdout}`);
    }
    assert.doesNotMatch(r.stdout, /issue\(s\) addressed/,
      '--check-only must not claim to have addressed anything');
  });
}

// ── Suspended auto-update is reported honestly (issue #40) ─────────────────
//
// When the updater has given up on a release (MAX_UPDATE_ATTEMPTS consecutive
// failed installs of the SAME version), doctor used to report "Auto-update: ok
// — up-to-date" and, on the neighbouring `update-incomplete` path, offer a
// repair that re-runs exactly the check that was suspended — printing
// "✅ Update check complete" and counting a fix that cannot happen.
test('doctor warns (with no phantom fix) when auto-update has suspended a release', (t) => {
  const home = freshHome(t);
  const cacheDir = path.join(home, '.cache', 'code-graph');
  fs.mkdirSync(cacheDir, { recursive: true });
  const { MAX_UPDATE_ATTEMPTS } = require('./auto-update');
  fs.writeFileSync(path.join(cacheDir, 'update-state.json'), JSON.stringify({
    latestVersion: '9.9.9',
    updateAvailable: true,
    updateAttempts: MAX_UPDATE_ATTEMPTS,
    binaryUpdated: true,
  }));

  const r = runDoctorCli(home, ['--check-only']);
  assert.match(r.stdout, /Auto-update/);
  assert.match(r.stdout, /failed to install 5×/, 'must name the failure count, not claim "up-to-date"');
  assert.match(r.stdout, /npm install -g @sdsrs\/code-graph/, 'must hand the user the manual route');

  // Control: one attempt below the cap is NOT reported as suspended — otherwise
  // this test would pass on any state file at all.
  const home2 = freshHome(t);
  fs.mkdirSync(path.join(home2, '.cache', 'code-graph'), { recursive: true });
  fs.writeFileSync(path.join(home2, '.cache', 'code-graph', 'update-state.json'), JSON.stringify({
    latestVersion: '9.9.9', updateAvailable: true, updateAttempts: MAX_UPDATE_ATTEMPTS - 1, binaryUpdated: true,
  }));
  assert.doesNotMatch(runDoctorCli(home2, ['--check-only']).stdout, /auto-retry suspended/);
});

// ── The binary repairs re-scan instead of trusting exit 0 (audit BIN-2) ─────
//
// `auto-update.js check` has no non-zero exit path at all: dev mode, the
// CODE_GRAPH_NO_AUTO_UPDATE opt-out, a suspended release, the rate-limit backoff
// and plain offline each print a line and exit 0. Both arms below counted "the
// spawn did not throw" as a fix, so doctor reported ✅ and exited 0 ("healthy")
// for a repair that provably could not have run — including the case the
// suspension notice sends the user here to try. Sibling of the hooks-invalid
// post-install re-scan, and of #8916 (exit code must reflect what REMAINS).

function captureStdout(t) {
  const lines = [];
  const orig = console.log;
  console.log = (...args) => lines.push(args.join(' '));
  t.after(() => { console.log = orig; });
  return lines;
}

for (const [fixId, resolvedKey, stillBroken] of [
  ['version-mismatch', 'binaryResolved', /binary version still does not match/],
  ['binary-stale', 'binaryResolved', /binary version still does not match/],
  ['update-incomplete', 'updateResolved', /binary download is still recorded as incomplete/],
]) {
  test(`runRepairs: ${fixId} counts a fix only when the post-check re-scan agrees`, (t) => {
    const { runRepairs } = require('./doctor');
    const issue = [{ name: 'x', status: 'warn', fixId }];
    // devMode false: the dev arm rebuilds with cargo, and this repo IS a dev
    // tree, so without this the test would compile Rust instead of testing.
    const base = { devMode: () => false, runAutoUpdate: () => { /* exits 0, does nothing */ } };

    const said = captureStdout(t);
    assert.equal(runRepairs(issue, { ...base, [resolvedKey]: () => false }), 0,
      'auto-update exiting 0 without repairing anything must not count as fixed');
    assert.match(said.join('\n'), stillBroken,
      'and the user must be told the check ran and changed nothing');
    assert.doesNotMatch(said.join('\n'), /✅/, 'no success tick for a repair that did not happen');

    assert.equal(runRepairs(issue, { ...base, [resolvedKey]: () => true }), 1,
      'control: a re-scan that comes back clean does count');
  });
}

test('runRepairs: a THROWING auto-update check is still reported as a failure', (t) => {
  const { runRepairs } = require('./doctor');
  const said = captureStdout(t);
  const fixed = runRepairs([{ fixId: 'version-mismatch' }], {
    devMode: () => false,
    runAutoUpdate: () => { throw new Error('spawn failed'); },
    // A throw must short-circuit BEFORE the re-scan, or a machine whose binary
    // happens to look fine would count a spawn failure as a repair.
    binaryResolved: () => { throw new Error('re-scan must not run after a throw'); },
  });
  assert.equal(fixed, 0);
  assert.match(said.join('\n'), /Update check failed/);
});

test('binaryVersionResolved mirrors the version-mismatch diagnosis', () => {
  const { binaryVersionResolved } = require('./doctor');
  const stub = (o) => ({ find: () => '/bin/cg', readVersion: () => '1.0.0', pluginVersion: () => '1.0.0', ...o });
  assert.equal(binaryVersionResolved(stub({})), true, 'versions agree → resolved');
  assert.equal(binaryVersionResolved(stub({ readVersion: () => '0.9.0' })), false, 'still behind → unresolved');
  assert.equal(binaryVersionResolved(stub({ readVersion: () => null })), false, 'unreadable → unresolved');
  assert.equal(binaryVersionResolved(stub({ find: () => null })), false, 'gone → unresolved');
});

test('updateIncompleteResolved mirrors the update-incomplete diagnosis', () => {
  const { updateIncompleteResolved } = require('./doctor');
  const at = (state) => updateIncompleteResolved({ readStateFile: () => state });
  assert.equal(at({ updateAvailable: true, binaryUpdated: false }), false,
    'the exact state that raised the issue is still unresolved');
  assert.equal(at({ updateAvailable: true, binaryUpdated: true }), true, 'the binary landed');
  assert.equal(at({ updateAvailable: false, binaryUpdated: false }), true, 'no update pending');
  assert.equal(at(null), true, 'no state file → nothing left to complete');
});

test('autoUpdateNoOpReason names why the updater did nothing (closes the doctor loop)', () => {
  const { autoUpdateNoOpReason } = require('./doctor');
  const { MAX_UPDATE_ATTEMPTS } = require('./auto-update');
  const suspended = {
    latestVersion: '9.9.9', updateAttempts: MAX_UPDATE_ATTEMPTS,
    suspendedAt: new Date().toISOString(),
  };
  // The suspension notice tells the user to run doctor; doctor must not then be
  // the one surface that fails to mention the suspension.
  assert.match(autoUpdateNoOpReason(suspended, {}), /SUSPENDED after 5 failed attempts on v9\.9\.9/);
  assert.match(autoUpdateNoOpReason({ rateLimited: true }, {}), /rate-limit backoff/);
  assert.match(autoUpdateNoOpReason(suspended, { CODE_GRAPH_NO_AUTO_UPDATE: '1' }),
    /CODE_GRAPH_NO_AUTO_UPDATE=1/, 'the opt-out outranks every state, since nothing runs at all');
  assert.equal(autoUpdateNoOpReason({ updateAvailable: true, updateAttempts: 1 }, {}), null,
    'no known blocker → say nothing rather than invent a cause');
  assert.equal(autoUpdateNoOpReason(null, {}), null);
});

test('autoUpdateNoOpReason names an EXHAUSTED binary self-heal', () => {
  const { autoUpdateNoOpReason } = require('./doctor');
  const { MAX_UPDATE_ATTEMPTS, isBinaryHealExhausted } = require('./auto-update');
  // The binary self-heal has its OWN budget, separate from the update
  // suspension: the updater is not suspended (updateAttempts is 0) but the
  // binary download has given up. That is exactly the state a `binary-broken`
  // row comes from, and it was the one parked state doctor could not name — so
  // the user was told "update manually" with no hint that the automatic repair
  // had already stopped trying (audit 2026-08-16 review Minor tail).
  const exhausted = {
    latestVersion: '9.9.9',
    binaryHealVersion: '9.9.9',
    binaryHealAttempts: MAX_UPDATE_ATTEMPTS,
    updateAttempts: 0,
  };
  assert.ok(isBinaryHealExhausted(exhausted), 'fixture must really be exhausted');
  assert.match(
    autoUpdateNoOpReason(exhausted, {}) || '',
    /binary/i,
    'doctor must name the exhausted binary self-heal',
  );
  assert.match(autoUpdateNoOpReason(exhausted, {}) || '', /9\.9\.9/);
  // Re-armed by a newer release: heal budget is keyed to the version it failed
  // on, so a moved `latestVersion` is NOT a blocker.
  assert.equal(
    autoUpdateNoOpReason({ ...exhausted, latestVersion: '9.9.10' }, {}), null,
    'a newer release re-arms the heal — do not report it as parked',
  );
});

// P2 (2026-08-16 audit §四): doctor's exit code is what `doctor && <next step>`
// and self-heal automation gate on, and ANY warn without a working repair pinned
// it at 1 for the life of the install — including a binary deliberately built
// without embed-model, and npm relics under a node version whose prefix the tool
// cannot reach. Neither is broken; both were permanent failures.
//
// The rule now: every warn/error row is either repairable (a fixId `runRepairs`
// really has a counting case for) or explicitly `advisory: true`. Inferring
// "advisory" from a missing fixId would have exempted the next forgotten row,
// so this checks the marker, not its absence.
test('every doctor row is repairable or explicitly advisory', () => {
  const src = fs.readFileSync(path.join(__dirname, 'doctor.js'), 'utf8');

  // fixIds `runRepairs` can actually resolve — the `case '…':` labels inside it.
  const repairsBody = src.slice(src.indexOf('function runRepairs'), src.indexOf('function unresolvedCount'));
  const repairable = new Set([...repairsBody.matchAll(/case '([a-z-]+)':/g)].map(m => m[1]));
  assert.ok(repairable.size >= 8, `expected the repair switch to be found, got ${[...repairable]}`);

  // Rows come from a real diagnostics run against a scratch HOME, so this reads
  // whatever the current build actually emits rather than a transcription.
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-doctor-rows-'));
  const prevHome = process.env.HOME;
  const prevConfig = process.env.CLAUDE_CONFIG_DIR;
  let rows;
  try {
    process.env.HOME = home;
    process.env.CLAUDE_CONFIG_DIR = path.join(home, '.claude');
    rows = runDiagnostics({ checkOnly: true });
  } finally {
    if (prevHome === undefined) delete process.env.HOME; else process.env.HOME = prevHome;
    if (prevConfig === undefined) delete process.env.CLAUDE_CONFIG_DIR;
    else process.env.CLAUDE_CONFIG_DIR = prevConfig;
    fs.rmSync(home, { recursive: true, force: true });
  }

  const stuck = rows
    .filter(r => r.status === 'warn' || r.status === 'error')
    .filter(r => !r.advisory && !(r.fixId && repairable.has(r.fixId)));
  assert.deepEqual(
    stuck.map(r => `${r.name}${r.fixId ? ` (fixId ${r.fixId})` : ' (no fixId)'}`),
    [],
    'these rows can never be resolved, so doctor would exit 1 forever: mark them ' +
    'advisory:true if nothing is broken, or wire a repair',
  );

  // Negative control: the check must be able to FAIL. A synthetic unrepairable
  // row has to be caught, or the assertion above is vacuous whenever the scratch
  // run happens to be clean.
  const synthetic = [{ name: 'Synthetic', status: 'warn', detail: 'x' }]
    .filter(r => !r.advisory && !(r.fixId && repairable.has(r.fixId)));
  assert.equal(synthetic.length, 1, 'the predicate must catch an unrepairable row');
});
