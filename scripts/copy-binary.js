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

// A raw stack trace out of `npm run build` reads as a bug in this script; the
// two failures that actually happen here are a full disk and an unwritable
// bin/ (audit 2026-08-29 JS-14). The missing-source case above already prints
// a sentence and exits 1 — these now match it rather than throwing past it.
try {
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  fs.copyFileSync(source, dest);
  fs.chmodSync(dest, 0o755);
} catch (err) {
  const why = err && err.code === 'ENOSPC'
    ? 'the disk is full'
    : err && (err.code === 'EACCES' || err.code === 'EPERM')
      ? `${dest} is not writable by this user`
      : (err && err.message) || String(err);
  console.error(`Could not install the binary into ${dest}: ${why}`);
  console.error(`Source is intact at ${source} — fix the above and re-run 'npm run build'.`);
  process.exit(1);
}

const size = (fs.statSync(dest).size / 1024 / 1024).toFixed(1);
console.log(`Copied binary to ${dest} (${size} MB)`);
