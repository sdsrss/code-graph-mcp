#!/usr/bin/env node
'use strict';
// FIRST statement, before this file's other requires (pre-tag review
// 2026-09-02): the handler installed after them could not catch a throw
// from `require('./lifecycle')` itself, which is exactly the broken-install
// case JS-12 exists for. Guarded on `require.main` so importing this module
// in a test does NOT install a process-wide handler that exits 0 — that
// would swallow the test's own failures.
if (require.main === module) require('./hook-fail-open').installHookFailOpen('statusLine');

const { execFileSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { findBinary } = require('./find-binary');
const { resolveProjectRoot } = require('./project-root');
const lifecycle = require('./lifecycle');
const { hidden } = require('./proc-opts');
const cleanupDisabledStatusline = lifecycle.cleanupDisabledStatusline || (() => ({ cleaned: false }));

// True when auto-update has a newer release queued or in flight (the background
// downloader in session-init.js hasn't promoted the new binary yet). Used to show
// a transient "updating" state instead of the alarming "offline" during that window.
// After this many consecutive failed download attempts (auto-update.js tracks
// updateAttempts), a pending update is treated as STUCK: the statusline stops
// showing "↻ updating" (which asserts an in-progress self-heal) and surfaces the
// real status instead. Without this, a persistently-failing update (missing
// tar/curl, full disk, blocked network) pins "updating" forever.
const STUCK_UPDATE_ATTEMPTS = 5;
function readUpdateState() {
  try {
    return JSON.parse(fs.readFileSync(
      path.join(os.homedir(), '.cache', 'code-graph', 'update-state.json'), 'utf8'));
  } catch { return null; /* no state file or unreadable */ }
}
function updatePending(st = readUpdateState()) {
  if (!st) return false;
  if ((st.updateAttempts || 0) >= STUCK_UPDATE_ATTEMPTS) return false;
  if (st.updateAvailable) return true;
  if (st.latestVersion && st.installedVersion && st.latestVersion !== st.installedVersion) return true;
  return false;
}
// The updater has given up on this release (auto-update.js MAX_UPDATE_ATTEMPTS).
// This has to be SHOWN, not merely not-lied-about: the updater's own stderr
// notice is written by a process session-init spawns `detached` with
// `stdio: 'ignore'`, so nobody ever sees it, and updatePending() going quiet
// above means the only remaining signal was running `doctor` by hand. A user
// who never runs doctor would sit on a permanently parked updater with no way
// to know (found by pre-release review of v0.111.0, fixed in v0.111.1).
function updateStuck(st = readUpdateState()) {
  return !!(st && st.updateAvailable && (st.updateAttempts || 0) >= STUCK_UPDATE_ATTEMPTS);
}

// The teardown is best-effort housekeeping, never a precondition for rendering.
// It writes ~/.claude/settings.json and ~/.cache/code-graph/statusline-registry.json,
// and on a read-only config dir (EROFS container mount, a `sudo` that left root
// ownership behind, restrictive umask) those writes throw — from module scope,
// where nothing catches them. The user's whole status line then goes blank plus a
// node stack trace, for a cleanup they never asked for. Swallow it: the next run
// with a writable dir does the work.
let disabledCleanup = { cleaned: false };
try {
  disabledCleanup = cleanupDisabledStatusline();
} catch { /* teardown is optional; rendering is not */ }
if (disabledCleanup.cleaned) process.exit(0);

// Only show status in projects that have a code-graph directory. The statusLine
// config is global, so we must exit silently for non-code-graph directories.
// Walk UP to the canonical project root (resolveProjectRoot) rather than keying
// on the bare process.cwd(): when the shell sits in a subdir, the bare-cwd gate
// either showed a STRAY nested subdir index (monorepo relic — the statusline
// "oscillating" between root/backend/frontend node counts) or, in a clean subdir
// with no local index, showed nothing at all. The resolver skips stray nested
// indexes, so the statusline tracks one DB — the project root — from any subdir.
//
// Start from Claude Code's AUTHORITATIVE current dir (CODE_GRAPH_STATUSLINE_CWD,
// forwarded by the composite from its stdin payload) rather than process.cwd().
// The spawned statusline's process.cwd() is an implementation detail of how
// Claude Code launches the command and need not track the session's working dir;
// the stdin `cwd` always does. Fall back to process.cwd() when unset (direct
// invocation, tests).
const startDir = process.env.CODE_GRAPH_STATUSLINE_CWD || process.cwd();
const root = resolveProjectRoot(startDir);
if (!root) {
  process.exit(0);
}
const codeGraphDir = path.join(root, '.code-graph');

// Check for background indexing progress file first
const progressFile = path.join(codeGraphDir, 'indexing-status.json');
try {
  // Staleness gate: the file is normally deleted by the server's IndexGuard, but
  // a killed process (session exit, SIGKILL, the 30s MCP connect-timeout kill)
  // skips Drop, and the orphan would pin "indexing N/M" here forever. A LIVE
  // indexer heartbeats the file at least once per batch and per finalize phase,
  // so an old mtime proves no indexer is writing it: ignore the file and fall
  // through to the health check. (Mirrors INDEXING_STATUS_STALE_SECS in
  // src/indexer/pipeline/mod.rs, which drives server/CLI-side stale cleanup.)
  const INDEXING_STALE_MS = 120000;
  const fresh = (Date.now() - fs.statSync(progressFile).mtimeMs) < INDEXING_STALE_MS;
  const p = fresh ? JSON.parse(fs.readFileSync(progressFile, 'utf8')) : null;
  if (p && p.s === 'indexing' && p.t > 0) {
    // floor, not round: skipped files (parse errors, oversized) keep d below t
    // even in the terminal progress write, and rounding displayed that state as
    // a confusing stuck "100%".
    const pct = Math.floor((p.d / p.t) * 100);
    process.stdout.write(`code-graph: \u21BB indexing ${p.d}/${p.t} (${pct}%)`);
    process.exit(0);
  }
  if (p && p.s === 'finalizing' && p.t > 0) {
    // Post-batch full-graph phases (context strings, import bind/prune, ANALYZE):
    // the file count no longer moves, so show an explicit phase label instead of
    // a frozen-looking counter.
    process.stdout.write(`code-graph: ↻ finalizing ${p.d}/${p.t}`);
    process.exit(0);
  }
} catch { /* no progress file or parse error — continue to health check */ }

// No indexing in progress — show normal health status
if (!fs.existsSync(path.join(codeGraphDir, 'index.db'))) {
  process.exit(0);
}

const bin = findBinary();
if (!bin) {
  // No usable binary yet. If an update is queued, the background downloader is
  // still fetching it \u2014 that is "updating", not a broken "offline" state.
  process.stdout.write(
    updateStuck() ? 'code-graph: \u26a0 update stuck'
      : updatePending() ? 'code-graph: \u21bb updating'
        : 'code-graph: offline');
  process.exit(0);
}

// Render the standard health line from a parsed health-check report. An
// unhealthy/empty index (healthy:false, 0 nodes) is a real, accurate state and
// is distinct from "offline" \u2014 the binary ran fine, the index just has no data.
function renderHealth(s) {
  const icon = s.healthy ? '\u2713' : '\u2717';
  let line = `code-graph: ${icon} ${s.nodes} nodes | ${s.files} files`;
  // Surface vector-backfill progress so a structurally-complete but only
  // partially-embedded index reads as "healthy and improving" (the embedding
  // backfill is resumable and runs in the background), not as something stuck.
  // Hidden when embeddings are complete, unavailable (no model), or there are no
  // nodes yet \u2014 only the in-progress states add the suffix.
  if (s.nodes > 0) {
    if (s.embedding_status === 'partial' && typeof s.embedding_coverage_pct === 'number') {
      line += ` | ${s.embedding_coverage_pct}% vec`;
    } else if (s.embedding_status === 'pending') {
      line += ' | vec pending';
    }
  }
  // An index built by an older extractor generation is usable but a rebuild is
  // owed (a background incremental-index revalidates it). Flag it so a stale
  // index doesn't masquerade as fully current.
  if (s.index_version_stale) line += ' | \u21bb rebuilding';
  if (s.watching) line += ' | watching';
  // A parked updater is otherwise invisible in normal use — see updateStuck().
  if (updateStuck()) line += ' | \u26a0 update stuck';
  return line;
}

// A genuine report carries a boolean `healthy` field. Returns null for anything
// that isn't a parseable report (empty string, crash banner, partial output).
function parseReport(text) {
  try {
    const s = JSON.parse(text);
    return (s && typeof s.healthy === 'boolean') ? s : null;
  } catch { return null; }
}

// No usable report: the binary couldn't produce one (crashed / missing / schema
// too new). A schema-version error means the resolved binary is OLDER than the
// index it is reading \u2014 the classic post-update window where the new binary is
// still downloading. That, or any pending update, is transient: show "updating"
// so the user knows it self-heals, rather than the misleading "offline".
function statusUnavailable(errText) {
  // Primary signal: the binary's STABLE schema-too-new marker (Rust
  // domain::SCHEMA_TOO_NEW_MARKER) \u2014 not reword-able prose. Fallback to the legacy
  // phrase so a cached binary predating the marker still reads as "updating".
  const errStr = errText || '';
  const binaryOutdated = errStr.includes('code-graph:schema-too-new') || /schema version/i.test(errStr);
  if (binaryOutdated || updatePending()) return 'code-graph: \u21bb updating';
  return updateStuck() ? 'code-graph: \u26a0 update stuck' : 'code-graph: offline';
}

let report = null;
let errText = '';
try {
  // 1500ms, NOT 3000ms: the composite wrapper kills this whole provider at
  // 3000ms (statusline-composite.js runProvider), so an inner budget equal to
  // the outer one guaranteed the OUTER timeout fired first on a slow
  // health-check (e.g. CPU saturated by the embedding backfill) and the segment
  // silently vanished. Keeping the inner budget well under the outer one turns
  // "slow health-check" into a rendered "offline"/"updating" instead of a blank.
  report = parseReport(execFileSync(bin, ['health-check', '--format', 'json'], hidden({
    timeout: 1500,
    // Render hot path: a wedged binary ignoring SIGTERM must not outlive the
    // budget (same reasoning as the composite's provider spawn, audit P1-17).
    killSignal: 'SIGKILL',
    stdio: ['pipe', 'pipe', 'pipe'],
    // Run the binary FROM the resolved root so its own project-root resolution
    // lands on the same DB the gate above picked (a subdir cwd would otherwise
    // re-resolve to a stray nested index inside the binary).
    cwd: root
  })).toString());
} catch (e) {
  // health-check exits NON-ZERO on an unhealthy/empty index but still writes the
  // full JSON report to stdout. The binary ran fine \u2014 recover the report from the
  // error object so an empty index shows "\u2717 0 nodes" rather than a bogus "offline".
  report = parseReport(((e && e.stdout) || '').toString());
  // Scan BOTH streams for the schema marker: the binary writes it to stderr, but
  // an empty stderr Buffer is truthy, so `stderr || stdout` would never fall
  // through — concatenate instead of short-circuiting.
  errText = [(e && e.stderr) || '', (e && e.stdout) || ''].map(String).join('\n');
}

process.stdout.write(report ? renderHealth(report) : statusUnavailable(errText));
