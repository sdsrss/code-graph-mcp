# Code Graph Integration Completion Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the code-graph Claude Code plugin integration — StatusLine, version sync, auto-update, and end-to-end validation.

**Architecture:** Session-init.js consolidates three concerns (health check, statusline registration, update check) into a single SessionStart hook script. Version bump script ensures all 8 version locations stay in sync. E2E validation uses a Node.js JSON-RPC test harness against the MCP stdio server.

**Tech Stack:** Node.js (scripts), Bash (bump-version), Rust (existing MCP server), JSON-RPC 2.0 (test harness)

**Spec:** `docs/superpowers/specs/2026-03-13-integration-completion-design.md`

---

## Chunk 1: Version Sync & Bump Script (P1)

### Task 1: Sync All Versions to 0.4.2

**Files:**
- Modify: `package.json:3` (version "0.4.0" → "0.4.2")
- Modify: `package.json:36-40` (optionalDependencies "0.4.0" → "0.4.2")
- Modify: `npm/linux-x64/package.json:3` (version "0.2.0" → "0.4.2")
- Modify: `npm/linux-arm64/package.json:3` (version "0.2.0" → "0.4.2")
- Modify: `npm/darwin-x64/package.json:3` (version "0.2.0" → "0.4.2")
- Modify: `npm/darwin-arm64/package.json:3` (version "0.2.0" → "0.4.2")
- Modify: `npm/win32-x64/package.json:3` (version "0.2.0" → "0.4.2")
- Modify: `claude-plugin/.claude-plugin/plugin.json:7` (version "0.3.0" → "0.4.2")

- [ ] **Step 1: Update root package.json version and optionalDependencies**

```json
"version": "0.4.2",
```
```json
"optionalDependencies": {
    "@sdsrs/code-graph-linux-x64": "0.4.2",
    "@sdsrs/code-graph-linux-arm64": "0.4.2",
    "@sdsrs/code-graph-darwin-x64": "0.4.2",
    "@sdsrs/code-graph-darwin-arm64": "0.4.2",
    "@sdsrs/code-graph-win32-x64": "0.4.2"
}
```

- [ ] **Step 2: Update all 5 platform package.json files**

Each `npm/<platform>/package.json`: change `"version": "0.2.0"` → `"version": "0.4.2"`

- [ ] **Step 3: Update plugin.json version**

`claude-plugin/.claude-plugin/plugin.json`: change `"version": "0.3.0"` → `"version": "0.4.2"`

- [ ] **Step 4: Verify all versions match**

Run:
```bash
echo "Cargo.toml: $(grep '^version' Cargo.toml | head -1)"
echo "Cargo.lock: $(grep 'name = "code-graph-mcp"' -A1 Cargo.lock | grep version)"
echo "package.json: $(node -e "console.log(require('./package.json').version)")"
for f in npm/*/package.json; do echo "$f: $(node -e "console.log(require('./$f').version)")"; done
echo "plugin.json: $(node -e "console.log(JSON.parse(require('fs').readFileSync('claude-plugin/.claude-plugin/plugin.json','utf8')).version)")"
```
Expected: All show `0.4.2`

- [ ] **Step 5: Commit**

```bash
git add package.json npm/*/package.json claude-plugin/.claude-plugin/plugin.json Cargo.lock
git commit -m "chore: sync all versions to 0.4.2"
```

---

### Task 2: Create bump-version.sh

**Files:**
- Create: `scripts/bump-version.sh`

- [ ] **Step 1: Write the bump script**

