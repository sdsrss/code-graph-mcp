#!/usr/bin/env node
'use strict';
const { execFileSync } = require('child_process');
const { findBinary } = require('./find-binary');

const bin = findBinary();
if (!bin) process.exit(0); // silent — binary not installed yet

try {
  execFileSync(bin, ['incremental-index', '--quiet'], {
    timeout: 8000,
    stdio: ['pipe', 'pipe', 'pipe']
  });
} catch { /* timeout or error — silent for hook */ }
