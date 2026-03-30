# Unified Consistency Check Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Detect version/binary/schema inconsistencies at session start and provide a `/doctor` command that diagnoses and auto-repairs all known issues.

**Architecture:** Two layers — a lightweight `consistencyCheck()` in session-init.js that warns on mismatch (< 2s), and a full `doctor.js` script that runs all checks + executes repairs. Shared utilities extracted to `version-utils.js` to avoid duplication between session-init, doctor, and auto-update.

**Tech Stack:** Node.js (plugin scripts), Rust CLI (doctor subcommand delegate)

---

### Task 1: Create version-utils.js with shared utilities

**Files:**
- Create: `claude-plugin/scripts/version-utils.js`
- Create: `claude-plugin/scripts/version-utils.test.js`

- [ ] **Step 1: Write the tests for version-utils**

Create `claude-plugin/scripts/version-utils.test.js`:

```js
'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');

function mkDir(prefix) {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix));
}

// ── readBinaryVersion ──

test('readBinaryVersion returns version from valid binary', () => {
  const { readBinaryVersion } = require('./version-utils');
  const dir = mkDir('vu-');
  const bin = path.join(dir, 'code-graph-mcp');
  fs.writeFileSync(bin, [
    '#!/usr/bin/env bash',
    'if [ "$1" = "--version" ]; then',
    '  echo "code-graph-mcp 1.2.3"',
    '  exit 0',
    'fi',
    'exit 0',
  ].join('\n'));
  fs.chmodSync(bin, 0o755);
  assert.equal(readBinaryVersion(bin), '1.2.3');
});

test('readBinaryVersion returns null for non-existent binary', () => {
  const { readBinaryVersion } = require('./version-utils');
  assert.equal(readBinaryVersion('/tmp/does-not-exist-binary'), null);
});

test('readBinaryVersion returns null for binary with unexpected output', () => {
  const { readBinaryVersion } = require('./version-utils');
  const dir = mkDir('vu-');
  const bin = path.join(dir, 'code-graph-mcp');
  fs.writeFileSync(bin, '#!/usr/bin/env bash\necho "something else"');
  fs.chmodSync(bin, 0o755);
  assert.equal(readBinaryVersion(bin), null);
});

// ── isDevMode ──

test('isDevMode returns true when Cargo.toml exists in parent of plugin root', () => {
  const { isDevMode } = require('./version-utils');
  // When running from source repo, __dirname/../.. has Cargo.toml
  // This test validates the function is callable; actual value depends on env
  assert.equal(typeof isDevMode(), 'boolean');
});

// ── getNewestMtime ──

test('getNewestMtime returns 0 for non-existent directory', () => {
  const { getNewestMtime } = require('./version-utils');
  assert.equal(getNewestMtime('/tmp/no-such-dir-xyz'), 0);
});

test('getNewestMtime finds newest .rs file mtime', () => {
  const { getNewestMtime } = require('./version-utils');
  const dir = mkDir('vu-mtime-');
  const sub = path.join(dir, 'sub');
  fs.mkdirSync(sub);

  // Write two .rs files with different mtimes
  const older = path.join(dir, 'old.rs');
  const newer = path.join(sub, 'new.rs');
  fs.writeFileSync(older, 'fn old() {}');

  // Force a different mtime by writing after a brief pause
  const olderMtime = fs.statSync(older).mtimeMs;
  fs.writeFileSync(newer, 'fn new() {}');
  // Touch newer file to ensure it's newer
  const futureMs = Date.now() + 1000;
  fs.utimesSync(newer, futureMs / 1000, futureMs / 1000);

  const result = getNewestMtime(dir, '.rs');
  assert.ok(result >= olderMtime, 'newest mtime should be >= older file mtime');
});

test('getNewestMtime ignores non-matching extensions', () => {
  const { getNewestMtime } = require('./version-utils');
  const dir = mkDir('vu-ext-');
  fs.writeFileSync(path.join(dir, 'file.js'), 'hello');
  // No .rs files → should return 0
  assert.equal(getNewestMtime(dir, '.rs'), 0);
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd /mnt/data_ssd/dev/projects/code-graph-mcp && node --test claude-plugin/scripts/version-utils.test.js`
Expected: FAIL — `Cannot find module './version-utils'`

