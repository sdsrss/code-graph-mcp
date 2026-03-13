# Code Graph Integration Completion Design

**Date**: 2026-03-13
**Scope**: P0 StatusLine + P1 Version Sync & Release + P4 Plugin Auto-Update + P2 E2E Validation
**Target user**: Self-use first, open-source later

---

## P0: StatusLine Auto-Registration

### Current State
- `claude-plugin/scripts/statusline.js` exists, outputs: `code-graph: ✓ 1247 nodes | 42 files | watching`
- No auto-registration — user must manually configure `~/.claude/settings.json`
- GSD plugin currently occupies the global `statusLine` singleton

### Design

**Approach: Composable StatusLine** — since `statusLine` is a global singleton and GSD already occupies it, we compose code-graph status into the existing statusline rather than competing for the slot.

**New file**: `claude-plugin/scripts/session-init.js`

Session-init.js responsibilities:
1. **Health check**: Run `code-graph-mcp health-check --format oneline` and output to stdout (preserves existing SessionStart behavior)
2. **StatusLine composition**: Check `~/.claude/settings.json`:
   - If `statusLine` is absent → register code-graph statusline
   - If `statusLine` already points to code-graph → no-op (idempotent)
   - If `statusLine` points to another plugin (e.g., GSD) → inject code-graph status into that script by writing a wrapper, OR skip and rely on the health-check output
3. **One-time registration**: Use a marker file `~/.cache/code-graph/statusline-registered` to avoid re-checking every session. Only attempt registration on first run or when marker is missing.

**Practical self-use approach**: Since both GSD and code-graph are self-controlled, the simplest path is to modify `statusline.js` to be callable as a module, then have the GSD statusline script import and append code-graph status. For open-source, fall back to "register if slot is empty, skip if occupied."

**Health check in session-init.js** (explicit):
```javascript
const { execSync } = require('child_process');
try {
  const output = execSync('code-graph-mcp health-check --format oneline', {
    timeout: 3000, stdio: ['pipe', 'pipe', 'pipe']
  }).toString().trim();
  process.stdout.write(output);
} catch { /* silently ignore */ }
```

**Modify**: `claude-plugin/hooks/hooks.json` — replace existing SessionStart health-check with session-init.js.

```json
"SessionStart": [
  {
    "hooks": [{
      "type": "command",
      "command": "node /path/to/claude-plugin/scripts/session-init.js",
      "timeout": 5
    }],
    "description": "Health check and StatusLine registration at session start"
  }
]
```

### Acceptance Criteria
- [ ] StatusLine shows code-graph status (standalone or composed with GSD)
- [ ] Health check output appears in session start log
- [ ] Does not overwrite non-code-graph statusline configs
- [ ] Idempotent — running twice produces same result
- [ ] No file corruption risk (one-time registration with marker file, not every-session write)

---

## P1: Version Sync & Automated Release

