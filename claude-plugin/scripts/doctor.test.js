'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');

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
  // Repoint one PreToolUse entry at an old plugin-cache version dir — present,
  // recognized as ours (description unchanged), but command no longer current.
  const bash = settings.hooks.PreToolUse.find(e => e.matcher === 'Bash');
  bash.hooks[0].command = bash.hooks[0].command.replace('/scripts/', '/0.0.1-old/scripts/');
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