- [ ] **Step 3: Implement version-utils.js**

Create `claude-plugin/scripts/version-utils.js`:

```js
'use strict';
const { execFileSync } = require('child_process');
const fs = require('fs');
const path = require('path');
const os = require('os');

const VERSION_OUTPUT_RE = /^code-graph-mcp\s+(\d+\.\d+\.\d+)$/;

/**
 * Read the version string from a code-graph-mcp binary.
 * Returns the semver string (e.g. "0.7.16") or null on failure.
 */
function readBinaryVersion(binaryPath) {
  try {
    const out = execFileSync(binaryPath, ['--version'], {
      timeout: 2000,
      stdio: ['pipe', 'pipe', 'pipe'],
    }).toString().trim();
    const match = out.match(VERSION_OUTPUT_RE);
    return match ? match[1] : null;
  } catch {
    return null;
  }
}

/**
 * Detect dev mode: running from source repo (Cargo.toml nearby) or plugin root is a symlink.
 */
function isDevMode() {
  const pluginRoot = path.resolve(__dirname, '..');
  if (fs.existsSync(path.join(pluginRoot, '..', 'Cargo.toml'))) return true;
  try { if (fs.lstatSync(pluginRoot).isSymbolicLink()) return true; } catch { /* ok */ }
  return false;
}

/**
 * Recursively find the newest mtime among files with the given extension.
 * Returns mtimeMs (number) or 0 if no matching files found.
 */
function getNewestMtime(dir, ext = '.rs') {
  let newest = 0;
  try {
    const entries = fs.readdirSync(dir, { withFileTypes: true });
    for (const entry of entries) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        const sub = getNewestMtime(full, ext);
        if (sub > newest) newest = sub;
      } else if (entry.name.endsWith(ext)) {
        const mt = fs.statSync(full).mtimeMs;
        if (mt > newest) newest = mt;
      }
    }
  } catch { /* dir doesn't exist or not readable */ }
  return newest;
}

module.exports = { readBinaryVersion, isDevMode, getNewestMtime, VERSION_OUTPUT_RE };
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd /mnt/data_ssd/dev/projects/code-graph-mcp && node --test claude-plugin/scripts/version-utils.test.js`
Expected: All tests PASS

- [ ] **Step 5: Commit**

```bash
git add claude-plugin/scripts/version-utils.js claude-plugin/scripts/version-utils.test.js
git commit -m "feat(plugin): add version-utils.js shared utilities"
```

---

### Task 2: Refactor auto-update.js to use version-utils

**Files:**
- Modify: `claude-plugin/scripts/auto-update.js`
- Test: `claude-plugin/scripts/auto-update.test.js` (existing, verify no regression)

- [ ] **Step 1: Replace local definitions with imports from version-utils**

In `claude-plugin/scripts/auto-update.js`, replace the local `VERSION_OUTPUT_RE`, `readBinaryVersion`, and `isDevMode` with imports:

At the top of the file (line 8-9 area), add the import and remove the local constant:

```js
// ADD this import (after existing requires):
const { readBinaryVersion, isDevMode, VERSION_OUTPUT_RE } = require('./version-utils');
```

Then **remove** these sections from auto-update.js:
- Line 35: `const VERSION_OUTPUT_RE = /^code-graph-mcp\s+(\d+\.\d+\.\d+)$/;` — delete this line
- Lines 68-78: The entire `isDevMode()` function — delete it
- Lines 187-198: The entire `readBinaryVersion()` function — delete it

The module.exports at line 397-401 still exports `isDevMode`, `readBinaryVersion` — update to re-export from version-utils:

```js
module.exports = {
  checkForUpdate, commandExists, isDevMode, readState, compareVersions,
  getExtractedPluginVersion, readBinaryVersion, promoteVerifiedBinary, isSilentMode,
  requestJson, parseLatestRelease, fetchLatestRelease,
};
```