### Current State
- CI/CD fully functional: `release.yml` does 5-platform build → npm publish → GitHub Release
- Version mismatch: Cargo.toml=0.4.2, package.json=0.4.0, npm/*=0.2.0, plugin.json=0.3.0
- CI already extracts version from tag and syncs to package.json during publish

### Design
1. **Sync all local versions to 0.4.2** (match Cargo.toml as source of truth):
   - `Cargo.toml` → 0.4.2 (already correct)
   - `package.json` → 0.4.2 (version + optionalDependencies)
   - `npm/linux-x64/package.json` → 0.4.2
   - `npm/linux-arm64/package.json` → 0.4.2
   - `npm/darwin-x64/package.json` → 0.4.2
   - `npm/darwin-arm64/package.json` → 0.4.2
   - `npm/win32-x64/package.json` → 0.4.2
   - `claude-plugin/.claude-plugin/plugin.json` → 0.4.2

2. **Add version bump helper script** `scripts/bump-version.sh`:
   ```bash
   #!/bin/bash
   set -euo pipefail
   VERSION=${1:?Usage: bump-version.sh <version>}

   # Update Cargo.toml
   sed -i "s/^version = .*/version = \"$VERSION\"/" Cargo.toml

   # Regenerate Cargo.lock
   cargo update -p code-graph-mcp

   # Update root package.json version
   npm version "$VERSION" --no-git-tag-version --allow-same-version

   # Update optionalDependencies in root package.json
   node -e "
     const pkg = require('./package.json');
     for (const key of Object.keys(pkg.optionalDependencies || {})) {
       pkg.optionalDependencies[key] = '$VERSION';
     }
     require('fs').writeFileSync('package.json', JSON.stringify(pkg, null, 2) + '\n');
   "

   # Update platform package.json files (subshell to avoid cd issues)
   for pkg in npm/*/package.json; do
     (cd "$(dirname "$pkg")" && npm version "$VERSION" --no-git-tag-version --allow-same-version)
   done

   # Update plugin.json
   node -e "
     const f = 'claude-plugin/.claude-plugin/plugin.json';
     const p = JSON.parse(require('fs').readFileSync(f, 'utf8'));
     p.version = '$VERSION';
     require('fs').writeFileSync(f, JSON.stringify(p, null, 2) + '\n');
   "

   echo "All versions updated to $VERSION"
   ```

3. **Release flow**: `./scripts/bump-version.sh 0.5.0 && git add -A && git commit -m "chore: bump to 0.5.0" && git tag v0.5.0 && git push && git push --tags`

### Acceptance Criteria
- [ ] All version numbers consistent across all 8 files (Cargo.toml, Cargo.lock, package.json, 5x npm/*/package.json, plugin.json)
- [ ] `git tag v0.4.2 && git push --tags` triggers successful CI release
- [ ] npm packages published with correct version
- [ ] GitHub Release created with binaries

---

## P4: Plugin Auto-Update

### Current State
- Plugin structure exists in `claude-plugin/`
- No auto-update mechanism
- npm distribution exists (`@sdsrs/code-graph`)

### Design
Integrate auto-update check into `session-init.js`:

1. On session start, check current installed version vs latest GitHub Release
2. If newer version available, print update notice to stderr (visible but non-blocking)
3. User can run `npx @sdsrs/code-graph@latest` to update

**Implementation in session-init.js**:
```javascript
const path = require('path');
const os = require('os');
const fs = require('fs');

async function checkUpdate() {
  const pluginJson = path.join(__dirname, '..', '.claude-plugin', 'plugin.json');
  const currentVersion = JSON.parse(fs.readFileSync(pluginJson, 'utf8')).version;

  // Rate limiting: check once per 24h
  const cacheDir = path.join(os.homedir(), '.cache', 'code-graph');
  const cacheFile = path.join(cacheDir, 'update-check');
  try {
    const stat = fs.statSync(cacheFile);
    if (Date.now() - stat.mtimeMs < 86400000) return; // < 24h, skip
  } catch { /* file doesn't exist, proceed */ }

  try {
    const res = await fetch(
      'https://api.github.com/repos/sdsrss/code-graph-mcp/releases/latest',
      { signal: AbortSignal.timeout(2000) }
    );
    if (!res.ok) return; // rate-limited or error, silently skip
    const data = await res.json();
    if (!data.tag_name) return;

    const latest = data.tag_name.replace(/^v/, '');

    // Semver comparison (X.Y.Z only, no pre-release)
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
    fs.mkdirSync(cacheDir, { recursive: true });
    fs.writeFileSync(cacheFile, new Date().toISOString());
  } catch { /* network failure, silently ignore */ }
}
```

### Acceptance Criteria
- [ ] Update notification appears when newer version exists on GitHub Releases
- [ ] No notification when up-to-date or when local version is newer (dev mode)
- [ ] Check cached for 24h in `~/.cache/code-graph/update-check` (no spam)
- [ ] Network failure silently ignored (no error output)
- [ ] GitHub API 403/404 silently ignored

---

## P2: End-to-End Validation

### Current State
- Unit tests exist for individual modules (`cargo test`)
- Integration tests exist in `tests/integration.rs`
- No systematic end-to-end validation of the full plugin experience

### Design
Dogfood: use code-graph to index itself, then systematically validate all integration points.

