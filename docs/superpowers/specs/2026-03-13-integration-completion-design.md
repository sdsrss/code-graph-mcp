# Code Graph Integration Completion Design

**Date**: 2026-03-13
**Scope**: P0 StatusLine + P1 Version Sync & Release + P4 Plugin Auto-Update + P2 E2E Validation
**Target user**: Self-use first, open-source later

---

## P0: StatusLine Auto-Registration

### Current State
- `claude-plugin/scripts/statusline.js` exists, outputs: `code-graph: ✓ 1247 nodes | 42 files | watching`
- No auto-registration — user must manually configure `~/.claude/settings.json`

### Design
Add a SessionStart hook script that auto-registers the StatusLine in `~/.claude/settings.json`.

**New file**: `claude-plugin/scripts/session-init.js`

Behavior:
1. Read `~/.claude/settings.json`
2. Check if `statusLine` is absent or already points to code-graph
3. If absent: write code-graph statusline config
4. If already set to another plugin: skip (don't overwrite)
5. Atomic write (temp file + rename)

StatusLine config to write:
```json
{
  "statusLine": {
    "type": "command",
    "command": "node \"/path/to/claude-plugin/scripts/statusline.js\""
  }
}
```

Path resolution: use `__dirname` relative path from session-init.js to statusline.js.

**Modify**: `claude-plugin/hooks/hooks.json` — replace existing SessionStart health-check with session-init.js (which does health-check + statusline registration).

```json
"SessionStart": [
  {
    "hooks": [{
      "type": "command",
      "command": "node /path/to/claude-plugin/scripts/session-init.js",
      "timeout": 5
    }],
    "description": "Register StatusLine and verify index health at session start"
  }
]
```

### Acceptance Criteria
- [ ] StatusLine shows `code-graph: ✓ N nodes | M files | watching` after session start
- [ ] Does not overwrite non-code-graph statusline configs
- [ ] Idempotent — running twice produces same result

---

## P1: Version Sync & Automated Release

### Current State
- CI/CD fully functional: `release.yml` does 5-platform build → npm publish → GitHub Release
- Version mismatch: Cargo.toml=0.4.2, package.json=0.4.0, npm/*=0.2.0, plugin.json=0.3.0
- CI already extracts version from tag and syncs to package.json during publish

### Design
1. **Sync all local versions to 0.4.2** (match Cargo.toml as source of truth):
   - `package.json` → 0.4.2
   - `npm/linux-x64/package.json` → 0.4.2
   - `npm/linux-arm64/package.json` → 0.4.2
   - `npm/darwin-x64/package.json` → 0.4.2
   - `npm/darwin-arm64/package.json` → 0.4.2
   - `npm/win32-x64/package.json` → 0.4.2
   - `claude-plugin/.claude-plugin/plugin.json` → 0.4.2

2. **Add version bump helper script** `scripts/bump-version.sh`:
   ```bash
   #!/bin/bash
   # Usage: ./scripts/bump-version.sh 0.5.0
   VERSION=$1
   # Update Cargo.toml
   sed -i "s/^version = .*/version = \"$VERSION\"/" Cargo.toml
   # Update all package.json files
   npm version $VERSION --no-git-tag-version --allow-same-version
   for pkg in npm/*/package.json; do
     cd $(dirname $pkg) && npm version $VERSION --no-git-tag-version --allow-same-version && cd ../..
   done
   # Update plugin.json
   node -e "const f='claude-plugin/.claude-plugin/plugin.json';const p=require('./'+f);p.version='$VERSION';require('fs').writeFileSync(f,JSON.stringify(p,null,2)+'\n')"
   ```

3. **Release flow**: `./scripts/bump-version.sh 0.5.0 && git commit && git tag v0.5.0 && git push --tags`

### Acceptance Criteria
- [ ] All version numbers consistent across all files
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
Integrate auto-update check into the SessionStart hook (`session-init.js`):

1. On session start, check current installed version vs latest GitHub Release
2. If newer version available, print update notice to stderr (visible but non-blocking)
3. User can run `npx @sdsrs/code-graph@latest` to update

**Implementation in session-init.js**:
```javascript
async function checkUpdate() {
  const currentVersion = require('../.claude-plugin/plugin.json').version;
  // Fetch latest release from GitHub API (with 2s timeout)
  const res = await fetch('https://api.github.com/repos/sdsrss/code-graph-mcp/releases/latest', {
    signal: AbortSignal.timeout(2000)
  });
  const latest = (await res.json()).tag_name.replace('v', '');
  if (latest !== currentVersion) {
    process.stderr.write(`[code-graph] Update available: ${currentVersion} → ${latest}. Run: npx @sdsrs/code-graph@latest\n`);
  }
}
```

**Rate limiting**: Cache last check timestamp in `/tmp/code-graph-update-check`. Only check once per 24 hours.

### Acceptance Criteria
- [ ] Update notification appears when newer version exists
- [ ] No notification when up-to-date
- [ ] Check cached for 24h (no spam)
- [ ] Network failure silently ignored (no error output)

---

## P2: End-to-End Validation

### Current State
- Unit tests exist for individual modules
- No systematic end-to-end validation of the full plugin experience

### Design
Dogfood: use code-graph to index itself, then systematically validate all integration points.

**Validation script**: `scripts/e2e-validate.sh`

#### Phase 1: Index & Health
```bash
code-graph-mcp health-check --format json  # Should show node/file counts
code-graph-mcp incremental-index --quiet    # Should complete without error
```

#### Phase 2: All 14 Tools via MCP
Use a test harness that sends JSON-RPC requests to the MCP server on stdio.
Test each tool with a real query against the code-graph-mcp codebase itself:

| Tool | Test Query |
|------|------------|
| semantic_code_search | "handle tool call" |
| get_call_graph | symbol="handle_call_tool", direction="both" |
| find_http_route | Not applicable (no HTTP routes in Rust MCP) — skip |
| trace_http_chain | Skip (same reason) |
| get_ast_node | file_path="src/mcp/server.rs", symbol_name="McpServer" |
| read_snippet | Use node_id from previous get_ast_node result |
| impact_analysis | symbol_name="handle_call_tool" |
| module_overview | path="src/mcp" |
| dependency_graph | file_path="src/mcp/server.rs" |
| find_similar_code | symbol_name="compress_if_needed" |
| start_watch | Start watcher |
| stop_watch | Stop watcher |
| get_index_status | Query status |
| rebuild_index | Force rebuild |

#### Phase 3: Hooks Validation
1. Start a Claude Code session in the project
2. Edit a file → verify `PostToolUse` hook triggers incremental-index
3. Verify SessionStart hook runs health-check + statusline registration

#### Phase 4: Commands Validation
Test /impact, /trace, /understand commands manually in a Claude Code session.

#### Phase 5: Token Efficiency Spot Check
For 3 representative queries, measure response size in tokens:
- semantic_code_search result size
- get_call_graph result size
- impact_analysis result size
Compare with equivalent Grep+Read approach (estimated).

### Acceptance Criteria
- [ ] All non-HTTP tools return valid results on self-indexed codebase
- [ ] Hooks trigger correctly on file edits
- [ ] StatusLine displays after session start
- [ ] Commands produce useful structured output
- [ ] Token efficiency: tool results < 2000 tokens per call on average

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
| Create | `claude-plugin/scripts/session-init.js` |
| Modify | `claude-plugin/hooks/hooks.json` |
| Modify | `claude-plugin/.claude-plugin/plugin.json` (version) |
| Modify | `package.json` (version) |
| Modify | `npm/*/package.json` (version, 5 files) |
| Create | `scripts/bump-version.sh` |
| Create | `scripts/e2e-validate.sh` |