```bash
#!/bin/bash
set -euo pipefail
VERSION=${1:?Usage: scripts/bump-version.sh <version>}
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

# 1. Cargo.toml (only match version in [package] section, before first [*] section)
sed -i '1,/^\[/s/^version = ".*"/version = "'"$VERSION"'"/' Cargo.toml
echo "Updated Cargo.toml → $VERSION"

# 2. Cargo.lock
cargo update -p code-graph-mcp 2>/dev/null || true
echo "Updated Cargo.lock"

# 3. Root package.json (version + optionalDependencies)
npm version "$VERSION" --no-git-tag-version --allow-same-version
node -e "
  const pkg = require('./package.json');
  for (const key of Object.keys(pkg.optionalDependencies || {})) {
    pkg.optionalDependencies[key] = '$VERSION';
  }
  require('fs').writeFileSync('package.json', JSON.stringify(pkg, null, 2) + '\n');
"
echo "Updated package.json → $VERSION"

# 4. Platform packages
for pkg in npm/*/package.json; do
  (cd "$(dirname "$pkg")" && npm version "$VERSION" --no-git-tag-version --allow-same-version)
done
echo "Updated npm/*/package.json → $VERSION"

# 5. Plugin manifest
node -e "
  const f = 'claude-plugin/.claude-plugin/plugin.json';
  const p = JSON.parse(require('fs').readFileSync(f, 'utf8'));
  p.version = '$VERSION';
  require('fs').writeFileSync(f, JSON.stringify(p, null, 2) + '\n');
"
echo "Updated plugin.json → $VERSION"

echo ""
echo "All versions updated to $VERSION"
echo "Next: git add -A && git commit -m 'chore: bump to $VERSION' && git tag v$VERSION && git push && git push --tags"
```

- [ ] **Step 2: Make executable**

Run: `chmod +x scripts/bump-version.sh`

- [ ] **Step 3: Test the script (dry run to current version)**

Run: `./scripts/bump-version.sh 0.4.2`
Expected: "All versions updated to 0.4.2", no file changes (already at 0.4.2)

- [ ] **Step 4: Commit**

```bash
git add scripts/bump-version.sh
git commit -m "chore: add version bump helper script"
```

---

## Chunk 2: Session Init — Health Check + StatusLine (P0)

### Task 3: Create session-init.js

**Files:**
- Create: `claude-plugin/scripts/session-init.js`

- [ ] **Step 1: Write session-init.js with health check and statusline registration**

```javascript
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
```

- [ ] **Step 2: Test session-init.js standalone**

Run: `node claude-plugin/scripts/session-init.js`
Expected: Prints health-check oneline output (or nothing if binary not in PATH). No errors.

- [ ] **Step 3: Verify marker file created**

Run: `ls -la ~/.cache/code-graph/statusline-registered`
Expected: File exists with current timestamp.

- [ ] **Step 4: Verify idempotency — run again**

Run: `node claude-plugin/scripts/session-init.js`
Expected: Same output, no settings.json modification (marker exists).

---

### Task 4: Update hooks.json to Use session-init.js

**Files:**
- Modify: `claude-plugin/hooks/hooks.json:16-27`

- [ ] **Step 1: Replace SessionStart hook**

Replace the entire SessionStart section:
```json
"SessionStart": [
  {
    "hooks": [
      {
        "type": "command",
        "command": "node claude-plugin/scripts/session-init.js",
        "timeout": 8
      }
    ],
    "description": "Health check, StatusLine registration, and update check at session start"
  }
]
```

**Note on path resolution**: The command path `claude-plugin/scripts/session-init.js` is relative to cwd. This works for local development (run from repo root). When distributed as an npm package, `claude-plugin/` is included in `package.json` `files` array, so it ships alongside the binary. For global plugin installs, the SessionStart hook would need updating to use an absolute path resolved at install time. This is acceptable for self-use; address for open-source distribution later.

- [ ] **Step 2: Validate hooks.json is valid JSON**

Run: `node -e "JSON.parse(require('fs').readFileSync('claude-plugin/hooks/hooks.json','utf8')); console.log('OK')"`
Expected: `OK`

- [ ] **Step 3: Commit P0 changes**

```bash
git add claude-plugin/scripts/session-init.js claude-plugin/hooks/hooks.json
git commit -m "feat(plugin): add session-init with health check and StatusLine registration"
```

---

## Chunk 3: Auto-Update Check (P4)

### Task 5: Add Update Check to session-init.js

**Files:**
- Modify: `claude-plugin/scripts/session-init.js` (append update check)

- [ ] **Step 1: Add update check function to session-init.js**

Append after the StatusLine registration section:

