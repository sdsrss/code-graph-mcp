#!/usr/bin/env node
'use strict';
const { spawn, execSync, execFileSync } = require('child_process');
const path = require('path');
const fs = require('fs');
const {
  install, update, readManifest, getPluginVersion, checkScopeConflict,
  cleanupDisabledStatusline, isPluginInactive, isPluginUninstalled, removeCacheResidue,
  readJson, CACHE_DIR, settingsPath, isStaleRelicContext, hookCmdScript,
} = require('./lifecycle');
const { readBinaryVersion, isDevMode, getNewestMtime } = require('./version-utils');
const { maybeAutoAdopt, isAdopted, unadopt } = require('./adopt');
const { isNonProjectCwd } = require('./project-detect');
const { hidden } = require('./proc-opts');

// v0.17.0 — quietHooks: unconditional quiet 默认。
// 项目地图与 MEMORY.md plugin contract + on-demand `project_map` 工具高度重叠，
// 默认每次 SessionStart 都注入 ≈2.3 KB 是不必要的常驻上下文成本。
// 优先级（高到低）：
//   1. legacy CODE_GRAPH_QUIET_HOOKS='0'  → forced noisy（向后兼容）
//   2. legacy CODE_GRAPH_QUIET_HOOKS='1'  → forced quiet（向后兼容）
//   3. CODE_GRAPH_VERBOSE_HOOKS='1'       → opt-in noisy（新）
//   4. 默认                                 → quiet
// `adopted` 参数已弃用（unconditional 默认不再依赖该信号），保留接口签名只为
// 不破坏既有调用 / 测试。
function computeQuietHooks({ env = {} } = {}) {
  const envQuiet = env.CODE_GRAPH_QUIET_HOOKS;
  if (envQuiet === '0') return false;
  if (envQuiet === '1') return true;
  if (env.CODE_GRAPH_VERBOSE_HOOKS === '1') return false;
  return true;
}

// SessionStart project-map injection gate. Beyond the (already default-quiet)
// verbose opt-in, the map is now also ADOPTED-ONLY: cross-project measurement
// (memory cross-project-interference) found the ≈2 KB dump is zero-referenced
// in projects the user hasn't adopted (no code-graph block in CLAUDE.md), so it
// only earns its standing-context cost for adopted projects. Unadopted projects
// get no map even under CODE_GRAPH_VERBOSE_HOOKS / legacy QUIET_HOOKS=0.
function shouldInjectMap({ available, quietHooks, adopted } = {}) {
  return !!(available && !quietHooks && adopted);
}

// v0.63 — SessionStart "live context": the recent-change blast radius from the
// AST index. UNLIKE injectProjectMap (default-OFF because the static module map
// duplicates the CLAUDE.md block + the on-demand project_map tool), this is git-delta-
// derived — it changes every session and the static CLAUDE.md block cannot carry it, so it earns
// being ON by default for adopted projects. It is the graph-unique, actionable
// counterpart to mem's SessionStart dashboard (which pushes recent activity to
// create engagement). Selectivity is automatic: a session with no recently
// changed *source* files (a deps-only or release commit, a clean checkout off a
// non-code commit) injects nothing — zero standing-context cost when idle.
//
// Gating differs from the static map on purpose: it respects only the hard
// kill-switch (CODE_GRAPH_QUIET_HOOKS=1) and a dedicated opt-out, NOT the
// default-quiet flag that suppresses the duplicative map.
function shouldInjectRecentImpact({ available, adopted, env = {} } = {}) {
  if (!available || !adopted) return false;
  if (env.CODE_GRAPH_QUIET_HOOKS === '1') return false;
  if (env.CODE_GRAPH_NO_RECENT_IMPACT === '1') return false;
  return true;
}

// Pure: given whether there are uncommitted (WIP) source changes and the
// SessionStart `source`, decide if the recent-impact injection is worth its
// standing-context cost. Marginal-information argument, not measured:
//   - WIP present → active work; the blast radius of what you're editing is the
//     highest-value case → always show.
//   - clean tree → we fall back to the LAST COMMIT. On a cold `startup` that's
//     low value (you just made it, or you're starting on something unrelated),
//     and the project's own evidence is that SessionStart dumps tend to go
//     unreferenced ([[project_cross_project_interference]]) → suppress. On a
//     resume (`clear`/`compact`/`resume`) the same last-commit reminder helps
//     re-establish context → show.
// Unknown source (direct calls / tests) defaults to showing — only an explicit
// cold `startup` with no WIP is suppressed.
function recentImpactWorthShowing({ isWip, source } = {}) {
  if (isWip) return true;
  return source !== 'startup';
}

// Source-file extensions the indexer extracts symbols from (AST-bearing). Config
// /lockfile/doc changes have no graph blast radius, so they're filtered out
// before the `affected` call — keeps the "Changed:" line free of Cargo.lock noise
// and avoids spending the CLI call on a commit that only bumped versions.
const RECENT_SRC_EXT =
  /\.(rs|ts|tsx|js|jsx|mjs|cjs|py|go|java|rb|php|swift|kt|kts|dart|c|h|cc|cpp|hpp|cs|sh|bash)$/i;

// Pure: filter file paths to indexed source files, capped. Accepts EITHER a raw
// `git diff --name-only` blob (string, split on newline) OR an already-parsed
// path array (from parseGitStatusPaths) — the array form is essential: passing
// the parser's array output to a string-only signature silently returned [] and
// broke WIP detection (caught by the composing test). Cap guards message length
// and the affected call's argv on a sweeping refactor.
function filterSourceFiles(input, cap = 25) {
  const lines = Array.isArray(input)
    ? input
    : (typeof input === 'string' ? input.split('\n') : []);
  return lines
    .map(s => s.trim())
    .filter(Boolean)
    .filter(f => RECENT_SRC_EXT.test(f))
    .slice(0, cap);
}

