#!/usr/bin/env node
'use strict';
const fs = require('fs');
const path = require('path');
const os = require('os');
const { claudeHome } = require('./claude-config');
const { hidden } = require('./proc-opts');

const PLUGIN_ID = 'code-graph-mcp@code-graph-mcp';
const OLD_PLUGIN_IDS = [
  'code-graph@sdsrss',           // v1 legacy ID
  'code-graph@sdsrss-code-graph', // v2 legacy ID (pre-rename)
];
const MARKETPLACE_NAME = 'code-graph-mcp';
const CACHE_DIR = path.join(os.homedir(), '.cache', 'code-graph');
// Always derive from __dirname — CLAUDE_PLUGIN_ROOT env var can leak from other
// plugins when hooks run in shared process context (e.g. claude-mem-lite sets it
// to its own marketplace path, polluting all subsequent settings.json hook processes).
const PLUGIN_ROOT = path.resolve(__dirname, '..');
const MANIFEST_FILE = path.join(CACHE_DIR, 'install-manifest.json');
const REGISTRY_FILE = path.join(CACHE_DIR, 'statusline-registry.json');
// Written by the launcher's background install when ITS `npm install -g` step
// introduced the global shell + platform packages. Uninstall only removes
// global packages it can prove the plugin installed (marker present) or when
// the user passes --purge-global — a deliberate user install is never yanked.
const GLOBAL_INSTALL_MARKER = path.join(CACHE_DIR, 'global-install-marker.json');
const INSTALL_LOCK_FILE = path.join(CACHE_DIR, 'install.lock');
const SHELL_PKG = '@sdsrs/code-graph';

// Lazy resolvers — Claude Code's config dir can be overridden by CLAUDE_CONFIG_DIR
// (multi-account isolation). Re-read every call so test subprocesses with a
// different env see the right path.
function settingsPath() { return path.join(claudeHome(), 'settings.json'); }
function installedPluginsPath() { return path.join(claudeHome(), 'plugins', 'installed_plugins.json'); }
// Durable mirror outside ~/.cache/ — survives cache cleanup. Captures the
// `_previous` snapshot (pre-install statusline) and any third-party providers
// (GSD, etc.). readRegistry() self-heals from this file when primary is missing.
function providersBackupFile() { return path.join(claudeHome(), 'statusline-providers.json'); }
function pluginsCacheDir() { return path.join(claudeHome(), 'plugins', 'cache'); }

// --- Helpers ---

// Read JSON while keeping *why* it failed. The distinction that matters to a
// caller about to REBUILD the file is exactly one bit — "may I treat this as a
// fresh install?" — and only a genuine ENOENT earns a yes.
//
//   missing: true   ENOENT only. Nothing is there; rebuilding destroys nothing.
//   corrupt: true   Everything else: the file EXISTS and we could not turn it
//                   into a settings object — unparseable (trailing comma),
//                   unreadable (EACCES after a stray `sudo`, EPERM, EIO), a
//                   directory (EISDIR), or valid JSON that isn't an object
//                   (`null` / `[]` / `123` / `"str"`).
//
// Collapsing ANY of those into "absent" is the bug: `readJson(...) || {}` then
// hands install() an empty object and the next atomic write replaces the user's
// whole settings.json. The first version of this fix split out only the
// unparseable case and left the unreadable one behind — a `chmod 000`
// settings.json was still destroyed, silently, with no backup. `err.code` is the
// whole gate; do not widen it back to a bare `catch`.
// `accept` decides what counts as a USABLE parsed value. It defaults to the
// settings shape (a plain object) but the statusline registry is a top-level
// ARRAY, which the default predicate calls corrupt — so the registry gets
// `accept: Array.isArray` rather than a second, drifting copy of this function.
function isSettingsObject(value) {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}

function readJsonResult(filePath, { accept = isSettingsObject } = {}) {
  // Read BYTES, decode separately. `readFileSync(p, 'utf8')` replaces every
  // invalid byte with U+FFFD, and `raw` is what backupCorruptFile writes to the
  // `.corrupt-*` copy before the original is overwritten — so a settings.json
  // containing any non-UTF-8 byte (a latin-1 path, a stray BOM pair) was
  // "preserved" as a lossy transcription and the true bytes were destroyed. The
  // backup is the user's only copy; it has to be byte-exact.
  let bytes;
  let raw;
  try {
    bytes = fs.readFileSync(filePath);
    raw = bytes.toString('utf8');
  } catch (err) {
    const missing = err && err.code === 'ENOENT';
    return { value: null, missing, corrupt: !missing, error: err };
  }
  // A zero-byte file is what a crash mid-write leaves behind. It carries nothing
  // to preserve, so back-up-then-rebuild would only litter `~/.claude` with an
  // empty `.corrupt-*` copy; treat it as absent instead.
  if (raw.trim() === '') {
    return { value: null, missing: true, corrupt: false, raw: bytes };
  }
  try {
    // Parse the TRIMMED text. A UTF-8 BOM is JS whitespace (so `.trim()` above
    // strips it) but `JSON.parse` rejects it — and a BOM is exactly what
    // PowerShell 5.1's `Out-File` / `Set-Content` write by default. Parsing the
    // untrimmed string classified a perfectly valid settings.json as corrupt and
    // rebuilt the live file from a backup-and-replace path it never needed.
    const value = JSON.parse(raw.trim());
    // `null` / `"str"` / `[]` parse fine but are not a settings object; treating
    // them as "absent" would rebuild over them just the same.
    if (!accept(value)) {
      return { value: null, missing: false, corrupt: true, raw: bytes };
    }
    // VALID JSON can still have been decoded lossily. `toString('utf8')`
    // substitutes U+FFFD for every invalid byte, and a latin-1/cp1252 byte
    // inside a path (a non-ASCII username on a legacy code page, a hand-edited
    // file) parses fine — so the object round-trips through JSON.stringify and
    // the atomic write replaces those bytes with U+FFFD PERMANENTLY. The
    // byte-exactness work above only covered the corrupt branch; this branch
    // rewrote the file with no backup and no message at all.
    //
    // Re-encoding the decoded text and comparing to the original bytes detects
    // exactly that: a lossless decode round-trips, a lossy one cannot. Reported
    // as `lossy` rather than `corrupt` because the VALUE is usable — the caller
    // preserves the true bytes first, then proceeds with it.
    if (!Buffer.from(raw, 'utf8').equals(bytes)) {
      return { value, missing: false, corrupt: false, lossy: true, raw: bytes };
    }
    return { value, missing: false, corrupt: false, raw: bytes };
  } catch (err) {
    return { value: null, missing: false, corrupt: true, raw: bytes, error: err };
  }
}

// Lenient reader kept exactly as-is for its 20+ callers (manifests, registries,
// plugin.json …) where "absent" and "unreadable" genuinely mean the same thing.
// A caller that will WRITE settings.json back must use readSettingsForWrite()
// + tryWriteSettings() instead — the pair that detects the lossy/corrupt cases
// and preserves the original bytes. Reading settings.json with THIS function is
// fine as long as nothing is written back (isPluginInactive, syncLifecycleConfig's
// self-heal probe); the destructive combination is `readJson(settingsPath())`
// followed by a write, which is how cleanupDisabledStatusline and uninstall
// destroyed non-UTF-8 bytes for four releases after the detector landed.
function readJson(filePath) {
  try { return JSON.parse(fs.readFileSync(filePath, 'utf8')); } catch { return null; }
}

// Preserve a file we are about to overwrite but could not turn into settings.
// `raw` is its contents when we managed to read them; when we did not (EACCES,
// EISDIR) it is undefined and we fall back to a filesystem-level copy — which
// will usually fail for the same reason the read did, and that failure is the
// point: it makes the caller refuse rather than overwrite. Returns the backup
// path, or null when no copy could be made.
/** How many `.corrupt-*` copies of one file to keep. */
const MAX_CORRUPT_BACKUPS = 5;

/**
 * Owner-only. What every file this module creates under `~/.claude` gets when
 * there is no prior file whose bits to preserve, and what a `.corrupt-*` copy
 * gets unconditionally: settings.json routinely carries an `env` block with API
 * keys, and a backup of one is the same secret in a second file (audit
 * 2026-09-05 JS-01).
 */
const SECRET_FILE_MODE = 0o600;

/**
 * Pin `target` at exactly `mode`, because neither route that creates our files
 * does it on its own: `writeFileSync`'s `mode` option goes to `open(O_CREAT)`
 * and is masked by umask (and ignored outright when the file already exists),
 * and `copyFileSync` carries the SOURCE's bits.
 *
 * Reported, not swallowed: the whole point of the call is that the file may
 * hold a key, so a permission we could not set is exactly the thing the user
 * needs to hear about. The write itself has already succeeded — a failure here
 * is a disclosure, not a reason to unwind it.
 */
function restrictMode(target, mode) {
  try {
    fs.chmodSync(target, mode);
  } catch (err) {
    // No return value: both call sites act on the stderr line, not on a bool,
    // and a discarded success flag is an invitation to branch on it later
    // without noticing that nothing ever did (pre-ship review 2026-09-05).
    console.error(
      `[code-graph] wrote ${target} but could not set its permissions to ` +
      `0${mode.toString(8)} (${err.code || err.name}). If it contains an API key, ` +
      `it may be readable by other users on this machine.`
    );
  }
}

/**
 * Delete all but the newest `MAX_CORRUPT_BACKUPS` copies of `filePath`.
 *
 * The copies are a safety net, and a safety net nobody ever empties is a leak:
 * each one is a full settings.json, they are created on a path that can repeat,
 * and nothing else deletes them (audit 2026-08-29 JS-09). Newest survive because
 * the reason to keep any is "undo what just happened".
 *
 * Deliberately narrow: same directory, exactly `<basename>.corrupt-` prefixed,
 * regular files only. It runs inside `~/.claude`, so the matcher is a prefix on
 * a name this function itself produced, never a glob.
 */
function pruneCorruptBackups(filePath, keep = MAX_CORRUPT_BACKUPS) {
  const dir = path.dirname(filePath);
  const prefix = `${path.basename(filePath)}.corrupt-`;
  let pruned = 0;
  try {
    const mine = fs.readdirSync(dir, { withFileTypes: true })
      .filter((e) => e.isFile() && e.name.startsWith(prefix))
      .map((e) => {
        const full = path.join(dir, e.name);
        // Sort by the timestamp we wrote into the NAME, not by mtime: a restore
        // or a copy re-stamps mtime and would reorder the history.
        return { full, name: e.name };
      })
      .sort((a, b) => b.name.localeCompare(a.name));
    for (const stale of mine.slice(keep)) {
      try { fs.unlinkSync(stale.full); pruned++; } catch { /* raced or read-only */ }
    }
  } catch { /* unreadable dir — the copy still succeeded, which is what matters */ }
  return pruned;
}

function backupCorruptFile(filePath, raw) {
  const stamp = new Date().toISOString().replace(/[:.]/g, '-');
  const dest = `${filePath}.corrupt-${stamp}`;
  try {
    // Buffer, not string: see readJsonResult. A string here would re-encode.
    if (Buffer.isBuffer(raw)) fs.writeFileSync(dest, raw, { mode: SECRET_FILE_MODE });
    else if (typeof raw === 'string') {
      fs.writeFileSync(dest, Buffer.from(raw, 'utf8'), { mode: SECRET_FILE_MODE });
    } else fs.copyFileSync(filePath, dest);
    // Owner-only regardless of what the original was: up to MAX_CORRUPT_BACKUPS
    // of these accumulate beside settings.json with its `env` block copied
    // verbatim, so a 0644 original must not mint 0644 duplicates.
    restrictMode(dest, SECRET_FILE_MODE);
    pruneCorruptBackups(filePath);
    return dest;
  } catch {
    return null;
  }
}