### Test Harness Approach
Use a Node.js script (`scripts/e2e-validate.js`) that:
1. Spawns `code-graph-mcp serve` as a child process (stdio pipes)
2. Sends MCP `initialize` request + `initialized` notification
3. Sends `tools/call` requests for each tool
4. Validates response structure (has `content`, no `error`)
5. Kills process on completion

This reuses the same JSON-RPC over stdio protocol that Claude Code uses.

#### Phase 1: CLI Health
```bash
code-graph-mcp health-check --format json  # Should show node/file counts
code-graph-mcp incremental-index --quiet    # Should complete without error
```

#### Phase 2: All 14 Tools via MCP
Test each tool against the code-graph-mcp codebase itself:

| Tool | Test Query | Skip? |
|------|------------|-------|
| semantic_code_search | "handle tool call" | |
| get_call_graph | symbol="handle_call_tool", direction="both" | |
| find_http_route | route="/api/test" | Expected: no result (no HTTP routes in Rust) |
| trace_http_chain | route="/api/test" | Expected: no result |
| get_ast_node | file_path="src/mcp/server.rs", symbol_name="McpServer" | |
| read_snippet | Use node_id from previous result | |
| impact_analysis | symbol_name="handle_call_tool" | |
| module_overview | path="src/mcp" | |
| dependency_graph | file_path="src/mcp/server.rs" | |
| find_similar_code | symbol_name="compress_if_needed" | |
| start_watch | Start watcher | |
| stop_watch | Stop watcher | |
| get_index_status | Query status | |
| rebuild_index | Force rebuild | |

Note: `find_http_route` and `trace_http_chain` are tested with expected-empty results. Full HTTP tool validation deferred to a separate test with a Go/TS fixture project.

#### Phase 3: Hooks Validation (Manual)
1. Start a Claude Code session in the project
2. Edit a file → verify `PostToolUse` hook triggers incremental-index
3. Verify SessionStart hook runs health-check

#### Phase 4: Commands Validation (Manual)
Test /impact, /trace, /understand commands in a Claude Code session.

#### Phase 5: Token Efficiency Spot Check
For 3 representative queries, measure JSON response byte size as proxy for token cost (bytes / 4 ≈ tokens):
- semantic_code_search result
- get_call_graph result
- impact_analysis result

Compare with equivalent Grep+Read approach (estimated by grep output byte size).

### Acceptance Criteria
- [ ] CLI health-check and incremental-index succeed
- [ ] All 14 tools return valid JSON-RPC responses (no errors)
- [ ] HTTP tools gracefully handle no-match case
- [ ] Hooks trigger correctly on file edits (manual verification)
- [ ] Commands produce useful structured output (manual verification)
- [ ] Token efficiency: tool results < 2000 tokens per call on average (measured by bytes/4)

---

## Execution Order

```
P0 (StatusLine)  ─┐
P1 (Version Sync) ─┼─→ P2 (E2E Validation) ─→ Tag & Release
P4 (Auto-Update)  ─┘
```

P0, P1, P4 are independent and can be parallelized.
P2 depends on all three being complete.
Tag & Release is the final step after P2 passes.

---

## Files to Create/Modify

| Action | File |
|--------|------|
| Create | `claude-plugin/scripts/session-init.js` (health check + statusline registration + update check) |
| Modify | `claude-plugin/hooks/hooks.json` (SessionStart → session-init.js) |
| Modify | `claude-plugin/.claude-plugin/plugin.json` (version → 0.4.2) |
| Modify | `package.json` (version + optionalDependencies → 0.4.2) |
| Modify | `npm/linux-x64/package.json` (version → 0.4.2) |
| Modify | `npm/linux-arm64/package.json` (version → 0.4.2) |
| Modify | `npm/darwin-x64/package.json` (version → 0.4.2) |
| Modify | `npm/darwin-arm64/package.json` (version → 0.4.2) |
| Modify | `npm/win32-x64/package.json` (version → 0.4.2) |
| Modify | `Cargo.lock` (regenerated by cargo update) |
| Create | `scripts/bump-version.sh` |
| Create | `scripts/e2e-validate.js` (Node.js MCP test harness) |
