#!/usr/bin/env node
'use strict';
const { execFileSync, execSync } = require('child_process');
const fs = require('fs');
const path = require('path');
const os = require('os');
const { readBinaryVersion, isDevMode, getNewestMtime } = require('./version-utils');
const {
  getPluginVersion, readJson, readJsonResult, healthCheck, scanForBrokenPaths, CACHE_DIR,
  settingsPath, surveyHookCoverage,
  installedGlobalPkgs, GLOBAL_INSTALL_MARKER, SHELL_PKG,
} = require('./lifecycle');
const { findBinary, clearCache: clearBinaryCache } = require('./find-binary');
const { hidden } = require('./proc-opts');
const { MAX_UPDATE_ATTEMPTS } = require('./auto-update');

// ── Diagnostics ───────────────────────────────────────────

/**
 * Classify embedding/vector availability from a `health-check --json` payload.
 * Pure (no I/O) so it is unit-testable. Surfaces a silent FTS5-only degradation
 * that the prior embedding_progress-only check false-greened as "no embeddable
 * nodes": when the binary is embed-capable and the index HAS embeddable nodes but
 * none are embedded, the vector channel is inactive and semantic search runs
 * FTS5-only — that is a 'warn', not 'ok'.
 * @returns {{name:string, status:'ok'|'warn', detail:string}}
 */
function classifyEmbeddings(hc) {
  const ep = (hc && hc.embedding_progress) || '0/0';
  const [done, total] = ep.split('/').map(Number);
  if (hc && hc.model_available === false) {
    return { name: 'Embeddings', status: 'warn',
      detail: 'binary built without embed-model — semantic search is FTS5-only; reinstall via npm/plugin for the hybrid binary' };
  }
  if (!total) {
    return { name: 'Embeddings', status: 'ok', detail: 'no embeddable nodes' };
  }
  if (!done) {
    // `model_download` carries the LAST recorded download outcome (issue #35).
    // Without it this printed the same optimistic "retry shortly" on every run,
    // so a machine whose download can never succeed read as "be patient"
    // forever instead of "this is broken". Absent field = never attempted,
    // which is itself a distinct diagnosis, not a reason to advise waiting.
    // EXCEPT when the weights are already on disk (`model_files_present`,
    // health-check --json ≥0.115): the npm plugin installs them without the
    // marker, so "never attempted" would contradict the filesystem — the only
    // missing step then is a server session to load them.
    const why = (hc && hc.embedding_status === 'pending')
      ? (hc.model_files_present
          ? 'model files present but not loaded — restart the MCP server (embeddings backfill there)'
          : hc.model_download
            ? `last model download: ${hc.model_download}`
            : 'model not loaded and NO download has ever been attempted on this machine — restart the MCP server, or set CODE_GRAPH_MODEL_DIR to a manually populated model dir (see README → Offline usage)')
      : `embedding_status=${(hc && hc.embedding_status) || 'unknown'}`;
    return { name: 'Embeddings', status: 'warn',
      detail: `vector INACTIVE — ${total} embeddable nodes, 0 embedded; semantic search is FTS5-only (${why})` };
  }
  if (done < total) {
    return { name: 'Embeddings', status: 'ok', detail: `hybrid — ${Math.round((done / total) * 100)}% embedded (${done}/${total})` };
  }
  return { name: 'Embeddings', status: 'ok', detail: `hybrid — embeddings complete (${done}/${total})` };
}

/**
 * Classify the `integrity` block that `health-check --json` has carried since
 * v0.113.0 (audit DB-1). Pure, same as `classifyEmbeddings`.
 *
 * Severity is deliberately NOT uniform across the three probes:
 *  - `quick_check` complaining means pages do not read back — the index is
 *    corrupt, and only a rebuild fixes it. Error + fixId.
 *  - `fts_drift != 0` means the FTS5 index and `nodes` disagree, so search
 *    silently misses (or invents) symbols. Wrong answers, no crash. Warn + fixId.
 *  - `orphan_vectors != 0` is dead weight that skews coverage math; answers stay
 *    correct. It is DISCLOSED in the detail but does not raise an issue on its
 *    own: the only repair is a full rebuild, and a permanent warn with a
 *    disproportionate fix is how `doctor` ends up exiting 1 forever on installs
 *    that are fine (the MCP server already reaps these at startup; a CLI-only
 *    install is exactly who would be stuck with it).
 * `null` on any probe means "could not be measured", never a fault.
 * @returns {{name:string, status:'ok'|'warn'|'error'|'skip', detail:string, fixId?:string}}
 */
function classifyIntegrity(hc) {
  const it = hc && hc.integrity;
  if (!it || typeof it !== 'object') {
    // Binary predates the probes. Say so rather than inventing a verdict — an
    // absent measurement is not a passing one.
    return { name: 'Integrity', status: 'skip', detail: 'not reported by this binary version' };
  }
  const orphanNote = it.orphan_vectors ? `; ${it.orphan_vectors} orphan vector(s)` : '';
  const qc = it.quick_check;
  if (typeof qc === 'string' && qc !== 'ok' && qc !== 'skipped_large') {
    return {
      name: 'Integrity', status: 'error', fixId: 'index-corrupt',
      detail: `database is CORRUPT — quick_check: ${qc}${orphanNote}`,
    };
  }
  if (typeof it.fts_drift === 'number' && it.fts_drift !== 0) {
    return {
      name: 'Integrity', status: 'warn', fixId: 'index-corrupt',
      detail: `FTS index drifted from nodes by ${it.fts_drift} row(s) — search silently misses symbols${orphanNote}`,
    };
  }
  if (qc === 'skipped_large') {
    // The size gate skipped the page scan on purpose. Reporting `ok` here would
    // claim a check that never ran.
    return { name: 'Integrity', status: 'skip',
      detail: `page check skipped (index over the size gate) — run: code-graph-mcp health-check --deep${orphanNote}` };
  }
  if (qc == null) {
    return { name: 'Integrity', status: 'skip', detail: `page check unavailable${orphanNote}` };
  }
  return { name: 'Integrity', status: 'ok', detail: `quick_check ok, FTS in sync${orphanNote}` };
}