// Pure: extract file paths from `git status --porcelain` output. One call covers
// tracked changes (staged + unstaged) AND untracked files — the diff-only path
// MISSED untracked new source files you're actively editing (finding #3). Format
// is `XY␣PATH`, or `XY␣ORIG -> PATH` for renames (take the new path). Git quotes
// paths with special chars; best-effort unquote.
function parseGitStatusPaths(statusOutput) {
  if (!statusOutput || typeof statusOutput !== 'string') return [];
  const paths = [];
  for (const line of statusOutput.split('\n')) {
    if (line.length < 4) continue;        // need 2 status chars + space + ≥1 path char
    let rest = line.slice(3);             // skip the XY status columns + separator space
    const arrow = rest.indexOf(' -> ');   // rename/copy → the path after the arrow is current
    if (arrow >= 0) rest = rest.slice(arrow + 4);
    rest = rest.trim();
    if (rest.startsWith('"') && rest.endsWith('"')) rest = rest.slice(1, -1);
    if (rest) paths.push(rest);
  }
  return paths;
}

// Above ~15 direct dependents the per-name list is noise, not signal:
// information scales INVERSELY with blast size. A high-fanout node (a constants
// module, a shared util) "touches everything", so its actionable content
// collapses to "this is high-risk — run the full suite"; the arbitrary first-N
// dependent names add nothing. Below the threshold the specific dependents ARE
// the signal (edit one fn → these 3 callers). So scale detail to actionability.
const FANOUT_LIST_MAX = 15;

// Pure: render the injected text from the parsed `affected --json` payload.
// Returns null when there's nothing graph-relevant to say (no indexed dependents),
// so the caller injects nothing rather than an empty banner.
function formatRecentImpact(changed, affected, dependentCap = 6) {
  if (!Array.isArray(changed) || changed.length === 0) return null;
  const all = (affected && Array.isArray(affected.affected_files)) ? affected.affected_files : [];
  if (all.length === 0) return null;
  const direct = all.filter(a => a.depth === 1 && !a.is_test).map(a => a.path);
  const testCount = Array.isArray(affected.tests) ? affected.tests.length : all.filter(a => a.is_test).length;

  const changedShown = changed.slice(0, 8).join(', ') + (changed.length > 8 ? `, +${changed.length - 8}` : '');
  const lines = [
    '[code-graph] Recent changes — blast radius from the AST index (graph-only; not in MEMORY.md):',
    `  Changed: ${changedShown}`,
  ];
  if (direct.length > FANOUT_LIST_MAX) {
    // High fanout: the name list is noise; surface risk + test scope only.
    lines.push(`  High-fanout change — ${all.length} file(s) impacted (${direct.length} direct); run the full suite (${testCount} test file(s)).`);
  } else {
    lines.push(`  Impacts ${all.length} file(s) (${direct.length} direct dependent(s)), ${testCount} test file(s) to re-run.`);
    if (direct.length > 0) {
      const shown = direct.slice(0, dependentCap).join(', ');
      const more = direct.length > dependentCap ? `, +${direct.length - dependentCap} more` : '';
      lines.push(`  Direct dependents: ${shown}${more}`);
    }
  }
  // Runnable verbatim when ≤4 changed; above that, show 4 + an explicit count
  // rather than a bare "…" — a pasted "…" command yields a SMALLER blast than the
  // numbers above (the computation used all changed files, up to the cap) — finding #4.
  const runCmd = changed.length <= 4
    ? `code-graph-mcp affected ${changed.join(' ')}`
    : `code-graph-mcp affected ${changed.slice(0, 4).join(' ')}  (+${changed.length - 4} more changed file(s) — pass all for the full blast)`;
  lines.push(`  Re-run impacted tests: ${runCmd}`);
  return lines.join('\n');
}

function launchBackgroundAutoUpdate(spawnFn = spawn, env = process.env, { force = false } = {}) {
  try {
    // Documented opt-out (issue #40). Checked HERE as well as inside
    // auto-update.js so an opted-out user doesn't pay for a node process per
    // session just to have it exit immediately.
    if (env.CODE_GRAPH_NO_AUTO_UPDATE === '1') return false;
    const args = [path.join(__dirname, 'auto-update.js'), 'check', '--silent'];
    // A session start / reload forces an immediate check (bypasses the soft
    // throttle down to auto-update.js's short anti-hammer floor + rate-limit
    // backoff), so an available update is picked up now rather than on the next tick.
    if (force) args.push('--force');
    const child = spawnFn(process.execPath, args, hidden({
      detached: true,
      stdio: 'ignore',
      env: { ...env, CODE_GRAPH_AUTO_UPDATE_SILENT: '1' },
    }));
    if (child && typeof child.unref === 'function') child.unref();
    return true;
  } catch {
    return false;
  }
}

// A session start / resume / clear / explicit reload is a strong "I'm here, get
// me the latest" signal → force an immediate update check. Automatic mid-session
// compaction is not high-intent, so it keeps auto-update.js's gentle background
// cadence. Unknown source (direct calls / tests) is treated as high-intent.
function isHighIntentSource(source) {
  return source !== 'compact';
}

