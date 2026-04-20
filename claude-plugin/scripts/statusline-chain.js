#!/usr/bin/env node
'use strict';
// Public CLI for third-party plugins (GSD, etc.) to register a statusline
// provider into code-graph's composite chain.
//
// Usage:
//   node statusline-chain.js register <id> <command> [--stdin]
//   node statusline-chain.js unregister <id>
//   node statusline-chain.js list
//
// Writes to ~/.cache/code-graph/statusline-registry.json (working copy) and
// mirrors to ~/.claude/statusline-providers.json (durable backup). The
// composite script reads both.
//
// Reserved ids: "_previous" (captures pre-install statusline), "code-graph"
// (this plugin's own provider). Third parties should use stable ids like
// "gsd", "claude-mem", etc.

const { readRegistry, registerStatuslineProvider, unregisterStatuslineProvider } = require('./lifecycle');

function usage(code = 1) {
  process.stderr.write(
    'Usage:\n' +
    '  node statusline-chain.js register <id> <command> [--stdin]\n' +
    '  node statusline-chain.js unregister <id>\n' +
    '  node statusline-chain.js list\n'
  );
  process.exit(code);
}

function runRegister(id, command, needsStdin) {
  if (id === 'code-graph' || id === '_previous') {
    process.stderr.write(`error: id "${id}" is reserved\n`);
    process.exit(2);
  }
  if (!id || !command) usage();
  const changed = registerStatuslineProvider(id, command, needsStdin);
  process.stdout.write(changed ? `registered ${id}\n` : `unchanged ${id}\n`);
}

function runUnregister(id) {
  if (!id) usage();
  const changed = unregisterStatuslineProvider(id);
  process.stdout.write(changed ? `unregistered ${id}\n` : `not-found ${id}\n`);
}

function runList() {
  const registry = readRegistry();
  if (registry.length === 0) {
    process.stdout.write('(empty)\n');
    return;
  }
  for (const entry of registry) {
    const stdin = entry.needsStdin ? ' [stdin]' : '';
    process.stdout.write(`${entry.id}${stdin}: ${entry.command}\n`);
  }
}

if (require.main === module) {
  const [, , cmd, ...rest] = process.argv;
  if (cmd === 'register') {
    const needsStdin = rest.includes('--stdin');
    const args = rest.filter((a) => a !== '--stdin');
    runRegister(args[0], args[1], needsStdin);
  } else if (cmd === 'unregister') {
    runUnregister(rest[0]);
  } else if (cmd === 'list') {
    runList();
  } else {
    usage();
  }
}

module.exports = { runRegister, runUnregister, runList };