// Read ~/.claude/settings.json for a caller that will WRITE it back.
//
// A settings.json that exists but yields no object used to be indistinguishable
// from an absent one, so `readJson(settingsPath()) || {}` handed
// install()/update() an empty object and the next atomic write replaced the
// user's model / env / permissions / enabledPlugins / own hooks with a two-key
// file — silently, with no copy left. Such a file is now copied aside first; if
// even the copy fails we refuse to touch the original and return null, and the
// caller skips its settings work entirely.
// Returns `{ settings, backedUpTo }`:
//   settings   — the object to write back, or null when we refused to touch the file
//   backedUpTo — path of the preserved original when we are about to REBUILD over
//                a file that had content, else null
//
// `backedUpTo` is not decoration. Rebuilding from `{}` REPLACES the user's whole
// settings.json, and callers report their outcome to a human: `doctor` was
// printing "Hooks ✅ 1 issue(s) auto-repaired" for a run that moved the user's
// model / env / permissions into a `.corrupt-*` file it never mentioned. A
// destructive repair has to be reported as one, with the path to get it back.
//
// `pre` lets a caller that ALREADY probed the file with readJsonResult reuse
// that result instead of reading twice. It matters for more than speed: the
// backup this function may take is a side effect, and a caller which only
// writes conditionally (cleanupDisabledStatusline runs on every statusline
// render) must be able to probe first and pay the backup only on the write
// path — otherwise a lossy settings.json accumulates one `.corrupt-*` copy per
// prompt. Passing `pre` also keeps the returned `settings` object IDENTICAL to
// the one the caller already mutated.
function readSettingsForWrite(pre) {
  const p = settingsPath();
  const res = pre || readJsonResult(p);
  if (res.value && res.lossy) {
    // Usable JSON whose bytes will not survive our rewrite (see readJsonResult).
    // Preserve the true bytes, then proceed with the parsed value — refusing
    // outright would strand the plugin over a byte we can still work around.
    // If even the copy fails, refuse: silently destroying bytes is worse than
    // skipping the settings work.
    const backup = backupCorruptFile(p, res.raw);
    if (!backup) {
      console.error(
        `[code-graph] ${p} contains bytes that are not valid UTF-8, and no backup copy ` +
        `could be made. Leaving it untouched and skipping settings changes — rewriting it ` +
        `would replace those bytes permanently.`
      );
      return { settings: null, backedUpTo: null };
    }
    console.error(
      `[code-graph] ${p} contains bytes that are not valid UTF-8; rewriting it will replace ` +
      `them. Saved the original to ${backup} first.`
    );
    return { settings: res.value, backedUpTo: backup };
  }
  if (res.value) return { settings: res.value, backedUpTo: null };
  if (res.missing) return { settings: {}, backedUpTo: null };
  const why = res.error ? res.error.message : 'it does not contain a JSON object';
  const backup = backupCorruptFile(p, res.raw);
  if (!backup) {
    console.error(
      `[code-graph] cannot use ${p} (${why}), and no backup copy could be made. ` +
      `Leaving it untouched and skipping settings changes — repair the file ` +
      `(or move it aside) and re-run.`
    );
    return { settings: null, backedUpTo: null };
  }
  console.error(
    `[code-graph] cannot use ${p} (${why}). ` +
    `Saved the original to ${backup} before rebuilding it.`
  );
  return { settings: {}, backedUpTo: backup };
}

// Replace-by-rename loses the permission bits unless they are carried over
// deliberately: the new inode is the TMP file's, created under our umask, so a
// settings.json the user had put at 0600 came back 0644 on the first
// install()/update()/cleanupDisabledStatusline() — with its `env` API keys in
// it, silently, on a file we were only meant to add two hook entries to (audit
// 2026-09-05 JS-01). Stat the original and restore its exact mode; when there
// is no original, create owner-only rather than at whatever umask says.
function writeJsonAtomic(filePath, data) {
  const dir = path.dirname(filePath);
  fs.mkdirSync(dir, { recursive: true });
  let mode = null;
  try { mode = fs.statSync(filePath).mode & 0o777; } catch { /* new file */ }
  const tmp = filePath + '.tmp.' + process.pid;
  fs.writeFileSync(tmp, JSON.stringify(data, null, 2) + '\n', { mode: mode ?? SECRET_FILE_MODE });
  // umask can only have cleared bits the original had, so chmod is what makes
  // this preservation rather than tightening.
  restrictMode(tmp, mode ?? SECRET_FILE_MODE);
  fs.renameSync(tmp, filePath);
}

// A settings.json we can READ but not WRITE (read-only ~/.claude, EROFS, a
// container mount) used to escape as a raw fs stack trace with no `[code-graph]`
// line — and because nothing of ours got registered, the follow-up `health` then
// reported "OK — all paths valid", since it only checks that the paths it FINDS
// are valid and it found none. Same class as the unreadable-file arm: report the
// real cause, change nothing, and make the caller's exit code non-zero.
function tryWriteSettings(settings) {
  try {
    writeJsonAtomic(settingsPath(), settings);
    return null;
  } catch (err) {
    console.error(
      `[code-graph] cannot write ${settingsPath()} (${err.code || err.name}: ${err.message}). ` +
      `Nothing was changed. The plugin stays inactive until the file is writable.`
    );
    return err;
  }
}

function readManifest() {
  return readJson(MANIFEST_FILE) || { version: null, config: {} };
}

// Same tolerant shape as tryWriteSettings, and for the same reason: this is the
// one write in install()/update() that could still throw. `~/.cache` on a
// read-only mount, a root-owned cache dir left by a `sudo` run, or a full disk
// turned a SessionStart into a raw ENOSPC/EACCES stack trace out of a hook whose
// settings work had already SUCCEEDED (audit 2026-08-16 P1-16). Report it,
// change nothing else, and let the caller decide.
// @returns {Error|null} the write error, or null on success
function writeManifest(manifest) {
  try {
    fs.mkdirSync(CACHE_DIR, { recursive: true });
    writeJsonAtomic(MANIFEST_FILE, manifest);
    return null;
  } catch (err) {
    console.error(
      `[code-graph] cannot write ${MANIFEST_FILE} (${err.code || err.name}: ${err.message}). ` +
      'Settings changes (if any) still applied; the next run will redo the version stamp.'
    );
    return err;
  }
}

function getPluginVersion() {
  const pj = readJson(path.join(PLUGIN_ROOT, '.claude-plugin', 'plugin.json'));
  return pj ? pj.version : '0.0.0';
}

function compositeCommand() {
  return `node ${JSON.stringify(path.join(PLUGIN_ROOT, 'scripts', 'statusline-composite.js'))}`;
}

function codeGraphStatuslineCommand() {
  return `node ${JSON.stringify(path.join(PLUGIN_ROOT, 'scripts', 'statusline.js'))}`;
}

function hasOwn(obj, key) {
  return !!obj && Object.prototype.hasOwnProperty.call(obj, key);
}

function hasInstalledPluginRecord() {
  const installed = readJson(installedPluginsPath());
  return !!(installed && installed.plugins && Array.isArray(installed.plugins[PLUGIN_ID]) && installed.plugins[PLUGIN_ID].length > 0);
}

/** The installPath Claude Code currently considers active (installed_plugins.json), or null. */
function activeInstallPath() {
  const installed = readJson(installedPluginsPath());
  const recs = installed && installed.plugins && installed.plugins[PLUGIN_ID];
  if (!Array.isArray(recs) || !recs[0] || typeof recs[0].installPath !== 'string') return null;
  return recs[0].installPath;
}

/**
 * Stale-relic detection: is THIS script running from an old plugin-cache
 * version dir while installed_plugins.json points at a different (active)
 * install that exists on disk?
 *
 * Why: a still-running Claude Code process keeps firing SessionStart from the
 * install path it loaded at startup. After auto-update installs vN+1 and
 * re-anchors manifest + settings.json, the next SessionStart in that old
 * process runs the vN scripts, whose syncLifecycleConfig sees
 * `manifest.version !== currentVersion` and — direction-blind — calls
 * update(), dragging manifest and every settings.json hook path back to the
 * vN dir. The two versions then ping-pong (observed live 2026-06-12: manifest
 * 0.49.0 → rewritten 0.48.0 fifteen minutes after a successful update).
 *
 * The authority is installed_plugins.json, not version direction: a deliberate
 * downgrade via /plugin lands installPath == the old dir, so the old scripts
 * keep full self-heal rights. Dev checkouts and npm installs are exempt
 * (pluginRoot not under the plugins cache).
 */
function isStaleRelicContext({
  pluginRoot = PLUGIN_ROOT,
  cacheRoot = pluginsCacheDir(),
  activePath = activeInstallPath(),
  existsSync = fs.existsSync,
} = {}) {
  if (!activePath) return false;
  const root = path.resolve(pluginRoot);
  const cache = path.resolve(cacheRoot);
  if (root !== cache && !root.startsWith(cache + path.sep)) return false;
  const active = path.resolve(activePath);
  if (active === root) return false;
  return existsSync(path.join(active, 'scripts', 'lifecycle.js'));
}

function isOurComposite(settings) {
  return settings.statusLine &&
    settings.statusLine.command &&
    settings.statusLine.command.includes('statusline-composite');
}

// --- StatusLine Registry ---
// Multiple providers can register. The composite script runs them all.

// Read the registry for a caller that may WRITE it back.
//
// The registry is USER DATA: `_previous` is the statusline they had before we
// installed (the only record of it), and third-party providers registered
// through us live beside it. The lenient reader collapsed "exists but
// unreadable/corrupt" into the same `[]` as "absent", and the very next
// writeRegistry() then persisted that empty list over the primary AND the
// durable backup — one `chmod 000` (a stray sudo, a restrictive umask) and the
// user's original statusline was unrecoverable, silently, from a call that
// reported success (audit 2026-08-16 P1-12). Exactly the settings.json bug
// readJsonResult was written for, on the file two functions below it.
//
// Returns `{ registry, refuse }`:
//   registry — entries to work with (possibly empty)
//   refuse   — a copy EXISTS and could not be read: write nothing, change nothing
function readRegistryForWrite() {
  const asArray = { accept: Array.isArray };
  const primary = readJsonResult(REGISTRY_FILE, asArray);
  if (primary.value && primary.value.length > 0) return { registry: primary.value, refuse: false };
  if (primary.corrupt) {
    // Fall through to the backup for READING (so callers still see the user's
    // providers) but never write while the primary is unusable: an atomic
    // rename replaces an unreadable file just fine, which is precisely how the
    // data was lost.
    const backup = readJsonResult(providersBackupFile(), asArray);
    return {
      registry: backup.value && backup.value.length > 0 ? backup.value : [],
      refuse: true,
      why: `${REGISTRY_FILE} exists but cannot be read as a provider list`,
    };
  }
  // Self-heal: primary missing or empty (e.g. user cleaned ~/.cache/code-graph/).
  // Durable backup in ~/.claude/ retains `_previous` + third-party providers.
  //
  // Our OWN entry is dropped unless it names the composite this install would
  // register right now. The backup lives in `~/.claude/`, which survives the
  // plugin cache — including an uninstall that refused to rewrite the registry
  // (`detachStatuslineIntegration`'s oneShot refusal leaves it in place by
  // design, because rewriting is how the data got lost the first time). So the
  // NEXT install self-healed the previous install's `code-graph` entry back to
  // life, pointing at a versioned cache directory that no longer exists — a
  // zombie provider in the composite chain (2026-08-16 audit §四). `_previous`
  // and third-party entries are kept: those are the user's data and the reason
  // this backup exists, and nothing else would restore them.
  const backup = readJsonResult(providersBackupFile(), asArray);
  if (backup.value && backup.value.length > 0) {
    // `codeGraphStatuslineCommand()`, NOT `compositeCommand()`. The registry row
    // for `code-graph` is written with the former (see the two
    // `registerStatuslineProvider('code-graph', …)` call sites); the composite is
    // only ever the value of `settings.statusLine.command`. Comparing against the
    // composite made this filter drop the row unconditionally — the CURRENT
    // install's own segment vanished after a cache wipe, which is worse than the
    // stale-entry resurrection the filter exists to prevent (found by the
    // v0.118.0 pre-tag review; CI could not see it).
    const live = codeGraphStatuslineCommand();
    const healed = backup.value.filter(p => p && (p.id !== 'code-graph' || p.command === live));
    if (healed.length > 0) {
      try { writeJsonAtomic(REGISTRY_FILE, healed); } catch { /* ok */ }
      return { registry: healed, refuse: false };
    }
  }
  if (backup.corrupt) {
    return { registry: [], refuse: true, why: `${providersBackupFile()} exists but cannot be read as a provider list` };
  }
  return { registry: [], refuse: false };
}