/**
 * Turn a parsed `health-check --json` payload into doctor rows.
 *
 * Extracted so the success path and the recovered-from-stdout path (below)
 * cannot classify the same payload differently. Pure.
 */
function classifyHealthReport(hc) {
  // No-index short-circuit — binary deliberately returns a structured JSON with
  // reason='no_index' instead of bailing, so we can route to the index-empty fix
  // without grepping stderr.
  if (hc.reason === 'no_index') {
    return [
      { name: 'Schema', status: 'ok', detail: 'binary ok (no index yet)' },
      { name: 'Index', status: 'warn', detail: 'missing — not indexed yet', fixId: 'index-empty' },
      { name: 'Embeddings', status: 'skip', detail: 'no index' },
    ];
  }
  // The database could not be opened at all, so schema/nodes/embeddings are not
  // unknown-but-probably-fine, they are unmeasurable. Reporting `Schema: ok
  // vnull` off the zeroed payload would be a fabricated pass; the Integrity row
  // below carries the real verdict and the `index-corrupt` fix.
  if (hc.reason === 'corrupt') {
    return [
      { name: 'Schema', status: 'skip', detail: 'index unreadable' },
      { name: 'Index', status: 'skip', detail: 'index unreadable' },
      { name: 'Embeddings', status: 'skip', detail: 'index unreadable' },
      classifyIntegrity(hc),
    ];
  }
  const rows = [];
  if (hc.issue && String(hc.issue).includes('schema')) {
    rows.push({ name: 'Schema', status: 'warn', detail: hc.issue, fixId: 'schema-mismatch' });
  } else {
    rows.push({ name: 'Schema', status: 'ok', detail: `v${hc.schema_version}` });
  }
  if (hc.nodes === 0) {
    rows.push({ name: 'Index', status: 'warn', detail: 'empty', fixId: 'index-empty' });
  } else {
    const age = hc.index_age ? ` (${hc.index_age})` : '';
    rows.push({
      name: 'Index', status: 'ok',
      detail: `${hc.nodes} nodes, ${hc.edges} edges, ${hc.files} files${age}`,
    });
  }
  // Embeddings / vector availability — pure classifier; warns on FTS5-only
  // degradation (model missing/not loaded) instead of false-greening it.
  rows.push(classifyEmbeddings(hc));
  rows.push(classifyIntegrity(hc));
  return rows;
}

/**
 * Recover a health-check payload from a buffer, or null if it isn't one.
 *
 * `health-check --json` prints the FULL report to stdout and THEN exits 1 when
 * the index is unhealthy, so `execFileSync` throwing does not mean there is no
 * report — it means there is a report saying something is wrong. Requiring a
 * key only this command emits keeps unrelated stdout (a panic, a wrapper's
 * noise) from being read as a clean bill of health.
 */
function parseHealthPayload(buf) {
  if (!buf) return null;
  try {
    const obj = JSON.parse(buf.toString().trim());
    if (!obj || typeof obj !== 'object' || Array.isArray(obj)) return null;
    // `reason` covers payloads the binary emits BEFORE it can read a schema
    // version (no_index, corrupt). Keying only on `schema_version` meant the
    // corrupt payload — the one case where recovering the report matters most —
    // was rejected and fell through to `binary-broken`. Caught by an end-to-end
    // run against a real clobbered index; neither side's unit tests could see
    // it, because each was asserted against its own fixture.
    const looksLikeReport = 'schema_version' in obj || typeof obj.reason === 'string';
    return looksLikeReport ? obj : null;
  } catch {
    return null;
  }
}

/** Spawn `health-check --json`. Separate + injectable: see `rebuildIndexInPlace`. */
function runHealthCheckCli(binary) {
  // Deliberately NOT `--deep`. That flag forces the integrity pragmas the
  // default path skips above INTEGRITY_PRAGMA_MAX_BYTES, and quick_check reads
  // every page at ~2.4 ms/MB — a multi-GB index would blow the 5 s budget below
  // and report a phantom "health-check failed" instead of the integrity answer
  // it went looking for. Raising the timeout to fit trades that for a doctor run
  // that appears hung. `--deep` stays a user-invoked escape hatch until this
  // call can size its own budget from the index.
  return execFileSync(binary, ['health-check', '--json'], hidden({
    cwd: process.cwd(),
    timeout: 5000,
    encoding: 'utf8',
    stdio: ['pipe', 'pipe', 'pipe'],
  })).trim();
}

/**
 * Schema / Index / Embeddings / Integrity rows from the binary's health-check.
 *
 * The failure branches are the point of this function existing separately.
 * `health-check --json` prints the FULL report to stdout and THEN exits 1 when
 * the index is unhealthy, so `execFileSync` throwing does not mean the probe
 * failed. The old code read the throw as "the binary is broken" and emitted
 * `Schema: error … fixId:'binary-broken'` — a fixId `runRepairs` has no case
 * for. Net effect: the integrity probes added in v0.113.0 never reached the
 * consumer they were built for, and a genuinely corrupt index was reported as a
 * binary problem with no repair offered.
 */
function healthRows(binary, { runHealthCheck = runHealthCheckCli } = {}) {
  try {
    return classifyHealthReport(JSON.parse(runHealthCheck(binary)));
  } catch (e) {
    const recovered = parseHealthPayload(e.stdout);
    if (recovered) return classifyHealthReport(recovered);

    const rawStderr = e.stderr ? e.stderr.toString() : '';
    const msg = rawStderr ? rawStderr.trim().slice(0, 100) : e.message.slice(0, 100);
    // "No index found" is a missing-index situation, not a broken binary — the
    // index-empty fix path knows how to create one. Without this branch the
    // fixId routes to nothing and the report shows "0/1 addressed".
    if (rawStderr.includes('No index found')) {
      return [
        { name: 'Schema', status: 'ok', detail: 'binary ok (no index yet)' },
        { name: 'Index', status: 'warn', detail: 'missing — not indexed yet', fixId: 'index-empty' },
        { name: 'Embeddings', status: 'skip', detail: 'no index' },
      ];
    }
    return [
      { name: 'Schema', status: 'error', detail: `health-check failed: ${msg}`, fixId: 'binary-broken' },
      { name: 'Index', status: 'skip', detail: 'health-check failed' },
      { name: 'Embeddings', status: 'skip', detail: 'health-check failed' },
    ];
  }
}