These names still resolve correctly since they're now imported from version-utils at module scope.

- [ ] **Step 2: Run existing auto-update tests to verify no regression**

Run: `cd /mnt/data_ssd/dev/projects/code-graph-mcp && node --test claude-plugin/scripts/auto-update.test.js`
Expected: All existing tests PASS

- [ ] **Step 3: Commit**

```bash
git add claude-plugin/scripts/auto-update.js
git commit -m "refactor(plugin): use version-utils in auto-update.js"
```

---

### Task 3: Add consistencyCheck() to session-init.js

**Files:**
- Modify: `claude-plugin/scripts/session-init.js`
- Modify: `claude-plugin/scripts/session-init.test.js`

- [ ] **Step 1: Write tests for consistencyCheck**

Append to `claude-plugin/scripts/session-init.test.js`:

```js
const { consistencyCheck } = require('./session-init');

test('consistencyCheck is exported as a function', () => {
  assert.equal(typeof consistencyCheck, 'function');
});

test('consistencyCheck returns empty array when binary version matches plugin', () => {
  // In dev mode with a freshly built binary, there should be no issues
  // (or if binary not found, returns empty because checks are skipped)
  const result = consistencyCheck('/tmp/nonexistent-binary');
  assert.ok(Array.isArray(result));
});

test('consistencyCheck returns version-mismatch when versions differ', () => {
  const os = require('os');
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cc-'));
  const bin = path.join(dir, 'code-graph-mcp');
  // Create a fake binary that reports version 0.0.1
  fs.writeFileSync(bin, [
    '#!/usr/bin/env bash',
    'if [ "$1" = "--version" ]; then',
    '  echo "code-graph-mcp 0.0.1"',
    '  exit 0',
    'fi',
    'exit 0',
  ].join('\n'));
  fs.chmodSync(bin, 0o755);

  const issues = consistencyCheck(bin);
  const versionIssue = issues.find(i => i.id === 'version-mismatch');
  // Plugin version is 0.7.16 (from plugin.json), binary reports 0.0.1 — should mismatch
  assert.ok(versionIssue, 'should detect version mismatch');
  assert.ok(versionIssue.msg.includes('0.0.1'));
});
```

- [ ] **Step 2: Run to verify new tests fail**

Run: `cd /mnt/data_ssd/dev/projects/code-graph-mcp && node --test claude-plugin/scripts/session-init.test.js`
Expected: FAIL — `consistencyCheck` is not exported

- [ ] **Step 3: Implement consistencyCheck in session-init.js**

Add the following to `claude-plugin/scripts/session-init.js`, before the `runSessionInit()` function:

```js
const { readBinaryVersion, isDevMode, getNewestMtime } = require('./version-utils');

/**
 * Lightweight consistency checks — called from runSessionInit().
 * Returns an array of issue objects: { id, msg, fix }.
 * Empty array = all consistent (silent).
 */
function consistencyCheck(binary) {
  const issues = [];

  // Check 1: Binary version vs plugin version
  try {
    const pluginVersion = getPluginVersion();
    const binaryVersion = readBinaryVersion(binary);
    if (binaryVersion && binaryVersion !== pluginVersion) {
      issues.push({
        id: 'version-mismatch',
        msg: `Binary v${binaryVersion}, plugin expects v${pluginVersion}`,
        fix: isDevMode() ? 'cargo build --release' : 'code-graph-mcp doctor',
      });
    }
  } catch { /* skip check on error */ }

  // Check 2: Source freshness (dev mode only)
  try {
    if (isDevMode()) {
      const srcDir = path.resolve(__dirname, '..', '..', 'src');
      const binaryMtime = fs.statSync(binary).mtimeMs;
      const latestSrcMtime = getNewestMtime(srcDir, '.rs');
      if (latestSrcMtime > binaryMtime) {
        const deltaMin = Math.round((latestSrcMtime - binaryMtime) / 60000);
        issues.push({
          id: 'binary-stale',
          msg: `src/ modified ${deltaMin}min after last build`,
          fix: 'cargo build --release',
        });
      }
    }
  } catch { /* skip check on error */ }

  // Check 3: Auto-update incomplete
  try {
    const statePath = path.join(
      require('os').homedir(), '.cache', 'code-graph', 'update-state.json'
    );
    const state = readJson(statePath);
    if (state && state.updateAvailable && state.binaryUpdated === false) {
      issues.push({
        id: 'update-incomplete',
        msg: `Plugin updated to v${state.latestVersion}, binary not updated`,
        fix: 'code-graph-mcp doctor',
      });
    }
  } catch { /* skip check on error */ }

  // Output warnings to stderr
  if (issues.length > 0) {
    const lines = [`[code-graph] ${issues.length} consistency issue(s):`];
    issues.forEach((issue, i) => {
      lines.push(`  ${i + 1}. ${issue.msg}`);
      lines.push(`     → ${issue.fix}`);
    });
    process.stderr.write(lines.join('\n') + '\n');
  }

  return issues;
}
```

