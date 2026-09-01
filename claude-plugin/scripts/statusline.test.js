'use strict';
// statusline.js renders code-graph health into the Claude Code status line.
// The states are easy to confuse, so each is pinned here with a stub binary:
//
//   ✓/✗ N nodes | M files   — health-check produced a report (exit 0 OR exit 1)
//   ↻ updating              — no report + (schema-version error OR pending update)
//   offline                 — no report + genuine failure, nothing pending
//
// The critical regression this guards: health-check exits NON-ZERO on an
// empty/unhealthy index but still emits the full JSON report. statusline must
// render that report ("✗ 0 nodes"), NOT collapse it into "offline".
const test = require('node:test');
const assert = require('node:assert/strict');
const { execFileSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

// `CLAUDE_CONFIG_DIR` is dropped from THIS process before anything runs.
//
// Every sandbox below redirects HOME and then spawns with `{...process.env}`,
// which passes the variable straight through — and `claudeHome()` is
// `CLAUDE_CONFIG_DIR || homedir/.claude`, so the env var WINS over the
// redirected HOME. For a developer who exports it (the documented multi-profile
// setup) these tests wrote into their LIVE config: measured, a `9.9.9` plugin
// version landed in the real plugins cache. Deleting it here fixes every spawn
// site at once instead of 40 call sites, and `tests/hardening.rs`'s
// `js_test_files_neutralize_claude_config_dir` keeps new files from skipping it.
delete process.env.CLAUDE_CONFIG_DIR;

const STATUSLINE = path.join(__dirname, 'statusline.js');

function mkHome(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-statusline-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
}

// Write an executable shell stub named `code-graph-mcp` (find-binary's
// isNativeBinary requires that exact basename) and register it in the binary
// cache so findBinary() returns it before any dev/platform discovery.
//   --version           → prints `version` (must be >= pkg version to be "fresh")
//   health-check ...    → prints `report` to stdout, exits with `exitCode`
function installStubBinary(home, { report, stderr = '', exitCode = 0, version = '9.9.9' }) {
  const binDir = path.join(home, 'stub-bin');
  fs.mkdirSync(binDir, { recursive: true });
  const binPath = path.join(binDir, 'code-graph-mcp');
  const sh = (s) => String(s).replace(/'/g, `'\\''`);
  const payload = typeof report === 'string' ? report : JSON.stringify(report);
  const lines = [
    '#!/usr/bin/env bash',
    'if [ "$1" = "--version" ]; then',
    `  echo "${version}"; exit 0`,
    'fi',
  ];
  if (payload) lines.push(`printf '%s' '${sh(payload)}'`);
  if (stderr) lines.push(`printf '%s' '${sh(stderr)}' >&2`);
  lines.push(`exit ${exitCode}`, '');
  fs.writeFileSync(binPath, lines.join('\n'));
  fs.chmodSync(binPath, 0o755);

  const cacheDir = path.join(home, '.cache', 'code-graph');
  fs.mkdirSync(cacheDir, { recursive: true });
  fs.writeFileSync(path.join(cacheDir, 'binary-path'), binPath);
  return binPath;
}

function setUpdateState(home, state) {
  const cacheDir = path.join(home, '.cache', 'code-graph');
  fs.mkdirSync(cacheDir, { recursive: true });
  fs.writeFileSync(path.join(cacheDir, 'update-state.json'), JSON.stringify(state));
}

// Run statusline.js in `projectDir` (must contain .code-graph/index.db) with a
// sandboxed HOME, and return its stdout. PATH retains bash for the stub.
function runStatusline(home, projectDir) {
  return execFileSync('node', [STATUSLINE], {
    cwd: projectDir,
    env: { ...process.env, HOME: home, USERPROFILE: home },
    encoding: 'utf8',
  }).trim();
}

// Run statusline.js from an arbitrary process.cwd() with extra env vars. Used to
// prove the gate keys on Claude Code's authoritative cwd (CODE_GRAPH_STATUSLINE_CWD,
// forwarded by the composite from the stdin payload), NOT the spawn's cwd.
function runStatuslineIn(home, processCwd, extraEnv) {
  return execFileSync('node', [STATUSLINE], {
    cwd: processCwd,
    env: { ...process.env, HOME: home, USERPROFILE: home, ...extraEnv },
    encoding: 'utf8',
  }).trim();
}

function mkProject(home) {
  const dir = path.join(home, 'project');
  const cg = path.join(dir, '.code-graph');
  fs.mkdirSync(cg, { recursive: true });
  fs.writeFileSync(path.join(cg, 'index.db'), 'stub'); // existence is all statusline checks
  return dir;
}

test('healthy index → ✓ with node/file counts', (t) => {
  const home = mkHome(t);
  const project = mkProject(home);
  installStubBinary(home, {
    report: { healthy: true, nodes: 3145, files: 205, watching: true },
    exitCode: 0,
  });
  assert.equal(runStatusline(home, project), 'code-graph: ✓ 3145 nodes | 205 files | watching');
});

test('empty index, health-check EXIT 1 with JSON → ✗ 0 nodes (not offline)', (t) => {
  // The core regression: non-zero exit must not mask a valid report.
  const home = mkHome(t);
  const project = mkProject(home);
  installStubBinary(home, {
    report: { healthy: false, nodes: 0, files: 0, watching: false },
    exitCode: 1,
  });
  assert.equal(runStatusline(home, project), 'code-graph: ✗ 0 nodes | 0 files');
});

test('no report + update pending → updating', (t) => {
  const home = mkHome(t);
  const project = mkProject(home);
  installStubBinary(home, { report: 'boom', exitCode: 1 });
  setUpdateState(home, { updateAvailable: true });
  assert.equal(runStatusline(home, project), 'code-graph: ↻ updating');
});

test('stuck update (updateAttempts exhausted) → says stuck, not "updating" and not a bare "offline"', (t) => {
  // A persistently-failing update must not pin "↻ updating" forever — that
  // asserts a self-heal that is not happening. It must not go SILENT either:
  // pre-release review of v0.111.0 found that the updater's own stderr notice
  // is written by a process session-init spawns detached with stdio:'ignore',
  // so a suspended user's only remaining signal was running doctor by hand.
  const home = mkHome(t);
  const project = mkProject(home);
  installStubBinary(home, { report: 'boom', exitCode: 1 });
  setUpdateState(home, { updateAvailable: true, updateAttempts: 5 });
  assert.equal(runStatusline(home, project), 'code-graph: ⚠ update stuck');
});

test('stuck update is surfaced on the HEALTHY line too', (t) => {
  // The common shape: the index is fine and the binary works, only the UPDATE
  // is parked. Without this the marker would only ever appear in the states
  // where something else is already broken.
  const home = mkHome(t);
  const project = mkProject(home);
  installStubBinary(home, { report: { healthy: true, nodes: 12, files: 3 } });
  setUpdateState(home, { updateAvailable: true, updateAttempts: 5 });
  assert.equal(runStatusline(home, project),
    'code-graph: ✓ 12 nodes | 3 files | ⚠ update stuck');

  // Control: same state one attempt below the cap is still an in-flight update,
  // so the healthy line stays clean.
  setUpdateState(home, { updateAvailable: true, updateAttempts: 4 });
  assert.equal(runStatusline(home, project), 'code-graph: ✓ 12 nodes | 3 files');
});

test('schema-version error on stderr (no report) → updating', (t) => {
  const home = mkHome(t);
  const project = mkProject(home);
  // The binary writes schema errors to stderr; no parseable report on stdout.
  installStubBinary(home, {
    report: '',
    stderr: 'Error: Database schema version v9 is newer than supported v8',
    exitCode: 1,
  });
  assert.equal(runStatusline(home, project), 'code-graph: ↻ updating');
});

test('schema-too-new MARKER on stderr → updating (keys on the stable token, not prose)', (t) => {
  // The binary appends domain::SCHEMA_TOO_NEW_MARKER; the statusline must detect
  // the post-update window via that token even when the surrounding prose is
  // reworded/translated (the old `/schema version/i` regex would miss this).
  const home = mkHome(t);
  const project = mkProject(home);
  installStubBinary(home, {
    report: '',
    stderr: 'Error: totally reworded wording here [code-graph:schema-too-new]',
    exitCode: 1,
  });
  assert.equal(runStatusline(home, project), 'code-graph: ↻ updating');
});

test('genuine crash (no report, nothing pending) → offline', (t) => {
  const home = mkHome(t);
  const project = mkProject(home);
  installStubBinary(home, { report: 'segfault, core dumped', exitCode: 139 });
  assert.equal(runStatusline(home, project), 'code-graph: offline');
});

test('partial embeddings show vector coverage', (t) => {
  const home = mkHome(t);
  const project = mkProject(home);
  installStubBinary(home, {
    report: { healthy: true, nodes: 14119, files: 922, embedding_status: 'partial', embedding_coverage_pct: 60 },
    exitCode: 0,
  });
  assert.equal(runStatusline(home, project), 'code-graph: ✓ 14119 nodes | 922 files | 60% vec');
});

test('pending embeddings show "vec pending"', (t) => {
  const home = mkHome(t);
  const project = mkProject(home);
  installStubBinary(home, {
    report: { healthy: true, nodes: 14119, files: 922, embedding_status: 'pending', embedding_coverage_pct: 0 },
    exitCode: 0,
  });
  assert.equal(runStatusline(home, project), 'code-graph: ✓ 14119 nodes | 922 files | vec pending');
});

test('complete embeddings add no vector suffix', (t) => {
  const home = mkHome(t);
  const project = mkProject(home);
  installStubBinary(home, {
    report: { healthy: true, nodes: 14119, files: 922, embedding_status: 'complete', embedding_coverage_pct: 100 },
    exitCode: 0,
  });
  assert.equal(runStatusline(home, project), 'code-graph: ✓ 14119 nodes | 922 files');
});

test('version-stale index shows rebuilding marker', (t) => {
  const home = mkHome(t);
  const project = mkProject(home);
  installStubBinary(home, {
    report: { healthy: true, nodes: 14119, files: 922, embedding_status: 'complete', index_version_stale: true },
    exitCode: 0,
  });
  assert.equal(runStatusline(home, project), 'code-graph: ✓ 14119 nodes | 922 files | ↻ rebuilding');
});

// CODE_GRAPH_STATUSLINE_CWD is Claude Code's authoritative current dir, forwarded by
// the composite from its stdin payload. The gate must trust it over process.cwd():
// Claude Code may spawn the statusline from a cwd unrelated to the session (the
// classic regression — the segment vanished when the shell sat in a subdir whose
// process.cwd() didn't resolve to the project root).
test('CODE_GRAPH_STATUSLINE_CWD overrides process.cwd() for the gate', (t) => {
  const home = mkHome(t);
  const project = mkProject(home);
  installStubBinary(home, {
    report: { healthy: true, nodes: 3145, files: 205 },
    exitCode: 0,
  });
  // process.cwd() = home (no .code-graph → resolves null → would be blank), but
  // the authoritative cwd points at the project → must render the health line.
  const out = runStatuslineIn(home, home, { CODE_GRAPH_STATUSLINE_CWD: project });
  assert.equal(out, 'code-graph: ✓ 3145 nodes | 205 files');
});

test('CODE_GRAPH_STATUSLINE_CWD in a subdir walks up to the project root', (t) => {
  const home = mkHome(t);
  const project = mkProject(home);
  const subdir = path.join(project, 'claude-plugin', 'scripts');
  fs.mkdirSync(subdir, { recursive: true });
  installStubBinary(home, {
    report: { healthy: true, nodes: 3145, files: 205 },
    exitCode: 0,
  });
  // The reported symptom: shell in <root>/claude-plugin/scripts. The subdir has
  // no .code-graph of its own; resolveProjectRoot must walk up to <root>.
  const out = runStatuslineIn(home, home, { CODE_GRAPH_STATUSLINE_CWD: subdir });
  assert.equal(out, 'code-graph: ✓ 3145 nodes | 205 files');
});

// --- indexing progress file states ---

function writeProgress(project, payload, { ageMs = 0 } = {}) {
  const file = path.join(project, '.code-graph', 'indexing-status.json');
  fs.writeFileSync(file, JSON.stringify(payload));
  if (ageMs > 0) {
    const past = (Date.now() - ageMs) / 1000;
    fs.utimesSync(file, past, past);
  }
  return file;
}

test('fresh indexing progress renders floor percent (skipped files never show 100%)', (t) => {
  // Skipped files (parse errors, oversized) keep d below t even in the terminal
  // progress write: 3868/3876 is 99.79%, and Math.round displayed it as a stuck
  // "100%". floor keeps the not-done state visibly below 100.
  const home = mkHome(t);
  const project = mkProject(home);
  writeProgress(project, { s: 'indexing', d: 3868, t: 3876 });
  assert.equal(runStatusline(home, project), 'code-graph: ↻ indexing 3868/3876 (99%)');
});

test('finalizing progress renders an explicit phase label', (t) => {
  // Post-batch full-graph phases: the count stops moving, so the server writes
  // s:"finalizing" — render the phase, not a frozen-looking "(100%)" counter.
  const home = mkHome(t);
  const project = mkProject(home);
  writeProgress(project, { s: 'finalizing', d: 2331, t: 2331 });
  assert.equal(runStatusline(home, project), 'code-graph: ↻ finalizing 2331/2331');
});

test('stale progress file is ignored — falls through to health check', (t) => {
  // A killed server (session exit, SIGKILL, MCP connect-timeout kill) skips the
  // IndexGuard drop that deletes the progress file. A live indexer heartbeats at
  // least once per batch/finalize phase, so an mtime older than the stale window
  // proves nobody is writing: the orphan must not pin "indexing N/M" forever.
  const home = mkHome(t);
  const project = mkProject(home);
  installStubBinary(home, {
    report: { healthy: true, nodes: 30753, files: 2331 },
    exitCode: 0,
  });
  writeProgress(project, { s: 'indexing', d: 2331, t: 2331 }, { ageMs: 10 * 60 * 1000 });
  assert.equal(runStatusline(home, project), 'code-graph: ✓ 30753 nodes | 2331 files');
});
