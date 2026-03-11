#!/usr/bin/env node

const { execFileSync, spawn } = require("child_process");
const path = require("path");
const fs = require("fs");
const os = require("os");

function getBinaryName() {
  return os.platform() === "win32" ? "code-graph-mcp.exe" : "code-graph-mcp";
}

function findBinary() {
  const binaryName = getBinaryName();

  // 1. Check bundled binary in the same directory
  const bundled = path.join(__dirname, binaryName);
  if (fs.existsSync(bundled)) {
    return bundled;
  }

  // 2. Check cargo build output (for development)
  const cargoRelease = path.join(__dirname, "..", "target", "release", binaryName);
  if (fs.existsSync(cargoRelease)) {
    return cargoRelease;
  }

  // 3. Check if available in PATH
  try {
    const which = os.platform() === "win32" ? "where" : "which";
    const result = execFileSync(which, [binaryName], { encoding: "utf8" }).trim();
    if (result) return result;
  } catch {
    // not in PATH
  }

  return null;
}

const binary = findBinary();

if (!binary) {
  console.error(
    "Error: code-graph-mcp binary not found.\n\n" +
    "To build from source:\n" +
    "  cargo build --release --no-default-features\n\n" +
    "Or install the platform-specific binary."
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