/**
 * Run all diagnostic checks. Returns an array of:
 *   { name: string, status: 'ok'|'warn'|'error'|'skip', detail: string, fixId?: string }
 */
// `checkOnly` must reach here, not just formatReport. `--check-only` is a
// SHIPPED read-only contract (CHANGELOG v0.82.1: "it never reaches runRepairs"),
// but the write was never in runRepairs — `healthCheck()` below calls
// `install()`, which REBUILDS an unusable settings.json. Reproduced: under
// `--check-only`, a settings.json holding `{"model":"opus",}` went 36 B -> 3318 B
// with the model key gone, and the report then said "Run without --check-only to
// fix." A read-only mode that rewrites the user's config is worse than one that
// lies about it.
function runDiagnostics({ checkOnly = false } = {}) {
  const results = [];
  const binary = findBinary();

  // 1. Binary executable
  if (!binary) {
    results.push({ name: 'Binary', status: 'error', detail: 'not found', fixId: 'binary-missing' });
    results.push({ name: 'Binary version', status: 'skip', detail: 'binary not found' });
    results.push({ name: 'Source fresh', status: 'skip', detail: 'binary not found' });
    results.push({ name: 'Schema', status: 'skip', detail: 'binary not found' });
    results.push({ name: 'Index', status: 'skip', detail: 'binary not found' });
    results.push({ name: 'Embeddings', status: 'skip', detail: 'binary not found' });
    // The deny hooks run `code-graph-mcp grep/show/overview` inside the hook to
    // answer in-place (the flagship conversion lever). A missing binary silently
    // disables that — denies fall back to bare advice — so call it out here.
    results.push({ name: 'Answer-in-deny', status: 'skip',
      detail: 'disabled — binary not found, deny hooks fall back to static advice' });
  } else {
    let execOk = true;
    try {
      fs.accessSync(binary, fs.constants.X_OK);
      results.push({ name: 'Binary exec', status: 'ok', detail: binary });
    } catch {
      results.push({ name: 'Binary exec', status: 'error', detail: `not executable: ${binary}`, fixId: 'binary-not-exec' });
      execOk = false;
    }

    // 2. Binary version vs plugin version
    const pluginVersion = getPluginVersion();
    const binaryVersion = execOk ? readBinaryVersion(binary) : null;
    if (!binaryVersion) {
      results.push({ name: 'Binary version', status: 'error', detail: 'failed to read version', fixId: 'binary-broken' });
    } else if (binaryVersion !== pluginVersion) {
      results.push({
        name: 'Binary version',
        status: 'warn',
        detail: `v${binaryVersion} (plugin expects v${pluginVersion})`,
        fixId: 'version-mismatch',
      });
    } else {
      results.push({ name: 'Binary version', status: 'ok', detail: `v${binaryVersion}` });
    }

    // 3. Source freshness (dev mode only)
    if (isDevMode()) {
      const srcDir = path.resolve(__dirname, '..', '..', 'src');
      try {
        const binaryMtime = fs.statSync(binary).mtimeMs;
        const latestSrcMtime = getNewestMtime(srcDir, '.rs');
        if (latestSrcMtime > binaryMtime) {
          const deltaMin = Math.round((latestSrcMtime - binaryMtime) / 60000);
          results.push({
            name: 'Source fresh',
            status: 'warn',
            detail: `src/ modified ${deltaMin}min after binary`,
            fixId: 'binary-stale',
          });
        } else {
          results.push({ name: 'Source fresh', status: 'ok', detail: 'binary up-to-date' });
        }
      } catch {
        results.push({ name: 'Source fresh', status: 'skip', detail: 'could not stat files' });
      }
    } else {
      results.push({ name: 'Source fresh', status: 'skip', detail: 'not dev mode' });
    }

    // 4. health-check (schema, index, embeddings, integrity) via binary --json
    if (execOk) {
      results.push(...healthRows(binary));
    } else {
      results.push({ name: 'Schema', status: 'skip', detail: 'binary not executable' });
      results.push({ name: 'Index', status: 'skip', detail: 'binary not executable' });
      results.push({ name: 'Embeddings', status: 'skip', detail: 'binary not executable' });
    }
  }

  // 5. Auto-update state
  try {
    const state = readJson(path.join(CACHE_DIR, 'update-state.json'));
    const attempts = (state && state.updateAttempts) || 0;
    if (state && state.updateAvailable && attempts >= MAX_UPDATE_ATTEMPTS) {
      // The updater has given up on this release (issue #40). Deliberately NO
      // fixId: re-running `auto-update.js check` is precisely the thing that was
      // suspended, so offering it as a repair would print "✅ Update check
      // complete" and count a fix that cannot happen. Say what is true and hand
      // the user the manual route.
      results.push({
        name: 'Auto-update',
        status: 'warn',
        detail: `v${state.latestVersion} failed to install ${attempts}× — auto-retry throttled to once a day. `
          + 'Update manually: `npm install -g @sdsrs/code-graph` (or `/plugin update code-graph-mcp`)',
      });
    } else if (state && state.updateAvailable && state.binaryUpdated === false) {
      results.push({
        name: 'Auto-update',
        status: 'warn',
        detail: `plugin v${state.latestVersion}, binary download incomplete`,
        fixId: 'update-incomplete',
      });
    } else {
      results.push({ name: 'Auto-update', status: 'ok', detail: 'up-to-date' });
    }
  } catch {
    results.push({ name: 'Auto-update', status: 'ok', detail: 'no update state' });
  }

  // 6. Hook paths validity
  // healthCheck() auto-attempts install() when broken paths are detected and
  // re-scans to verify; repaired:true is now contingent on the re-scan
  // returning clean. If repaired:false despite install() running, the
  // re-scan still found broken paths — surfacing 'remaining' makes that
  // honest instead of telling the user we fixed nothing.
  // In check-only mode, SCAN without the auto-repair half of healthCheck().
  const hookResult = checkOnly
    ? (() => {
        const issues = scanForBrokenPaths();
        return { healthy: issues.length === 0, issues, repaired: false, rebuiltFrom: null };
      })()
    : healthCheck();
  if (hookResult.healthy) {
    results.push({ name: 'Hooks', status: 'ok', detail: 'all paths valid' });
  } else if (hookResult.repaired && hookResult.rebuiltFrom) {
    // The repair WORKED, but it worked by replacing an unusable settings.json
    // with a freshly built one — the user's model / env / permissions / own
    // hooks now exist only in the backup. Reporting that as `✅ auto-repaired`
    // (which this did) describes a destructive event as a clean one and never
    // names the file that holds their config.
    results.push({
      name: 'Hooks',
      status: 'warn',
      detail:
        `settings.json was unusable and has been REBUILT — your original is at ` +
        `${hookResult.rebuiltFrom}. Merge anything you need back by hand.`,
    });
  } else if (hookResult.repaired) {
    results.push({
      name: 'Hooks',
      status: 'ok',
      detail: `${hookResult.issues.length} issue(s) auto-repaired`,
    });
  } else {
    const remaining = Array.isArray(hookResult.remaining)
      ? hookResult.remaining
      : hookResult.issues;
    // An unusable settings.json is not a broken PATH — auto-repair correctly
    // refuses to touch the file, so "invalid path(s)" would send the user
    // hunting for a missing script instead of at the file that actually needs
    // repairing.
    const unusable = remaining.find((i) => i.type === 'settings-unusable');
    results.push({
      name: 'Hooks',
      status: 'warn',
      detail: unusable
        ? `settings.json unusable (${unusable.reason}) — hooks cannot be verified or repaired`
        : `${remaining.length} invalid path(s) — auto-repair did not resolve`,
      fixId: 'hooks-invalid',
    });
  }

  // 7. settings.json hook coverage — v0.32.0 inversion. Current Claude Code
  //    silently ignores plugin-cache hooks.json for PreToolUse/PostToolUse/
  //    UserPromptSubmit. lifecycle.js install/update is responsible for
  //    registering them in settings.json. "Missing" is the bug (previously
  //    "present" was treated as legacy debris — that was wrong).
  try {
    // Sibling of the `scanForBrokenPaths` read above, and it was left on the old
    // collapsed-`null` idiom: an unusable settings.json became `{}`, which has no
    // hooks, so this reported "missing 6/6 settings.json entries" — a confident,
    // wrong diagnosis sitting in the SAME table as the correct "settings.json
    // unusable" line two rows up.
    const settingsRead = readJsonResult(settingsPath());
    const settings = settingsRead.value || {};
    const cov = surveyHookCoverage(settings);
    if (settingsRead.corrupt) {
      // No `fixId`: the repair is `install()`, which is already driven by the
      // `Hooks` row above. Raising `missing-hooks-in-settings` here too would
      // make doctor attempt the same repair twice and count the issue twice.
      results.push({
        name: 'Hook coverage',
        status: 'warn',
        detail: 'not determinable — settings.json could not be read or parsed',
      });
    } else if (cov.missing.length === 0 && cov.stale.length === 0) {
      results.push({
        name: 'Hook coverage',
        status: 'ok',
        detail: `settings.json has all ${cov.expected.length} expected entries`,
      });
    } else if (cov.missing.length > 0) {
      results.push({
        name: 'Hook coverage',
        status: 'warn',
        detail: `missing ${cov.missing.length}/${cov.expected.length} settings.json entries: ${cov.missing.join(', ')}`,
        fixId: 'missing-hooks-in-settings',
      });
    } else {
      // Present but stale path(s) — re-register rewrites them to the current
      // version. A stale PreToolUse hook can keep the conversion metric dark.
      results.push({
        name: 'Hook coverage',
        status: 'warn',
        detail: `${cov.stale.length}/${cov.expected.length} settings.json entries point at a stale path (re-register to current version): ${cov.stale.join(', ')}`,
        fixId: 'missing-hooks-in-settings',
      });
    }
  } catch (err) {
    // Do NOT swallow silently: this catch hid a plain ReferenceError (a helper
    // that was not imported) by simply dropping the whole Hook-coverage row, so
    // the table looked complete while a check had not run at all. A probe that
    // cannot run is itself a finding.
    results.push({
      name: 'Hook coverage',
      status: 'warn',
      detail: `probe failed: ${err && err.message ? err.message : err}`,
    });
  }

  // 8. Hook firing (v0.67.0) — coverage (#7) proves the hook is WIRED into
  //    settings.json; this proves the script actually RUNS. Spawns each
  //    registered hook with a synthetic CC payload in a throwaway fixture and
  //    checks it exits 0. Catches the registered-but-inert class (broken
  //    require-chain / incompatible node / corrupt install) that a string/path
  //    check cannot see. (It does NOT prove CC dispatches to it — that needs a
  //    live session; the dispatch canary in session-init.js covers that.)
  try {
    const { verifyHooksFire } = require('./lifecycle');
    const fire = verifyHooksFire();
    if (fire.ok) {
      results.push({ name: 'Hook firing', status: 'ok', detail: `${fire.results.length} hooks fire cleanly` });
    } else {
      const failed = fire.results.filter(r => !r.ok).map(r => r.label).join(', ') || fire.error || 'unknown';
      results.push({ name: 'Hook firing', status: 'warn', detail: `did not fire: ${failed}` });
    }
  } catch { /* probe failed — skip */ }

  // 9. Global npm residue — the launcher's background install (or the user)
  //    may have `npm install -g`'d the shell + platform packages. Surface what
  //    exists and who owns cleanup: with the plugin-install marker,
  //    `lifecycle.js uninstall` removes them; without it they are treated as
  //    user-installed and a plugin uninstall leaves them on PATH.
  try {
    const { globalPkgVersion, inactiveNodeGlobalRelics } = require('./auto-update');
    const { PLATFORM_PKG } = require('./find-binary');
    const found = [SHELL_PKG, PLATFORM_PKG]
      .map((name) => ({ name, version: globalPkgVersion(name) }))
      .filter((p) => p.version);

    // Relics stranded under a NON-active node version (nvm keeps a per-node
    // global prefix). selfHealGlobalPkgs / the check above only see the active
    // node, so these drift unseen for months and can seed stale settings.json
    // hooks — the v24.11.1@0.46.0 relic behind the RCA. Report-only: `npm i -g`
    // can't target another node's prefix, so hand the user the exact remediation.
    const relics = inactiveNodeGlobalRelics();
    if (relics.length) {
      const home = require('os').homedir();
      results.push({
        name: 'Global npm relics',
        status: 'warn',
        detail: relics.map((r) => `${r.name}@${r.version} (${r.nodeModulesDir.replace(home, '~')})`).join('; ')
          + ' — installed under a non-active node version; auto-heal cannot reach another node\'s prefix. '
          + 'Remove each via `nvm use <that node> && npm rm -g <pkg>`, or uninstall the unused node (`nvm uninstall <ver>`).',
      });
    }

    if (found.length) {
      const marker = !!readJson(GLOBAL_INSTALL_MARKER);
      // Heal-exhausted is otherwise invisible: selfHealGlobalPkgs stops after
      // 3 failed npm runs per target version and stays silent until the next
      // release re-arms the counter — a drifted CLI shim just sits there.
      const state = readJson(path.join(CACHE_DIR, 'update-state.json')) || {};
      const healGaveUp = (state.globalPkgHealAttempts || 0) >= 3;
      results.push({
        name: 'Global npm packages',
        status: healGaveUp ? 'warn' : 'ok',
        detail: found.map((p) => `${p.name}@${p.version}`).join(', ') + (healGaveUp
          ? ` — self-heal gave up after ${state.globalPkgHealAttempts} failed npm runs targeting v${state.globalPkgHealVersion}; ` +
            `your npm env likely can't install globally (EACCES/system node). Run manually: npm install -g ${found.map((p) => `${p.name}@${state.globalPkgHealVersion}`).join(' ')}`
          : (marker
            ? ' — plugin-installed; `node lifecycle.js uninstall` removes them'
            : ` — no plugin-install marker; uninstall leaves them (remove: npm uninstall -g ${found.map((p) => p.name).join(' ')})`)),
      });
    }
  } catch { /* probe failed — skip */ }

  return results;
}

