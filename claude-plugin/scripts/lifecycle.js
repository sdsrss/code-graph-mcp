#!/usr/bin/env node
'use strict';
const fs = require('fs');
const path = require('path');
const os = require('os');

const PLUGIN_ID = 'code-graph-mcp@code-graph-mcp';
const OLD_PLUGIN_IDS = [
  'code-graph@sdsrss',           // v1 legacy ID
  'code-graph@sdsrss-code-graph', // v2 legacy ID (pre-rename)
];
const MARKETPLACE_NAME = 'code-graph-mcp';
const CACHE_DIR = path.join(os.homedir(), '.cache', 'code-graph');
const PLUGIN_ROOT = process.env.CLAUDE_PLUGIN_ROOT || path.resolve(__dirname, '..');
const MANIFEST_FILE = path.join(CACHE_DIR, 'install-manifest.json');
const SETTINGS_PATH = path.join(os.homedir(), '.claude', 'settings.json');
const INSTALLED_PLUGINS_PATH = path.join(os.homedir(), '.claude', 'plugins', 'installed_plugins.json');
const REGISTRY_FILE = path.join(CACHE_DIR, 'statusline-registry.json');

// --- Helpers ---

function readJson(filePath) {
  try { return JSON.parse(fs.readFileSync(filePath, 'utf8')); } catch { return null; }
}

function writeJsonAtomic(filePath, data) {
  const dir = path.dirname(filePath);
  fs.mkdirSync(dir, { recursive: true });
  const tmp = filePath + '.tmp.' + process.pid;
  fs.writeFileSync(tmp, JSON.stringify(data, null, 2) + '\n');
  fs.renameSync(tmp, filePath);
}

function readManifest() {
  return readJson(MANIFEST_FILE) || { version: null, config: {} };
}

function writeManifest(manifest) {
  fs.mkdirSync(CACHE_DIR, { recursive: true });
  writeJsonAtomic(MANIFEST_FILE, manifest);
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
  const installed = readJson(INSTALLED_PLUGINS_PATH);
  return !!(installed && installed.plugins && Array.isArray(installed.plugins[PLUGIN_ID]) && installed.plugins[PLUGIN_ID].length > 0);
}

function isOurComposite(settings) {
  return settings.statusLine &&
    settings.statusLine.command &&
    settings.statusLine.command.includes('statusline-composite');
}

// --- StatusLine Registry ---
// Multiple providers can register. The composite script runs them all.

function readRegistry() {
  return readJson(REGISTRY_FILE) || [];
}

function writeRegistry(registry) {
  if (!registry || registry.length === 0) {
    try { fs.unlinkSync(REGISTRY_FILE); } catch { /* ok */ }
    return;
  }
  writeJsonAtomic(REGISTRY_FILE, registry);
}

function registerStatuslineProvider(id, command, needsStdin) {
  const registry = readRegistry();
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
  const registry = readRegistry();
  const filtered = registry.filter(p => p.id !== id);
  if (filtered.length === registry.length) return false;
  writeRegistry(filtered);
  return true;
}

function isPluginExplicitlyDisabled(settings = readJson(SETTINGS_PATH) || {}) {
  return hasOwn(settings.enabledPlugins, PLUGIN_ID) && settings.enabledPlugins[PLUGIN_ID] === false;
}

function isPluginInactive(settings = readJson(SETTINGS_PATH) || {}) {
  if (isPluginExplicitlyDisabled(settings)) return true;

  const hasComposite = isOurComposite(settings);
  const hasCodeGraphRegistry = readRegistry().some((provider) => provider.id === 'code-graph');
  if (!hasComposite && !hasCodeGraphRegistry) return false;

  const installed = readJson(INSTALLED_PLUGINS_PATH);
  if (!installed || !installed.plugins) return false;
  return !hasInstalledPluginRecord();
}

function detachStatuslineIntegration(settings) {
  let settingsChanged = false;

  unregisterStatuslineProvider('code-graph');
  const previous = readRegistry().find(p => p.id === '_previous' && p.command);

  // If our composite is still configured while the plugin is disabled/uninstalled,
  // prefer restoring the prior statusline (or removing ours entirely) so the plugin
  // truly stops affecting Claude Code.
  if (isOurComposite(settings)) {
    if (previous) {
      settings.statusLine = { type: 'command', command: previous.command };
    } else {
      delete settings.statusLine;
    }
    settingsChanged = true;
  }

  unregisterStatuslineProvider('_previous');
  return settingsChanged;
}

function cleanupDisabledStatusline() {
  const settings = readJson(SETTINGS_PATH);
  if (!settings || !isPluginInactive(settings)) {
    return { cleaned: false, settingsChanged: false };
  }

  const settingsChanged = detachStatuslineIntegration(settings);
  if (settingsChanged) {
    writeJsonAtomic(SETTINGS_PATH, settings);
  }

  return { cleaned: true, settingsChanged };
}

// --- Scope Conflict Detection ---