function readRegistry() {
  return readRegistryForWrite().registry;
}

// One place to say why a registry mutation did nothing. Stderr, not silence:
// the caller returns `false`, which is indistinguishable from "already
// registered" to everything upstream.
function warnRegistryUnusable(action, why) {
  console.error(
    `[code-graph] ${why}. Skipping the statusline ${action} — rewriting it would ` +
    'destroy your previous statusline and any third-party provider entries. ' +
    'Repair or move the file aside and re-run.'
  );
}

function writeRegistry(registry) {
  if (!registry || registry.length === 0) {
    try { fs.unlinkSync(REGISTRY_FILE); } catch { /* ok */ }
    try { fs.unlinkSync(providersBackupFile()); } catch { /* ok */ }
    return;
  }
  writeJsonAtomic(REGISTRY_FILE, registry);
  // Mirror to durable location so cache cleanup doesn't strand `_previous`
  // or third-party provider entries.
  try { writeJsonAtomic(providersBackupFile(), registry); } catch { /* ok */ }
}

function registerStatuslineProvider(id, command, needsStdin) {
  const { registry, refuse, why } = readRegistryForWrite();
  if (refuse) {
    warnRegistryUnusable('registration', why);
    return false;
  }
  const idx = registry.findIndex(p => p.id === id);
  const entry = { id, command, needsStdin: !!needsStdin };
  if (idx >= 0) {
    // Update existing entry only if command changed
    if (registry[idx].command === command) return false;
    registry[idx] = entry;
  } else {
    registry.push(entry);
  }
  writeRegistry(registry);
  return true;
}

function unregisterStatuslineProvider(id) {
  const { registry, refuse, why } = readRegistryForWrite();
  if (refuse) {
    warnRegistryUnusable('removal', why);
    return false;
  }
  const filtered = registry.filter(p => p.id !== id);
  if (filtered.length === registry.length) return false;
  writeRegistry(filtered);
  return true;
}

function isPluginExplicitlyDisabled(settings = readJson(settingsPath()) || {}) {
  return hasOwn(settings.enabledPlugins, PLUGIN_ID) && settings.enabledPlugins[PLUGIN_ID] === false;
}

function isPluginInactive(settings = readJson(settingsPath()) || {}) {
  if (isPluginExplicitlyDisabled(settings)) return true;

  const hasComposite = isOurComposite(settings);
  const hasCodeGraphRegistry = readRegistry().some((provider) => provider.id === 'code-graph');
  if (!hasComposite && !hasCodeGraphRegistry) return false;

  const installed = readJson(installedPluginsPath());
  if (!installed || !installed.plugins) return false;
  return !hasInstalledPluginRecord();
}

function detachStatuslineIntegration(settings, { compositeDoomed = true, oneShot = false } = {}) {
  let settingsChanged = false;

  // An unusable (not merely absent) registry means we may not WRITE it, and it
  // may leave us unable to tell whether a `_previous` or third-party entry
  // exists — which every branch below that rewrites `settings.statusLine`
  // depends on (batch review of audit 2026-08-16 P1-12: the register path
  // refused correctly while this detach path still destroyed the slot).
  //
  // Whether refusing is safe depends on the CALLER, so it is a parameter:
  //   retryable (statusline render) — leave everything alone; the next frame
  //     retries once the file is usable. Touching the slot on a bad read is
  //     how the user's statusline got destroyed in the first place.
  //   oneShot (uninstall) — the composite script dies with the plugin cache in
  //     this same run, so leaving the slot pointing at it is PERMANENT
  //     breakage with no plugin code left to repair it (pre-tag review). We
  //     still must not write the registry, but the entries we already READ are
  //     enough to choose the slot: `readRegistryForWrite` reads through to the
  //     durable backup even while refusing, so a corrupt primary alone does
  //     not lose `_previous`. When even that is unreadable the list is empty
  //     and we clear the slot — Claude Code's default beats a dead path.
  const { registry, refuse, why } = readRegistryForWrite();
  if (refuse && !oneShot) {
    warnRegistryUnusable('detach', why);
    return false;
  }
  if (refuse) {
    warnRegistryUnusable('registry rewrite (the settings slot is still neutralized — uninstall cannot retry)', why);
  } else {
    unregisterStatuslineProvider('code-graph');
  }
  const previous = registry.find(p => p.id === '_previous' && p.command);
  // Third-party providers registered through our registry (e.g. gsd). They
  // must not be silently orphaned: with the composite gone from settings
  // their segments stop rendering while the registry entries dangle.
  const thirdParty = registry.filter(p => p.id !== '_previous' && p.id !== 'code-graph' && p.command);

  // If our composite is still configured while the plugin is disabled/uninstalled,
  // stop affecting Claude Code — but keep surviving third parties rendering.
  if (isOurComposite(settings)) {
    if (thirdParty.length > 0 && !compositeDoomed) {
      // Temporary disable: the composite script survives on disk and keeps
      // rendering the remaining providers — only our segment was unregistered.
    } else if (thirdParty.length > 0) {
      // Genuine uninstall: our composite runner dies with the plugin cache.
      // Hand the slot to the first surviving third-party provider; the rest
      // stay listed in the registry backup for manual re-wiring.
      settings.statusLine = { type: 'command', command: thirdParty[0].command };
      settingsChanged = true;
    } else if (previous) {
      settings.statusLine = { type: 'command', command: previous.command };
      settingsChanged = true;
    } else {
      delete settings.statusLine;
      settingsChanged = true;
    }
  }

  // _previous only becomes removable once no third party still relies on the
  // registry file (writeRegistry unlinks primary+backup when emptied). Skipped
  // entirely while refusing: that path may not write the registry at all.
  if (!refuse && thirdParty.length === 0) unregisterStatuslineProvider('_previous');
  return settingsChanged;
}

function cleanupDisabledStatusline() {
  // Probe without side effects first — this runs on every statusline render and
  // usually finds nothing to do. A corrupt/unreadable file yields no value and
  // is left strictly alone (same outcome the old lenient readJson produced).
  const probe = readJsonResult(settingsPath());
  const settings = probe.value;
  if (!settings || !isPluginInactive(settings)) {
    return { cleaned: false, settingsChanged: false };
  }

  // Decide BEFORE mutating: isPluginUninstalled reads the same composite/
  // registry markers detachStatuslineIntegration is about to remove.
  const uninstalled = isPluginUninstalled(settings);

  let settingsChanged = detachStatuslineIntegration(settings, { compositeDoomed: uninstalled });
  if (removeHooksFromSettings(settings)) settingsChanged = true;
  if (settingsChanged) {
    // Now that a write is certain, take the guarded path: readSettingsForWrite
    // preserves bytes JSON.stringify would destroy (the lossy-UTF8 case) and
    // returns null when it could not, and tryWriteSettings turns a read-only
    // ~/.claude into a diagnosed no-op instead of an uncaught EACCES — this
    // function is called at the top of statusline.js, where a throw blanks the
    // user's status line. `guarded` is the same object we just mutated.
    const { settings: guarded } = readSettingsForWrite(probe);
    if (!guarded) return { cleaned: false, settingsChanged: false };
    if (tryWriteSettings(guarded)) settingsChanged = false;
  }

  // Genuine uninstall (not a temporary disable): reclaim ~/.cache/code-graph
  // too. This statusline-render path is the ONLY plugin code guaranteed to
  // still run after `/plugin uninstall` — Claude Code stops loading the
  // plugin's hooks.json, so the SessionStart teardown in session-init.js never
  // fires post-uninstall. Without this, the ~40MB cached binary leaked forever.
  //
  // Adoption comes off in the SAME breath, and BEFORE the wipe: install's
  // auto-adopt writes a managed block into every project's CLAUDE.md, and until
  // now nothing reachable removed it. session-init.js owned that step, on the
  // branch its own comment concedes "usually never runs again" after a real
  // uninstall — so the block (steering Claude at a CLI that is being deleted two
  // lines below) survived forever in every adopted repo. The registry that names
  // those projects lives inside CACHE_DIR, which is exactly why this must run
  // first: same capture-before-cleanup ordering the rest of this teardown
  // already learned. This branch fires at most once — the write above removes
  // our composite from settings.json, so Claude Code stops invoking us.
  let cacheRemoved = false;
  let unadopted = [];
  let registryUnusable = false;
  if (uninstalled) {
    ({ unadopted, registryUnusable } = unadoptRegisteredProjects());
    cacheRemoved = removeCacheResidue();
  }

  return { cleaned: true, settingsChanged, cacheRemoved, unadopted, registryUnusable };
}

/**
 * Strip the managed CLAUDE.md block + generated `.claude/plugin_code_graph_mcp.md`
 * from every project in the adopted-projects registry. `unadopt` is
 * sentinel-guarded, so user prose outside the managed block is preserved and a
 * project that was already cleaned is a no-op.
 *
 * Fully swallowed per project AND overall: this runs inside a statusline render
 * (statusline.js / statusline-composite.js call the caller at their top), where
 * an uncaught throw blanks the user's status line — and an unreadable project
 * must not cost the remaining ones their cleanup.
 */
function unadoptRegisteredProjects() {
  const out = [];
  let readAdoptedResult, unadopt;
  try { ({ readAdoptedResult, unadopt } = require('./adopt')); }
  catch { return { unadopted: out, registryUnusable: false }; } // POSIX-only helper unavailable — teardown continues
  // readAdoptedResult, NOT the lenient readAdoptedProjects(): that wrapper
  // collapses "unreadable / truncated / wrong shape" into `[]`, which here is
  // indistinguishable from "no projects to clean" — so a corrupt registry would
  // silently sweep nothing AND still be deleted by removeCacheResidue() below,
  // taking the only record of which repos carry a managed block with it. adopt.js
  // documents the same bit for the same reason: only a genuinely ABSENT file may
  // be read as empty.
  let res;
  try { res = readAdoptedResult(); } catch { return { unadopted: out, registryUnusable: true }; }
  if (res && res.unusable) return { unadopted: out, registryUnusable: true };
  for (const project of (res && res.list) || []) {
    try {
      const r = unadopt({ cwd: project });
      // Three outcomes, not two (audit 2026-08-29 JS-06). A project whose block
      // the user already removed by hand comes back with nothing pruned and NO
      // error — which used to land in the "Could NOT clean" list, sending them
      // to hand-edit a file that is already clean. `unadopt` reports real
      // failure separately (`claudeMdUnreadable` / `claudeMdUnwritable`, the
      // same pair adopt.js folds into its own `cleanupFailed`), so use it rather
      // than inferring failure from "nothing happened".
      const cleaned = !!(r && (r.blockPruned || r.fileRemoved || r.claudeMdRemoved));
      const failed = !!(r && (r.claudeMdUnreadable || r.claudeMdUnwritable));
      out.push({ project, cleaned, failed });
    } catch (e) {
      out.push({ project, cleaned: false, failed: true, error: (e && e.message) || String(e) });
    }
  }
  reportUnadoptSweep(out);
  return { unadopted: out, registryUnusable: false };
}

/**
 * Tell the user their repos were just edited. Everything else about this
 * teardown is invisible by construction: it runs inside a statusline render,
 * both callers `process.exit(0)` on `cleaned` and discard the return value, and
 * Claude Code fires no uninstall hook — so without this line the first signal is
 * unexplained `CLAUDE.md` diffs in `git status` across several repositories.
 * stderr, because a statusline's stdout IS the status line.
 */
function reportUnadoptSweep(entries) {
  try {
    const cleaned = entries.filter((e) => e && e.cleaned).map((e) => e.project);
    // Only genuine failures. "Nothing to clean" is neither a success worth
    // announcing nor a problem worth sending someone to fix (JS-06).
    const failed = entries.filter((e) => e && !e.cleaned && e.failed).map((e) => e.project);
    if (!cleaned.length && !failed.length) return;
    const lines = [];
    if (cleaned.length) {
      lines.push(`[code-graph] Plugin uninstalled — removed the managed CLAUDE.md block from ${cleaned.length} project(s):`);
      for (const p of cleaned.slice(0, 10)) lines.push(`             ${p}`);
      if (cleaned.length > 10) lines.push(`             …and ${cleaned.length - 10} more`);
      lines.push('             Your own text outside the block was kept; .code-graph/ index dirs are untouched.');
    }
    if (failed.length) {
      lines.push(`[code-graph] Could NOT clean ${failed.length} project(s) — remove the block by hand or run \`code-graph-mcp unadopt\` there:`);
      for (const p of failed.slice(0, 10)) lines.push(`             ${p}`);
    }
    process.stderr.write(lines.join('\n') + '\n');
  } catch { /* a notice must never be able to fail a teardown */ }
}

