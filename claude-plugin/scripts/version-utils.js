'use strict';
const { execFileSync } = require('child_process');
const fs = require('fs');
const path = require('path');
const { hidden } = require('./proc-opts');

// Tolerant match: the version line anywhere in the output (m flag), optional
// "v" prefix, and anything after the numeric triple (build-metadata suffixes
// like "1.2.3 (abc123)" or "-dev"). The old fully-anchored /^...$/ turned ANY
// deviation into null, which upstream reads as "broken binary" → judged
// permanently stale → re-download every session, with the fresh download
// rejected by the same parse: a self-sustaining loop.
const VERSION_OUTPUT_RE = /^code-graph-mcp\s+v?(\d+\.\d+\.\d+)/m;

function readBinaryVersion(binaryPath, { timeoutMs = 5000 } = {}) {
  try {
    const out = execFileSync(binaryPath, ['--version'], hidden({
      // 5s default: a cold exec of a freshly-written ~40MB binary (page-in,
      // Windows AV scan) regularly exceeded the old 2s, misclassifying a good
      // binary. `timeoutMs` exists for callers running inside a hook budget —
      // session-init spends what is LEFT of its 5s SessionStart allowance here,
      // which is otherwise the single largest unclamped child in that hook
      // (audit 2026-09-05 NEW-05). Must be an integer: `child_process`
      // validates it and throws ERR_OUT_OF_RANGE on a fraction.
      timeout: timeoutMs,
      stdio: ['pipe', 'pipe', 'pipe'],
    })).toString().trim();
    const match = out.match(VERSION_OUTPUT_RE);
    return match ? match[1] : null;
  } catch {
    return null;
  }
}

/**
 * Compare semver-ish version strings; returns -1, 0, or 1. Numeric triple
 * compared ordinally (missing/non-numeric parts → 0); a pre-release suffix
 * sorts BELOW its release ("1.2.3-rc1" < "1.2.3"), two pre-releases compare
 * as plain strings. Single canonical implementation — auto-update.js and
 * find-binary.js each carried a divergent copy whose pre-release semantics
 * disagreed (Number("4-rc1")→NaN→0 vs parseInt("3-rc1")→3), a silent
 * mis-ordering trap if release tags ever adopt "-rc" suffixes.
 */
function compareVersions(a, b) {
  const parse = (v) => {
    const s = String(v);
    const dash = s.indexOf('-');
    const core = dash === -1 ? s : s.slice(0, dash);
    return {
      nums: core.split('.').map((x) => parseInt(x, 10)),
      pre: dash === -1 ? null : s.slice(dash + 1),
    };
  };
  const pa = parse(a), pb = parse(b);
  for (let i = 0; i < 3; i++) {
    const x = Number.isFinite(pa.nums[i]) ? pa.nums[i] : 0;
    const y = Number.isFinite(pb.nums[i]) ? pb.nums[i] : 0;
    if (x !== y) return x < y ? -1 : 1;
  }
  if (pa.pre && !pb.pre) return -1;
  if (!pa.pre && pb.pre) return 1;
  if (pa.pre && pb.pre && pa.pre !== pb.pre) return pa.pre < pb.pre ? -1 : 1;
  return 0;
}

function isDevMode(pluginRoot = path.resolve(__dirname, '..')) {
  // Explicit opt-in always wins (also lets users force dev mode in any layout)
  if (process.env.CODE_GRAPH_DEV === '1') return true;
  // Plugin root is a symlink (e.g. `npm link`)
  try { if (fs.lstatSync(pluginRoot).isSymbolicLink()) return true; } catch { /* ok */ }
  // Source repo: Cargo.toml AND target/ at parent. Marketplace installs ship
  // Cargo.toml (git-tracked) but NOT target/ (gitignored), so target/ is the
  // discriminator — without it, a marketplace clone was being misclassified as
  // dev mode and the launcher's GitHub-release fallback was unreachable
  // (see GitHub issue #12). If a dev hasn't built yet, they fall through to
  // the user-mode auto-install path, which still produces a working binary.
  const parent = path.dirname(pluginRoot);
  if (fs.existsSync(path.join(parent, 'Cargo.toml')) &&
      fs.existsSync(path.join(parent, 'target'))) {
    return true;
  }
  return false;
}

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

module.exports = { readBinaryVersion, compareVersions, isDevMode, getNewestMtime, VERSION_OUTPUT_RE };
