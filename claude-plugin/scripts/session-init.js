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
const { UPDATE_STATE_FILE } = require('./cache-paths');
const { readBinaryVersion, isDevMode, getNewestMtime } = require('./version-utils');
const { maybeAutoAdopt, isAdopted, unadopt, unadoptCommand, adoptCommand } = require('./adopt');
const { capContext } = require('./hook-emit');
const { isNonProjectCwd } = require('./project-detect');
const { hidden } = require('./proc-opts');
const { installHookFailOpen, remainingMs } = require('./hook-fail-open');
// Module scope on purpose: `detectHookDark` reads it inside a try/catch that
// treats any throw as "nothing to conclude", so a lazy require in there would
// turn a resolution failure into a silent disable (pre-tag review, JS-08).
const { resolveProjectRoot } = require('./project-root');

// ── SessionStart budget (audit 2026-09-05 NEW-05) ─────────────────────────
//
// Claude Code kills this hook at 5 s (`HOOK_TIMEOUT_SECONDS['session-init.js']`,
// mirrored by hooks.json). Its blocking children were each sized alone and run
// in SERIES: the darwin quarantine probe 3 s, `git log` 2 s, `health-check` 3 s,
// `map --compact` 5 s, `git status` 1 s, `git diff` 1 s, `affected` 1.5 s and
// `readBinaryVersion` 5 s — 21.5 s worst case. Nothing enforced the sum, so a
// binary wedged on `index.lock` got the hook killed and took the SessionStart
// output the user had already earned with it. JS-03 fixed the other six hooks;
// this one was left because clamping it is a per-child decision, not a
// mechanical edit.
//
// Every blocking child now spends `budgetFor(...)`, which returns null when the
// budget is gone — meaning SKIP, never "run unbounded" (node reads `timeout: 0`
// as no timeout at all).
//
// Two skips would otherwise fabricate a POSITIVE result, so they get their own
// answer rather than folding into an existing bucket — the mistake NEW-09 fixed
// in cg-answer, where "budget exhausted" arrived as `no-hits`:
//   * a skipped freshness probe reports 'unknown', NOT 'fresh';
//   * a skipped Gatekeeper probe reports `quarantine-probe-skipped`, NOT a
//     silent "the binary runs".
// The rest degrade to "nothing injected", which is a state they already reach
// for ordinary reasons (no index, clean tree) and which claims nothing untrue.
// `budgetSkipped` in the return value names whichever ones actually skipped, so
// a starved SessionStart is legible to `doctor` and to tests instead of just
// being quieter than usual.
let budgetSkips = [];

// What this run has to say, delivered as ONE JSON envelope when the hook exits
// (sessionStartOutput): `notices` become the user-facing `systemMessage`,
// `contextParts` the model-facing `additionalContext`. Claude Code never shows
// a SessionStart hook's stderr when it exits 0, so the fifteen notices this
// hook used to write there — "CLAUDE.md was modified", "hooks look dark",
// "binary missing" — reached no one (decision D5, 2026-09-28 usage
// evaluation). Only notices a user must act on are kept. Module state, reset
// per run like budgetSkips.
let notices = [];
let contextParts = [];

function sessionStartOutput() {
  if (notices.length === 0 && contextParts.length === 0) return '';
  const out = {};
  if (notices.length > 0) out.systemMessage = notices.join('\n');
  if (contextParts.length > 0) {
    out.hookSpecificOutput = {
      hookEventName: 'SessionStart',
      additionalContext: capContext(contextParts.join('\n')),
    };
  }
  return JSON.stringify(out) + '\n';
}