// Every install()/update() in this file goes through these wrappers so the
// "your settings.json was rebuilt" notice cannot be wired into some call sites
// and not others — there are seven, and the previous fix wired the notice only
// into `doctor`, the path a user runs deliberately, while THIS path runs on
// every SessionStart and stayed silent.
//
// The notice goes to STDOUT on purpose. lifecycle.js already logs it to stderr,
// which a SessionStart hook discards — so from the user's side their model /
// env / permissions vanished with no message at all. stdout is the channel
// Claude Code surfaces (same one injectProjectMap uses).
function reportRebuild(r) {
  if (r && r.settingsRebuiltFrom) {
    process.stdout.write(
      `[code-graph] ${settingsPath()} could not be parsed and has been REBUILT. ` +
      `Your original is saved at ${r.settingsRebuiltFrom} — merge anything you ` +
      `still need (model / env / permissions / your own hooks) back by hand.\n`
    );
  }
  // install()/update() have reported `manifestUnwritable` since they learned not
  // to throw on it, and NOBODY read the field — so a manifest that could not be
  // written (EACCES after a stray sudo, EROFS, a full disk) produced a silent
  // partial install. It is not cosmetic: `syncLifecycleConfig` keys entirely off
  // `manifest.version`, so an unwritten manifest makes every future SessionStart
  // re-run install() and re-report 'installed', forever, with nothing to show
  // for it (audit 2026-08-16 review Minor tail).
  if (r && r.manifestUnwritable) {
    process.stdout.write(
      `[code-graph] The plugin manifest could not be written (${r.manifestUnwritable}). ` +
      'Hooks are registered but the install will not be remembered, so this runs again ' +
      'every session. Check permissions on ~/.claude/plugins/, then run ' +
      '`code-graph-mcp doctor`.\n'
    );
  }
  return r;
}
function installReporting(...args) { return reportRebuild(install(...args)); }
function updateReporting(...args) { return reportRebuild(update(...args)); }

function syncLifecycleConfig() {
  // v0.49.1: stale-relic guard. A still-running Claude Code process fires
  // SessionStart from the plugin-cache dir it loaded at startup; after
  // auto-update installs a newer version, those old scripts would see
  // `manifest.version !== currentVersion` below and — direction-blind —
  // call update(), dragging manifest + every settings.json hook path back
  // to the old dir (upgrade↔downgrade ping-pong, observed live 2026-06-12).
  // installed_plugins.json is the authority on which install may self-heal.
  if (isStaleRelicContext()) return 'deferred-to-active-install';

  const manifest = readManifest();
  const currentVersion = getPluginVersion();

  if (!manifest.version) {
    installReporting();
    return 'installed';
  }
  if (manifest.version !== currentVersion) {
    updateReporting();
    return 'updated';
  }
  // Self-heal: version matches but statusLine may have been lost or path corrupted
  // (e.g. plugin removed and reinstalled, or CLAUDE_PLUGIN_ROOT leaked from another plugin).
  // install() is idempotent — isOurComposite guard prevents duplicate work.
  const settings = readJson(settingsPath()) || {};
  if (!settings.statusLine || !settings.statusLine.command ||
      !settings.statusLine.command.includes('statusline-composite')) {
    installReporting();
    return 'self-healed';
  }
  // Also self-heal if composite path points to a non-existent script (path
  // pollution). hookCmdScript, not another inline `/node\s+"…"/`: that spelling
  // cannot read a command whose interpreter is an absolute path
  // (`"C:\Program Files\nodejs\node.exe" "…\statusline-composite.js"`), and an
  // unreadable command silently reads as a healthy one.
  const compositeScript = hookCmdScript(settings.statusLine.command);
  if (compositeScript && !fs.existsSync(compositeScript)) {
    installReporting();
    return 'self-healed-bad-path';
  }
  // v0.49.1: also self-heal when the composite path exists but is not the one
  // we'd write now (old plugin-cache version dir that still exists on disk —
  // invisible to the existence check above; same fault class as the binary pin).
  //
  // "Not the one we'd write now" is NOT the same as stale, and this is the
  // second gate that had to learn it: two copies of the plugin derive different
  // absolute paths for the same current composite, so a bare string mismatch
  // made each session take the slot back from the other. It also has to agree
  // with `install()`, which now refuses to rewrite a live composite belonging to
  // another delivery surface — otherwise this reports
  // 'self-healed-stale-statusline' every single session while install() quietly
  // changes nothing.
  const { compositeCommand, compositeSlotIsStale } = require('./lifecycle');
  if (settings.statusLine.command !== compositeCommand()
      && compositeSlotIsStale(settings.statusLine.command)) {
    installReporting();
    return 'self-healed-stale-statusline';
  }
  // Self-heal if any hook command points to a non-existent script (path pollution)
  if (settings.hooks) {
    for (const entries of Object.values(settings.hooks)) {
      if (!Array.isArray(entries)) continue;
      for (const entry of entries) {
        if (!entry.hooks) continue;
        for (const h of entry.hooks) {
          const script = h.command && hookCmdScript(h.command);
          if (script && script.includes('code-graph') && !fs.existsSync(script)) {
            installReporting();
            return 'self-healed-bad-hook';
          }
        }
      }
    }
  }
  // v0.32.0: self-heal if our settings.json hook coverage is incomplete
  // (e.g. user manually edited settings.json, or settings.json got rewritten
  // by another tool that didn't preserve our entries). Without this, the
  // user silently loses PreToolUse/PostToolUse hooks until next plugin update.
  // v0.49.1: upgraded from matcher-presence to surveyHookCoverage so a
  // present-but-stale command path (old plugin-cache version dir that still
  // exists) also heals. Previously only doctor checked staleness, so if the
  // auto-update re-register step failed silently, users kept running old hook
  // code indefinitely — the settings.json sibling of the binary-pin bug.
  const { surveyHookCoverage } = require('./lifecycle');
  const cov = surveyHookCoverage(settings);
  if (cov.missing.length > 0) {
    installReporting();
    return 'self-healed-missing-settings-hook';
  }
  if (cov.stale.length > 0) {
    installReporting();
    return 'self-healed-stale-settings-hook';
  }
  return 'noop';
}

