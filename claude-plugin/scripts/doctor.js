#!/usr/bin/env node
'use strict';
const { execFileSync, execSync } = require('child_process');
const fs = require('fs');
const path = require('path');
const os = require('os');
const { readBinaryVersion, isDevMode, getNewestMtime } = require('./version-utils');
const {
  getPluginVersion, readJson, healthCheck, CACHE_DIR,
  isOurHookEntry, settingsPath, buildSettingsHookEntries,
} = require('./lifecycle');
const { findBinary, clearCache: clearBinaryCache } = require('./find-binary');

// ── Diagnostics ───────────────────────────────────────────

/**
 * Run all diagnostic checks. Returns an array of:
 *   { name: string, status: 'ok'|'warn'|'error'|'skip', detail: string, fixId?: string }
 */
function runDiagnostics() {
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

    // 4. health-check (schema, index, embeddings) via binary --json
    if (execOk) {
      try {
        const cwd = process.cwd();
        const hcOutput = execFileSync(binary, ['health-check', '--json'], {
          cwd,
          timeout: 5000,
          encoding: 'utf8',
          stdio: ['pipe', 'pipe', 'pipe'],
        }).trim();
        const hc = JSON.parse(hcOutput);

        // No-index short-circuit — binary deliberately returns a structured
        // JSON with reason='no_index' instead of bailing, so we can route to
        // the index-empty fix without grepping stderr. Falls through to the
        // rest of runDiagnostics so Auto-update / Hooks still report.
        if (hc.reason === 'no_index') {
          results.push({ name: 'Schema', status: 'ok', detail: 'binary ok (no index yet)' });
          results.push({ name: 'Index', status: 'warn', detail: 'missing — not indexed yet', fixId: 'index-empty' });
          results.push({ name: 'Embeddings', status: 'skip', detail: 'no index' });
        } else {
          // Schema
          if (hc.issue && hc.issue.includes('schema')) {
            results.push({ name: 'Schema', status: 'warn', detail: hc.issue, fixId: 'schema-mismatch' });
          } else {
            results.push({ name: 'Schema', status: 'ok', detail: `v${hc.schema_version}` });
          }

          // Index
          if (hc.nodes === 0) {
            results.push({ name: 'Index', status: 'warn', detail: 'empty', fixId: 'index-empty' });
          } else {
            const age = hc.index_age ? ` (${hc.index_age})` : '';
            results.push({
              name: 'Index',
              status: 'ok',
              detail: `${hc.nodes} nodes, ${hc.edges} edges, ${hc.files} files${age}`,
            });
          }

          // Embeddings
          const ep = hc.embedding_progress || '0/0';
          const [done, total] = ep.split('/').map(Number);
          if (total > 0 && done < total) {
            const pct = Math.round((done / total) * 100);
            results.push({ name: 'Embeddings', status: 'ok', detail: `${pct}% (${done}/${total})` });
          } else if (total === 0) {
            results.push({ name: 'Embeddings', status: 'ok', detail: 'no embeddable nodes' });
          } else {
            results.push({ name: 'Embeddings', status: 'ok', detail: `100% (${done}/${total})` });
          }
        }
      } catch (e) {
        const rawStderr = e.stderr ? e.stderr.toString() : '';
        const msg = rawStderr ? rawStderr.trim().slice(0, 100) : e.message.slice(0, 100);
        // "No index found" is a missing-index situation, not a broken binary —
        // the index-empty fix path knows how to create one. Without this branch
        // the fixId routes to nothing and the report shows "0/1 addressed".
        if (rawStderr.includes('No index found')) {
          results.push({ name: 'Schema', status: 'ok', detail: 'binary ok (no index yet)' });
          results.push({ name: 'Index', status: 'warn', detail: 'missing — not indexed yet', fixId: 'index-empty' });
          results.push({ name: 'Embeddings', status: 'skip', detail: 'no index' });
        } else {
          results.push({ name: 'Schema', status: 'error', detail: `health-check failed: ${msg}`, fixId: 'binary-broken' });
          results.push({ name: 'Index', status: 'skip', detail: 'health-check failed' });
          results.push({ name: 'Embeddings', status: 'skip', detail: 'health-check failed' });
        }
      }
    } else {
      results.push({ name: 'Schema', status: 'skip', detail: 'binary not executable' });
      results.push({ name: 'Index', status: 'skip', detail: 'binary not executable' });
      results.push({ name: 'Embeddings', status: 'skip', detail: 'binary not executable' });
    }
  }

  // 5. Auto-update state
  try {
    const state = readJson(path.join(CACHE_DIR, 'update-state.json'));
    if (state && state.updateAvailable && state.binaryUpdated === false) {
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
  const hookResult = healthCheck();
  if (hookResult.healthy) {
    results.push({ name: 'Hooks', status: 'ok', detail: 'all paths valid' });
  } else if (hookResult.repaired) {
    results.push({
      name: 'Hooks',
      status: 'ok',
      detail: `${hookResult.issues.length} issue(s) auto-repaired`,
    });
  } else {
    const remainingCount = Array.isArray(hookResult.remaining)
      ? hookResult.remaining.length
      : hookResult.issues.length;
    results.push({
      name: 'Hooks',
      status: 'warn',
      detail: `${remainingCount} invalid path(s) — auto-repair did not resolve`,
      fixId: 'hooks-invalid',
    });
  }

  // 7. settings.json hook coverage — v0.32.0 inversion. Current Claude Code
  //    silently ignores plugin-cache hooks.json for PreToolUse/PostToolUse/
  //    UserPromptSubmit. lifecycle.js install/update is responsible for
  //    registering them in settings.json. "Missing" is the bug (previously
  //    "present" was treated as legacy debris — that was wrong).
  try {
    const settings = readJson(settingsPath()) || {};
    const cov = surveyHookCoverage(settings);
    if (cov.missing.length === 0 && cov.stale.length === 0) {
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
  } catch { /* probe failed — skip */ }

  return results;
}

// Inventory of (event, matcher) tuples we expect to find in settings.json after
// install. Used by doctor to detect missing entries.
function surveyHookCoverage(settings) {
  const desired = buildSettingsHookEntries();
  const expected = [];
  const desiredCmd = {}; // key -> command string we would write now
  for (const [event, entries] of Object.entries(desired)) {
    for (const e of entries) {
      const key = `${event}:${e.matcher || '*'}`;
      expected.push(key);
      desiredCmd[key] = e.hooks && e.hooks[0] && e.hooks[0].command;
    }
  }

  const present = new Set();
  const presentCmd = {}; // key -> command currently registered
  if (settings && settings.hooks) {
    for (const [event, entries] of Object.entries(settings.hooks)) {
      if (!Array.isArray(entries)) continue;
      for (const entry of entries) {
        if (isOurHookEntry(entry)) {
          const key = `${event}:${entry.matcher || '*'}`;
          present.add(key);
          if (entry.hooks && entry.hooks[0] && entry.hooks[0].command) {
            presentCmd[key] = entry.hooks[0].command;
          }
        }
      }
    }
  }

  const missing = expected.filter(k => !present.has(k));
  // Stale = present but the registered command no longer matches what we'd write
  // now (points at an old plugin-cache version dir / moved path). A stale path can
  // run pre-recordRecommendation hook code, so the hook fires but the conversion
  // metric stays dark — invisible to a present/absent check. This is the
  // 0.45.1-registered-while-0.45.4-active case the RCA surfaced.
  const stale = expected.filter(k =>
    present.has(k) && desiredCmd[k] && presentCmd[k] && presentCmd[k] !== desiredCmd[k]
  );
  return { expected, present: [...present], missing, stale };
}

// ── Report Formatting ─────────────────────────────────────

const STATUS_ICONS = { ok: '\u2705', warn: '\u26a0\ufe0f', error: '\u274c', skip: '\u2796' };

function formatReport(results) {
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
    lines.push(`  ${issues.length} issue(s) found.${fixable.length > 0 ? ' Fixing...' : ''}`);
  }

  return lines.join('\n');
}

// ── Repair Actions ────────────────────────────────────────

function runRepairs(results) {
  const fixable = results.filter(r => r.fixId);
  if (fixable.length === 0) return 0;

  let fixed = 0;
  for (const issue of fixable) {
    switch (issue.fixId) {
      case 'binary-stale':
      case 'version-mismatch': {
        if (!isDevMode()) {
          console.log('\n  Triggering binary update...');
          try {
            execFileSync(process.execPath, [path.join(__dirname, 'auto-update.js'), 'check'], {
              timeout: 60000,
              stdio: 'inherit',
            });
            console.log('  \u2705 Update check complete');
            fixed++;
          } catch {
            console.log('  \u274c Update check failed — install manually');
          }
          break;
        }
        console.log('\n  Building binary...');
        console.log('    \u2192 cargo build --release --no-default-features');
        try {
          const projectRoot = path.resolve(__dirname, '..', '..');
          execSync('cargo build --release --no-default-features', {
            cwd: projectRoot,
            stdio: 'inherit',
            timeout: 300000,
          });
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
        if (isDevMode()) {
          console.log('    \u2192 cargo build --release --no-default-features');
          try {
            const projectRoot = path.resolve(__dirname, '..', '..');
            execSync('cargo build --release --no-default-features', {
              cwd: projectRoot,
              stdio: 'inherit',
              timeout: 300000,
            });
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
            execFileSync(binary, ['incremental-index'], {
              cwd: process.cwd(),
              stdio: 'inherit',
              timeout: 120000,
            });
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
          execFileSync(process.execPath, [path.join(__dirname, 'auto-update.js'), 'check'], {
            timeout: 60000,
            stdio: 'inherit',
          });
          console.log('  \u2705 Update check complete');
          fixed++;
        } catch {
          console.log('  \u274c Update check failed');
        }
        break;
      }

      case 'hooks-invalid': {
        console.log('\n  Repairing hooks...');
        const { install } = require('./lifecycle');
        install();
        console.log('  \u2705 Hooks repaired \u2014 restart Claude Code to apply');
        fixed++;
        break;
      }

      case 'missing-hooks-in-settings': {
        console.log('\n  Registering code-graph hooks in settings.json...');
        const { install } = require('./lifecycle');
        const r = install();
        if (r.hooksRegistered) {
          console.log('  \u2705 settings.json updated — restart Claude Code to apply');
          fixed++;
        } else {
          console.log('  \u2796 install reported no change (settings already had entries)');
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

function runDoctor(opts = {}) {
  const results = runDiagnostics();
  console.log(formatReport(results));

  const issues = results.filter(r => r.status === 'warn' || r.status === 'error');

  if (issues.length > 0 && !opts.checkOnly) {
    const fixed = runRepairs(results);
    console.log(`\n  ${fixed}/${issues.length} issue(s) addressed.`);
  }

  return { results, issueCount: issues.length };
}

module.exports = { runDiagnostics, formatReport, runRepairs, runDoctor, surveyHookCoverage };

if (require.main === module) {
  const args = process.argv.slice(2);
  const checkOnly = args.includes('--check-only');
  const { issueCount } = runDoctor({ checkOnly });
  process.exit(issueCount > 0 ? 1 : 0);
}