// --- Scope Conflict Detection ---

function checkScopeConflict() {
  const installed = readJson(installedPluginsPath());
  if (!installed || !installed.plugins) return null;
  for (const [key, entries] of Object.entries(installed.plugins)) {
    if (key === PLUGIN_ID) continue;
    // Detect any old code-graph plugin IDs still installed
    if (key.startsWith('code-graph@') || key.startsWith('code-graph-mcp@')) {
      return { existingId: key, scope: entries[0] && entries[0].scope, entries };
    }
  }
  return null;
}

// --- Migration: clean up old plugin ID remnants ---

function migrateOldPluginIds(settings) {
  let changed = false;

  for (const oldId of OLD_PLUGIN_IDS) {
    // Clean old ID from enabledPlugins
    if (settings.enabledPlugins && oldId in settings.enabledPlugins) {
      delete settings.enabledPlugins[oldId];
      changed = true;
    }
  }

  // Clean old IDs from installed_plugins.json — Claude Code's OWN file.
  //
  // Read once and write once, through the same three-way read the other
  // read-modify-write sites in this file use. The lenient `readJson` was the
  // last caller left here, and it cannot tell "no such file" (nothing to do)
  // from "unreadable" (say so): both came back null and were skipped in
  // silence. The write was also the only unguarded one in install()/update() —
  // with `~/.claude` unwritable (EACCES) and a legacy ID still present, it threw
  // a bare stack out of both of doctor's repair arms, which is a repair tool
  // crashing on the state it exists to repair (audit 2026-08-22 P2-10).
  const installedRead = readJsonResult(installedPluginsPath());
  if (installedRead.corrupt || installedRead.lossy) {
    console.error(
      `[code-graph] cannot read ${installedPluginsPath()} — leaving legacy plugin ` +
      'IDs in place. Claude Code may still list an old code-graph entry.'
    );
  } else {
    const installed = installedRead.value;
    let ipChanged = false;
    if (installed && installed.plugins) {
      for (const oldId of OLD_PLUGIN_IDS) {
        if (oldId in installed.plugins) {
          delete installed.plugins[oldId];
          ipChanged = true;
        }
      }
    }
    if (ipChanged) {
      try {
        writeJsonAtomic(installedPluginsPath(), installed);
      } catch (err) {
        console.error(
          `[code-graph] cannot write ${installedPluginsPath()} (${err.code || err.name}) — ` +
          'a legacy code-graph entry remains. Remove it with `/plugin uninstall`.'
        );
      }
    }
  }

  // Clean old marketplace names from extraKnownMarketplaces
  if (settings.extraKnownMarketplaces) {
    for (const oldName of ['sdsrss-code-graph']) {
      if (oldName in settings.extraKnownMarketplaces) {
        delete settings.extraKnownMarketplaces[oldName];
        changed = true;
      }
    }
  }

  // Clean old cache paths
  const cacheRoot = pluginsCacheDir();
  const oldCacheDirs = [
    path.join(cacheRoot, 'sdsrss', 'code-graph'),
    path.join(cacheRoot, 'sdsrss-code-graph', 'code-graph'),
    path.join(cacheRoot, 'sdsrss-code-graph'),
  ];
  for (const dir of oldCacheDirs) {
    try { fs.rmSync(dir, { recursive: true, force: true }); } catch { /* ok */ }
  }

  return changed;
}

// --- Hook identity ---
//
// v0.32.0 ARCHITECTURE CORRECTION (see project_hooks_settings.md / feedback_pretooluse_dark_under_green_health.md):
//
// Empirical finding 2026-05-24: current Claude Code only loads SessionStart
// hooks from cache/<mp>/<plugin>/<ver>/hooks/hooks.json. PreToolUse, PostToolUse,
// UserPromptSubmit, Stop, SessionEnd entries in plugin-cache hooks.json are
// SILENTLY IGNORED. Only ~/.claude/settings.json entries reach CC for those events.
//
// Therefore lifecycle.js now ACTIVELY WRITES non-SessionStart hook entries to
// settings.json (with description markers for cleanup), and the shipped
// claude-plugin/hooks/hooks.json carries only SessionStart. SessionStart entries
// in claude-plugin/hooks/hooks.json continue to be CC-loaded as before.
//
// Pattern mirrors claude-mem-lite's install.mjs (cache hooks.json cleared
// to prevent duplicate registration).

const OUR_HOOK_SCRIPTS = [
  'session-init.js',
  'incremental-index.js',
  'user-prompt-context.js',
  'pre-edit-guide.js',
  'pre-grep-guide.js',   // v0.32.0 — was in plugin-cache only, never fired
  'pre-read-guide.js',   // v0.32.0 — was in plugin-cache only, never fired
  'post-grep-inject.js', // compound-grep — PostToolUse(Bash) permission-neutral answer inject
];

// Description markers — primary cleanup discriminator (immune to env/path
// pollution per feedback_plugin_env_isolation.md). New v0.32.0 markers carry
// the version so older lifecycle.js still recognizes them as ours.
const SETTINGS_HOOK_DESC = {
  preToolUse:       '[code-graph-mcp v0.32+] PreToolUse re-routed via settings.json (cache hooks.json silently ignored for this event by current CC)',
  postToolUseEdit:  '[code-graph-mcp v0.32+] PostToolUse Write|Edit incremental-index update',
  postToolUseInject:'[code-graph-mcp v0.32+] PostToolUse Bash compound-grep answer inject (permission-neutral additionalContext)',
  userPromptSubmit: '[code-graph-mcp v0.32+] UserPromptSubmit context push',
};

const OUR_DESCRIPTIONS = [
  // Legacy v0.7.x / 0.8.x descriptions — kept so very-old installs still get cleaned up.
  'StatusLine self-heal, lifecycle sync, project map injection',
  'Auto-inject impact analysis when editing functions with 2+ callers',
  'Auto-update code graph index after file edits',
  'Inject code-graph structural context based on user intent',
  // v0.32.0 — new re-route markers
  SETTINGS_HOOK_DESC.preToolUse,
  SETTINGS_HOOK_DESC.postToolUseEdit,
  SETTINGS_HOOK_DESC.postToolUseInject,
  SETTINGS_HOOK_DESC.userPromptSubmit,
];

function isOurHookEntry(entry) {
  if (!entry || !entry.hooks) return false;
  // Primary: match by description (immune to path pollution).
  if (entry.description && OUR_DESCRIPTIONS.includes(entry.description)) return true;
  // Fallback: script basename + a delivery-surface marker in the path. TWO
  // surfaces ship these scripts: the marketplace plugin-cache (dir
  // 'code-graph-mcp') AND the global npm package (dir '@sdsrs/code-graph' — note
  // NO '-mcp' suffix). v0.32.1 tightened from bare 'code-graph' (which would
  // claim a user's own ~/code-graph/foo.js) to MARKETPLACE_NAME, but that alone
  // missed the npm-global surface, so `npm i -g`-delivered hooks were never
  // evicted and orphan-accumulated across node/version switches (RCA 2026-07-24).
  // Both markers are specific enough not to claim a user's unrelated file.
  return entry.hooks.some(h =>
    h.command && OUR_HOOK_SCRIPTS.some(s => h.command.includes(s)) &&
    (h.command.includes(MARKETPLACE_NAME) || h.command.includes(SHELL_PKG))
  );
}

function removeHooksFromSettings(settings) {
  if (!settings.hooks) return false;
  let changed = false;

  for (const event of Object.keys(settings.hooks)) {
    if (!Array.isArray(settings.hooks[event])) continue;
    const before = settings.hooks[event].length;
    settings.hooks[event] = settings.hooks[event].filter(e => !isOurHookEntry(e));
    if (settings.hooks[event].length !== before) changed = true;
    if (settings.hooks[event].length === 0) delete settings.hooks[event];
  }
  if (Object.keys(settings.hooks).length === 0) delete settings.hooks;

  return changed;
}

// --- v0.32.0: settings.json hook registration ---

// PLUGIN_ROOT (module-level, line 18) is the canonical __dirname-derived
// absolute path — never CLAUDE_PLUGIN_ROOT env (env leaks across plugins
// in settings.json hook execution context per feedback_plugin_env_isolation.md).

function buildSettingsHookEntries() {
  const root = PLUGIN_ROOT;
  // The budget is read from the table the hooks THEMSELVES spend against
  // (hook-fail-open.js), not written twice: a number registered here that the
  // hook does not know about is the JS-03 shape — the hook overruns a limit it
  // cannot see and Claude Code kills it mid-tool-call.
  const { HOOK_TIMEOUT_SECONDS } = require('./hook-fail-open');
  const scriptCmd = (name) => {
    const timeout = HOOK_TIMEOUT_SECONDS[name];
    const script = path.join(root, 'scripts', name);
    // POSIX: existence-guarded. After `/plugin uninstall`, CC may delete the
    // plugin-cache dir before our statusline teardown gets to strip these
    // entries — in that window every Edit/Bash/Read/prompt errored on a dead
    // path. The `if` form preserves node's own exit code (PreToolUse deny =
    // exit 2); `&& … || exit 0` would swallow it. Windows keeps the bare
    // command — the hook shell there is not reliably cmd, so `if exist`
    // can't be assumed.
    const command = process.platform === 'win32'
      ? `node "${script}"`
      : `if [ -f "${script}" ]; then node "${script}"; fi`;
    return { type: 'command', command, timeout };
  };

  return {
    PreToolUse: [
      { description: SETTINGS_HOOK_DESC.preToolUse, matcher: 'Edit', hooks: [scriptCmd('pre-edit-guide.js')] },
      { description: SETTINGS_HOOK_DESC.preToolUse, matcher: 'Bash', hooks: [scriptCmd('pre-grep-guide.js')] },
      { description: SETTINGS_HOOK_DESC.preToolUse, matcher: 'Read', hooks: [scriptCmd('pre-read-guide.js')] },
    ],
    PostToolUse: [
      { description: SETTINGS_HOOK_DESC.postToolUseEdit, matcher: 'Write|Edit', hooks: [scriptCmd('incremental-index.js')] },
      { description: SETTINGS_HOOK_DESC.postToolUseInject, matcher: 'Bash', hooks: [scriptCmd('post-grep-inject.js')] },
    ],
    UserPromptSubmit: [
      { description: SETTINGS_HOOK_DESC.userPromptSubmit, matcher: '', hooks: [scriptCmd('user-prompt-context.js')] },
    ],
  };
}