/**
 * Cheap probe for a rebuild reason that mtime/git can't see: an INDEX_VERSION
 * mismatch (the on-disk index was built by an older extractor generation). Since
 * P0, a reader (statusline `health-check`, `grep`) no longer wipes such an index —
 * it serves the stale structural data and reports "rebuild pending" — so in a
 * project where the MCP server isn't running, nothing else nudges a rebuild.
 * health-check carries the verdict in `index_version_stale`. Best-effort: any
 * failure → false (never force work off a bad probe).
 */
function indexNeedsRevalidation(bin, cwd) {
  try {
    let out;
    try {
      out = execFileSync(bin, ['health-check', '--format', 'json'],
        hidden({ cwd, timeout: 3000, stdio: ['pipe', 'pipe', 'pipe'] })).toString();
    } catch (e) {
      // health-check exits non-zero on an unhealthy index but still writes JSON.
      out = ((e && e.stdout) || '').toString();
    }
    return JSON.parse(out).index_version_stale === true;
  } catch {
    return false;
  }
}

/**
 * Decide whether the index needs a background refresh and, if so, spawn a
 * detached `incremental-index` (which revalidates: an indexer open wipes +
 * rebuilds a version-stale index, then re-indexes all files).
 *
 * Two independent triggers:
 *   1. git HEAD newer than index.db mtime — content drifted since last index.
 *   2. INDEX_VERSION mismatch (post-upgrade) — see indexNeedsRevalidation.
 *
 * Returns 'fresh' | 'refreshing' | 'skipped'.
 */
function ensureIndexFresh() {
  const { findBinary } = require('./find-binary');
  const bin = findBinary();
  if (!bin) return 'skipped';

  // Canonical index root, not the bare session cwd: a session launched in a
  // linked worktree (resolves to the main checkout) or a subdir otherwise
  // gate-fails here and freshness never runs (sibling of the statusline/hook
  // subdir-cwd dark class).
  const { resolveProjectRoot } = require('./project-root');
  const cwd = resolveProjectRoot(process.cwd()) || process.cwd();
  const dbPath = path.join(cwd, '.code-graph', 'index.db');
  if (!fs.existsSync(dbPath)) return 'skipped';

  let needsRefresh = false;
  // Trigger 1: git HEAD newer than index mtime.
  try {
    const dbMtime = fs.statSync(dbPath).mtimeMs;
    const gitTs = parseInt(
      execSync('git log -1 --format=%ct', hidden({ cwd, timeout: 2000, encoding: 'utf8', stdio: ['pipe', 'pipe', 'pipe'] })).trim()
    ) * 1000;
    if (gitTs > dbMtime) needsRefresh = true;
  } catch { /* no git / not a repo — fall through to the version probe */ }

  // Trigger 2: INDEX_VERSION mismatch (only probe when mtime looked fresh).
  if (!needsRefresh && indexNeedsRevalidation(bin, cwd)) needsRefresh = true;

  if (!needsRefresh) return 'fresh';

  const child = spawn(bin, ['incremental-index', '--quiet'], hidden({
    cwd,
    detached: true,
    stdio: 'ignore',
  }));
  if (child && typeof child.unref === 'function') child.unref();
  return 'refreshing';
}

/**
 * Verify binary is available and executable.
 * On macOS, detect Gatekeeper quarantine (common after npm/GitHub download).
 * Returns { available, binary, issue? }.
 */
function verifyBinary() {
  const { findBinary } = require('./find-binary');
  const binary = findBinary();
  if (!binary) {
    process.stderr.write(
      '[code-graph] Binary not found — MCP server cannot start.\n' +
      'Install: npm install -g @sdsrs/code-graph\n'
    );
    return { available: false, binary: null };
  }

  // Check executable permission
  try {
    fs.accessSync(binary, fs.constants.X_OK);
  } catch {
    process.stderr.write(
      `[code-graph] Binary not executable: ${binary}\n` +
      `Fix: chmod +x "${binary}"\n`
    );
    if (process.platform === 'darwin') {
      process.stderr.write(`Also try: xattr -d com.apple.quarantine "${binary}"\n`);
    }
    return { available: false, binary, issue: 'not-executable' };
  }

  // On macOS, verify the binary can actually run (Gatekeeper may block it)
  if (process.platform === 'darwin') {
    try {
      execFileSync(binary, ['--version'], hidden({ timeout: 3000, stdio: 'pipe' }));
    } catch (err) {
      const msg = (err.message || '') + (err.stderr ? err.stderr.toString() : '');
      if (msg.includes('quarantine') || msg.includes('not permitted') ||
          msg.includes('killed') || err.status === 137 || err.signal === 'SIGKILL') {
        process.stderr.write(
          `[code-graph] macOS Gatekeeper is blocking the binary: ${binary}\n` +
          `Fix: xattr -d com.apple.quarantine "${binary}"\n` +
          `Then restart Claude Code to reconnect the MCP server.\n`
        );
        return { available: false, binary, issue: 'quarantine' };
      }
      // Other errors (e.g., missing libs) — still report
      process.stderr.write(
        `[code-graph] Binary found but failed to run: ${binary}\n` +
        `Error: ${msg.slice(0, 200)}\n`
      );
      return { available: false, binary, issue: 'runtime-error' };
    }
  }

  return { available: true, binary };
}

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
    const statePath = path.join(CACHE_DIR, 'update-state.json');
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

