#!/usr/bin/env node

const fs = require("fs");
const path = require("path");
const os = require("os");

const binaryName = os.platform() === "win32" ? "code-graph-mcp.exe" : "code-graph-mcp";
const source = path.join(__dirname, "..", "target", "release", binaryName);
const dest = path.join(__dirname, "..", "bin", binaryName);

if (!fs.existsSync(source)) {
  console.error(`Binary not found at ${source}`);
  console.error("Run 'cargo build --release --no-default-features' first.");
  process.exit(1);
}

fs.copyFileSync(source, dest);
fs.chmodSync(dest, 0o755);

const size = (fs.statSync(dest).size / 1024 / 1024).toFixed(1);
console.log(`Copied binary to ${dest} (${size} MB)`);