// ── Report Formatting ─────────────────────────────────────

const STATUS_ICONS = { ok: '\u2705', warn: '\u26a0\ufe0f', error: '\u274c', skip: '\u2796' };

function formatReport(results, { checkOnly = false } = {}) {
  const pluginVersion = getPluginVersion();
  const lines = [`\ud83d\udd0d code-graph doctor v${pluginVersion}`, ''];

  const maxName = Math.max(...results.map(r => r.name.length));
  for (const r of results) {
    const icon = STATUS_ICONS[r.status] || '?';
    const pad = ' '.repeat(maxName - r.name.length + 2);
    lines.push(`  ${r.name}${pad}${icon}  ${r.detail}`);
  }

  const issues = results.filter(r => r.status === 'warn' || r.status === 'error');
  lines.push('');
  if (issues.length === 0) {
    lines.push('  All checks passed.');
  } else {
    const fixable = issues.filter(r => r.fixId);
    // `--check-only` is read-only (it never reaches runRepairs), so it must not
    // claim "Fixing..." — that contradicts the documented contract and alarms
    // the user into thinking their settings.json/MEMORY.md was just rewritten.
    const suffix = fixable.length === 0
      ? ''
      : checkOnly
        ? ' Run without --check-only to fix.'
        : ' Fixing...';
    lines.push(`  ${issues.length} issue(s) found.${suffix}`);
  }

  return lines.join('\n');
}