function runSessionInit({ source } = {}) {
  // GC the shared tmp dir before anything else, so it happens even on the
  // inactive / non-project early returns below — those sessions still wrote
  // cooldown flags on the way in. Cheap (one readdir + a stat per entry) and
  // fully swallowed: reclaiming disk must never be able to fail a SessionStart.
  try { require('./tmp-dir').pruneCgTmp(); } catch { /* best-effort GC */ }

  if (isPluginInactive()) {
    // Capture the uninstalled-vs-disabled verdict BEFORE cleanupDisabledStatusline()
    // runs — it removes our composite + registry entry, which are the very signals
    // isPluginUninstalled()/isPluginInactive() read, so calling it afterwards would
    // always see "no composite/registry" and report not-uninstalled (teardown skipped).
    const uninstalled = isPluginUninstalled();
    // Third caller of the same unguarded teardown (statusline.js and
    // statusline-composite.js are the others): a read-only ~/.claude turns this
    // into an uncaught throw that takes down the whole SessionStart hook.
    try { cleanupDisabledStatusline(); } catch { /* best-effort teardown */ }
    // Genuine uninstall (not a temporary disable) leaves residue the settings-only
    // self-heal can't reach: ~/.cache/code-graph (the ~40MB binary + state) and the
    // current project's CLAUDE.md adoption block. CC fires no uninstall hook, AND it
    // stops loading this plugin's hooks.json the moment the install record is gone —
    // so after a real `/plugin uninstall` this SessionStart usually never runs again.
    // The reachable teardown is cleanupDisabledStatusline() via the composite
    // statusline (still wired in settings.json); it removes the cache residue too.
    // This branch remains for the disable→uninstall-while-running edge and as the
    // only place project unadoption can happen automatically.
    let teardown = null;
    if (uninstalled) {
      // Unadopt BEFORE the wipe. The adopted-projects registry lives inside
      // CACHE_DIR, so wiping first destroyed the record of every OTHER adopted
      // repo — this branch only unadopts the current cwd, and a later
      // `uninstall --unadopt-all` then read an empty registry and reported
      // `unadopted: []` while the blocks stayed behind. Same capture-before-
      // cleanup ordering `uninstall()` already had to learn; this was its
      // sibling.
      //
      // Honest scope: this reorder alone does NOT save the registry. On the
      // genuine post-uninstall path `cleanupDisabledStatusline()` runs earlier
      // in this same function and already calls removeCacheResidue() itself, so
      // the wipe still happens before we get here. What actually protects the
      // other projects is the preservation inside removeCacheResidue(); this
      // ordering is the belt to that pair of braces, and it is what keeps the
      // single-project case from depending on the preservation at all.
      let unadopted = false;
      try {
        if (!isNonProjectCwd(process.cwd())) {
          const r = unadopt({ cwd: process.cwd() });
          unadopted = !!(r && (r.blockPruned || r.fileRemoved || r.claudeMdRemoved));
        }
      } catch { /* best-effort — never let teardown break SessionStart */ }
      const cacheRemoved = removeCacheResidue();
      teardown = { cacheRemoved, unadopted };
    }
    return { inactive: true, lifecycle: 'noop', autoUpdateLaunched: false, teardown };
  }

  // Global-config self-heal runs BEFORE the non-project gate: settings.json is
  // user-global and install() only writes claudeHome paths (idempotent, noop in
  // steady state — a handful of JSON reads). Previously this sat behind the
  // gate, so a missing/stale hook entry never healed while sessions started in
  // marker-less cwds (e.g. the claude-mem-lite headless /tmp fleet) — the
  // structural residue of the daagu weeks-dark bash-guard incident
  // (project_cross_project_interference).
  const lifecycle = syncLifecycleConfig();
  // v0.49.1: a stale relic (see isStaleRelicContext) must not write ANY
  // versioned state — that includes the adoption template: maybeAutoAdopt's
  // drift-refresh would "refresh" MEMORY.md back to the relic's OLD shipped
  // template, the adoption-surface twin of the settings.json downgrade war.
  const isRelic = lifecycle === 'deferred-to-active-install';

  // Non-project cwd (no .git/manifest — e.g. /tmp, where claude-mem-lite
  // spawns headless `claude -p` calls that never use code-graph): otherwise
  // no-op. Returns BEFORE verifyBinary / ensureIndexFresh / maybeAutoAdopt /
  // injectProjectMap so the plugin leaves zero PROJECT footprint (no
  // incremental-index spawn, no map injection, no adoption, no .code-graph).
  // The MCP launcher applies the same gate — see project-detect.js.
  if (isNonProjectCwd(process.cwd())) {
    return { inactive: false, nonProject: true, lifecycle, autoUpdateLaunched: false };
  }

  const conflict = checkScopeConflict();
  if (conflict) {
    process.stderr.write(
      `[code-graph] Warning: conflicting install detected — ${conflict.existingId} (${conflict.scope || 'unknown'} scope). ` +
      `Use /plugin to remove one to avoid config conflicts.\n`
    );
  }

  // Verify binary availability — catch issues early with actionable diagnostics
  const binaryCheck = verifyBinary();

  const autoUpdateLaunched = launchBackgroundAutoUpdate(spawn, process.env, { force: isHighIntentSource(source) });
  const indexFreshness = binaryCheck.available ? ensureIndexFresh() : 'skipped';

  // 上下文感知默认：插件模式下首次 SessionStart 自动安装（创建/注入 CLAUDE.md 块 +
  // .claude/ detail 文件），并清理旧 memory-dir 制品（升级自动迁移）。shipped 漂移
  // 时刷新。三种情况发一次 stderr 提示，让用户知道发生了什么 + 如何回退。
  // Adoption is OPTIONAL; the rest of this hook is not. It touches files the
  // user owns (CLAUDE.md, .claude/) which can be unreadable, a directory, or on
  // a read-only mount — and a throw here used to abort every remaining step
  // (map injection, recent impact, consistency check, both hook canaries) with a
  // raw stack trace (audit 2026-08-16 P1-16). adopt() now returns reasons rather
  // than throwing; this is the belt to that suspenders, so a future unguarded
  // read inside it cannot take the session down again.
  let autoAdopt = { attempted: false, result: null };
  if (!isRelic) {
    try {
      autoAdopt = maybeAutoAdopt({ scriptPath: __dirname });
    } catch (e) {
      autoAdopt = { attempted: true, reason: 'threw', result: null, error: (e && e.message) || String(e) };
      process.stderr.write(
        `[code-graph] Skipped CLAUDE.md adoption for this project (${(e && e.code) || (e && e.message) || 'unknown error'}).\n` +
        '            Everything else in this session start continues normally.\n'
      );
    }
  }
  if (autoAdopt.result && autoAdopt.result.ok === false &&
      (autoAdopt.result.reason === 'claude-md-unreadable' || autoAdopt.result.reason === 'claude-md-unwritable' ||
       autoAdopt.result.reason === 'detail-unwritable')) {
    // A refusal is not a silent no-op: the user's steering block is NOT
    // installed/refreshed, and only this line says so.
    process.stderr.write(
      `[code-graph] Could not install the CLAUDE.md steering block (${autoAdopt.result.reason}: ` +
      `${autoAdopt.result.error || 'unknown'}). Nothing was changed.\n` +
      '            Opt out permanently: CODE_GRAPH_NO_AUTO_ADOPT=1\n'
    );
  }
  const migrated = autoAdopt.migrated || {};
  if (migrated.memoryIndexPruned || migrated.legacyDetailRemoved) {
    process.stderr.write(
      '[code-graph] Migrated to CLAUDE.md steering — cleaned legacy memory-dir artifacts\n' +
      '            (MEMORY.md sentinel + detail file). Your other memories are untouched.\n'
    );
  }
  if (autoAdopt.attempted && autoAdopt.result && autoAdopt.result.ok) {
    if (autoAdopt.reason === 'refreshed') {
      // Name BOTH refreshed surfaces: the drift-refresh fully overwrites the
      // generated detail doc, so a user who hand-edited it must learn why
      // their edits vanished and how to lock the file.
      const detailNote = autoAdopt.result.detailWritten
        ? ' + .claude/plugin_code_graph_mcp.md (manual edits to that generated file are overwritten)'
        : '';
      process.stderr.write(
        `[code-graph] Refreshed CLAUDE.md decision block to latest shipped version${detailNote}.\n` +
        '            Lock files: CODE_GRAPH_NO_TEMPLATE_REFRESH=1 in ~/.claude/settings.json env\n'
      );
    } else {
      process.stderr.write(
        '[code-graph] Installed code-graph block into project CLAUDE.md (plugin install → knowing consent).\n' +
        '            Detail table: .claude/plugin_code_graph_mcp.md (generated; safe to gitignore)\n' +
        '            Opt out:    CODE_GRAPH_NO_AUTO_ADOPT=1 in ~/.claude/settings.json env\n' +
        '            Reverse:    code-graph-mcp unadopt\n'
      );
    }
    // `adopt()` has returned `registryRecorded` since it stopped throwing on a
    // broken registry, and nothing read it. The consequence is not cosmetic:
    // `uninstall()` walks that registry to strip our managed block from every
    // adopted project's CLAUDE.md, so an unrecorded project keeps the block
    // FOREVER after uninstall, with no plugin code left to remove it — the
    // teardown-asymmetry class this repo has already been bitten by (audit
    // 2026-08-16 review Minor tail).
    if (autoAdopt.result.registryRecorded === false) {
      process.stderr.write(
        '[code-graph] Note: this project could not be recorded in the adopted-projects registry,\n' +
        '            so `/plugin uninstall` will NOT strip the block from this CLAUDE.md.\n' +
        '            Remove it by hand with `code-graph-mcp unadopt` before uninstalling.\n'
      );
    }
  }

  // quietHooks: default quiet (project_map injection duplicates MEMORY.md +
  // on-demand tool). CODE_GRAPH_VERBOSE_HOOKS=1 to opt in to the dump;
  // legacy CODE_GRAPH_QUIET_HOOKS=0/1 still force the old behavior. The opt-in
  // dump is further gated to adopted projects (shouldInjectMap).
  const adopted = isAdopted();
  const quietHooks = computeQuietHooks({ env: process.env });

  const mapInjected = shouldInjectMap({ available: binaryCheck.available, quietHooks, adopted })
    ? injectProjectMap()
    : false;
  // v0.63 — live context: recent-change blast radius. Default-ON for adopted
  // projects (separate gate from the duplicative static map); self-selecting
  // (nothing injected when no source files changed recently).
  const recentImpactInjected = shouldInjectRecentImpact({ available: binaryCheck.available, adopted, env: process.env })
    ? injectRecentImpact({ source })
    : false;
  const consistencyIssues = binaryCheck.available
    ? consistencyCheck(binaryCheck.binary)
    : [];
  // v0.67.0 — hook reliability (active-project path only): Layer A pushes a
  // FAILED firing self-test result (and refreshes it in the background, off the
  // 5s budget); Layer B is the runtime dispatch-dark canary. Both best-effort.
  const hookFireWarn = checkHookFiring();
  if (hookFireWarn) process.stderr.write(hookFireWarn);
  const hookDarkWarn = detectHookDark();
  if (hookDarkWarn) process.stderr.write(hookDarkWarn);
  return {
    inactive: false, lifecycle,
    autoUpdateLaunched, indexFreshness, mapInjected, recentImpactInjected, binaryCheck, consistencyIssues,
    quietHooks, adopted, autoAdopted: autoAdopt.attempted,
    hookFireWarn: !!hookFireWarn, hookDarkWarn: !!hookDarkWarn,
  };
}

