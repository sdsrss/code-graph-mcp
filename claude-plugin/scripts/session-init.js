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
