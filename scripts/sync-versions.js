#!/usr/bin/env node
'use strict';
/**
 * Sync version across all project files.
 * Usage: node scripts/sync-versions.js <version>
 * Example: node scripts/sync-versions.js 0.5.27
 */
const fs = require('fs');
const path = require('path');

const version = process.argv[2];
if (!version || !/^\d+\.\d+\.\d+$/.test(version)) {
  console.error('Usage: node scripts/sync-versions.js <semver>');
  console.error('Example: node scripts/sync-versions.js 0.5.27');
  process.exit(1);
}

const root = path.resolve(__dirname, '..');

const updates = [
  {
    file: 'Cargo.toml',
    transform: (content) => content.replace(/^version = ".*"/m, `version = "${version}"`),
  },
  {
    file: 'package.json',
    json: true,
    transform: (obj) => { obj.version = version; return obj; },
  },
  {
    file: 'claude-plugin/.claude-plugin/plugin.json',
    json: true,
    transform: (obj) => { obj.version = version; return obj; },
  },
  {
    file: '.claude-plugin/marketplace.json',
    json: true,
    transform: (obj) => {
      if (obj.metadata) obj.metadata.version = version;
      if (obj.plugins && obj.plugins[0]) obj.plugins[0].version = version;
      return obj;
    },
  },
];

let changed = 0;
for (const { file, json, transform } of updates) {
  const filePath = path.join(root, file);
  if (!fs.existsSync(filePath)) {
    console.warn(`  skip: ${file} (not found)`);
    continue;
  }
  const original = fs.readFileSync(filePath, 'utf8');
  let result;
  if (json) {
    const obj = JSON.parse(original);
    result = JSON.stringify(transform(obj), null, 2) + '\n';
  } else {
    result = transform(original);
  }
  if (result !== original) {
    fs.writeFileSync(filePath, result);
    console.log(`  updated: ${file}`);
    changed++;
  } else {
    console.log(`  unchanged: ${file}`);
  }
}

console.log(`\nVersion synced to ${version} (${changed} file${changed !== 1 ? 's' : ''} updated)`);