/**
 * Inject project_map summary into session context if index exists.
 * Similar to aider's repo-map — gives Claude project structure upfront.
 */
function injectProjectMap() {
  try {
    const { resolveProjectRoot } = require('./project-root');
    const cwd = resolveProjectRoot(process.cwd()) || process.cwd();
    const dbPath = path.join(cwd, '.code-graph', 'index.db');
    if (!fs.existsSync(dbPath)) return false;

    // findBinary, not bare 'code-graph-mcp' on PATH (finding #8): a stale/global
    // PATH binary reads a different/older index and returns "(empty project)"
    // even when the local index is populated — the sibling bug injectRecentImpact
    // already avoided via findBinary().
    const { findBinary } = require('./find-binary');
    const bin = findBinary();
    if (!bin) return false;

    const output = execFileSync(bin, ['map', '--compact'], hidden({
      cwd,
      timeout: 5000,
      encoding: 'utf8',
      stdio: ['pipe', 'pipe', 'pipe'],
      // Hook-internal delivery, not a model conversion — keep record_cli_use from
      // logging this `map` run as a phantom `use` (mirror injectRecentImpact's affected call).
      env: { ...process.env, CODE_GRAPH_INTERNAL: '1' },
    }));

    if (output && output.trim()) {
      process.stdout.write(
        '[code-graph] Project map (indexed):\n' + output.trim() + '\n'
      );
      return true;
    }
  } catch {
    // Index not ready or binary not found — skip silently
  }
  return false;
}