```javascript
// --- 3. Update check (once per 24h, non-blocking) ---
(async () => {
  try {
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
        `[code-graph] Update available: ${currentVersion} → ${latest}. ` +
        `Run: npx @sdsrs/code-graph@latest\n`
      );
    }

    // Update cache timestamp
    fs.mkdirSync(MARKER_DIR, { recursive: true });
    fs.writeFileSync(CHECK_CACHE, new Date().toISOString());
  } catch { /* network or parse error — silent */ }
})();
```

- [ ] **Step 2: Test update check**

Run: `node claude-plugin/scripts/session-init.js`
Expected: Health check output + (if version is behind latest release) update notice on stderr. If version matches, no update notice.

- [ ] **Step 3: Verify 24h cache**

Run: `cat ~/.cache/code-graph/update-check`
Expected: ISO timestamp.

Run again: `node claude-plugin/scripts/session-init.js`
Expected: No GitHub API call (cached). Verify with: `ls -la ~/.cache/code-graph/update-check` — timestamp unchanged.

- [ ] **Step 4: Commit**

```bash
git add claude-plugin/scripts/session-init.js
git commit -m "feat(plugin): add auto-update check with 24h rate limiting"
```

---

## Chunk 4: E2E Validation (P2)

### Task 6: Create E2E Validation Script

**Files:**
- Create: `scripts/e2e-validate.js`

- [ ] **Step 1: Write the MCP JSON-RPC test harness**