function budgetFor(label, defaultMs) {
  const ms = remainingMs(defaultMs);
  if (ms === null) budgetSkips.push(label);
  return ms;
}

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
    notices.push(
      `[code-graph] ${settingsPath()} could not be parsed and has been REBUILT. ` +
      `Your original is saved at ${r.settingsRebuiltFrom} — merge anything you ` +
      `still need (model / env / permissions / your own hooks) back by hand.`
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
    notices.push(
      `[code-graph] The plugin manifest could not be written (${r.manifestUnwritable}). ` +
      'Hooks are registered but the install will not be remembered, so this runs again ' +
      'every session. Check permissions on ~/.claude/plugins/, then run ' +
      '`code-graph-mcp doctor`.'
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
  const { surveyHookCoverage, hooksFromPluginManifest } = require('./lifecycle');
  const cov = surveyHookCoverage(settings);
  // Decision D2: a plugin session gets every hook from the plugin's hooks.json,
  // so "missing from settings.json" is the healthy state there, and an entry
  // still in settings.json (written by a pre-0.164 install, or re-added by
  // hand) fires its hook a second time. install() removes ours on this path.
  if (hooksFromPluginManifest(settings)) {
    if (cov.present.length > 0) {
      installReporting();
      return 'removed-settings-hooks';
    }
    return 'noop';
  }
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
 *
 * Returns true | false | null | 'corrupt', where null means the SessionStart
 * budget ran out before the probe could run. `false` says "asked, not stale";
 * conflating the two would let the caller report a freshness it never
 * established. 'corrupt' is health-check's `reason:"corrupt"` verdict.
 */
function indexNeedsRevalidation(bin, cwd) {
  const budget = budgetFor('health-check', 3000);
  if (budget === null) return null;
  try {
    let out;
    try {
      out = execFileSync(bin, ['health-check', '--format', 'json'],
        hidden({ cwd, timeout: budget, stdio: ['pipe', 'pipe', 'pipe'] })).toString();
    } catch (e) {
      // health-check exits non-zero on an unhealthy index but still writes JSON.
      out = ((e && e.stdout) || '').toString();
    }
    const report = JSON.parse(out);
    // A corrupt index answers every hook with nothing, and a reader never
    // rebuilds it (it reports and preserves). Only an indexer does, so the
    // caller must start one — before this it read as "not stale" and every
    // hook stayed dark with no word to the user (hook audit 2026-09-28 P1-7).
    if (report.reason === 'corrupt') return 'corrupt';
    return report.index_version_stale === true;
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
 * Returns 'fresh' | 'refreshing' | 'skipped' | 'unknown', where 'unknown' means
 * the SessionStart budget ran out before either trigger could be evaluated —
 * distinct from 'fresh' (both triggers asked and neither fired) and from
 * 'skipped' (no binary / no index, so there was nothing to ask).
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
  let unprobed = false;
  let corrupt = false;
  // Trigger 1: git HEAD newer than index mtime.
  const gitBudget = budgetFor('git-log', 2000);
  if (gitBudget === null) {
    unprobed = true;
  } else {
    try {
      const dbMtime = fs.statSync(dbPath).mtimeMs;
      const gitTs = parseInt(
        execSync('git log -1 --format=%ct', hidden({ cwd, timeout: gitBudget, encoding: 'utf8', stdio: ['pipe', 'pipe', 'pipe'] })).trim()
      ) * 1000;
      if (gitTs > dbMtime) needsRefresh = true;
    } catch { /* no git / not a repo — fall through to the version probe */ }
  }

  // Trigger 2: INDEX_VERSION mismatch (only probe when mtime looked fresh).
  if (!needsRefresh) {
    const stale = indexNeedsRevalidation(bin, cwd);
    if (stale === 'corrupt') corrupt = needsRefresh = true;
    else if (stale === true) needsRefresh = true;
    else if (stale === null) unprobed = true;
  }

  // No trigger fired. Whether that means "fresh" depends on whether anything
  // was actually asked: with a spent budget this is a claim about an index
  // nothing looked at, and 'fresh' is the one answer that would stop a caller
  // from looking again.
  if (!needsRefresh) return unprobed ? 'unknown' : 'fresh';

  const child = spawn(bin, ['incremental-index', '--quiet'], hidden({
    cwd,
    detached: true,
    stdio: 'ignore',
  }));
  if (child && typeof child.unref === 'function') child.unref();
  if (corrupt) {
    notices.push(
      '[code-graph] The index at .code-graph/index.db was corrupt, so every hook had nothing to say.\n' +
      '            Rebuilding it in the background; hooks resume when it finishes.\n' +
      '            If this repeats: code-graph-mcp rebuild-index --confirm'
    );
    return 'rebuilding-corrupt';
  }
  return 'refreshing';
}

/**
 * What to tell a user whose binary is missing. A missing binary is the NORMAL
 * state of the first session after `/plugin install` — nothing ships the ~40MB
 * engine with the plugin — and both automatic install paths are already
 * running by the time the user reads this: launchBackgroundAutoUpdate() below
 * (a missing binary bypasses the check throttle) and the MCP launcher's own
 * install chain. Telling that user "MCP server cannot start. Install: npm
 * install -g" reads as a failed install and sends them to fix something that
 * is already fixing itself.
 *
 * The manual instruction is still the right answer when the user has opted out
 * of auto-update, because then nothing else will fetch it. Pure so both arms
 * are testable without a binary-less machine.
 */
function missingBinaryMessage(env = process.env) {
  if (env.CODE_GRAPH_NO_AUTO_UPDATE === '1') {
    return '[code-graph] Binary not found, and auto-download is off (CODE_GRAPH_NO_AUTO_UPDATE=1).\n' +
           '            Install it yourself:  npm install -g @sdsrs/code-graph\n';
  }
  return '[code-graph] Binary not found — fetching it in the background (~40MB, first run only).\n' +
         '            Tools appear as soon as it lands; no restart needed.\n' +
         '            Still missing next session? Run `code-graph-mcp doctor`.\n';
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
    notices.push(missingBinaryMessage().trimEnd());
    return { available: false, binary: null };
  }

  // Check executable permission
  try {
    fs.accessSync(binary, fs.constants.X_OK);
  } catch {
    notices.push(
      `[code-graph] Binary not executable: ${binary}\n` +
      `Fix: chmod +x "${binary}"` +
      (process.platform === 'darwin' ? `\nAlso try: xattr -d com.apple.quarantine "${binary}"` : '')
    );
    return { available: false, binary, issue: 'not-executable' };
  }

  // On macOS, verify the binary can actually run (Gatekeeper may block it)
  if (process.platform === 'darwin') {
    const budget = budgetFor('quarantine-probe', 3000);
    if (budget === null) {
      // Out of budget before the probe. The binary exists and is executable, so
      // `available: false` here would be a false alarm that sends the user to
      // `xattr -d` for nothing. But "it actually runs" is precisely what the
      // probe establishes and we did not establish it — so say which half is
      // unverified instead of returning the clean shape.
      return { available: true, binary, issue: 'quarantine-probe-skipped' };
    }
    try {
      execFileSync(binary, ['--version'], hidden({ timeout: budget, stdio: 'pipe' }));
    } catch (err) {
      const msg = (err.message || '') + (err.stderr ? err.stderr.toString() : '');
      if (msg.includes('quarantine') || msg.includes('not permitted') ||
          msg.includes('killed') || err.status === 137 || err.signal === 'SIGKILL') {
        notices.push(
          `[code-graph] macOS Gatekeeper is blocking the binary: ${binary}\n` +
          `Fix: xattr -d com.apple.quarantine "${binary}"\n` +
          'Then restart Claude Code to reconnect the MCP server.'
        );
        return { available: false, binary, issue: 'quarantine' };
      }
      // Other errors (e.g., missing libs) — still report
      notices.push(
        `[code-graph] Binary found but failed to run: ${binary}\n` +
        `Error: ${msg.slice(0, 200)}`
      );
      return { available: false, binary, issue: 'runtime-error' };
    }
  }

  return { available: true, binary };
}

// `unadoptCommand` now lives in adopt.js (issue #41's rationale travels with
// it). It moved because THIS file was not the only printer handing a user that
// command — adopt.js's own `formatResult` prints a `Reverse:` line too, and it
// was still spending the bare name after 0.141.0 fixed the announcement below.
// Two spellings of one remedy is what let that survive; re-exported here so the
// name stays importable from this module.

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
    // The last child of the hook and the most expensive one (5s of its own
    // against a 5s total). Skipping folds into `binaryVersion === null`, which
    // this check already treats as "no comparison to make" — it only ever
    // reports a MISMATCH, so a skip suppresses a warning rather than inventing
    // an all-clear.
    const versionBudget = budgetFor('binary-version', 5000);
    const binaryVersion = versionBudget === null
      ? null
      : readBinaryVersion(binary, { timeoutMs: versionBudget });
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
    const statePath = UPDATE_STATE_FILE;
    const state = readJson(statePath);
    if (state && state.updateAvailable && state.binaryUpdated === false) {
      issues.push({
        id: 'update-incomplete',
        msg: `Plugin updated to v${state.latestVersion}, binary not updated`,
        fix: 'code-graph-mcp doctor',
      });
    }
  } catch { /* skip check on error */ }

  // Returned, not printed: stderr never reaches the user, and a version skew
  // self-heals through the background auto-update (decision D5).
  return issues;
}