// ── Repair Actions ────────────────────────────────────────

/**
 * v0.50.0: settings-writing repairs get the same stale-relic guard as
 * session-init. A doctor launched from an old plugin-cache version dir would
 * otherwise install() and re-anchor manifest + settings.json hook paths to the
 * relic — the exact downgrade war the guard exists for, just user-triggered.
 * Returns true (and prints redirection) when this copy must NOT write config.
 * `relic` is injectable for tests.
 */
function relicRepairGuard({ log = console.log, relic = undefined } = {}) {
  const { isStaleRelicContext, activeInstallPath } = require('./lifecycle');
  const isRelic = relic !== undefined ? relic : isStaleRelicContext();
  if (!isRelic) return false;
  const active = activeInstallPath();
  log('  ⚠ This doctor copy is not the active install (installed_plugins.json points elsewhere) — skipping settings repair.');
  if (active) {
    log(`  Run the active copy instead: node "${path.join(active, 'scripts', 'doctor.js')}"`);
  }
  return true;
}

// A dev-mode rebuild must PRESERVE the existing binary's feature set. This repair
// used to hardcode `--no-default-features`, which silently downgraded a hybrid
// (embed-model) dev binary to FTS5-only and ping-ponged against a manual
// `cargo build --release --features embed-model`. Probe the binary's COMPILED
// feature via `health-check --json` → `model_available` (= cfg!(feature =
// "embed-model"), reported even with no index) and rebuild to match. Returns
// true/false, or null when the binary can't be probed (missing/broken) — the
// caller then defaults to FTS5 + an explicit note, never a silent downgrade.
// End users never reach this path (binary-stale → auto-update; binary-missing
// non-dev → install instructions); it is purely the source-repo dev convenience.
function detectEmbedModel(binary, run = execFileSync) {
  if (!binary) return null;
  try {
    const out = run(binary, ['health-check', '--json'], hidden({
      timeout: 10000, encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'],
    }));
    return JSON.parse(out).model_available === true;
  } catch { return null; }
}

function devBuildCommand(embed) {
  return embed
    ? 'cargo build --release --features embed-model'
    : 'cargo build --release --no-default-features';
}

// ── Post-repair re-scan for the auto-update-driven arms ────────────────────
//
// `auto-update.js check` has NO non-zero exit path: dev mode, the opt-out,
// suspension, the rate-limit backoff and plain offline all print a line and exit
// 0. So "execFileSync did not throw" carries no information about whether
// anything was repaired, and the two arms below counted every one of those
// no-ops as a fix — including the one the suspension notice sends the user here
// to run, producing "✅ Update check complete" for a check that is suspended.
// Re-read the same predicate the DIAGNOSIS used and let that decide, exactly as
// the `hooks-invalid` arm does with its post-install re-scan.

