#!/usr/bin/env node
'use strict';
/**
 * Composite StatusLine — combines multiple statusline providers.
 * Reads stdin (JSON context from Claude Code), pipes to the primary
 * statusline (GSD), then appends code-graph status.
 */
const { execFileSync } = require('child_process');
const path = require('path');
const { readRegistry } = require('./lifecycle');

const SEPARATOR = ' \x1b[2m|\x1b[0m ';

// Collect stdin (Claude Code pipes JSON context)
let stdinData = '';
let ran = false;
const stdinTimeout = setTimeout(() => { if (!ran) { ran = true; run(''); } }, 2000);
process.stdin.setEncoding('utf8');
process.stdin.on('data', (chunk) => { stdinData += chunk; });
process.stdin.on('end', () => { clearTimeout(stdinTimeout); if (!ran) { ran = true; run(stdinData); } });

function run(stdin) {
  const registry = readRegistry();
  if (registry.length === 0) {
    // Fallback: no registry, run code-graph only
    const cg = runProvider(codeGraphCommand(), false, stdin);
    if (cg) process.stdout.write(cg);
    return;
  }

  const outputs = [];
  for (const provider of registry) {
    const out = runProvider(provider.command, provider.needsStdin, stdin);
    if (out) outputs.push(out);
  }
  if (outputs.length > 0) {
    process.stdout.write(outputs.join(SEPARATOR));
  }
}

function runProvider(command, needsStdin, stdin) {
  if (!command) return null;
  try {
    // Parse command into executable + args
    const parts = parseCommand(command);
    if (!parts) return null;

    const out = execFileSync(parts[0], parts.slice(1), {
      timeout: 3000,
      stdio: ['pipe', 'pipe', 'pipe'],
      input: needsStdin ? stdin : '',
    }).toString().trim();

    return out || null;
  } catch { return null; }
}

function parseCommand(cmd) {
  // Handle: node "/path/to/script.js"
  const match = cmd.match(/^(\S+)\s+"([^"]+)"(.*)$/);
  if (match) {
    const args = [match[2]];
    if (match[3].trim()) args.push(...match[3].trim().split(/\s+/));
    return [match[1], ...args];
  }
  // Handle: node /path/to/script.js
  const parts = cmd.split(/\s+/);
  return parts.length > 0 ? parts : null;
}

function codeGraphCommand() {
  const pluginRoot = process.env.CLAUDE_PLUGIN_ROOT || path.resolve(__dirname, '..');
  return `node "${path.join(pluginRoot, 'scripts', 'statusline.js')}"`;
}

module.exports = { run };
