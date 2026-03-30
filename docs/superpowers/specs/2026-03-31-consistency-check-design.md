# Unified Consistency Check System

**Date:** 2026-03-31
**Status:** Draft
**Scope:** Plugin consistency validation + diagnostic repair

## Problem

Multiple components must stay in sync: Rust binary version, plugin.json version, database schema, index data, and hook paths. When any of these drift (forgotten rebuild, partial auto-update, schema change), users encounter cryptic errors instead of clear diagnostics.

## Architecture

Two-layer defense with a single user-facing entry point:

```
Session Start ──→ consistencyCheck() in session-init.js
                    │  (lightweight, <2s, within 5s hook timeout)
                    ├─ All consistent → silent (no output)
                    └─ Issues found → stderr warnings with fix commands

User on-demand ──→ doctor.js
                    │  (deep diagnosis + repair, no timeout)
                    ├─ Run all checks (superset of health-check)
                    ├─ Print diagnostic report
                    └─ Execute fixes (rebuild, redownload, reindex)
```

## Layer 1: session-init Consistency Check

### Location

New `consistencyCheck()` function in `claude-plugin/scripts/session-init.js`, called from `runSessionInit()` after `verifyBinary()` succeeds.

### Checks

**Check 1 — Binary version vs plugin version**

```js
function checkVersionMatch(binary) {
  const pluginVersion = getPluginVersion();        // from plugin.json
  const binaryVersion = readBinaryVersion(binary); // binary --version → regex parse
  if (binaryVersion && binaryVersion !== pluginVersion) {
    return {
      id: 'version-mismatch',
      msg: `Binary v${binaryVersion}, plugin expects v${pluginVersion}`,
      fix: isDevMode()
        ? 'cargo build --release'
        : 'Run /doctor to redownload binary',
    };
  }
  return null;
}
```

- `readBinaryVersion()` already exists in `auto-update.js` — extract to shared util or inline
- Uses `execFileSync(binary, ['--version'], { timeout: 2000 })` + regex `/^code-graph-mcp\s+(\d+\.\d+\.\d+)$/`

**Check 2 — Source freshness (dev mode only)**

```js
function checkSourceFreshness(binary) {
  if (!isDevMode()) return null;

  const srcDir = path.resolve(__dirname, '..', '..', 'src');
  const binaryStat = fs.statSync(binary);
  const latestSrcMtime = getNewestMtime(srcDir); // recursive newest .rs file

  if (latestSrcMtime > binaryStat.mtimeMs) {
    const delta = Math.round((latestSrcMtime - binaryStat.mtimeMs) / 60000);
    return {
      id: 'binary-stale',
      msg: `src/ modified ${delta}min after last build`,
      fix: 'cargo build --release',
    };
  }
  return null;
}
```

- `getNewestMtime(dir)`: scan `src/**/*.rs` for max mtime (fast — typically <200 files)
- Only runs when `isDevMode()` returns true (Cargo.toml nearby or symlink)

**Check 3 — Auto-update incomplete state**

```js
function checkAutoUpdateState() {
  const state = readJson(path.join(CACHE_DIR, 'update-state.json'));
  if (!state) return null;

  if (state.updateAvailable && state.binaryUpdated === false) {
    return {
      id: 'update-incomplete',
      msg: `Plugin updated to v${state.latestVersion}, binary not updated`,
      fix: 'Run /doctor to complete update',
    };
  }
  return null;
}
```

- Reads existing `~/.cache/code-graph/update-state.json` (written by auto-update.js)
- No new state files needed

### Output Format

```
⚠️ [code-graph] 2 consistency issues:
  1. Binary stale: src/ modified 5min after last build
     → cargo build --release
  2. Auto-update incomplete: plugin v0.7.17, binary not updated
     → Run /doctor to fix
```

All checks pass → **no output** (silent success).

### Integration Point

In `runSessionInit()`, after `verifyBinary()` returns `{ available: true }`:

```js
const consistencyIssues = binaryCheck.available
  ? consistencyCheck(binaryCheck.binary)
  : [];
```

Issues are written to `process.stderr` (hook stdout is for context injection).

### Performance Budget

| Check | Expected time |
|-------|---------------|
| Binary --version | <500ms (cached binary path) |
| Source mtime scan | <200ms (~200 .rs files) |
| Read update-state.json | <5ms |
| **Total** | **<1s** |

Well within the 5s hook timeout, even with existing session-init work (~2s for project map).

## Layer 2: doctor.js

### Location

New file: `claude-plugin/scripts/doctor.js`

### Invocation

```bash
# Via CLI (primary user-facing path)
code-graph-mcp doctor

# Via lifecycle.js CLI
node lifecycle.js doctor

# Direct (dev/debug)
node claude-plugin/scripts/doctor.js

# Diagnostic only (no repair)
code-graph-mcp doctor --check-only
```

The `doctor` subcommand is added to the Rust CLI (`src/cli.rs`), which delegates to `node doctor.js` (same pattern as other CLI commands that wrap JS). This makes `/doctor` natural in session-init warnings: `→ code-graph-mcp doctor`.

### Diagnostic Phase (read-only)

Runs all checks and prints a report:

| Check | Source | Method |
|-------|--------|--------|
| Binary version vs plugin version | `--version` + plugin.json | Same as session-init check 1 |
| Source freshness (dev mode) | fs.stat comparison | Same as session-init check 2 |
| Auto-update state | update-state.json | Same as session-init check 3 |
| Schema version | `code-graph-mcp health-check --json` | Parse JSON output |
| Index health (nodes/edges/files) | `code-graph-mcp health-check --json` | Parse JSON output |
| Embedding coverage | `code-graph-mcp health-check --json` | Parse JSON output |
| Hook paths validity | Existing `healthCheck()` from lifecycle.js | Reuse directly |
| Binary executable | `fs.accessSync(X_OK)` | Already in verifyBinary() |

### Output Format

```
🔍 code-graph doctor v0.7.16

  Binary version  ⚠️  v0.7.15 (plugin expects v0.7.16)
  Source fresh     ⚠️  src/ modified 3min after binary
  Auto-update     ⚠️  binary download incomplete
  Schema          ✅  v6
  Index           ✅  1542 nodes, 3847 edges, 247 files (2h ago)
  Embeddings      ✅  98% (1510/1542)
  Hooks           ✅  all paths valid
  Binary exec     ✅  /home/user/.cache/code-graph/bin/code-graph-mcp

  3 issues found. Fixing...
```

### Repair Phase

When issues are detected, doctor.js executes fixes automatically (no interactive prompt — the user already invoked /doctor with intent to fix):

| Issue | Repair Action |
|-------|---------------|
| Binary stale (dev mode) | `cargo build --release --no-default-features` from project root, then clear binary cache |
| Binary version mismatch (non-dev) | Reuse `auto-update.js` binary download logic for the expected version |
| Auto-update incomplete | Same as above — download binary for `state.latestVersion` |
| Index empty | Spawn `code-graph-mcp full-index` |
| Index stale | Spawn `code-graph-mcp incremental-index` |
| Hook paths invalid | Call `lifecycle.install()` (existing self-heal) |
| Schema mismatch | Warn only — schema migration happens automatically on next binary run. If binary is older than DB schema, warn user to update binary |

### Repair Output

```
  Fixing binary...
    → cargo build --release --no-default-features
    ✅ Built v0.7.16 (42s)

  Fixing index...
    → code-graph-mcp incremental-index
    ✅ Indexed 247 files

  3/3 issues resolved.
```

## Shared Utilities

Extract from existing code into a shared location (e.g., `claude-plugin/scripts/version-utils.js`):

```js
// From auto-update.js — binary version reading
function readBinaryVersion(binaryPath) { ... }

// From auto-update.js — dev mode detection
function isDevMode() { ... }

// New — recursive newest mtime for .rs files
function getNewestMtime(dir, ext = '.rs') { ... }

// From lifecycle.js — re-export
const { CACHE_DIR, readJson, writeJsonAtomic, getPluginVersion } = require('./lifecycle');
```

This avoids duplicating `readBinaryVersion` and `isDevMode` across session-init.js and doctor.js.

## Files to Create/Modify

| File | Action | Description |
|------|--------|-------------|
| `claude-plugin/scripts/version-utils.js` | **Create** | Shared utilities (readBinaryVersion, isDevMode, getNewestMtime) |
| `claude-plugin/scripts/session-init.js` | **Modify** | Add `consistencyCheck()`, call from `runSessionInit()` |
| `claude-plugin/scripts/doctor.js` | **Create** | Full diagnostic + repair script |
| `claude-plugin/scripts/lifecycle.js` | **Modify** | Add `doctor` CLI command (delegates to doctor.js), export CACHE_DIR |
| `claude-plugin/scripts/auto-update.js` | **Modify** | Import `readBinaryVersion`/`isDevMode` from version-utils instead of defining locally |
| `src/cli.rs` | **Modify** | Add `doctor` subcommand that delegates to `node doctor.js` |

## What Does NOT Change

- **Rust side**: `health-check` command unchanged, doctor.js calls it via `--json`
- **Schema migration**: Automatic on DB open — no changes needed
- **sync-versions.js / pre-commit.sh**: Dev-time guards stay as-is
- **auto-update.js core logic**: Only refactor to share utils, behavior unchanged

## Edge Cases

1. **Binary not found at all**: Already handled by existing `verifyBinary()` — consistency checks are skipped (no binary to compare)
2. **health-check --json fails**: doctor.js catches error, reports "binary not functional" and suggests rebuild
3. **Dev mode + auto-update**: `isDevMode()` suppresses auto-update (existing behavior), so check 3 won't fire alongside check 2
4. **First install (no DB yet)**: health-check reports "index empty" — doctor triggers full-index, not treated as consistency error in session-init
5. **Concurrent sessions**: Read-only checks are safe. Doctor repairs are idempotent (cargo build, install() are safe to run concurrently)

## Testing Strategy

### Unit Tests (in `claude-plugin/scripts/`)

- `version-utils.test.js`: Test `readBinaryVersion` regex parsing, `getNewestMtime` with mock fs, `isDevMode` detection
- `session-init.test.js`: Add tests for `consistencyCheck()` — mock binary version, mock file mtimes, mock update-state.json
- `doctor.test.js`: Test diagnostic report generation, mock `execFileSync` for health-check output, test repair action selection

### Integration Test

- Build binary, modify a source file's mtime, verify session-init warns about staleness
- Run doctor.js, verify it rebuilds and warning clears

## Success Criteria

1. Session-init detects all three inconsistency types within <2s
2. Doctor provides complete diagnostic covering all existing health-check functionality + new consistency checks
3. Doctor auto-repairs fixable issues without user interaction
4. Zero false positives in normal dev workflow (clean build, matching versions)
5. Existing auto-update and lifecycle flows continue working unchanged