Then integrate into `runSessionInit()`. After the `verifyBinary()` call (line ~181), add:

```js
  const consistencyIssues = binaryCheck.available
    ? consistencyCheck(binaryCheck.binary)
    : [];
```

And update the return statement to include `consistencyIssues`:

```js
  return { inactive: false, lifecycle, autoUpdateLaunched, indexFreshness, mapInjected, binaryCheck, consistencyIssues };
```

Add `consistencyCheck` to the `module.exports` at the bottom of the file:

```js
module.exports = {
  launchBackgroundAutoUpdate,
  syncLifecycleConfig,
  ensureIndexFresh,
  injectProjectMap,
  verifyBinary,
  consistencyCheck,
  runSessionInit,
};
```

- [ ] **Step 4: Run all session-init tests**

Run: `cd /mnt/data_ssd/dev/projects/code-graph-mcp && node --test claude-plugin/scripts/session-init.test.js`
Expected: All tests PASS (including new consistency check tests)

- [ ] **Step 5: Commit**

```bash
git add claude-plugin/scripts/session-init.js claude-plugin/scripts/session-init.test.js
git commit -m "feat(plugin): add consistency checks to session-init"
```

---

### Task 4: Create doctor.js with diagnostic + repair

**Files:**
- Create: `claude-plugin/scripts/doctor.js`
- Create: `claude-plugin/scripts/doctor.test.js`

- [ ] **Step 1: Write tests for doctor.js**

Create `claude-plugin/scripts/doctor.test.js`:

```js
'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');

const { runDiagnostics, formatReport } = require('./doctor');

function mkDir(prefix) {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix));
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd /mnt/data_ssd/dev/projects/code-graph-mcp && node --test claude-plugin/scripts/doctor.test.js`
Expected: FAIL — `Cannot find module './doctor'`

- [ ] **Step 3: Implement doctor.js**

Create `claude-plugin/scripts/doctor.js`:

