'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');

const { runDiagnostics, formatReport } = require('./doctor');

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