/**
 * Do two paths name the same project? The registry stores what `adopt` was
 * given and `process.cwd()` is what the shell resolved, so a symlinked repo
 * path (`/tmp` → `/private/tmp` on macOS) compares unequal as raw strings.
 * realpath both, fall back to the resolved literal when a side no longer exists.
 */
function samePath(a, b) {
  if (!a || !b) return false;
  const real = (p) => { try { return fs.realpathSync(p); } catch { return path.resolve(p); } };
  return real(a) === real(b);
}

/**
 * Whether to show the out-of-date-block notice in this project, recording it
 * as shown. Once per shipped-template fingerprint: 0.164.0 showed it at every
 * session start. Recorded in `.code-graph/` only when that directory already
 * exists — creating it would put an unexcluded directory in `git status`, the
 * side effect D3/D4 removed. Anything unrecordable (no fingerprint, no
 * directory, a failed write) shows the notice: a failure must never silence it.
 */
function staleNoticeDue(cwd, fingerprint) {
  if (!fingerprint) return true;
  const dir = path.join(cwd, '.code-graph');
  const marker = path.join(dir, 'stale-block-notice');
  try {
    if (fs.readFileSync(marker, 'utf8').trim() === fingerprint) return false;
  } catch { /* not recorded yet, or unreadable */ }
  try {
    if (fs.statSync(dir).isDirectory()) fs.writeFileSync(marker, fingerprint + '\n');
  } catch { /* unrecorded: shown again next session */ }
  return true;
}