function triggerAutoUpdateCheck() {
  execFileSync(process.execPath, [path.join(__dirname, 'auto-update.js'), 'check'], hidden({
    timeout: 60000,
    stdio: 'inherit',
  }));
}

/**
 * Mirror of the `version-mismatch` diagnosis (runDiagnostics step 2): the binary
 * on disk reports the version the plugin expects. find-binary memoizes, and the
 * promote happened in a CHILD process, so the cache has to be dropped first or
 * this re-reads the pre-repair answer.
 */
function binaryVersionResolved({
  find = findBinary, readVersion = readBinaryVersion, pluginVersion = getPluginVersion,
} = {}) {
  clearBinaryCache();
  const binary = find();
  if (!binary) return false;
  const actual = readVersion(binary);
  return Boolean(actual) && actual === pluginVersion();
}

/** Mirror of the `update-incomplete` diagnosis (runDiagnostics step 5). */
function updateIncompleteResolved({ readStateFile = readUpdateState } = {}) {
  const state = readStateFile();
  return !(state && state.updateAvailable && state.binaryUpdated === false);
}

/**
 * Mirror of the `index-corrupt` diagnosis: re-run the probes against the
 * database the rebuild just produced and re-classify with the SAME function the
 * diagnosis used, so "resolved" cannot mean something different from "not
 * raised". A rebuild that exits 0 over failing hardware still fails this.
 */
function integrityResolved({ probe = probeHealth, classify = classifyIntegrity } = {}) {
  const hc = probe();
  if (!hc) return false;
  const row = classify(hc);
  return row.status !== 'error' && row.status !== 'warn';
}

/**
 * Run `rebuild-index --confirm`. Returns false when there is no binary to run
 * it with; throws whatever the child failed with (including the deliberate bail
 * when another process holds the index lock, added in v0.113.0).
 *
 * Separate + injectable because `execFileSync` is destructured at load here, so
 * a test that patches `child_process.execFileSync` after the fact stubs nothing
 * and silently exercises the real spawn.
 */
function rebuildIndexInPlace({ find = findBinary } = {}) {
  const binary = find();
  if (!binary) return false;
  execFileSync(binary, ['rebuild-index', '--confirm'], hidden({
    cwd: process.cwd(),
    stdio: 'inherit',
    timeout: 600000,  // full reindex, not a diff — same budget as a build
  }));
  return true;
}

/**
 * Read a `health-check --json` payload, tolerating the exit-1-on-unhealthy
 * contract (the report is on stdout either way). null when there is no payload.
 */
function probeHealth({ find = findBinary } = {}) {
  const binary = find();
  if (!binary) return null;
  const opts = hidden({ cwd: process.cwd(), timeout: 5000, encoding: 'utf8', stdio: ['pipe', 'pipe', 'pipe'] });
  try {
    return parseHealthPayload(execFileSync(binary, ['health-check', '--json'], opts));
  } catch (e) {
    return parseHealthPayload(e.stdout);
  }
}

function readUpdateState() {
  try { return readJson(path.join(CACHE_DIR, 'update-state.json')); } catch { return null; }
}

/**
 * Why a just-run `auto-update.js check` can have done nothing, in the updater's
 * own terms. Without this the user is told to update manually with no idea that
 * the updater is deliberately parked — which is the state the doctor prompt
 * itself came from.
 * @returns {string|null} one clause, or null when nothing is known to block it
 */
function autoUpdateNoOpReason(state = readUpdateState(), env = process.env) {
  if (env.CODE_GRAPH_NO_AUTO_UPDATE === '1') {
    return 'auto-update is switched off by CODE_GRAPH_NO_AUTO_UPDATE=1';
  }
  if (!state) return null;
  if (state.suspendedAt && (state.updateAttempts || 0) >= MAX_UPDATE_ATTEMPTS) {
    return `auto-update is SUSPENDED after ${state.updateAttempts} failed attempts on v${state.latestVersion} `
      + '(it retries once a day, and immediately when a newer release is published)';
  }
  if (state.rateLimited) {
    return 'the updater is in its GitHub rate-limit backoff (up to 1h)';
  }
  return null;
}

function reportAutoUpdateNoOp(what) {
  console.log(`  ❌ ${what}`);
  const why = autoUpdateNoOpReason();
  if (why) console.log(`     Why: ${why}.`);
  console.log('     Update manually: `npm install -g @sdsrs/code-graph` (or `/plugin update code-graph-mcp`)');
}