```javascript
#!/usr/bin/env node
'use strict';
const { spawn } = require('child_process');
const path = require('path');
const readline = require('readline');

const BINARY = process.env.CODE_GRAPH_BIN || 'code-graph-mcp';
let requestId = 0;
let child;
let rl;
const pending = new Map();

function send(method, params) {
  return new Promise((resolve, reject) => {
    const id = ++requestId;
    const msg = JSON.stringify({ jsonrpc: '2.0', id, method, params });
    pending.set(id, { resolve, reject, method });
    child.stdin.write(msg + '\n');
    // Timeout per request
    setTimeout(() => {
      if (pending.has(id)) {
        pending.delete(id);
        reject(new Error(`Timeout: ${method}`));
      }
    }, 30000);
  });
}

function notify(method, params) {
  const msg = JSON.stringify({ jsonrpc: '2.0', method, params });
  child.stdin.write(msg + '\n');
}

async function callTool(name, args) {
  const result = await send('tools/call', { name, arguments: args });
  return result;
}

async function run() {
  console.log('=== Code Graph MCP E2E Validation ===\n');

  // Spawn MCP server
  child = spawn(BINARY, ['serve'], {
    stdio: ['pipe', 'pipe', 'pipe'],
    cwd: process.cwd()
  });

  child.stderr.on('data', () => {}); // suppress tracing logs

  rl = readline.createInterface({ input: child.stdout });
  rl.on('line', (line) => {
    try {
      const msg = JSON.parse(line);
      if (msg.id && pending.has(msg.id)) {
        const { resolve, reject } = pending.get(msg.id);
        pending.delete(msg.id);
        if (msg.error) reject(new Error(`${msg.error.message} (${msg.error.code})`));
        else resolve(msg.result);
      }
    } catch { /* ignore non-JSON lines */ }
  });

  // Wait for process to be ready
  await new Promise(r => setTimeout(r, 500));

  // Initialize
  const initResult = await send('initialize', {
    protocolVersion: '2024-11-05',
    capabilities: {},
    clientInfo: { name: 'e2e-test', version: '1.0.0' }
  });
  console.log(`Server: ${initResult.serverInfo.name} v${initResult.serverInfo.version}`);
  console.log(`Tools: ${initResult.capabilities.tools ? 'yes' : 'no'}`);
  console.log(`Resources: ${initResult.capabilities.resources ? 'yes' : 'no'}`);
  console.log(`Prompts: ${initResult.capabilities.prompts ? 'yes' : 'no'}`);
  notify('notifications/initialized', {});

  // Wait for startup indexing
  console.log('\nWaiting for startup indexing (10s)...');
  await new Promise(r => setTimeout(r, 10000));

  // --- Phase 0: tools/list sanity check ---
  console.log('\n--- Phase 0: Tool List ---');
  const toolList = await send('tools/list', {});
  const toolCount = toolList.tools?.length || 0;
  console.log(`  tools/list: ${toolCount} tools registered`);
  if (toolCount !== 14) {
    console.log(`  WARNING: Expected 14 tools, got ${toolCount}`);
  }

  // --- Phase 1: Index status ---
  console.log('\n--- Phase 1: Index Status ---');
  const status = await callTool('get_index_status', {});
  console.log('  get_index_status: OK');
  const statusText = status.content?.[0]?.text || '';
  console.log(`  ${statusText.substring(0, 200)}`);

  // --- Phase 2: Tool validation ---
  console.log('\n--- Phase 2: Tool Validation ---');

  const tests = [
    ['semantic_code_search', { query: 'handle tool call' }],
    ['get_call_graph', { symbol_name: 'handle_call_tool', direction: 'both', depth: 2 }],
    ['find_http_route', { route_path: '/api/test' }],
    ['trace_http_chain', { route_path: '/api/test', depth: 3 }],
    ['get_ast_node', { file_path: 'src/mcp/server.rs', symbol_name: 'McpServer' }],
    ['impact_analysis', { symbol_name: 'handle_call_tool' }],
    ['module_overview', { path: 'src/mcp' }],
    ['dependency_graph', { file_path: 'src/mcp/server.rs' }],
    ['find_similar_code', { symbol_name: 'compress_if_needed' }],
  ];

  let passed = 0;
  let failed = 0;
  let nodeIdForSnippet = null;

  for (const [tool, args] of tests) {
    try {
      const result = await callTool(tool, args);
      const text = result.content?.[0]?.text || '';
      const bytes = Buffer.byteLength(text, 'utf8');
      const approxTokens = Math.round(bytes / 4);
      console.log(`  ${tool}: OK (${bytes} bytes, ~${approxTokens} tok)`);

      // Capture a node_id for read_snippet test
      if (tool === 'get_ast_node' && text) {
        try {
          const parsed = JSON.parse(text);
          if (parsed.node_id) nodeIdForSnippet = parsed.node_id;
        } catch {}
      }
      passed++;
    } catch (err) {
      console.log(`  ${tool}: FAIL — ${err.message}`);
      failed++;
    }
  }

  // read_snippet (depends on get_ast_node result)
  if (nodeIdForSnippet) {
    try {
      await callTool('read_snippet', { node_id: nodeIdForSnippet, context_lines: 3 });
      console.log(`  read_snippet: OK`);
      passed++;
    } catch (err) {
      console.log(`  read_snippet: FAIL — ${err.message}`);
      failed++;
    }
  } else {
    console.log(`  read_snippet: SKIP (no node_id from get_ast_node)`);
  }

  // Watcher tools
  try {
    await callTool('start_watch', {});
    console.log(`  start_watch: OK`);
    passed++;
    await callTool('stop_watch', {});
    console.log(`  stop_watch: OK`);
    passed++;
  } catch (err) {
    console.log(`  start_watch/stop_watch: FAIL — ${err.message}`);
    failed += 2;
  }

  // rebuild_index (last, as it may take time)
  try {
    await callTool('rebuild_index', {});
    console.log(`  rebuild_index: OK`);
    passed++;
  } catch (err) {
    console.log(`  rebuild_index: FAIL — ${err.message}`);
    failed++;
  }

  // --- Phase 3: Resources ---
  console.log('\n--- Phase 3: Resources ---');
  try {
    const resList = await send('resources/list', {});
    console.log(`  resources/list: ${resList.resources?.length || 0} resources`);
    if (resList.resources?.length > 0) {
      const uri = resList.resources[0].uri;
      const resRead = await send('resources/read', { uri });
      const text = resRead.contents?.[0]?.text || '';
      console.log(`  resources/read ${uri}: OK (${Buffer.byteLength(text)} bytes)`);
    }
  } catch (err) {
    console.log(`  resources: FAIL — ${err.message}`);
  }

  // --- Phase 4: Prompts ---
  console.log('\n--- Phase 4: Prompts ---');
  try {
    const promptList = await send('prompts/list', {});
    console.log(`  prompts/list: ${promptList.prompts?.length || 0} prompts`);
    for (const p of (promptList.prompts || []).slice(0, 3)) {
      const promptGet = await send('prompts/get', { name: p.name });
      const msgCount = promptGet.messages?.length || 0;
      console.log(`  prompts/get ${p.name}: OK (${msgCount} messages)`);
    }
  } catch (err) {
    console.log(`  prompts: FAIL — ${err.message}`);
  }

  // --- Phase 5: Token efficiency check ---
  console.log('\n--- Phase 5: Token Efficiency ---');
  // Computed from Phase 2 results (bytes/4 approximation already logged)
  // No assertion here — logged for manual review

  // --- Summary ---
  console.log(`\n=== Summary: ${passed} passed, ${failed} failed ===`);

  child.kill();
  process.exit(failed > 0 ? 1 : 0);
}

run().catch(err => {
  console.error('Fatal:', err);
  if (child) child.kill();
  process.exit(1);
});
```