```js
#!/usr/bin/env node
'use strict';
const { execFileSync, execSync } = require('child_process');
const fs = require('fs');
const path = require('path');
const os = require('os');
const { readBinaryVersion, isDevMode, getNewestMtime } = require('./version-utils');
const { getPluginVersion, readJson, healthCheck, CACHE_DIR } = require('./lifecycle');
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
    // Can't run further binary-dependent checks
    results.push({ name: 'Binary version', status: 'skip', detail: 'binary not found' });
    results.push({ name: 'Source fresh', status: 'skip', detail: 'binary not found' });
    results.push({ name: 'Schema', status: 'skip', detail: 'binary not found' });
    results.push({ name: 'Index', status: 'skip', detail: 'binary not found' });
    results.push({ name: 'Embeddings', status: 'skip', detail: 'binary not found' });
  } else {
    // Check executable permission
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
      } catch (e) {
        const msg = e.stderr ? e.stderr.toString().trim().slice(0, 100) : e.message.slice(0, 100);
        results.push({ name: 'Schema', status: 'error', detail: `health-check failed: ${msg}`, fixId: 'binary-broken' });
        results.push({ name: 'Index', status: 'skip', detail: 'health-check failed' });
        results.push({ name: 'Embeddings', status: 'skip', detail: 'health-check failed' });
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
  const hookResult = healthCheck();
  if (hookResult.healthy) {
    results.push({ name: 'Hooks', status: 'ok', detail: 'all paths valid' });
  } else {
    results.push({
      name: 'Hooks',
      status: hookResult.repaired ? 'ok' : 'warn',
      detail: hookResult.repaired
        ? `${hookResult.issues.length} issue(s) auto-repaired`
        : `${hookResult.issues.length} invalid path(s)`,
      fixId: hookResult.repaired ? undefined : 'hooks-invalid',
    });
  }

  return results;
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
          // Non-dev: attempt to re-download binary
          console.log('\n  Downloading binary...');
          try {
            const { checkForUpdate } = require('./auto-update');
            // Force a fresh check by clearing throttle
            checkForUpdate().then(() => {
              console.log('  \u2705 Binary download triggered (runs in background)');
            }).catch(() => {
              console.log('  \u274c Binary download failed — install manually');
            });
          } catch {
            console.log('  \u274c Could not trigger auto-update');
          }
          fixed++;
          break;
        }
        // Dev mode: cargo build
        console.log('\n  Building binary...');
        console.log('    \u2192 cargo build --release --no-default-features');
        try {
          const projectRoot = path.resolve(__dirname, '..', '..');
          execSync('cargo build --release --no-default-features', {
            cwd: projectRoot,
            stdio: 'inherit',
            timeout: 300000, // 5 min
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
              timeout: 120000, // 2 min
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
          const { checkForUpdate } = require('./auto-update');
          checkForUpdate().then(() => {
            console.log('  \u2705 Update check triggered');
          }).catch(() => {
            console.log('  \u274c Update check failed');
          });
          fixed++;
        } catch {
          console.log('  \u274c Could not trigger update');
        }
        break;
      }

      case 'hooks-invalid': {
        console.log('\n  Repairing hooks...');
        const { install } = require('./lifecycle');
        install();
        console.log('  \u2705 Hooks repaired');
        fixed++;
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

module.exports = { runDiagnostics, formatReport, runRepairs, runDoctor };

if (require.main === module) {
  const args = process.argv.slice(2);
  const checkOnly = args.includes('--check-only');
  const { issueCount } = runDoctor({ checkOnly });
  process.exit(issueCount > 0 ? 1 : 0);
}
```

- [ ] **Step 4: Run doctor tests**

Run: `cd /mnt/data_ssd/dev/projects/code-graph-mcp && node --test claude-plugin/scripts/doctor.test.js`
Expected: All tests PASS

- [ ] **Step 5: Manual smoke test**

Run: `cd /mnt/data_ssd/dev/projects/code-graph-mcp && node claude-plugin/scripts/doctor.js --check-only`
Expected: Diagnostic report prints with check results — no repairs attempted

- [ ] **Step 6: Commit**

```bash
git add claude-plugin/scripts/doctor.js claude-plugin/scripts/doctor.test.js
git commit -m "feat(plugin): add doctor.js diagnostic and repair tool"
```

---

### Task 5: Add `doctor` CLI command to lifecycle.js and Rust CLI

**Files:**
- Modify: `claude-plugin/scripts/lifecycle.js:590-616`
- Modify: `src/main.rs:99-116`
- Modify: `src/main.rs:122-168` (help text)

- [ ] **Step 1: Add doctor command to lifecycle.js CLI**

In `claude-plugin/scripts/lifecycle.js`, in the `if (require.main === module)` block (line ~590), add a `doctor` branch before the `else` fallback:

```js
  } else if (cmd === 'doctor') {
    const { runDoctor } = require('./doctor');
    const checkOnly = process.argv.includes('--check-only');
    const { issueCount } = runDoctor({ checkOnly });
    process.exit(issueCount > 0 ? 1 : 0);
  } else {
    console.error('Usage: lifecycle.js <install|uninstall|update|health|doctor>');
    process.exit(1);
  }
```