// Idempotent two-pass: (1) evict ALL our entries (legacy v0.7+/0.8+ markers
// AND v0.32+ markers) across EVERY event — catches legacy SessionStart/
// PostToolUse entries in settings.json pointing to stale plugin-cache paths;
// (2) write fresh v0.32+ entries for the events we own. SessionStart stays
// in plugin-cache hooks.json (it's still loaded from there), so we don't
// re-write it to settings.json.
function registerHooksToSettings(settings) {
  // `hooks` must be a plain object. `settings.hooks || {}` accepted an ARRAY —
  // and every named property assigned onto it below (`hooks.PreToolUse = [...]`)
  // is silently dropped by JSON.stringify, which serializes an array by index.
  // The result was total, reported-as-success inertness: `install` printed
  // "Installed | settings=true", `health` printed "OK — all paths valid", and
  // `"hooks": []` came back out with zero of our six hooks registered. A string
  // or number was worse — an uncaught "Cannot create property on string".
  // Anything non-object is replaced, same as a missing key: we cannot merge into
  // a shape the schema does not allow, and leaving it means the plugin never
  // works while claiming it does.
  if (!settings.hooks || typeof settings.hooks !== 'object' || Array.isArray(settings.hooks)) {
    settings.hooks = {};
  }

  // Idempotent across delivery surfaces: if every desired (event,matcher) is
  // already present exactly once, pointing at a current, existing script
  // (plugin-cache OR global-npm), do nothing. Stops the settings.json ping-pong
  // where the cache session-init and the npm-global CLI doctor each rewrote the
  // other's valid entry every run (RCA 2026-07-24). Any missing/stale/dead entry
  // — or a duplicate ( oursCount > expected) — still triggers evict+rewrite.
  const survey = surveyHookCoverage(settings);
  let oursCount = 0;
  for (const entries of Object.values(settings.hooks)) {
    if (Array.isArray(entries)) oursCount += entries.filter(isOurHookEntry).length;
  }
  if (survey.missing.length === 0 && survey.stale.length === 0
      && oursCount === survey.expected.length) {
    return false;
  }

  const before = JSON.stringify(settings.hooks);

  // Pass 1: evict our entries across every event.
  for (const event of Object.keys(settings.hooks)) {
    if (!Array.isArray(settings.hooks[event])) continue;
    settings.hooks[event] = settings.hooks[event].filter(e => !isOurHookEntry(e));
    if (settings.hooks[event].length === 0) delete settings.hooks[event];
  }

  // Pass 2: write fresh entries for our desired events.
  const desired = buildSettingsHookEntries();
  for (const [event, desiredEntries] of Object.entries(desired)) {
    const existing = Array.isArray(settings.hooks[event]) ? settings.hooks[event] : [];
    settings.hooks[event] = [...existing, ...desiredEntries];
  }

  return before !== JSON.stringify(settings.hooks);
}

// Extract the .js script path a hook command invokes — bare (`node "…"`) or
// existence-guarded (`if [ -f "…" ]; then node "…"; fi`).
// Is the composite command currently in the statusLine slot one we should
// replace? Mirrors the staleness rule `surveyHookCoverage` applies to hooks —
// the two slots are claimed by the same install() and drift the same way.
//
// Stale means: unparseable, pointing at a script that no longer exists (a node
// version was uninstalled, a checkout deleted), or pinned to an OLDER
// plugin-cache version dir than ours. A live composite from a different
// delivery surface at the same or a newer version is left alone — that is the
// whole point. An in-place path (global npm, dev checkout) carries no version
// dir and can never go version-stale: npm overwrites the same path on upgrade.
function compositeSlotIsStale(currentCmd) {
  const script = hookCmdScript(currentCmd);
  if (!script) return true;
  if (!fs.existsSync(script)) return true;
  const pv = cacheDirVersion(script);
  if (!pv) return false;
  const { compareVersions } = require('./version-utils');
  const dv = cacheDirVersion(hookCmdScript(compositeCommand())) || getPluginVersion();
  return compareVersions(pv, dv) < 0;
}

function hookCmdScript(cmd) {
  const m = (cmd || '').match(/node "([^"]+\.js)"/) || (cmd || '').match(/"([^"]+\.js)"/);
  return m ? m[1] : null;
}

// Version encoded in a plugin-cache path (.../code-graph-mcp/code-graph-mcp/<ver>/scripts/…).
// Null for in-place installs (global npm), whose path never carries a version
// dir — npm overwrites the same path on upgrade, so such a path never goes
// version-stale (only dead-path-stale, caught separately by fs.existsSync).
function cacheDirVersion(scriptPath) {
  // Separator-agnostic: the command string we parse is built with path.join,
  // which yields `\` on Windows. A `/`-only pattern returned null there, so
  // `compositeSlotIsStale` answered "not stale" for EVERY plugin-cache path
  // and the statusline slot was never healed on Windows — it self-corrected
  // only once cleanupOldCacheVersions eventually deleted the old version dir.
  // The repo's own `an older plugin-cache version dir must still be healed`
  // test could not catch it: plugin-tests runs on ubuntu only.
  const m = (scriptPath || '')
    .replace(/\\/g, '/')
    .match(/\/code-graph-mcp\/code-graph-mcp\/(\d+\.\d+\.\d+[^/]*)\//);
  return m ? m[1] : null;
}

// Inventory of (event, matcher) tuples we expect to find in settings.json after
// install. Consumed by doctor (report + fix) and session-init (self-heal):
// `missing` = entry absent; `stale` = present but the registered command no
// longer matches what we'd write now (points at an old plugin-cache version
// dir / moved path). A stale path can run pre-recordRecommendation hook code,
// so the hook fires but the conversion metric stays dark — invisible to a
// present/absent check. This is the 0.45.1-registered-while-0.45.4-active
// case the RCA surfaced.
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

  const { compareVersions } = require('./version-utils');
  const missing = expected.filter(k => !present.has(k));
  // Version/surface-tolerant staleness. Was an exact command-string compare,
  // which made two registration authorities (plugin-cache session-init vs
  // global-npm CLI doctor — different absolute paths) each flag the other's
  // VALID CURRENT entry stale and rewrite it → settings.json ping-pong on every
  // alternating run (RCA 2026-07-24). An entry is stale only when its script is
  // a dead path OR resolves to an OLDER plugin-cache version dir than we'd write
  // now. A present entry on a different but valid, current surface (npm in-place
  // install: file exists, no version in path) is NOT stale.
  const stale = expected.filter(k => {
    if (!present.has(k) || !presentCmd[k]) return false;
    const pScript = hookCmdScript(presentCmd[k]);
    if (!pScript) return false;
    if (!fs.existsSync(pScript)) return true;             // dead path
    const pv = cacheDirVersion(pScript);
    if (pv) {
      // Pinned to a plugin-cache version dir: stale iff older than us. Compare
      // against the desired cache dir when we're the cache authority; the
      // global-npm/dev authority's desired path carries no version dir, so
      // fall back to our own plugin version — an old-version cache pin must be
      // healed from EITHER surface, not only when the desired path happens to
      // be cache-shaped. Newer-than-us stays (downgrade-war guard, §1.11).
      const dv = cacheDirVersion(hookCmdScript(desiredCmd[k])) || getPluginVersion();
      return compareVersions(pv, dv) < 0;
    }
    return false;                                         // in-place/current surface
  });
  return { expected, present: [...present], missing, stale };
}

// --- Firing self-test (v0.67.0) ---
//
// surveyHookCoverage proves a hook is WIRED; it does NOT prove the script RUNS.
// A renamed sibling module, an incompatible node, or a corrupt install leaves a
// hook registered-but-inert — invisible to every string/path check. verifyHooksFire
// spawns each registered hook the way Claude Code does (node + a synthetic CC
// stdin payload) inside a throwaway fixture and asserts it exits 0. This is the
// "does it really fire" check. What it CANNOT prove is that CC *dispatches* real
// tool-calls to it (only a live session shows that — see the Layer-B dispatch
// canary in session-init.js). Best-effort; never throws.

// Representative CC stdin payload that drives each matcher's path. The Bash
// payload is engaging (a source-tree search → a deny/hint IS emitted); the Edit
// payload's short old_string short-circuits before any binary spawn; the rest
// just exercise the require-chain + stdin parse.
function hookFirePayload(matcher) {
  switch (matcher) {
    case 'Bash':
      // A QUOTED, identifier-like pattern → classifyBlock-positive → the
      // PreToolUse deny tier emits (the hint-tier dark stdout fallthrough was
      // removed in the compound-grep change, so a non-foldable pattern would now
      // produce no output and falsely read as "didn't fire").
      return { tool_name: 'Bash', tool_input: { command: 'grep -rn "SomeUniqueSymbol" src/' } };
    case 'Read':
      return { tool_name: 'Read', tool_input: { file_path: 'src/example.rs' } };
    case 'Edit':
    case 'Write|Edit':
      return { tool_name: 'Edit', tool_input: { file_path: 'src/example.rs', old_string: 'a', new_string: 'b' } };
    case '': // UserPromptSubmit
      // A SYMPTOM-flavoured prompt, for the same reason the Bash probe uses a
      // quoted identifier: it must reach a path that actually emits. The old
      // payload ('where is the parse function defined') produced
      // `determineQueryType(...) === null` — no query, no output — and the two
      // result-injecting paths that would emit need a real indexed binary, which
      // this throwaway fixture does not have. `symptom-hint` is prose-only, so it
      // engages on the fixture alone. Field name is `prompt`: this constructor
      // has always had it right, while the hook itself read `message`
      // (audit 2026-08-29 JS-01).
      return { prompt: 'why does the parser crash on empty input' };
    default:
      return { tool_name: 'Unknown', tool_input: {} };
  }
}

// The hooks CC actually loads from settings.json (PreToolUse/PostToolUse/
// UserPromptSubmit). SessionStart (hooks.json) runs every session → its own
// liveness proof → excluded here.
function defaultHookFireProbes() {
  const probes = [];
  for (const [event, entries] of Object.entries(buildSettingsHookEntries())) {
    for (const e of entries) {
      const cmd = e.hooks && e.hooks[0] && e.hooks[0].command;
      const m = (cmd || '').match(/"([^"]+\.js)"/);
      if (!m) continue;
      probes.push({ label: `${event}:${e.matcher || '*'}`, script: m[1], payload: hookFirePayload(e.matcher || '') });
    }
  }
  return probes;
}

function verifyHooksFire({ hooks, env, timeoutMs = 4000, tmpBase } = {}) {
  const { spawnSync } = require('child_process');
  const probes = hooks || defaultHookFireProbes();

  // Throwaway fixture carrying the .code-graph/index.db marker so resolveProjectRoot
  // resolves (otherwise the hooks early-return before exercising anything).
  // Base is os.tmpdir() directly (a UNIQUE mkdtemp), NOT the shared cgTmpDir()
  // subdir — a concurrent process clearing `<tmp>/code-graph-mcp` mid-run would
  // otherwise yank this fixture out from under an in-flight spawn (cwd ENOENT).
  let fixture = null;
  try {
    const base = tmpBase || os.tmpdir();
    fixture = fs.mkdtempSync(path.join(base, 'cg-hookfire-'));
    fs.mkdirSync(path.join(fixture, '.code-graph'), { recursive: true });
    fs.writeFileSync(path.join(fixture, '.code-graph', 'index.db'), '');
  } catch (e) {
    return { ok: false, results: [], error: `fixture: ${e && e.message}` };
  }

  // Force a non-silenced, no-binary-spawn firing config: the smoke tests the
  // machinery regardless of the user's silence prefs and never invokes the binary
  // (CODE_GRAPH_NO_ANSWER_IN_DENY keeps cg-answer out of the deny path).
  // Redirect the hooks' tmp state (per-command grep cooldown, read-fanout state)
  // into the throwaway fixture via TMPDIR so the smoke neither collides with a
  // real 60s cooldown (which would suppress the deny → false "didn't fire") nor
  // pollutes the user's real cooldown/state.
  const baseEnv = {
    ...process.env,
    CODE_GRAPH_QUIET_HOOKS: '0',
    CODE_GRAPH_NO_ANSWER_IN_DENY: '1',
    TMPDIR: fixture, TMP: fixture, TEMP: fixture,
    ...(env || {}),
  };

  const results = [];
  for (const h of probes) {
    let error = null, ok = false, emitted = false, code = null, signal = null;
    try {
      const r = spawnSync(process.execPath, [h.script], hidden({
        input: JSON.stringify(h.payload || {}),
        cwd: fixture,
        env: baseEnv,
        timeout: timeoutMs,
        encoding: 'utf8',
      }));
      code = r.status;
      signal = r.signal;
      ok = !r.error && r.status === 0;
      emitted = !!(r.stdout && r.stdout.trim());
      if (!ok) {
        error = r.error
          ? String(r.error.message || r.error)
          : ((r.stderr || '').trim().slice(0, 200) || `exit ${r.status}`);
      }
    } catch (e) {
      error = String((e && e.message) || e);
    }
    results.push({ label: h.label, script: h.script, ok, code, signal, emitted, error });
  }

  try { fs.rmSync(fixture, { recursive: true, force: true }); } catch { /* ok */ }

  return { ok: results.length > 0 && results.every(r => r.ok), results };
}

// --- Install (idempotent) ---