- [ ] **Step 2: Make executable**

Run: `chmod +x scripts/e2e-validate.js`

- [ ] **Step 3: Build the binary**

Run: `cargo build --release`
Expected: Build succeeds.

- [ ] **Step 4: Run E2E validation**

Run: `CODE_GRAPH_BIN=./target/release/code-graph-mcp node scripts/e2e-validate.js`
Expected: All tools pass (possibly except HTTP tools which return empty results — that's OK). Summary shows 0 failed.

- [ ] **Step 5: Commit**

```bash
git add scripts/e2e-validate.js
git commit -m "test(e2e): add MCP JSON-RPC end-to-end validation script"
```

---

### Task 7: Run Full Validation & Final Commit

- [ ] **Step 1: Run CLI health check**

Run: `./target/release/code-graph-mcp health-check --format json`
Expected: JSON with `healthy: true`, node/file counts.

- [ ] **Step 2: Run CLI incremental-index**

Run: `./target/release/code-graph-mcp incremental-index --quiet`
Expected: Exit code 0, no output (quiet mode).

- [ ] **Step 3: Run session-init.js**

Run: `node claude-plugin/scripts/session-init.js`
Expected: Health check oneline output. StatusLine registered (check `~/.claude/settings.json`).

- [ ] **Step 4: Run E2E validation**

Run: `CODE_GRAPH_BIN=./target/release/code-graph-mcp node scripts/e2e-validate.js`
Expected: All tools pass, 0 failed.

- [ ] **Step 5: Verify bump script**

Run: `./scripts/bump-version.sh 0.4.2`
Expected: "All versions updated to 0.4.2", no file diffs (already at 0.4.2).

- [ ] **Step 6: Commit E2E results**

If any fixes were needed during validation, commit them now.

---

### Task 8: Manual Verification Checklist

These items require a live Claude Code session. Run through them after all automated checks pass.

- [ ] **Step 1: Hooks — PostToolUse incremental-index**

In a Claude Code session in this project:
1. Ask Claude to edit a file (e.g., add a comment to `src/main.rs`)
2. Check stderr/logs for `code-graph-mcp incremental-index --quiet` execution
3. Verify: index updated within 10 seconds of edit

- [ ] **Step 2: Hooks — SessionStart**

Start a new Claude Code session:
1. Verify session-init.js runs (health check output in session log)
2. Check `~/.claude/settings.json` for statusLine config

- [ ] **Step 3: Commands — /impact**

In Claude Code session, try: `/impact handle_call_tool`
Expected: Structured output showing callers, affected files, risk level.

- [ ] **Step 4: Commands — /trace**

Try: `/trace /api/test` (will show no routes for Rust project — verify graceful handling)

- [ ] **Step 5: Commands — /understand**

Try: `/understand src/mcp`
Expected: Module overview with exports, dependencies, summary.

- [ ] **Step 6: Tag and release if all manual checks pass**

This step requires user confirmation:
```bash
git tag v0.4.2
git push && git push --tags
```