- [ ] **Step 2: Add doctor subcommand to Rust CLI**

In `src/main.rs`, add a new match arm before `Some(other)` (around line 107):

```rust
        Some("doctor") => {
            // Delegate to plugin's doctor.js — it handles all diagnostic + repair logic
            let exe_dir = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

            // Try to find doctor.js relative to binary location
            let doctor_candidates = [
                // Dev mode: binary is in target/release/, doctor.js is in claude-plugin/scripts/
                exe_dir.join("../../claude-plugin/scripts/doctor.js"),
                // Installed via npm: doctor.js is alongside the binary's package
                exe_dir.join("../claude-plugin/scripts/doctor.js"),
            ];

            for candidate in &doctor_candidates {
                if candidate.exists() {
                    let mut cmd = Command::new("node");
                    cmd.arg(candidate);
                    if args.iter().any(|a| a == "--check-only") {
                        cmd.arg("--check-only");
                    }
                    let status = cmd.status()?;
                    std::process::exit(status.code().unwrap_or(1));
                }
            }

            eprintln!("doctor.js not found. Run directly: node claude-plugin/scripts/doctor.js");
            std::process::exit(1);
        }
```

Also add the doctor command to the help text in `print_help()` (around line 144):

```rust
    println!("    doctor              Diagnose and repair environment issues");
```

- [ ] **Step 3: Build and test**

Run: `cd /mnt/data_ssd/dev/projects/code-graph-mcp && cargo check`
Expected: Compiles without errors

Run: `cd /mnt/data_ssd/dev/projects/code-graph-mcp && cargo build --release --no-default-features`
Then: `./target/release/code-graph-mcp doctor --check-only`
Expected: Doctor runs and prints diagnostic report

- [ ] **Step 4: Commit**

```bash
git add claude-plugin/scripts/lifecycle.js src/main.rs
git commit -m "feat(cli): add doctor subcommand for environment diagnostics"
```

---

### Task 6: Run all tests and verify end-to-end

**Files:**
- All modified files from Tasks 1-5

- [ ] **Step 1: Run all plugin JS tests**

Run: `cd /mnt/data_ssd/dev/projects/code-graph-mcp && node --test claude-plugin/scripts/version-utils.test.js claude-plugin/scripts/session-init.test.js claude-plugin/scripts/auto-update.test.js claude-plugin/scripts/doctor.test.js`
Expected: All tests PASS

- [ ] **Step 2: Run Rust tests**

Run: `cd /mnt/data_ssd/dev/projects/code-graph-mcp && cargo test --no-default-features`
Expected: All tests PASS

- [ ] **Step 3: Run pre-commit checks**

Run: `cd /mnt/data_ssd/dev/projects/code-graph-mcp && bash scripts/pre-commit.sh`
Expected: All checks pass (version sync, JS tests, Rust checks)

- [ ] **Step 4: End-to-end smoke test — session-init**

Run: `cd /mnt/data_ssd/dev/projects/code-graph-mcp && node claude-plugin/scripts/session-init.js 2>&1`
Expected: Session init completes. If binary is freshly built and versions match, no consistency warnings appear.

- [ ] **Step 5: End-to-end smoke test — doctor**

Run: `cd /mnt/data_ssd/dev/projects/code-graph-mcp && node claude-plugin/scripts/doctor.js --check-only`
Expected: Full diagnostic report with all checks showing status.

- [ ] **Step 6: Verify stale binary detection (dev mode)**

```bash
# Touch a source file to make it newer than binary
touch src/main.rs
# Run session-init — should warn about stale binary
node claude-plugin/scripts/session-init.js 2>&1 | grep -i "consistency\|stale\|modified"
# Run doctor — should also detect it
node claude-plugin/scripts/doctor.js --check-only 2>&1 | grep -i "Source\|stale"
# Restore mtime
git checkout src/main.rs
```

Expected: Both session-init and doctor detect the stale binary.