function install({ reclaimStatusline = false } = {}) {
  const version = getPluginVersion();
  const manifest = readManifest();
  // Probe FIRST, and pay the backup only on the write path (audit 2026-08-29
  // JS-09). `readSettingsForWrite()` with no argument takes its `.corrupt-*`
  // copy eagerly, which is right for a caller that is certainly going to
  // rewrite the file and wrong here: install() is idempotent and usually
  // changes nothing. Combined with any condition that re-runs it every session
  // — the documented `manifestUnwritable` loop is one — a settings.json with a
  // single non-UTF-8 byte grew one timestamped copy in ~/.claude per session,
  // unbounded, for a file nothing ever rewrote. Same reasoning as
  // `cleanupDisabledStatusline`, which already probes.
  const probe = readJsonResult(settingsPath());
  const deferBackup = Boolean(probe.value && probe.lossy);
  let { settings, backedUpTo } = deferBackup
    ? { settings: probe.value, backedUpTo: null }
    : readSettingsForWrite(probe);
  if (!settings) {
    // Unusable settings.json that we could not even copy aside. Bail without
    // touching it — and without stamping the manifest, so the next run retries
    // the whole install once the user has repaired the file.
    return {
      version,
      settingsChanged: false,
      statusLineClaimed: manifest.config.statusLine,
      hooksRegistered: false,
      settingsUnreadable: true,
    };
  }
  let settingsChanged = false;

  // 0. Migrate from old plugin IDs
  if (migrateOldPluginIds(settings)) {
    settingsChanged = true;
  }

  // 1. StatusLine — composite approach
  //    a. Capture existing statusline as a provider (if not already composite)
  //    b. Register code-graph as a provider
  //    c. Set statusLine to composite script
  if (!isOurComposite(settings)) {
    // Displacement tracking: we held the slot before (manifest.config.statusLine)
    // but a foreign command sits there now — either another slot-claiming plugin
    // (whose own self-heal re-takes it just like ours would → statusline
    // ping-pong every session) or the user's deliberate choice. Either way,
    // silently re-claiming forever is wrong: after >2 observed displacements
    // stand down — stay registered as a provider, leave the slot alone.
    // Explicit `lifecycle.js install` (or CODE_GRAPH_FORCE_STATUSLINE=1)
    // resets the counter and re-claims.
    const currentCmd = settings.statusLine && settings.statusLine.command;
    if (reclaimStatusline || process.env.CODE_GRAPH_FORCE_STATUSLINE === '1') {
      manifest.config.statuslineDisplaced = 0;
    } else if (!currentCmd) {
      // RE-ARM: the slot is EMPTY. Stand-down exists to stop a tug-of-war with
      // another provider, and there is nobody to fight — whoever displaced us
      // has been uninstalled, or the user cleared the slot. Without this the
      // counter was write-only: once past the threshold the plugin stayed
      // silently statusline-less for the life of the manifest, and the only way
      // back was an env var nobody knows to set.
      manifest.config.statuslineDisplaced = 0;
    } else if (manifest.config.statusLine === true) {
      manifest.config.statuslineDisplaced = (manifest.config.statuslineDisplaced || 0) + 1;
    }
    if ((manifest.config.statuslineDisplaced || 0) > 2) {
      if (manifest.config.statusLine === true) {
        // Transition into stand-down exactly once: release claimed ownership
        // (stops the counter) and leave a breadcrumb.
        manifest.config.statusLine = false;
        process.stderr.write(
          '[code-graph] statusLine slot keeps being re-claimed by another provider — standing down.\n' +
          '            Re-claim: CODE_GRAPH_FORCE_STATUSLINE=1 or `node lifecycle.js install`\n'
        );
      }
    } else {
      // Preserve existing statusline as first provider
      if (currentCmd) {
        registerStatuslineProvider('_previous', currentCmd, true);
      }
      // Set composite as the statusLine
      settings.statusLine = { type: 'command', command: compositeCommand() };
      settingsChanged = true;
      manifest.config.statusLine = true;
    }
  } else {
    // Composite exists — heal it only when it is actually stale. An exact
    // string mismatch is NOT staleness: two copies of this plugin (plugin cache
    // + global npm, or a dev checkout) derive different absolute paths for the
    // same current composite, and rewriting on mismatch made each install()
    // take the slot back from the other — a 2-cycle that rewrote settings.json
    // on every SessionStart. Identical shape to the hook ping-pong ea0166d
    // fixed; that fix's regression test asserted only `settings.hooks`, so this
    // half of the pair stayed open.
    const cmd = compositeCommand();
    if (settings.statusLine.command !== cmd && compositeSlotIsStale(settings.statusLine.command)) {
      settings.statusLine.command = cmd;
      settingsChanged = true;
    }
    // We hold the slot — any displacement episode is over.
    if (manifest.config.statuslineDisplaced) manifest.config.statuslineDisplaced = 0;
    manifest.config.statusLine = true;
  }

  // Register code-graph provider
  registerStatuslineProvider('code-graph', codeGraphStatuslineCommand(), false);

  // 2. Hooks — v0.32.0: actively write PreToolUse/PostToolUse/UserPromptSubmit
  //    to settings.json. Plugin-cache hooks.json is silently ignored by current
  //    Claude Code for these events (SessionStart still loads from cache).
  //    registerHooksToSettings is idempotent: strips priors then appends fresh.
  const hooksRegistered = registerHooksToSettings(settings);
  if (hooksRegistered) settingsChanged = true;

  // NOTE: enabledPlugins is managed by Claude Code's plugin system, not by lifecycle.
  // Do NOT add enabledPlugins entries here — it causes phantom plugin entries
  // when the ID doesn't match the marketplace name.

  // 3. Write settings atomically if changed
  if (settingsChanged) {
    if (deferBackup) {
      // Now it IS a rewrite, so the deferred copy is owed. A failure to make it
      // is the same refusal as the eager path: skipping the settings work beats
      // destroying bytes we cannot restore.
      const late = readSettingsForWrite(probe);
      if (!late.settings) {
        return {
          version,
          settingsChanged: false,
          statusLineClaimed: manifest.config.statusLine,
          hooksRegistered: false,
          settingsUnreadable: true,
        };
      }
      backedUpTo = late.backedUpTo;
    }
    const writeErr = tryWriteSettings(settings);
    if (writeErr) {
      // Do NOT fall through to the manifest stamp. A manifest carrying the
      // current version tells the next run "already installed", so it would skip
      // the retry and the plugin would stay inactive after the user makes the
      // file writable again — the same trap as the unreadable arm.
      return {
        version,
        settingsChanged: false,
        hooksRegistered: false,
        settingsUnwritable: true,
        error: writeErr.code || writeErr.name,
      };
    }
  }

  // 4. Write manifest with version
  manifest.version = version;
  manifest.installedAt = manifest.installedAt || new Date().toISOString();
  manifest.updatedAt = new Date().toISOString();
  const manifestErr = writeManifest(manifest);

  return {
    version,
    settingsChanged,
    statusLineClaimed: manifest.config.statusLine,
    hooksRegistered,
    // Unstamped manifest: the install DID land in settings.json, but the next
    // run will not know it and will redo the work (idempotent). Surfaced so
    // doctor/session-init can say so instead of implying a clean install.
    manifestUnwritable: manifestErr ? (manifestErr.code || manifestErr.name) : undefined,
    // Non-null => the previous settings.json was REPLACED and lives here now.
    settingsRebuiltFrom: backedUpTo,
  };
}

// --- Uninstall (clean all config) ---

/** Which of our npm packages exist at a global top level right now. */
function installedGlobalPkgs() {
  const { globalNodeModulesCandidates, PLATFORM_PKG } = require('./find-binary');
  const found = [];
  for (const name of [SHELL_PKG, PLATFORM_PKG]) {
    for (const root of globalNodeModulesCandidates()) {
      if (fs.existsSync(path.join(root, name, 'package.json'))) { found.push(name); break; }
    }
  }
  return found;
}

function defaultRunNpm(args) {
  const { spawnSync } = require('child_process');
  const { npmInvocation } = require('./npm-exec');
  try {
    const npm = npmInvocation(args, { timeout: 120000, stdio: 'pipe', encoding: 'utf8' });
    const r = spawnSync(npm.file, npm.args, npm.opts);
    return !r.error && r.status === 0;
  } catch { return false; }
}

function uninstall({ purgeGlobal = false, unadoptAll = false, runNpm = defaultRunNpm, scanGlobalPkgs = installedGlobalPkgs } = {}) {
  // Same guarded pair as install()/update(): probe without side effects, and
  // only take the backup + write path once there is something to write. Reading
  // this file with the lenient readJson and writing it back raw is what
  // destroyed non-UTF-8 bytes here — a teardown has even less license to lose
  // the user's model / env / permissions than an install does.
  const probe = readJsonResult(settingsPath());
  const settings = probe.value;
  let settingsChanged = false;

  if (settings) {
    // 1. StatusLine: remove code-graph integration and restore prior statusline.
    // `oneShot`: steps 6-7 below delete the plugin cache, taking
    // statusline-composite.js with it, so this is the last chance to move the
    // slot off a script that is about to stop existing (pre-tag review).
    if (detachStatuslineIntegration(settings, { oneShot: true })) {
      settingsChanged = true;
    }

    // 2. Hooks: remove from settings.json
    if (removeHooksFromSettings(settings)) {
      settingsChanged = true;
    }

    // 3. Remove all known IDs from enabledPlugins
    if (settings.enabledPlugins) {
      for (const id of [PLUGIN_ID, ...OLD_PLUGIN_IDS]) {
        if (id in settings.enabledPlugins) {
          delete settings.enabledPlugins[id];
          settingsChanged = true;
        }
      }
    }

    // 4. Write settings if changed
    if (settingsChanged) {
      const { settings: guarded } = readSettingsForWrite(probe);
      if (!guarded || tryWriteSettings(guarded)) settingsChanged = false;
    }
  }

  // 5. Remove all known IDs from installed_plugins.json
  //
  // Read-modify-write of Claude Code's OWN file. The write is already gated on a
  // successful parse, so an unusable file is skipped rather than clobbered (the
  // destructive `|| {}` shape never existed here) — but the skip was SILENT, and
  // steps 6-7 below still delete the plugin cache. The user then keeps a plugin
  // record pointing at a directory we removed, with `uninstall` reporting
  // success. Say so instead (audit 2026-08-16 P1-12 sweep).
  const installedRead = readJsonResult(installedPluginsPath());
  const installedPlugins = installedRead.value;
  let installedPluginsUnusable = false;
  if (installedPlugins && installedPlugins.plugins) {
    let ipChanged = false;
    for (const id of [PLUGIN_ID, ...OLD_PLUGIN_IDS]) {
      if (id in installedPlugins.plugins) {
        delete installedPlugins.plugins[id];
        ipChanged = true;
      }
    }
    if (ipChanged) {
      try {
        writeJsonAtomic(installedPluginsPath(), installedPlugins);
      } catch (err) {
        installedPluginsUnusable = true;
        console.error(
          `[code-graph] cannot write ${installedPluginsPath()} (${err.code || err.name}). ` +
          'Claude Code still lists this plugin — remove it with `/plugin uninstall code-graph-mcp`.'
        );
      }
    }
  } else if (!installedRead.missing) {
    installedPluginsUnusable = true;
    console.error(
      `[code-graph] cannot read ${installedPluginsPath()} ` +
      `(${installedRead.error ? installedRead.error.code || installedRead.error.message : 'not a JSON object'}). ` +
      'Left untouched — Claude Code may still list this plugin; remove it with `/plugin uninstall code-graph-mcp`.'
    );
  }

  // 5.5. Global npm packages + adoption inventory — read BEFORE step 6 wipes
  // CACHE_DIR (both the install marker and the adopted-projects registry live
  // there). The launcher's background install runs `npm install -g` on the
  // user's behalf; nothing on the Claude Code uninstall path ever removes those
  // packages (~40MB platform binary + CLI shim left on PATH forever).
  const pluginInstalledGlobals = !!readJson(GLOBAL_INSTALL_MARKER);
  let adoptedProjects = [];
  try { adoptedProjects = require('./adopt').readAdoptedProjects(); } catch { /* POSIX-only helper — ok */ }

  // 5.4. --unadopt-all: sweep every registered project's managed CLAUDE.md
  // block + generated detail file (unadopt is marker-guarded, so user files
  // are never touched; the .code-graph/ index dir is project DATA and stays —
  // its removal is listed in the guidance instead of automated).
  const unadopted = [];
  if (unadoptAll && adoptedProjects.length) {
    let unadoptFn = null;
    try { unadoptFn = require('./adopt').unadopt; } catch { /* POSIX-only — skip */ }
    if (unadoptFn) {
      for (const project of adoptedProjects) {
        try {
          const r = unadoptFn({ cwd: project });
          unadopted.push({ project, ok: !!(r && r.ok), cleaned: !!(r && (r.blockPruned || r.fileRemoved || r.claudeMdRemoved)) });
        } catch (e) {
          unadopted.push({ project, ok: false, error: (e && e.message) || String(e) });
        }
      }
      try { adoptedProjects = require('./adopt').readAdoptedProjects(); } catch { /* ok */ }
    }
  }
  let globalPkgsRemoved = [];
  let globalPkgsRemaining = scanGlobalPkgs();
  if (globalPkgsRemaining.length && (pluginInstalledGlobals || purgeGlobal)) {
    if (runNpm(['uninstall', '-g', ...globalPkgsRemaining])) {
      globalPkgsRemoved = globalPkgsRemaining;
      globalPkgsRemaining = scanGlobalPkgs(); // re-scan: report only what actually survived
    }
  }

  // 6. Remove cache directory
  try { fs.rmSync(CACHE_DIR, { recursive: true, force: true }); } catch { /* ok */ }

  // 6.5. The shared tmp dir (cooldown flags, read-fanout state, interrupted
  // `update-*` staging). Nothing in it outlives an uninstall, and the periodic
  // prune that keeps it bounded stops running the moment the hooks are gone —
  // so without this it is residue with no remaining owner.
  try {
    fs.rmSync(require('./tmp-dir').CG_TMP_DIR, { recursive: true, force: true });
  } catch { /* ok */ }

  // 7. Remove plugin files from cache (all known paths, including parent dirs)
  const cacheRoot = pluginsCacheDir();
  const pluginCacheDirs = [
    path.join(cacheRoot, MARKETPLACE_NAME),
    path.join(cacheRoot, 'sdsrss-code-graph'),
    path.join(cacheRoot, 'sdsrss', 'code-graph'),
  ];
  for (const dir of pluginCacheDirs) {
    try { fs.rmSync(dir, { recursive: true, force: true }); } catch { /* ok */ }
  }

  return { settingsChanged, pluginInstalledGlobals, globalPkgsRemoved, globalPkgsRemaining, adoptedProjects, unadopted, installedPluginsUnusable };
}