function checkScopeConflict() {
  const installed = readJson(INSTALLED_PLUGINS_PATH);
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

    // Clean old ID from installed_plugins.json
    const installed = readJson(INSTALLED_PLUGINS_PATH);
    if (installed && installed.plugins && oldId in installed.plugins) {
      delete installed.plugins[oldId];
      writeJsonAtomic(INSTALLED_PLUGINS_PATH, installed);
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
  const oldCacheDirs = [
    path.join(os.homedir(), '.claude', 'plugins', 'cache', 'sdsrss', 'code-graph'),
    path.join(os.homedir(), '.claude', 'plugins', 'cache', 'sdsrss-code-graph', 'code-graph'),
    path.join(os.homedir(), '.claude', 'plugins', 'cache', 'sdsrss-code-graph'),
  ];
  for (const dir of oldCacheDirs) {
    try { fs.rmSync(dir, { recursive: true, force: true }); } catch { /* ok */ }
  }

  return changed;
}

// --- Install (idempotent) ---

function install() {
  const version = getPluginVersion();
  const manifest = readManifest();
  const settings = readJson(SETTINGS_PATH) || {};
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
    // Preserve existing statusline as first provider
    if (settings.statusLine && settings.statusLine.command) {
      registerStatuslineProvider('_previous', settings.statusLine.command, true);
    }
    // Set composite as the statusLine
    settings.statusLine = { type: 'command', command: compositeCommand() };
    settingsChanged = true;
    manifest.config.statusLine = true;
  }

  // Register code-graph provider
  registerStatuslineProvider('code-graph', codeGraphStatuslineCommand(), false);

  // NOTE: enabledPlugins is managed by Claude Code's plugin system, not by lifecycle.
  // Do NOT add enabledPlugins entries here — it causes phantom plugin entries
  // when the ID doesn't match the marketplace name.

  // 2. Write settings atomically if changed
  if (settingsChanged) {
    writeJsonAtomic(SETTINGS_PATH, settings);
  }

  // 3. Write manifest with version
  manifest.version = version;
  manifest.installedAt = manifest.installedAt || new Date().toISOString();
  manifest.updatedAt = new Date().toISOString();
  writeManifest(manifest);

  return { version, settingsChanged, statusLineClaimed: manifest.config.statusLine };
}

// --- Uninstall (clean all config) ---

function uninstall() {
  const settings = readJson(SETTINGS_PATH);
  let settingsChanged = false;

  if (settings) {
    // 1. StatusLine: remove code-graph integration and restore prior statusline.
    if (detachStatuslineIntegration(settings)) {
      settingsChanged = true;
    }

    // 2. Remove all known IDs from enabledPlugins
    if (settings.enabledPlugins) {
      for (const id of [PLUGIN_ID, ...OLD_PLUGIN_IDS]) {
        if (id in settings.enabledPlugins) {
          delete settings.enabledPlugins[id];
          settingsChanged = true;
        }
      }
    }

    // 3. Write settings if changed
    if (settingsChanged) {
      writeJsonAtomic(SETTINGS_PATH, settings);
    }
  }

  // 4. Remove all known IDs from installed_plugins.json
  const installedPlugins = readJson(INSTALLED_PLUGINS_PATH);
  if (installedPlugins && installedPlugins.plugins) {
    let ipChanged = false;
    for (const id of [PLUGIN_ID, ...OLD_PLUGIN_IDS]) {
      if (id in installedPlugins.plugins) {
        delete installedPlugins.plugins[id];
        ipChanged = true;
      }
    }
    if (ipChanged) writeJsonAtomic(INSTALLED_PLUGINS_PATH, installedPlugins);
  }

  // 5. Remove cache directory
  try { fs.rmSync(CACHE_DIR, { recursive: true, force: true }); } catch { /* ok */ }

  // 6. Remove plugin files from cache (all known paths, including parent dirs)
  const pluginCacheDirs = [
    path.join(os.homedir(), '.claude', 'plugins', 'cache', MARKETPLACE_NAME),
    path.join(os.homedir(), '.claude', 'plugins', 'cache', 'sdsrss-code-graph'),
    path.join(os.homedir(), '.claude', 'plugins', 'cache', 'sdsrss', 'code-graph'),
  ];
  for (const dir of pluginCacheDirs) {
    try { fs.rmSync(dir, { recursive: true, force: true }); } catch { /* ok */ }
  }

  return { settingsChanged };
}

// --- Update (refresh config points) ---

function update() {
  const version = getPluginVersion();
  const manifest = readManifest();
  const oldVersion = manifest.version;
  const settings = readJson(SETTINGS_PATH) || {};
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

  // NOTE: enabledPlugins is managed by Claude Code's plugin system, not by lifecycle.

  // 3. Write settings if changed
  if (settingsChanged) {
    writeJsonAtomic(SETTINGS_PATH, settings);
  }

  // 4. Clear update-check cache (force re-check after update)
  const updateCache = path.join(CACHE_DIR, 'update-check');
  try { fs.unlinkSync(updateCache); } catch { /* ok */ }

  // 5. Update manifest
  manifest.version = version;
  manifest.updatedAt = new Date().toISOString();
  writeManifest(manifest);

  return { oldVersion, version, settingsChanged };
}

module.exports = {
  install, uninstall, update, checkScopeConflict,
  isPluginExplicitlyDisabled, isPluginInactive, cleanupDisabledStatusline,
  readManifest, readJson, writeJsonAtomic,
  readRegistry, writeRegistry,
  getPluginVersion,
  PLUGIN_ID, OLD_PLUGIN_IDS, MARKETPLACE_NAME, CACHE_DIR, REGISTRY_FILE,
};

// CLI: node lifecycle.js <install|uninstall|update>
if (require.main === module) {
  const cmd = process.argv[2];
  if (cmd === 'install') {
    const r = install();
    console.log(`Installed v${r.version} | settings=${r.settingsChanged} | statusLine=${r.statusLineClaimed}`);
  } else if (cmd === 'uninstall') {
    const r = uninstall();
    console.log(`Uninstalled | settings cleaned=${r.settingsChanged}`);
  } else if (cmd === 'update') {
    const r = update();
    console.log(`Updated ${r.oldVersion} → ${r.version} | settings=${r.settingsChanged}`);
  } else {
    console.error('Usage: lifecycle.js <install|uninstall|update>');
    process.exit(1);
  }
}