function runRepairs(results, {
  devMode = isDevMode,
  runAutoUpdate = triggerAutoUpdateCheck,
  binaryResolved = binaryVersionResolved,
  updateResolved = updateIncompleteResolved,
  integrityOk = integrityResolved,
  rebuildIndex = rebuildIndexInPlace,
} = {}) {
  const fixable = results.filter(r => r.fixId);
  if (fixable.length === 0) return 0;

  let fixed = 0;
  for (const issue of fixable) {
    switch (issue.fixId) {
      case 'binary-stale':
      case 'version-mismatch': {
        if (!devMode()) {
          console.log('\n  Triggering binary update...');
          try {
            runAutoUpdate();
          } catch {
            console.log('  \u274c Update check failed — install manually');
            break;
          }
          // Exited 0 — which says nothing about whether the binary moved (see
          // the re-scan note above). Ask the disk, not the exit code.
          if (binaryResolved()) {
            console.log('  \u2705 Binary now matches the version the plugin expects');
            fixed++;
          } else {
            reportAutoUpdateNoOp('Update check ran, but the binary version still does not match the plugin');
          }
          break;
        }
        // Preserve the current binary's feature set \u2014 never silently downgrade
        // a hybrid (embed-model) dev binary to FTS5-only (which also ping-pongs
        // against a manual `--features embed-model` build).
        const embed = detectEmbedModel(findBinary());
        const buildCmd = devBuildCommand(embed === true);
        console.log('\n  Building binary...');
        if (embed === null) {
          console.log('    (could not detect current feature set \u2014 building FTS5-only;');
          console.log('     for semantic search rebuild with `cargo build --release --features embed-model`)');
        }
        console.log(`    \u2192 ${buildCmd}`);
        try {
          const projectRoot = path.resolve(__dirname, '..', '..');
          execSync(buildCmd, hidden({
            cwd: projectRoot,
            stdio: 'inherit',
            timeout: 600000,  // embed-model (Candle) builds exceed the old 5min
          }));
          clearBinaryCache();
          console.log('  \u2705 Build complete');
          fixed++;
        } catch {
          console.log('  \u274c Build failed');
        }
        break;
      }

      case 'binary-missing': {
        console.log('\n  Installing binary...');
        if (devMode()) {
          // No binary to probe \u2014 build the fast FTS5 binary, but point at the
          // hybrid option so FTS5 isn't silently presented as the only choice.
          console.log('    \u2192 cargo build --release --no-default-features');
          console.log('      (for semantic search: cargo build --release --features embed-model)');
          try {
            const projectRoot = path.resolve(__dirname, '..', '..');
            execSync('cargo build --release --no-default-features', hidden({
              cwd: projectRoot,
              stdio: 'inherit',
              timeout: 600000,
            }));
            clearBinaryCache();
            console.log('  \u2705 Build complete');
            fixed++;
          } catch {
            console.log('  \u274c Build failed');
          }
        } else {
          console.log('    Install: npm install -g @sdsrs/code-graph');
          console.log('    Or download from: https://github.com/sdsrss/code-graph-mcp/releases');
        }
        break;
      }

      case 'binary-not-exec': {
        const binary = findBinary();
        if (binary) {
          try {
            fs.chmodSync(binary, 0o755);
            console.log(`\n  \u2705 Fixed permissions: chmod +x ${binary}`);
            fixed++;
          } catch {
            console.log(`\n  \u274c Could not fix permissions: ${binary}`);
          }
          if (os.platform() === 'darwin') {
            console.log(`  Also try: xattr -d com.apple.quarantine "${binary}"`);
          }
        }
        break;
      }

      case 'index-empty': {
        const binary = findBinary();
        if (binary) {
          console.log('\n  Rebuilding index...');
          console.log('    \u2192 code-graph-mcp incremental-index');
          try {
            execFileSync(binary, ['incremental-index'], hidden({
              cwd: process.cwd(),
              stdio: 'inherit',
              timeout: 120000,
            }));
            console.log('  \u2705 Index rebuilt');
            fixed++;
          } catch {
            console.log('  \u274c Index rebuild failed');
          }
        }
        break;
      }

      case 'update-incomplete': {
        console.log('\n  Completing auto-update...');
        try {
          runAutoUpdate();
        } catch {
          console.log('  \u274c Update check failed');
          break;
        }
        // Same as the version-mismatch arm: exit 0 is not evidence. Re-read
        // the state file the diagnosis read.
        if (updateResolved()) {
          console.log('  \u2705 Auto-update completed — the binary download is no longer pending');
          fixed++;
        } else {
          reportAutoUpdateNoOp('Update check ran, but the binary download is still recorded as incomplete');
        }
        break;
      }

      case 'hooks-invalid': {
        console.log('\n  Repairing hooks...');
        if (relicRepairGuard()) break;
        const { install, scanForBrokenPaths } = require('./lifecycle');
        const installResult = install();
        // Diagnosis already ran install()+re-scan and the paths were STILL
        // broken (that `repaired:false` is what raised hooks-invalid). Verify
        // this second attempt actually cleared them before counting it fixed \u2014
        // a blind fixed++ here would let doctor exit 0 ("healthy") while the
        // hook paths stay broken.
        const remaining = scanForBrokenPaths();
        if (remaining.length === 0) {
          console.log('  \u2705 Hooks repaired \u2014 restart Claude Code to apply');
          fixed++;
        } else if (installResult && installResult.settingsUnwritable) {
          // `scanForBrokenPaths` cannot surface this one: the file READS fine, so
          // it reports no `settings-unusable` issue and the branch below is
          // skipped. Without this arm a chmod on ~/.claude was diagnosed as
          // "plugin scripts may be missing — reinstall the npm package". The
          // sibling arm at `missing-hooks-in-settings` learned the unwritable
          // case in the previous round and this one did not; the two arms print
          // about the same install() call and have to agree about why it failed.
          console.log('  ❌ settings.json is not writable — hooks NOT repaired');
          console.log('     Fix the permissions on it (or on ~/.claude) and re-run; see the error above.');
        } else if (remaining.some((i) => i.type === 'settings-unusable')) {
          // Same branch runDiagnostics needs, for the same reason. This arm only
          // became REACHABLE for unusable settings once scanForBrokenPaths began
          // reporting them, so it inherited a diagnosis written for a different
          // cause \u2014 telling the user to reinstall an npm package because their
          // settings.json has a permissions problem or a trailing comma.
          console.log('  \u274c settings.json could not be read or parsed \u2014 hooks cannot be verified or repaired');
          console.log('     Repair it (or move it aside) and re-run; see the error above.');
        } else {
          console.log(`  \u274c ${remaining.length} hook path(s) still invalid \u2014 plugin scripts may be missing.`);
          console.log('     Reinstall: npm install -g @sdsrs/code-graph  (or re-run the plugin installer)');
        }
        break;
      }

      case 'missing-hooks-in-settings': {
        console.log('\n  Registering code-graph hooks in settings.json...');
        if (relicRepairGuard()) break;
        const { install } = require('./lifecycle');
        const r = install();
        if (r.hooksRegistered) {
          console.log('  \u2705 settings.json updated — restart Claude Code to apply');
          fixed++;
        } else if (r.settingsUnwritable) {
          // Symmetric with the unreadable arm below. Round-5 finding: the
          // unwritable case was wired into lifecycle's CLI but not here, so
          // doctor printed "install reported no change (settings already had
          // entries)" for a settings.json it had just failed to write.
          console.log('  \u274c settings.json is not writable \u2014 hooks NOT registered');
          console.log('     Fix the permissions on it (or on ~/.claude) and re-run; see the error above.');
        } else if (r.settingsUnreadable) {
          // install() refused because settings.json exists but cannot be turned
          // into an object (unparseable / unreadable / not an object). Reporting
          // "already had entries" here states the exact opposite of the truth,
          // at the one moment the user most needs the real cause \u2014 which
          // otherwise appears only on stderr, contradicted by this very line.
          console.log('  \u274c settings.json could not be read or parsed \u2014 hooks NOT registered');
          console.log('     Repair it (or move it aside) and re-run; see the error above.');
        } else {
          console.log('  \u2796 install reported no change (settings already had entries)');
        }
        break;
      }

      case 'index-corrupt': {
        // `incremental-index` (the index-empty arm) cannot help here: it opens
        // the same damaged file and diffs against it. The index is a rebuildable
        // cache, and this is the command health-check itself prints.
        console.log('\n  Rebuilding corrupt index from scratch...');
        console.log('    → code-graph-mcp rebuild-index --confirm');
        try {
          if (!rebuildIndex()) break;   // no binary to run it with
        } catch {
          // Includes the deliberate bail when another process holds the index
          // lock (v0.113.0), which is a refusal, not a failure to repair.
          console.log('  ❌ Index rebuild failed — see the error above');
          break;
        }
        // Exit 0 says the command ran, not that the corruption cleared. Ask the
        // database again, the same way the hooks arm re-scans after install().
        if (integrityOk()) {
          console.log('  ✅ Index rebuilt — integrity checks now pass');
          fixed++;
        } else {
          console.log('  ❌ Rebuild completed but the integrity check still fails');
          console.log('     The problem may be the filesystem or disk, not the index.');
        }
        break;
      }

      case 'schema-mismatch': {
        console.log('\n  Schema migration happens automatically when the binary runs.');
        console.log('  If binary is older than DB, update the binary first.');
        break;
      }

      default:
        break;
    }
  }
  return fixed;
}