// --- Update (refresh config points) ---

function update() {
  const version = getPluginVersion();
  const manifest = readManifest();
  const oldVersion = manifest.version;
  const { settings, backedUpTo } = readSettingsForWrite();
  if (!settings) {
    return { oldVersion, version, settingsChanged: false, hooksRegistered: false, settingsUnreadable: true };
  }
  let settingsChanged = false;

  // 0. Migrate from old plugin IDs
  if (migrateOldPluginIds(settings)) {
    settingsChanged = true;
  }

  // 1. Update composite command path if version changed
  if (isOurComposite(settings)) {
    const cmd = compositeCommand();
    if (settings.statusLine.command !== cmd) {
      settings.statusLine.command = cmd;
      settingsChanged = true;
    }
  }

  // 2. Update code-graph provider in registry
  registerStatuslineProvider('code-graph', codeGraphStatuslineCommand(), false);

  // 3. Hooks — v0.32.0: register PreToolUse/PostToolUse/UserPromptSubmit in
  //    settings.json (idempotent; absolute paths re-anchor on every update).
  const hooksRegistered = registerHooksToSettings(settings);
  if (hooksRegistered) settingsChanged = true;

  // NOTE: enabledPlugins is managed by Claude Code's plugin system, not by lifecycle.

  // 4. Write settings if changed
  if (settingsChanged) {
    const writeErr = tryWriteSettings(settings);
    if (writeErr) {
      // Same reasoning as install(): stamping the manifest here would make the
      // next run believe the update landed.
      return {
        oldVersion, version,
        settingsChanged: false,
        hooksRegistered: false,
        settingsUnwritable: true,
        error: writeErr.code || writeErr.name,
      };
    }
  }

  // 5. Clear update-check cache (force re-check after update)
  const updateCache = path.join(CACHE_DIR, 'update-check');
  try { fs.unlinkSync(updateCache); } catch { /* ok */ }

  // 6. Update manifest
  manifest.version = version;
  manifest.updatedAt = new Date().toISOString();
  const manifestErr = writeManifest(manifest);

  // 7. Clean up old cached versions (keep the newest few). NOTE: older cache
  //    dirs are NOT always inert — a running MCP server's launcher path
  //    (<version>/scripts/mcp-launcher.js) is resolved + cached by Claude Code
  //    for the whole session, so pruning the version a live process is bound to
  //    breaks `/mcp` reconnect with -32000 (MODULE_NOT_FOUND). cleanupOldCacheVersions
  //    therefore skips any version still referenced by a live process cmdline.
  cleanupOldCacheVersions(5);

  return {
    oldVersion, version, settingsChanged, hooksRegistered, settingsRebuiltFrom: backedUpTo,
    manifestUnwritable: manifestErr ? (manifestErr.code || manifestErr.name) : undefined,
  };
}

/**
 * Remove old plugin cache versions, keeping the N most recent.
 * Cache layout: ~/.claude/plugins/cache/<marketplace>/<plugin>/<version>/
 */
function cleanupOldCacheVersions(
  keep = 5,
  getActiveCmdlines = readActiveProcessCmdlines,
  cacheParent = path.join(pluginsCacheDir(), MARKETPLACE_NAME),
) {
  // Command lines of all live processes — used by the in-use guard below to
  // avoid deleting a version a running MCP server is still bound to. Failure to
  // enumerate ⇒ [] ⇒ pruning falls back to recency-only (pre-guard behavior).
  let cmdlines;
  try { cmdlines = getActiveCmdlines() || []; } catch { cmdlines = []; }
  try {
    // List all subdirectories under the marketplace cache
    const entries = fs.readdirSync(cacheParent, { withFileTypes: true });
    for (const entry of entries) {
      if (!entry.isDirectory()) continue;
      const pluginDir = path.join(cacheParent, entry.name);
      try {
        const versions = fs.readdirSync(pluginDir, { withFileTypes: true })
          .filter(d => d.isDirectory())
          .map(d => ({
            name: d.name,
            path: path.join(pluginDir, d.name),
            mtime: fs.statSync(path.join(pluginDir, d.name)).mtimeMs,
          }))
          .sort((a, b) => b.mtime - a.mtime); // newest first

        if (versions.length <= keep) continue;

        for (const v of versions.slice(keep)) {
          // In-use guard: never delete a version dir a live process is running
          // from. Claude Code caches the resolved launcher path
          // (<version>/scripts/mcp-launcher.js) for the session; deleting that
          // dir breaks `/mcp` reconnect with -32000. Trailing separator stops
          // `0.8` from matching `0.80.x`.
          if (cmdlines.some(c => c.includes(v.path + path.sep))) continue;
          try {
            fs.rmSync(v.path, { recursive: true, force: true });
          } catch { /* permission error — skip */ }
        }
      } catch { /* can't read plugin dir — skip */ }
    }
  } catch { /* cache dir doesn't exist — nothing to clean */ }
}

/**
 * Best-effort list of running process command lines, for cleanupOldCacheVersions'
 * in-use guard. Linux reads /proc/<pid>/cmdline; macOS/BSD shells out to `ps`;
 * any other platform or failure returns [] (pruning then falls back to
 * recency-only — the same behavior as before the guard existed).
 */
function readActiveProcessCmdlines() {
  try {
    if (process.platform === 'linux' && fs.existsSync('/proc')) {
      const out = [];
      for (const pid of fs.readdirSync('/proc')) {
        if (!/^\d+$/.test(pid)) continue;
        try {
          const raw = fs.readFileSync(path.join('/proc', pid, 'cmdline'), 'utf8');
          if (raw) out.push(raw.replace(/\0/g, ' '));
        } catch { /* pid exited or unreadable — skip */ }
      }
      return out;
    }
  } catch { /* fall through to ps */ }
  try {
    const { execFileSync } = require('child_process');
    return execFileSync('ps', ['-axww', '-o', 'command='], hidden({
      encoding: 'utf8', maxBuffer: 8 * 1024 * 1024,
    })).split('\n').filter(Boolean);
  } catch { /* unsupported platform — caller falls back to recency-only */ }
  return [];
}

// --- Health Check ---
// Validates all registered paths in settings.json point to existing scripts.
// Returns { healthy, issues, repaired, remaining }.
//   issues:    pre-repair detection list (what was wrong on entry)
//   repaired:  true only after a post-repair re-scan returned zero issues
//              (was previously set blindly to true whenever install() ran,
//              which would lie if install() couldn't actually fix something)
//   remaining: post-repair detection list — present iff install() was invoked;
//              empty array means repair succeeded

function scanForBrokenPaths() {
  // The READ-side member of the same collapsed-`null` class the write side was
  // fixed for: `readJson(...) || {}` on an unusable settings.json yields an
  // empty object, every loop below finds nothing to check, and the caller
  // reports "all paths valid" — the most confidently wrong answer available,
  // delivered during the exact incident it should be flagging. Surface it as an
  // issue instead.
  //
  // NOT auto-repairable in the sense that this issue carries no `fixId`. Be
  // precise about what `install()` then does, because an earlier version of this
  // comment claimed it "correctly refuses this file" and that is only half true:
  // it refuses only the subset it cannot copy aside (unreadable file, read-only
  // dir, path-is-a-directory). For the common case — unparseable but writable —
  // it BACKS UP and REBUILDS, so `healthCheck` reports `repaired: true` and hands
  // back `rebuiltFrom`. Callers must render that as the destructive repair it is.
  const settingsRead = readJsonResult(settingsPath());
  if (settingsRead.corrupt) {
    return [{
      type: 'settings-unusable',
      path: settingsPath(),
      reason: settingsRead.error ? settingsRead.error.message : 'not a JSON object',
    }];
  }
  const settings = settingsRead.value || {};
  const issues = [];

  // Every path extraction below goes through hookCmdScript, the same parser
  // compositeSlotIsStale and surveyHookCoverage use. These three sites each had
  // their own inline `/node\s+"([^"]+)"/`, which requires the literal word
  // `node` followed by whitespace — so a command naming the interpreter by
  // absolute path, the spelling a Windows install produces
  // (`"C:\Program Files\nodejs\node.exe" "C:\…\script.js"`), matched nothing.
  // A path this scan cannot READ is reported as no path at all, i.e. as healthy,
  // which is the failure mode the whole function exists to prevent. Six sites,
  // two spellings, one of them blind: collapse to one.
  const scriptOf = (cmd) => hookCmdScript(cmd);

  // Check statusLine path
  if (isOurComposite(settings)) {
    const script = scriptOf(settings.statusLine.command);
    if (script && !fs.existsSync(script)) {
      issues.push({ type: 'statusLine', path: script });
    }
  }

  // Check hook paths
  if (settings.hooks) {
    for (const [event, entries] of Object.entries(settings.hooks)) {
      if (!Array.isArray(entries)) continue;
      for (const entry of entries) {
        if (!isOurHookEntry(entry) || !entry.hooks) continue;
        for (const h of entry.hooks) {
          const script = h.command && scriptOf(h.command);
          if (script && !fs.existsSync(script)) {
            issues.push({ type: 'hook', event, path: script });
          }
        }
      }
    }
  }

  // Check registry paths
  const registry = readRegistry();
  for (const provider of registry) {
    if (provider.id === '_previous') continue;
    const script = provider.command && scriptOf(provider.command);
    if (script && !fs.existsSync(script)) {
      issues.push({ type: 'registry', id: provider.id, path: script });
    }
  }

  return issues;
}