function runSessionInit({ source } = {}) {
  // Fresh per run: this is module state, and the test suite calls this function
  // many times in one process. A carried-over array would report last run's
  // skips as this one's.
  budgetSkips = [];
  notices = [];
  contextParts = [];
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
    let cleanup = null;
    try { cleanup = cleanupDisabledStatusline(); } catch { /* best-effort teardown */ }
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
      //
      // cleanupDisabledStatusline() now sweeps the WHOLE adopted-projects
      // registry (it is the only teardown that still runs after a real
      // uninstall), so this cwd is usually already clean by the time we get
      // here. Fold that in rather than re-deriving it: with the work moved
      // earlier, the local `unadopt` returns "nothing to clean" and reporting
      // `unadopted:false` for a project whose block IS gone would be a false
      // negative in the one field a caller could act on.
      let unadopted = !!(cleanup && Array.isArray(cleanup.unadopted)
        && cleanup.unadopted.some((u) => u && u.cleaned && samePath(u.project, process.cwd())));
      try {
        if (!unadopted && !isNonProjectCwd(process.cwd())) {
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

  // `doctor` reports a conflicting install; a SessionStart line about it went
  // to stderr, which no one sees (decision D5).
  checkScopeConflict();

  // Verify binary availability — catch issues early with actionable diagnostics
  const binaryCheck = verifyBinary();

  const autoUpdateLaunched = launchBackgroundAutoUpdate(spawn, process.env, { force: isHighIntentSource(source) });
  const indexFreshness = binaryCheck.available ? ensureIndexFresh() : 'skipped';

  // SessionStart no longer writes CLAUDE.md (decision D4); maybeAutoAdopt only
  // cleans this plugin's legacy memory-dir artifacts and reports the project's
  // adoption state. A block that has drifted from the shipped template is the
  // one case worth a notice: its guidance is out of date and only the user can
  // decide to refresh or remove it. Never throws out of this hook (P1-16).
  let autoAdopt = { attempted: false, reason: null };
  if (!isRelic) {
    try {
      autoAdopt = maybeAutoAdopt({ scriptPath: __dirname });
    } catch (e) {
      autoAdopt = { attempted: false, reason: 'threw', error: (e && e.message) || String(e) };
    }
  }
  if (autoAdopt.reason === 'stale' && staleNoticeDue(process.cwd(), autoAdopt.fingerprint)) {
    notices.push(
      "[code-graph] This project's CLAUDE.md carries an out-of-date code-graph block.\n" +
      `            Refresh it: ${adoptCommand()}\n` +
      `            Remove it:  ${unadoptCommand()}\n` +
      '            (Shown once per shipped template. SessionStart no longer edits CLAUDE.md itself;\n' +
      '            CODE_GRAPH_NO_TEMPLATE_REFRESH=1 silences this.)'
    );
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
  const consistencyIssues = binaryCheck.available
    ? consistencyCheck(binaryCheck.binary)
    : [];
  // v0.67.0 — hook reliability (active-project path only): Layer A pushes a
  // FAILED firing self-test result (and refreshes it in the background, off the
  // 5s budget); Layer B is the runtime dispatch-dark canary. Both best-effort.
  const hookFireWarn = checkHookFiring();
  if (hookFireWarn) notices.push(hookFireWarn.trimEnd());
  const hookDarkWarn = detectHookDark();
  if (hookDarkWarn) notices.push(hookDarkWarn.trimEnd());
  return {
    inactive: false, lifecycle,
    autoUpdateLaunched, indexFreshness, mapInjected, binaryCheck, consistencyIssues,
    quietHooks, adopted, autoAdopted: autoAdopt.attempted,
    hookFireWarn: !!hookFireWarn, hookDarkWarn: !!hookDarkWarn,
    // Which children the 5s budget cut, in the order they were reached. Empty
    // on every healthy run; non-empty is the only signal that this SessionStart
    // did less than it looks like it did.
    budgetSkipped: budgetSkips.slice(),
    notices: notices.slice(),
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
    // even when the local index is populated.
    const { findBinary } = require('./find-binary');
    const bin = findBinary();
    if (!bin) return false;

    // Skipping here degrades to "nothing injected", the same state a project
    // with no index reaches — no false claim, just less context. Safe to fold
    // (unlike the freshness/quarantine probes) because this dump is opt-in,
    // off by default, and duplicates MEMORY.md plus the on-demand tool.
    const mapBudget = budgetFor('map', 5000);
    if (mapBudget === null) return false;
    const output = execFileSync(bin, ['map', '--compact'], hidden({
      cwd,
      timeout: mapBudget,
      encoding: 'utf8',
      stdio: ['pipe', 'pipe', 'pipe'],
      // Hook-internal delivery, not a model conversion — keep record_cli_use from
      // logging this `map` run as a phantom `use`.
      env: { ...process.env, CODE_GRAPH_INTERNAL: '1' },
    }));

    if (output && output.trim()) {
      contextParts.push('[code-graph] Project map (indexed):\n' + output.trim());
      return true;
    }
  } catch {
    // Index not ready or binary not found — skip silently
  }
  return false;
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
    // JS-08 (audit 2026-08-29). Every WRITER of this file records into the
    // RESOLVED root — subdir cwd walks up to the project root, a linked worktree
    // reads the main checkout's index — while this reader used a bare
    // `process.cwd()`. So in exactly the sessions the subdir-cwd fix exists for,
    // the dark DETECTOR was itself dark: no file at that path, no claim made,
    // nothing said. `resolveProjectRoot` returns null when nothing on the walk is
    // indexed; cwd remains the fallback for that case (unchanged behaviour).
    //
    // Required at module scope (see top of file), not here: inside this
    // try/catch a module-resolution failure would be indistinguishable from
    // "no recommendations.jsonl", which is the silent-disable shape this whole
    // finding is about. The sibling hooks all require it at top level too.
    const root = resolveProjectRoot(process.cwd()) || process.cwd();
    const recPath = path.join(root, '.code-graph', 'recommendations.jsonl');
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
  verifyBinary, missingBinaryMessage, unadoptCommand,
  consistencyCheck,
  runSessionInit, staleNoticeDue,
  computeQuietHooks,
  shouldInjectMap,
  hookFireWarning, checkHookFiring, analyzeHookDark, detectHookDark, // v0.67.0
};

if (require.main === module) {
  // Arms the 5s deadline every `budgetFor` above spends against, and adds the
  // async-throw coverage the try/catch below cannot reach (a rejected promise
  // from a spawn or a read). Only here, never on `require`: the test suite
  // calls `runSessionInit` in-process, where an armed deadline from a foreign
  // argv[1] would clamp children against a budget nobody granted.
  installHookFailOpen('SessionStart');
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
    const out = sessionStartOutput();
    if (out) process.stdout.write(out);
  } catch (e) {
    process.stderr.write(
      `[code-graph] SessionStart hook error (${(e && e.code) || (e && e.name) || 'Error'}): ` +
      `${(e && e.message) || String(e)}\n` +
      '            The session continues; run `code-graph-mcp doctor` if this repeats.\n'
    );
    process.exit(0);
  }
}