// ── Main ──────────────────────────────────────────────────

// Exit status for a doctor run reflects what remains BROKEN, not what was found:
//   --check-only → every found issue is unresolved (report cleanliness, no repair).
//   repair mode  → issueCount minus what runRepairs resolved. A run that fixes
//                  everything ("N/N addressed") reports 0 so `doctor && …` and
//                  self-heal automation don't read a successful repair as a
//                  failure. runRepairs counts an issue fixed only when its repair
//                  reports success — and the hooks arm re-scans after install() to
//                  confirm, so a still-broken re-scan is NOT counted (stays
//                  unresolved → exit 1). An issue with no working repair
//                  (schema-mismatch is advisory only) likewise keeps this > 0.
function unresolvedCount({ checkOnly, issueCount, fixed }) {
  return checkOnly ? issueCount : issueCount - fixed;
}

function runDoctor(opts = {}) {
  const results = runDiagnostics({ checkOnly: opts.checkOnly });
  console.log(formatReport(results, { checkOnly: opts.checkOnly }));

  const issues = results.filter(r => r.status === 'warn' || r.status === 'error');

  let fixed = 0;
  if (issues.length > 0 && !opts.checkOnly) {
    fixed = runRepairs(results);
    console.log(`\n  ${fixed}/${issues.length} issue(s) addressed.`);
  }

  const unresolved = unresolvedCount({
    checkOnly: opts.checkOnly, issueCount: issues.length, fixed,
  });
  return { results, issueCount: issues.length, unresolved };
}

module.exports = { runDiagnostics, formatReport, runRepairs, runDoctor, runDoctorCli, parseDoctorArgs, unresolvedCount, surveyHookCoverage, relicRepairGuard, classifyEmbeddings, classifyIntegrity, classifyHealthReport, parseHealthPayload, integrityResolved, healthRows, detectEmbedModel, devBuildCommand, binaryVersionResolved, updateIncompleteResolved, autoUpdateNoOpReason };

// Shared by BOTH doctor entry points: `node doctor.js …` and `node lifecycle.js
// doctor …`. It exists as one function because the first version of this guard
// lived only in doctor.js's `require.main` block, leaving lifecycle's arm on the
// original `process.argv.includes('--check-only')` — so the exact bug being
// fixed (a typo'd flag running the repair pass) survived on the sibling entry
// point. Same half-applied shape this whole batch keeps producing.
//
// Returns `{ checkOnly }` to run, `{ help: true }` to print usage, or
// `{ error }` naming the offending arguments.
const DOCTOR_KNOWN_FLAGS = new Set(['--check-only', '--help', '-h']);
// Kept in sync with the `doctor` help text in src/main.rs, which intercepts
// `--help` before this script is spawned so that help stays side-effect-free.
// Two texts for one command drift silently; a user who reaches this one (direct
// `node doctor.js`) should read the same thing as one who reaches the other.
const DOCTOR_USAGE = [
  'code-graph-mcp doctor — diagnose and repair environment issues',
  '',
  'USAGE:',
  '    code-graph-mcp doctor [--check-only]',
  '',
  'By default doctor repairs detected issues (re-registers hooks in',
  '~/.claude/settings.json, fixes stale binary/model paths). Pass',
  '--check-only to report issues without changing anything.',
].join('\n');

function parseDoctorArgs(args) {
  const unknown = args.filter((a) => !DOCTOR_KNOWN_FLAGS.has(a));
  if (unknown.length) return { error: `doctor: unknown argument(s): ${unknown.join(' ')}` };
  if (args.includes('--help') || args.includes('-h')) return { help: true };
  return { checkOnly: args.includes('--check-only') };
}

// Run the CLI for a parsed argv tail and return the process exit code. Shared so
// the two entry points cannot drift on exit-code semantics either.
function runDoctorCli(args) {
  const parsed = parseDoctorArgs(args);
  if (parsed.error) {
    console.error(parsed.error);
    console.error(DOCTOR_USAGE);
    return 2;
  }
  if (parsed.help) {
    console.log(DOCTOR_USAGE);
    return 0;
  }
  const { unresolved } = runDoctor({ checkOnly: parsed.checkOnly });
  return unresolved > 0 ? 1 : 0;
}

if (require.main === module) {
  process.exit(runDoctorCli(process.argv.slice(2)));
}
