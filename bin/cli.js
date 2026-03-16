#!/usr/bin/env node

const { spawn } = require("child_process");
const path = require("path");

// Tell find-binary.js our package root so it can locate bundled binaries
// and detect dev mode from bin/ → repo root (one level up)
process.env._FIND_BINARY_ROOT = path.resolve(__dirname, "..");

const { findBinary } = require("../claude-plugin/scripts/find-binary");

const binary = findBinary();

if (!binary) {
  console.error(
    "Error: code-graph-mcp binary not found.\n\n" +
    "To install:\n" +
    "  npm install -g @sdsrs/code-graph\n\n" +
    "To build from source:\n" +
    "  cargo build --release\n"
  );
  process.exit(1);
}

// Spawn the binary, forwarding stdio for MCP JSON-RPC communication
const child = spawn(binary, process.argv.slice(2), {
  stdio: "inherit",
  env: process.env,
});

child.on("error", (err) => {
  console.error(`Failed to start code-graph-mcp: ${err.message}`);
  process.exit(1);
});

child.on("exit", (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
  } else {
    process.exit(code ?? 1);
  }
});