/**
 * v0.63 — inject the recent-change blast radius (graph-unique "live" context).
 * Changed source files = working-tree changes vs HEAD (WIP), else the last commit.
 * The last-commit fallback is suppressed on a cold `startup` (low marginal value)
 * via recentImpactWorthShowing. One bounded `affected --json` call; degrades to
 * silent no-op on any error. Records a `live_impact` event so `stats` can see the
 * feature fire (else it's a dark metric). Returns true iff something was injected.
 */
function injectRecentImpact({ source } = {}) {
  try {
    // Index + telemetry live at the canonical root (worktree → main checkout,
    // subdir → project root); git WIP detection stays in the SESSION dir — the
    // worktree's branch state is what this session edits. Repo-relative git
    // paths translate 1:1 to root-relative index paths (a worktree mirrors the
    // checkout layout).
    const { resolveProjectRoot } = require('./project-root');
    const sessionDir = process.cwd();
    const cwd = resolveProjectRoot(sessionDir) || sessionDir;
    const dbPath = path.join(cwd, '.code-graph', 'index.db');
    if (!fs.existsSync(dbPath)) return false;

    // WIP = ALL working-tree changes (staged + unstaged + UNTRACKED) in one
    // `git status` call — the old diff-only path missed untracked new source
    // files you're actively editing (finding #3). Clean tree → fall back to the
    // last commit. Timeouts tightened (finding #1): worst-case cap sum is now
    // status(1s) + HEAD~1(1s) + affected(1.5s) = 3.5s, comfortably under the 5s
    // SessionStart hook budget; the old 2+2+3=7s could get the whole hook killed.
    const gitOpts = hidden({ cwd: sessionDir, timeout: 1000, encoding: 'utf8', stdio: ['pipe', 'pipe', 'pipe'] });
    let changed = [];
    let isWip = false;
    try {
      changed = filterSourceFiles(parseGitStatusPaths(
        execSync('git status --porcelain --untracked-files=all', gitOpts)));
      isWip = changed.length > 0;
      // Clean tree → fall back to what the last commit touched.
      if (!isWip) {
        changed = filterSourceFiles(execSync('git diff --name-only HEAD~1 HEAD', gitOpts));
      }
    } catch {
      return false; // not a git repo / no commits — nothing to diff
    }
    if (changed.length === 0) return false;

    // Marginal-value gate: cold startup + clean tree (last-commit fallback) → skip,
    // before spending the affected CLI call.
    if (!recentImpactWorthShowing({ isWip, source })) return false;

    const { findBinary } = require('./find-binary');
    const bin = findBinary();
    if (!bin) return false;

    let affected;
    try {
      const raw = execFileSync(bin, ['affected', ...changed, '--json'], hidden({
        cwd, timeout: 1500, encoding: 'utf8', stdio: ['pipe', 'pipe', 'pipe'],
        env: { ...process.env, CODE_GRAPH_INTERNAL: '1' },
      }));
      affected = JSON.parse(raw);
    } catch {
      return false;
    }

    const text = formatRecentImpact(changed, affected);
    if (!text) return false;
    process.stdout.write(text + '\n');

    // Instrumentation (step 3a): record that the injection fired so `stats`
    // surfaces it instead of the feature being dark. Carries the blast/direct
    // counts + WIP flag for later reference-rate / A-B analysis. Best-effort.
    try {
      const all = Array.isArray(affected.affected_files) ? affected.affected_files : [];
      const direct = all.filter(a => a.depth === 1 && !a.is_test).length;
      const { recordRecommendation } = require('./recommendation-log');
      recordRecommendation(cwd, { hook: 'session', action: 'live_impact', blast: all.length, direct, wip: isWip });
    } catch { /* telemetry must never break the injection */ }
    return true;
  } catch {
    return false;
  }
}

