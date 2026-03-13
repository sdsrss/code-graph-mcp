#!/usr/bin/env node
'use strict';
const { execSync } = require('child_process');
const fs = require('fs');
const path = require('path');
const os = require('os');

// --- 1. Health check (always runs) ---
try {
  const out = execSync('code-graph-mcp health-check --format oneline', {
    timeout: 2000,
    stdio: ['pipe', 'pipe', 'pipe']
  }).toString().trim();
  if (out) process.stdout.write(out);
} catch { /* binary not found or timeout — silent */ }

// --- 2. StatusLine registration (one-time) ---
const MARKER_DIR = path.join(os.homedir(), '.cache', 'code-graph');
const MARKER_FILE = path.join(MARKER_DIR, 'statusline-registered');

if (!fs.existsSync(MARKER_FILE)) {
  try {
    const settingsPath = path.join(os.homedir(), '.claude', 'settings.json');
    let settings = {};
    try { settings = JSON.parse(fs.readFileSync(settingsPath, 'utf8')); } catch { /* no settings yet */ }

    const statuslineScript = path.resolve(__dirname, 'statusline.js');

    if (!settings.statusLine) {
      // Slot is empty — claim it
      settings.statusLine = {
        type: 'command',
        command: `node ${JSON.stringify(statuslineScript)}`
      };
      // Atomic write
      const tmpFile = settingsPath + '.tmp.' + process.pid;
      fs.writeFileSync(tmpFile, JSON.stringify(settings, null, 2) + '\n');
      fs.renameSync(tmpFile, settingsPath);
    }

    // Write marker regardless (slot empty or occupied, we checked once)
    fs.mkdirSync(MARKER_DIR, { recursive: true });
    fs.writeFileSync(MARKER_FILE, new Date().toISOString());
  } catch { /* settings write failed — not critical */ }
}

// --- 3. Update check (once per 24h, non-blocking) ---
(async () => {
  try {
    fs.mkdirSync(MARKER_DIR, { recursive: true });
    const CHECK_CACHE = path.join(MARKER_DIR, 'update-check');
    try {
      const stat = fs.statSync(CHECK_CACHE);
      if (Date.now() - stat.mtimeMs < 86400000) return; // checked within 24h
    } catch { /* no cache file, proceed */ }

    const pluginJson = path.join(__dirname, '..', '.claude-plugin', 'plugin.json');
    const currentVersion = JSON.parse(fs.readFileSync(pluginJson, 'utf8')).version;

    const res = await fetch(
      'https://api.github.com/repos/sdsrss/code-graph-mcp/releases/latest',
      { signal: AbortSignal.timeout(2000) }
    );
    if (!res.ok) return;
    const data = await res.json();
    if (!data.tag_name) return;

    const latest = data.tag_name.replace(/^v/, '');

    // Simple semver comparison (X.Y.Z)
    const toNum = (v) => v.split('.').map(Number);
    const [lM, lm, lp] = toNum(latest);
    const [cM, cm, cp] = toNum(currentVersion);
    const isNewer = lM > cM || (lM === cM && lm > cm) || (lM === cM && lm === cm && lp > cp);

    if (isNewer) {
      process.stderr.write(
        `[code-graph] Update available: ${currentVersion} \u2192 ${latest}. ` +
        `Run: npx @sdsrs/code-graph@latest\n`
      );
    }

    // Update cache timestamp
    fs.writeFileSync(CHECK_CACHE, new Date().toISOString());
  } catch { /* network or parse error — silent */ }
})();