function healthCheck() {
  const issues = scanForBrokenPaths();

  if (issues.length === 0) {
    return { healthy: true, issues, repaired: false };
  }

  // Attempt auto-repair, then re-scan to confirm the issues actually went
  // away. install() may legitimately fail to resolve a problem (binary path
  // permanently gone, registry corrupted, etc.) and the previous code lied
  // by always returning repaired:true.
  const r = install();
  const remaining = scanForBrokenPaths();
  return {
    healthy: false,
    issues,
    repaired: remaining.length === 0,
    remaining,
    // A "repair" that rebuilt an unusable settings.json REPLACED the user's
    // file — model / env / permissions / their own hooks now live only in the
    // backup. Callers must not render that as a plain success.
    rebuiltFrom: r.settingsRebuiltFrom || null,
  };
}

// True when the plugin has been UNINSTALLED (removed from installed_plugins.json),
// as opposed to merely toggled OFF (isPluginExplicitlyDisabled — the user may
// re-enable). The distinction matters because the uninstall teardown below is
// destructive (deletes the cached binary, unwinds project adoption); doing that
// on a temporary disable would force a re-download + re-adopt on re-enable.
function isPluginUninstalled(settings = readJson(settingsPath()) || {}) {
  if (isPluginExplicitlyDisabled(settings)) return false;
  return isPluginInactive(settings);
}

// Remove the ~/.cache/code-graph residue (the ~40MB binary, update-state,
// statusline-registry, install-manifest). The settings-only self-heal
// (cleanupDisabledStatusline) leaves this behind; the SessionStart teardown calls
// this so a CC `/plugin uninstall` (which fires no uninstall hook) still reclaims
// the disk. Idempotent: rm is force, so repeat SessionStarts are no-ops. Does NOT
// touch the plugin-cache script dirs — those are CC-managed and may be executing.
// One thing in CACHE_DIR is NOT residue: adopted-projects.json, the only record
// of which repos carry a managed CLAUDE.md block. Wiping it strands every block
// — `uninstall({unadoptAll:true})` afterwards reads an empty registry, reports
// `unadopted: []`, and the blocks stay in the user's repos with nothing left
// that knows where they are. `uninstall()` captures the list before calling
// here; `cleanupDisabledStatusline` does not, and by this function's own comment
// that is the ONE path guaranteed to run after `/plugin uninstall`. So the
// preservation belongs here, at the wipe, rather than at each caller — the same
// "fix it at the shared layer, not per surface" the <external> query filter
// needed.
function removeCacheResidue() {
  // Path comes from adopt.js rather than a second spelling of the basename —
  // a literal here would silently stop matching the day adopt.js renames it,
  // and the failure mode is exactly the data loss this guard exists to stop.
  // Preserve ONLY when the registry still names projects. A registry that is
  // absent, empty, or already fully unadopted (the normal SessionStart teardown
  // order, which unadopts first) strands nothing, and re-creating CACHE_DIR to
  // hold `[]` would just be new residue.
  let registryPath = null;
  let registry = null;
  try {
    registryPath = require('./adopt').adoptedRegistryFile();
    const raw = fs.existsSync(registryPath) ? fs.readFileSync(registryPath) : null;
    if (raw) {
      let parsed = null;
      let usable = true;
      try { parsed = JSON.parse(raw.toString('utf8')); } catch { usable = false; }
      // Preserve a NON-EMPTY list (projects still carry a block) and anything we
      // could not READ as a list. The unusable case used to fall into the same
      // "nothing to preserve" catch as a missing file — the strictly worse
      // outcome, since a registry we cannot parse is the one whose contents we
      // are least able to reconstruct, and the sweep above deliberately skips it
      // rather than guessing. Only a genuinely EMPTY array strands nothing and
      // is allowed to go with the cache dir.
      if (!usable || !Array.isArray(parsed) || parsed.length) registry = raw;
    }
  } catch { /* POSIX-only helper or an unreadable path — nothing to preserve */ }
  try {
    fs.rmSync(CACHE_DIR, { recursive: true, force: true });
  } catch { return false; }
  if (registryPath && registry) {
    try {
      fs.mkdirSync(path.dirname(registryPath), { recursive: true });
      fs.writeFileSync(registryPath, registry);
    } catch { /* best-effort: the binary is still reclaimed */ }
  }
  return true;
}

module.exports = {
  install, uninstall, update, healthCheck, scanForBrokenPaths, checkScopeConflict,
  isPluginExplicitlyDisabled, isPluginInactive, isPluginUninstalled, removeCacheResidue,
  cleanupDisabledStatusline, unadoptRegisteredProjects,
  reportUnadoptSweep,                                                  // exported so its three-way bucketing is testable (audit 2026-08-29 JS-06)
  readManifest, readJson, readJsonResult, readSettingsForWrite, writeJsonAtomic,
  backupCorruptFile, pruneCorruptBackups, MAX_CORRUPT_BACKUPS,                                                   // auto-update.js repoints installed_plugins.json and owes the same preserve-then-proceed route
  migrateOldPluginIds,                                                 // exported so its failure arms are testable (audit 2026-08-22 P2-10)
  readRegistry, readRegistryForWrite, writeRegistry,
  getPluginVersion, cleanupOldCacheVersions,
  removeHooksFromSettings, isOurHookEntry,
  registerHooksToSettings, buildSettingsHookEntries,                  // v0.32.0
  surveyHookCoverage, compositeCommand, compositeSlotIsStale,          // v0.49.1 — version-aware self-heal
  codeGraphStatuslineCommand,   // exported so a test asserts the row shape the product really writes
  hookCmdScript,                                                       // the ONE hook-command path parser (session-init reuses it)
  cacheDirVersion,                                                     // exported for the separator-agnostic test

  verifyHooksFire, defaultHookFireProbes,                              // v0.67.0 — firing self-test
  activeInstallPath, isStaleRelicContext,                              // v0.49.1 — stale-relic downgrade guard
  SETTINGS_HOOK_DESC, OUR_HOOK_SCRIPTS, OUR_DESCRIPTIONS,              // v0.32.0 — for tests
  PLUGIN_ROOT,                                                         // v0.32.1 — for tests / consumers
  registerStatuslineProvider, unregisterStatuslineProvider, detachStatuslineIntegration,
  installedGlobalPkgs, GLOBAL_INSTALL_MARKER, INSTALL_LOCK_FILE, SHELL_PKG,   // uninstall residue
  PLUGIN_ID, OLD_PLUGIN_IDS, MARKETPLACE_NAME, CACHE_DIR, REGISTRY_FILE,
  settingsPath, installedPluginsPath, providersBackupFile, pluginsCacheDir,
};

// CLI: node lifecycle.js <install|uninstall|update|health>
if (require.main === module) {
  const cmd = process.argv[2];
  if (cmd === 'install') {
    // Explicit CLI install = user intent: reset any statusline stand-down and re-claim.
    const r = install({ reclaimStatusline: true });
    // Refusing to touch an unusable settings.json means NOTHING was installed —
    // no hooks, no statusline, no manifest stamp. Printing "Installed" and
    // exiting 0 there would make `lifecycle.js install && …` chains read the
    // refusal as success; the true diagnosis is already on stderr.
    if (r.settingsUnreadable || r.settingsUnwritable) {
      const why = r.settingsUnreadable ? 'unusable' : 'not writable';
      console.log(`Not installed: ${settingsPath()} is ${why} (see the error above). Nothing was changed.`);
      process.exit(1);
    }
    console.log(`Installed v${r.version} | settings=${r.settingsChanged} | statusLine=${r.statusLineClaimed}`);
  } else if (cmd === 'uninstall') {
    const r = uninstall({
      purgeGlobal: process.argv.includes('--purge-global'),
      unadoptAll: process.argv.includes('--unadopt-all'),
    });
    console.log(`Uninstalled | settings cleaned=${r.settingsChanged}`);
    if (r.unadopted.length) {
      const cleaned = r.unadopted.filter((u) => u.cleaned).length;
      console.log(`  Unadopted ${cleaned}/${r.unadopted.length} registered project(s):`);
      for (const u of r.unadopted) {
        console.log(`    ${u.ok ? (u.cleaned ? 'cleaned' : 'nothing-to-clean') : `FAILED (${u.error || 'unknown'})`}  ${u.project}`);
      }
      console.log('    Their .code-graph/ index dirs are project data — remove per project with `rm -rf .code-graph` if unwanted.');
    }
    if (r.globalPkgsRemoved.length) {
      console.log(`  Removed global npm package(s): ${r.globalPkgsRemoved.join(', ')}`);
    }
    if (r.globalPkgsRemaining.length) {
      console.log(`  Global npm package(s) still installed: ${r.globalPkgsRemaining.join(', ')}`);
      console.log(`    Remove with: npm uninstall -g ${r.globalPkgsRemaining.join(' ')}`);
      if (!r.pluginInstalledGlobals) {
        console.log('    (left in place: no plugin-install marker, so they may be your own install; --purge-global forces removal)');
      }
    }
    if (r.adoptedProjects.length) {
      console.log('  Adopted project(s) still carrying a managed CLAUDE.md block + .code-graph/ index:');
      for (const p of r.adoptedProjects) console.log(`    ${p}`);
      console.log('    Clean all at once: re-run with --unadopt-all, or per project `code-graph-mcp unadopt` + `rm -rf .code-graph`.');
    }
    console.log('  Note: also run `/plugin uninstall code-graph-mcp` inside Claude Code to sync its UI state.');
  } else if (cmd === 'update') {
    const r = update();
    if (r.settingsUnreadable || r.settingsUnwritable) {
      const why = r.settingsUnreadable ? 'unusable' : 'not writable';
      console.log(`Not updated: ${settingsPath()} is ${why} (see the error above). Nothing was changed.`);
      process.exit(1);
    }
    console.log(`Updated ${r.oldVersion} → ${r.version} | settings=${r.settingsChanged}`);
  } else if (cmd === 'health') {
    const r = healthCheck();
    if (r.healthy) {
      console.log('Health: OK — all paths valid');
    } else {
      console.log(`Health: ${r.issues.length} issue(s) found${r.repaired ? ' — repaired' : ''}`);
      for (const issue of r.issues) {
        console.log(`  ${issue.type}: ${issue.path || issue.id}`);
      }
    }
  } else if (cmd === 'doctor') {
    // Delegate to doctor.js's shared CLI so the two entry points cannot drift on
    // flag validation or exit codes. This arm used to do its own
    // `process.argv.includes('--check-only')`, which silently ignored every other
    // argument — `lifecycle.js doctor --check-onlyy` ran the full repair pass.
    // Exit code still reflects issues that remain UNRESOLVED after repair, not
    // issues found (see unresolvedCount in doctor.js).
    //
    // LAZY ON PURPOSE — do not hoist (audit 2026-08-29 JS-13). The plugin's JS
    // has exactly one require cycle: auto-update → lifecycle → doctor →
    // auto-update. Deferring this one edge to call time is the only thing that
    // keeps the top-level graph a DAG. Hoisted, `require('./auto-update')`
    // reaches `doctor` before auto-update has finished evaluating, so doctor
    // sees a half-built module and dies on load — from a module nobody in that
    // chain was even asking about. Pinned by
    // `every_hook_module_loads_first_in_a_cold_process` in lifecycle.test.js,
    // which loads each module first in its own process.
    const { runDoctorCli } = require('./doctor');
    process.exit(runDoctorCli(process.argv.slice(3)));
  } else if (cmd === 'verify-hooks-fire') {
    // v0.67.0 — Layer-A firing self-test. Spawned detached by session-init
    // (off the SessionStart budget); writes a small state file the next
    // SessionStart reads to surface failures.
    const res = verifyHooksFire();
    const failures = res.results.filter(r => !r.ok).map(r => r.label);
    try {
      fs.mkdirSync(CACHE_DIR, { recursive: true });
      writeJsonAtomic(path.join(CACHE_DIR, 'hook-fire-state.json'),
        { ts: new Date().toISOString(), ok: res.ok, failures });
    } catch { /* best-effort telemetry */ }
    console.log(`Hook firing: ${res.ok ? 'OK' : 'FAIL'} (${res.results.length} probed${failures.length ? ', failed: ' + failures.join(', ') : ''})`);
  } else {
    console.error('Usage: lifecycle.js <install|uninstall|update|health|doctor|verify-hooks-fire>');
    process.exit(1);
  }
}
