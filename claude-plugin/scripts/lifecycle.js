#!/usr/bin/env node
'use strict';
const fs = require('fs');
const path = require('path');
const os = require('os');

const PLUGIN_ID = 'code-graph@sdsrss';
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

// --- Install (idempotent) ---

function install() {
  const version = getPluginVersion();
  const manifest = readManifest();
  const settings = readJson(SETTINGS_PATH) || {};
  let settingsChanged = false;

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

  // 2. enabledPlugins — add if missing
  if (!settings.enabledPlugins) settings.enabledPlugins = {};
  if (!(PLUGIN_ID in settings.enabledPlugins)) {
    settings.enabledPlugins[PLUGIN_ID] = true;
    settingsChanged = true;
    manifest.config.enabledPlugins = true;
  }

  // 3. Write settings atomically if changed
  if (settingsChanged) {
    writeJsonAtomic(SETTINGS_PATH, settings);
  }

  // 4. Write manifest with version
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
    // 1. StatusLine: remove code-graph from registry
    unregisterStatuslineProvider('code-graph');
    const remaining = readRegistry();

    if (isOurComposite(settings)) {
      if (remaining.length === 1 && remaining[0].id === '_previous') {
        // Only the previous provider remains — restore it directly
        settings.statusLine = { type: 'command', command: remaining[0].command };
        unregisterStatuslineProvider('_previous');
        settingsChanged = true;
      } else if (remaining.length === 0) {
        // No providers left — remove statusLine entirely
        delete settings.statusLine;
        settingsChanged = true;
      }
      // else: other providers still using composite — leave it
    }

    // 2. Remove from enabledPlugins
    if (settings.enabledPlugins && PLUGIN_ID in settings.enabledPlugins) {
      delete settings.enabledPlugins[PLUGIN_ID];
      settingsChanged = true;
    }

    // 3. Write settings if changed
    if (settingsChanged) {
      writeJsonAtomic(SETTINGS_PATH, settings);
    }
  }

  // 4. Remove from installed_plugins.json
  const installedPlugins = readJson(INSTALLED_PLUGINS_PATH);
  if (installedPlugins && installedPlugins.plugins && PLUGIN_ID in installedPlugins.plugins) {
    delete installedPlugins.plugins[PLUGIN_ID];
    writeJsonAtomic(INSTALLED_PLUGINS_PATH, installedPlugins);
  }

  // 5. Remove cache directory
  try { fs.rmSync(CACHE_DIR, { recursive: true, force: true }); } catch { /* ok */ }

  // 6. Remove plugin files from cache
  const pluginCacheDir = path.join(os.homedir(), '.claude', 'plugins', 'cache', 'sdsrss', 'code-graph');
  try { fs.rmSync(pluginCacheDir, { recursive: true, force: true }); } catch { /* ok */ }

  return { settingsChanged };
}

// --- Update (refresh config points) ---

function update() {
  const version = getPluginVersion();
  const manifest = readManifest();
  const oldVersion = manifest.version;
  const settings = readJson(SETTINGS_PATH) || {};
  let settingsChanged = false;

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

  // 3. Ensure enabledPlugins entry exists
  if (!settings.enabledPlugins) settings.enabledPlugins = {};
  if (!(PLUGIN_ID in settings.enabledPlugins)) {
    settings.enabledPlugins[PLUGIN_ID] = true;
    settingsChanged = true;
  }

  // 4. Write settings if changed
  if (settingsChanged) {
    writeJsonAtomic(SETTINGS_PATH, settings);
  }

  // 5. Clear update-check cache (force re-check after update)
  const updateCache = path.join(CACHE_DIR, 'update-check');
  try { fs.unlinkSync(updateCache); } catch { /* ok */ }

  // 6. Update manifest
  manifest.version = version;
  manifest.updatedAt = new Date().toISOString();
  writeManifest(manifest);

  return { oldVersion, version, settingsChanged };
}

module.exports = {
  install, uninstall, update,
  readManifest, readJson, writeJsonAtomic,
  readRegistry, writeRegistry,
  getPluginVersion,
  PLUGIN_ID, CACHE_DIR, REGISTRY_FILE,
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