// ── v0.67.0 hook-reliability surfaces ─────────────────────────────────
//
// Layer A (firing): surface the cached `verify-hooks-fire` result. PUSH — the
// model/user learns of a broken hook without running doctor. Pure interpreter
// + an I/O wrapper that background-refreshes the cache off the SessionStart budget.
function hookFireWarning(state) {
  if (state && state.ok === false && Array.isArray(state.failures) && state.failures.length) {
    return `[code-graph] Hook firing self-test failed for: ${state.failures.join(', ')}.\n` +
           `            Registered but did not run on this machine — run: code-graph-mcp doctor\n`;
  }
  return null;
}

function checkHookFiring({ now = Date.now() } = {}) {
  const STALE_MS = 24 * 60 * 60 * 1000;
  let warn = null;
  try {
    const state = readJson(path.join(CACHE_DIR, 'hook-fire-state.json'));
    warn = hookFireWarning(state);
    const fresh = state && state.ts && (now - new Date(state.ts).getTime() < STALE_MS);
    if (!fresh) {
      // Background refresh — detached + unref'd + stdio ignore → zero budget impact.
      // First session after install has no cache → schedules a check; failures
      // surface from the next start. Re-checks daily (catches post-install drift,
      // e.g. a node upgrade that breaks a hook).
      try {
        const child = spawn(process.execPath, [path.join(__dirname, 'lifecycle.js'), 'verify-hooks-fire'], hidden({
          detached: true, stdio: 'ignore',
        }));
        if (child && typeof child.unref === 'function') child.unref();
      } catch { /* ok */ }
    }
  } catch { /* best-effort */ }
  return warn;
}

// Layer B (dispatch): verifyHooksFire proves the script RUNS; only a live session
// proves Claude Code DISPATCHES tool-calls to it. Pure: given recommendations.jsonl
// text, the grep/read matchers are "dark" when the edit hook has fired repeatedly
// (dispatch demonstrably works) but grep AND read never have. Relative sibling
// comparison → low false-positive (full darkness leaves no file → no claim).
function analyzeHookDark(recText) {
  let edit = 0, grepOrRead = 0;
  for (const line of (recText || '').split('\n')) {
    if (!line) continue;
    let ev; try { ev = JSON.parse(line); } catch { continue; }
    if (ev.hook === 'grep' || ev.hook === 'read') grepOrRead++;
    else if (ev.hook === 'edit') edit++;
  }
  if (edit >= 3 && grepOrRead === 0) {
    return `[code-graph] The grep/read PreToolUse hooks have recorded nothing in this project,\n` +
           `            though the edit hook fired ${edit}× — grep/read interception may be dark. Run: code-graph-mcp doctor\n`;
  }
  return null;
}

function detectHookDark() {
  try {
    const recPath = path.join(process.cwd(), '.code-graph', 'recommendations.jsonl');
    return analyzeHookDark(fs.readFileSync(recPath, 'utf8'));
  } catch { return null; } // no recommendations.jsonl → nothing to conclude
}

module.exports = {
  launchBackgroundAutoUpdate,
  isHighIntentSource,
  syncLifecycleConfig,
  ensureIndexFresh,
  indexNeedsRevalidation,
  injectProjectMap,
  injectRecentImpact,
  verifyBinary,
  consistencyCheck,
  runSessionInit,
  computeQuietHooks,
  shouldInjectMap,
  shouldInjectRecentImpact,
  recentImpactWorthShowing,
  filterSourceFiles,
  parseGitStatusPaths,
  formatRecentImpact,
  hookFireWarning, checkHookFiring, analyzeHookDark, detectHookDark, // v0.67.0
};

if (require.main === module) {
  // SessionStart passes {source:"startup"|"clear"|"compact"|"resume"} on stdin.
  // Best-effort + TTY-guarded: a hook gets piped JSON (EOF closes it), but a
  // manual `node session-init.js` in a terminal must not block on fd 0.
  let source;
  try {
    if (!process.stdin.isTTY) {
      source = JSON.parse(fs.readFileSync(0, 'utf8')).source;
    }
  } catch { /* no/garbled stdin → treat as unknown source */ }
  // Hooks FAIL OPEN. This one had no wrapper at all, so anything unhandled
  // anywhere in the sequence surfaced as a node stack trace plus a non-zero exit
  // in the user's session start — for work that is entirely optional
  // housekeeping (audit 2026-08-16 P1-16). One `[code-graph]` line, exit 0.
  try {
    runSessionInit({ source });
  } catch (e) {
    process.stderr.write(
      `[code-graph] SessionStart hook error (${(e && e.code) || (e && e.name) || 'Error'}): ` +
      `${(e && e.message) || String(e)}\n` +
      '            The session continues; run `code-graph-mcp doctor` if this repeats.\n'
    );
    process.exit(0);
  }
}
