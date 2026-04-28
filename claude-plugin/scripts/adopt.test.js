'use strict';
const test = require('node:test');
const assert = require('node:assert');
const fs = require('fs');
const path = require('path');
const os = require('os');
const {
  adopt, unadopt, memoryDir, stripSentinelBlock,
  isAdopted, isPluginModeInstall, maybeAutoAdopt, needsRefresh, isProjectRoot,
  SENTINEL_BEGIN, SENTINEL_END, INDEX_LINE, TEMPLATE_PATH, TARGET_NAME,
  PROJECT_MARKERS,
} = require('./adopt');

function makeSandbox() {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-adopt-home-'));
  const cwd = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-adopt-cwd-'));
  // Pre-create the memory dir (claude-mem convention — we don't create it).
  const dir = memoryDir(cwd, home);
  fs.mkdirSync(dir, { recursive: true });
  return { home, cwd, dir, cleanup: () => {
    fs.rmSync(home, { recursive: true, force: true });
    fs.rmSync(cwd, { recursive: true, force: true });
  }};
}

test('memoryDir slugifies cwd path', () => {
  const dir = memoryDir('/home/alice/proj', '/home/alice');
  assert.strictEqual(dir, '/home/alice/.claude/projects/-home-alice-proj/memory');
});

test('memoryDir replaces underscores and dots (Claude Code slug convention)', () => {
  // Real-world bug: /mnt/data_ssd/... needs data-ssd slug, not data_ssd
  assert.strictEqual(
    memoryDir('/mnt/data_ssd/dev/projects/code-graph-mcp', '/home/u'),
    '/home/u/.claude/projects/-mnt-data-ssd-dev-projects-code-graph-mcp/memory'
  );
  // Hidden dirs: /home/sds/.claude/x → -home-sds--claude-x (double-dash)
  assert.strictEqual(
    memoryDir('/home/sds/.claude/x', '/home/sds'),
    '/home/sds/.claude/projects/-home-sds--claude-x/memory'
  );
  // Preserves case and hyphens
  assert.strictEqual(
    memoryDir('/Users/Alice/my-Project_v2.1', '/'),
    '/.claude/projects/-Users-Alice-my-Project-v2-1/memory'
  );
});

test('adopt writes template and appends sentinel block when index absent', () => {
  const sb = makeSandbox();
  try {
    const res = adopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(res.ok, true);
    assert.strictEqual(res.indexed, true);
    assert.ok(fs.existsSync(res.target), 'plugin file written');
    const index = fs.readFileSync(res.indexPath, 'utf8');
    assert.match(index, /^# Memory Index/);
    assert.ok(index.includes(SENTINEL_BEGIN));
    assert.ok(index.includes(SENTINEL_END));
    assert.ok(index.includes(INDEX_LINE));
  } finally { sb.cleanup(); }
});

test('adopt is idempotent — no duplicate sentinel on re-run', () => {
  const sb = makeSandbox();
  try {
    adopt({ cwd: sb.cwd, home: sb.home });
    const res2 = adopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(res2.indexed, false, 'second run leaves index alone');
    const index = fs.readFileSync(res2.indexPath, 'utf8');
    const matches = index.match(new RegExp(SENTINEL_BEGIN.replace(/[\\/[\]^$.*+?()|{}]/g, '\\$&'), 'g'));
    assert.strictEqual(matches.length, 1, 'sentinel appears exactly once');
  } finally { sb.cleanup(); }
});

test('adopt preserves existing MEMORY.md content and appends', () => {
  const sb = makeSandbox();
  try {
    const indexPath = path.join(sb.dir, 'MEMORY.md');
    fs.writeFileSync(indexPath, '# Memory Index\n\n- [foo.md](foo.md) — existing entry\n');
    adopt({ cwd: sb.cwd, home: sb.home });
    const index = fs.readFileSync(indexPath, 'utf8');
    assert.ok(index.includes('existing entry'), 'preserves prior entries');
    assert.ok(index.includes(SENTINEL_BEGIN), 'appends sentinel');
  } finally { sb.cleanup(); }
});

test('adopt fails gracefully when cwd is not a project root', () => {
  // v0.16.9: behavior change — adopt now mkdir's the memory dir when cwd has
  // a project marker (.git / Cargo.toml / package.json / ...). Bare mkdtemp
  // without markers still fails with the more specific 'not-a-project' reason
  // to prevent /tmp pollution.
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-adopt-home-'));
  const cwd = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-adopt-cwd-'));
  try {
    const res = adopt({ cwd, home });
    assert.strictEqual(res.ok, false);
    assert.strictEqual(res.reason, 'not-a-project');
  } finally {
    fs.rmSync(home, { recursive: true, force: true });
    fs.rmSync(cwd, { recursive: true, force: true });
  }
});

test('unadopt removes file and sentinel block, preserves other entries', () => {
  const sb = makeSandbox();
  try {
    adopt({ cwd: sb.cwd, home: sb.home });
    // add a neighboring entry
    const indexPath = path.join(sb.dir, 'MEMORY.md');
    const withNeighbor = fs.readFileSync(indexPath, 'utf8') + '- [bar.md](bar.md) — neighbor\n';
    fs.writeFileSync(indexPath, withNeighbor);

    const res = unadopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(res.fileRemoved, true);
    assert.strictEqual(res.indexPruned, true);
    assert.ok(!fs.existsSync(res.target), 'plugin file gone');
    const final = fs.readFileSync(indexPath, 'utf8');
    assert.ok(!final.includes(SENTINEL_BEGIN), 'sentinel removed');
    assert.ok(final.includes('neighbor'), 'neighbor preserved');
  } finally { sb.cleanup(); }
});

test('unadopt is a no-op when never adopted', () => {
  const sb = makeSandbox();
  try {
    const res = unadopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(res.fileRemoved, false);
    assert.strictEqual(res.indexPruned, false);
  } finally { sb.cleanup(); }
});

test('template file exists and contains decision table', () => {
  assert.ok(fs.existsSync(TEMPLATE_PATH), `template at ${TEMPLATE_PATH}`);
  const content = fs.readFileSync(TEMPLATE_PATH, 'utf8');
  assert.ok(content.includes('get_call_graph'), 'mentions get_call_graph');
  assert.ok(content.includes('impact_analysis'), 'mentions impact_analysis');
  assert.ok(content.includes('CODE_GRAPH_QUIET_HOOKS'), 'mentions env gate');
});

test('stripSentinelBlock removes well-formed block', () => {
  const before = `# Index\n${SENTINEL_BEGIN}\n${INDEX_LINE}\n${SENTINEL_END}\n- [x.md](x.md)\n`;
  const after = stripSentinelBlock(before);
  assert.ok(!after.includes(SENTINEL_BEGIN));
  assert.ok(!after.includes(SENTINEL_END));
  assert.ok(after.includes('- [x.md](x.md)'), 'preserves neighbors');
});

test('stripSentinelBlock self-heals orphan BEGIN without END', () => {
  // Truncation / partial edit scenario
  const before = `# Index\n- [a.md](a.md) — entry\n${SENTINEL_BEGIN}\n${INDEX_LINE}\n\n- [b.md](b.md) — survivor\n`;
  const after = stripSentinelBlock(before);
  assert.ok(!after.includes(SENTINEL_BEGIN), 'orphan BEGIN removed');
  assert.ok(after.includes('survivor'), 'content past blank-line boundary preserved');
  assert.ok(after.includes('entry'), 'content before BEGIN preserved');
});

test('stripSentinelBlock self-heals orphan END line', () => {
  const before = `# Index\n- [a.md](a.md)\n${SENTINEL_END}\n- [b.md](b.md)\n`;
  const after = stripSentinelBlock(before);
  assert.ok(!after.includes(SENTINEL_END));
  assert.ok(after.includes('- [a.md](a.md)') && after.includes('- [b.md](b.md)'));
});

test('adopt heals malformed sentinel (orphan BEGIN) on re-run', () => {
  const sb = makeSandbox();
  try {
    const indexPath = path.join(sb.dir, 'MEMORY.md');
    // Simulate truncated prior adopt — BEGIN line + stale entry, no END
    fs.writeFileSync(
      indexPath,
      `# Memory Index\n- [old.md](old.md) — preserved\n${SENTINEL_BEGIN}\n- [stale](stale.md) — wrong entry\n\n- [neighbor.md](neighbor.md) — survives\n`
    );
    const res = adopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(res.ok, true);
    assert.strictEqual(res.healed, true, 'reports healed');
    const final = fs.readFileSync(indexPath, 'utf8');
    // Exactly one well-formed block now
    const beginCount = (final.match(new RegExp(SENTINEL_BEGIN.replace(/[\\/[\]^$.*+?()|{}]/g, '\\$&'), 'g')) || []).length;
    const endCount = (final.match(new RegExp(SENTINEL_END.replace(/[\\/[\]^$.*+?()|{}]/g, '\\$&'), 'g')) || []).length;
    assert.strictEqual(beginCount, 1, 'one BEGIN');
    assert.strictEqual(endCount, 1, 'one END');
    assert.ok(final.includes('preserved'), 'preserves pre-BEGIN content');
    assert.ok(final.includes('neighbor.md'), 'preserves post-malformed-block content');
    assert.ok(!final.includes('stale.md'), 'old wrong entry purged');
    assert.ok(final.includes(INDEX_LINE), 'fresh canonical line written');
  } finally { sb.cleanup(); }
});

test('adopt is a true no-op when desired block is already present verbatim', () => {
  const sb = makeSandbox();
  try {
    adopt({ cwd: sb.cwd, home: sb.home });
    const indexPath = path.join(sb.dir, 'MEMORY.md');
    const before = fs.readFileSync(indexPath, 'utf8');
    const beforeMtime = fs.statSync(indexPath).mtimeMs;
    const res2 = adopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(res2.indexed, false);
    assert.strictEqual(res2.healed, false);
    assert.strictEqual(fs.readFileSync(indexPath, 'utf8'), before, 'file content identical');
    // mtime may equal beforeMtime since we skipped the write
    assert.strictEqual(fs.statSync(indexPath).mtimeMs, beforeMtime, 'no write occurred');
  } finally { sb.cleanup(); }
});

test('unadopt heals malformed sentinel (orphan BEGIN)', () => {
  const sb = makeSandbox();
  try {
    const indexPath = path.join(sb.dir, 'MEMORY.md');
    fs.writeFileSync(
      indexPath,
      `# Index\n${SENTINEL_BEGIN}\n${INDEX_LINE}\n\n- [keep.md](keep.md) — survives\n`
    );
    const res = unadopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(res.indexPruned, true);
    const final = fs.readFileSync(indexPath, 'utf8');
    assert.ok(!final.includes(SENTINEL_BEGIN), 'orphan BEGIN purged');
    assert.ok(final.includes('keep.md'), 'content past blank-line preserved');
  } finally { sb.cleanup(); }
});

// ──────────────────────────────────────────────────────────────────────────
// v0.9.0 — C' context-aware auto-adopt
// ──────────────────────────────────────────────────────────────────────────

test('isAdopted returns false on fresh project (no files)', () => {
  const sb = makeSandbox();
  try {
    assert.strictEqual(isAdopted({ cwd: sb.cwd, home: sb.home }), false);
  } finally { sb.cleanup(); }
});

test('isAdopted returns true after adopt()', () => {
  const sb = makeSandbox();
  try {
    adopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(isAdopted({ cwd: sb.cwd, home: sb.home }), true);
  } finally { sb.cleanup(); }
});

test('isAdopted returns false after unadopt()', () => {
  const sb = makeSandbox();
  try {
    adopt({ cwd: sb.cwd, home: sb.home });
    unadopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(isAdopted({ cwd: sb.cwd, home: sb.home }), false);
  } finally { sb.cleanup(); }
});

test('isAdopted returns false when target file exists but index has no sentinel', () => {
  const sb = makeSandbox();
  try {
    const indexPath = path.join(sb.dir, 'MEMORY.md');
    fs.writeFileSync(indexPath, '# Memory Index\n- [foo.md](foo.md) — unrelated\n');
    fs.writeFileSync(path.join(sb.dir, 'plugin_code_graph_mcp.md'), 'stale copy');
    assert.strictEqual(isAdopted({ cwd: sb.cwd, home: sb.home }), false);
  } finally { sb.cleanup(); }
});

test('isPluginModeInstall recognizes ~/.claude/plugins/... paths', () => {
  const pluginPath = '/home/user/.claude/plugins/cache/code-graph-mcp@0.9.0/scripts';
  assert.strictEqual(isPluginModeInstall(pluginPath), true);
});

test('isPluginModeInstall rejects npm global install paths', () => {
  const npmPath = '/usr/local/lib/node_modules/@sdsrs/code-graph/claude-plugin/scripts';
  assert.strictEqual(isPluginModeInstall(npmPath), false);
});

test('isPluginModeInstall rejects dev-checkout paths', () => {
  const devPath = '/mnt/data_ssd/dev/projects/code-graph-mcp/claude-plugin/scripts';
  assert.strictEqual(isPluginModeInstall(devPath), false);
});

test('isPluginModeInstall rejects npx cache paths', () => {
  const npxPath = '/tmp/npx-abc123/node_modules/@sdsrs/code-graph/claude-plugin/scripts';
  assert.strictEqual(isPluginModeInstall(npxPath), false);
});

test('maybeAutoAdopt skips when CODE_GRAPH_NO_AUTO_ADOPT=1', () => {
  const sb = makeSandbox();
  try {
    const res = maybeAutoAdopt({
      cwd: sb.cwd, home: sb.home,
      scriptPath: '/home/u/.claude/plugins/cache/code-graph-mcp/scripts',
      env: { CODE_GRAPH_NO_AUTO_ADOPT: '1' },
    });
    assert.strictEqual(res.attempted, false);
    assert.strictEqual(res.reason, 'opted-out');
    assert.strictEqual(isAdopted({ cwd: sb.cwd, home: sb.home }), false);
  } finally { sb.cleanup(); }
});

test('maybeAutoAdopt skips when not plugin-mode (npm install)', () => {
  const sb = makeSandbox();
  try {
    const res = maybeAutoAdopt({
      cwd: sb.cwd, home: sb.home,
      scriptPath: '/usr/local/lib/node_modules/@sdsrs/code-graph/claude-plugin/scripts',
      env: {},
    });
    assert.strictEqual(res.attempted, false);
    assert.strictEqual(res.reason, 'not-plugin-mode');
    assert.strictEqual(isAdopted({ cwd: sb.cwd, home: sb.home }), false);
  } finally { sb.cleanup(); }
});

test('maybeAutoAdopt skips when already adopted (idempotent)', () => {
  const sb = makeSandbox();
  try {
    adopt({ cwd: sb.cwd, home: sb.home });
    const res = maybeAutoAdopt({
      cwd: sb.cwd, home: sb.home,
      scriptPath: '/home/u/.claude/plugins/cache/code-graph-mcp/scripts',
      env: {},
    });
    assert.strictEqual(res.attempted, false);
    assert.strictEqual(res.reason, 'already-adopted');
  } finally { sb.cleanup(); }
});

test('maybeAutoAdopt runs adopt when plugin-mode + unadopted + no opt-out', () => {
  const sb = makeSandbox();
  try {
    const res = maybeAutoAdopt({
      cwd: sb.cwd, home: sb.home,
      scriptPath: '/home/u/.claude/plugins/cache/code-graph-mcp/scripts',
      env: {},
    });
    assert.strictEqual(res.attempted, true);
    assert.strictEqual(res.result.ok, true);
    assert.strictEqual(res.result.indexed, true);
    assert.strictEqual(isAdopted({ cwd: sb.cwd, home: sb.home }), true);
  } finally { sb.cleanup(); }
});

test('maybeAutoAdopt fails with not-a-project when cwd has no project marker', () => {
  // v0.16.9: bare mkdtemp cwd without .git/Cargo.toml/etc. surfaces
  // 'not-a-project' so plugin-mode auto-adopt doesn't litter ~/.claude/projects/
  // with bogus slugs from non-project working directories.
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-adopt-home-'));
  const cwd = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-adopt-cwd-'));
  try {
    const res = maybeAutoAdopt({
      cwd, home,
      scriptPath: '/home/u/.claude/plugins/cache/code-graph-mcp/scripts',
      env: {},
    });
    assert.strictEqual(res.attempted, true);
    assert.strictEqual(res.result.ok, false);
    assert.strictEqual(res.result.reason, 'not-a-project');
  } finally {
    fs.rmSync(home, { recursive: true, force: true });
    fs.rmSync(cwd, { recursive: true, force: true });
  }
});

// v0.11.0 — template-refresh on drift

test('needsRefresh returns false when target matches shipped template + INDEX_LINE', () => {
  const sb = makeSandbox();
  try {
    adopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(needsRefresh({ cwd: sb.cwd, home: sb.home }), false);
  } finally { sb.cleanup(); }
});

test('needsRefresh returns true when target content drifted from shipped template', () => {
  const sb = makeSandbox();
  try {
    adopt({ cwd: sb.cwd, home: sb.home });
    const target = path.join(sb.dir, TARGET_NAME);
    fs.writeFileSync(target, '# stale content from earlier plugin version\n');
    assert.strictEqual(needsRefresh({ cwd: sb.cwd, home: sb.home }), true);
  } finally { sb.cleanup(); }
});

test('needsRefresh returns true when MEMORY.md INDEX_LINE drifted', () => {
  const sb = makeSandbox();
  try {
    adopt({ cwd: sb.cwd, home: sb.home });
    const indexPath = path.join(sb.dir, 'MEMORY.md');
    const stale = `# Memory Index\n\n${SENTINEL_BEGIN}\n- old 12-tool index line\n${SENTINEL_END}\n`;
    fs.writeFileSync(indexPath, stale);
    assert.strictEqual(needsRefresh({ cwd: sb.cwd, home: sb.home }), true);
  } finally { sb.cleanup(); }
});

test('needsRefresh returns false when not adopted (nothing to refresh)', () => {
  const sb = makeSandbox();
  try {
    assert.strictEqual(needsRefresh({ cwd: sb.cwd, home: sb.home }), false);
  } finally { sb.cleanup(); }
});

test('maybeAutoAdopt refreshes drifted target on re-run (reason=refreshed)', () => {
  const sb = makeSandbox();
  try {
    adopt({ cwd: sb.cwd, home: sb.home });
    const target = path.join(sb.dir, TARGET_NAME);
    fs.writeFileSync(target, '# stale\n');
    const res = maybeAutoAdopt({
      cwd: sb.cwd, home: sb.home,
      scriptPath: '/home/u/.claude/plugins/cache/code-graph-mcp/scripts',
      env: {},
    });
    assert.strictEqual(res.attempted, true);
    assert.strictEqual(res.reason, 'refreshed');
    assert.strictEqual(res.result.ok, true);
    // Target now matches shipped template (after stripping the leading
    // "<!-- adopted-by: ... -->\n" collision marker added by adopt v0.16.9).
    const shipped = fs.readFileSync(TEMPLATE_PATH);
    const current = fs.readFileSync(target);
    const nl = current.indexOf(0x0a);
    const body = nl > 0 && /^<!-- adopted-by: /.test(current.subarray(0, nl).toString())
      ? current.subarray(nl + 1) : current;
    assert.ok(shipped.equals(body), 'target re-synced to shipped template');
    // Sentinel preserved in MEMORY.md
    assert.strictEqual(isAdopted({ cwd: sb.cwd, home: sb.home }), true);
  } finally { sb.cleanup(); }
});

test('maybeAutoAdopt refreshes drifted INDEX_LINE in MEMORY.md', () => {
  const sb = makeSandbox();
  try {
    adopt({ cwd: sb.cwd, home: sb.home });
    const indexPath = path.join(sb.dir, 'MEMORY.md');
    const stale = `# Memory Index\n\n${SENTINEL_BEGIN}\n- old 12-tool index line\n${SENTINEL_END}\n`;
    fs.writeFileSync(indexPath, stale);
    const res = maybeAutoAdopt({
      cwd: sb.cwd, home: sb.home,
      scriptPath: '/home/u/.claude/plugins/cache/code-graph-mcp/scripts',
      env: {},
    });
    assert.strictEqual(res.attempted, true);
    assert.strictEqual(res.reason, 'refreshed');
    const index = fs.readFileSync(indexPath, 'utf8');
    assert.ok(index.includes(INDEX_LINE), 'INDEX_LINE restored from current constant');
    assert.ok(!index.includes('old 12-tool index line'), 'stale line removed');
  } finally { sb.cleanup(); }
});

test('maybeAutoAdopt skips refresh when CODE_GRAPH_NO_TEMPLATE_REFRESH=1 (locks manual edits)', () => {
  const sb = makeSandbox();
  try {
    adopt({ cwd: sb.cwd, home: sb.home });
    const target = path.join(sb.dir, TARGET_NAME);
    const userEdit = '# my hand-edited decision table\n';
    fs.writeFileSync(target, userEdit);
    const res = maybeAutoAdopt({
      cwd: sb.cwd, home: sb.home,
      scriptPath: '/home/u/.claude/plugins/cache/code-graph-mcp/scripts',
      env: { CODE_GRAPH_NO_TEMPLATE_REFRESH: '1' },
    });
    assert.strictEqual(res.attempted, false);
    assert.strictEqual(res.reason, 'already-adopted');
    assert.strictEqual(fs.readFileSync(target, 'utf8'), userEdit, 'user edit preserved');
  } finally { sb.cleanup(); }
});

test('maybeAutoAdopt stays already-adopted when in sync (no gratuitous refresh)', () => {
  const sb = makeSandbox();
  try {
    adopt({ cwd: sb.cwd, home: sb.home });
    const target = path.join(sb.dir, TARGET_NAME);
    const mtimeBefore = fs.statSync(target).mtimeMs;
    const res = maybeAutoAdopt({
      cwd: sb.cwd, home: sb.home,
      scriptPath: '/home/u/.claude/plugins/cache/code-graph-mcp/scripts',
      env: {},
    });
    assert.strictEqual(res.attempted, false);
    assert.strictEqual(res.reason, 'already-adopted');
    const mtimeAfter = fs.statSync(target).mtimeMs;
    assert.strictEqual(mtimeAfter, mtimeBefore, 'target file not touched when in sync');
  } finally { sb.cleanup(); }
});

test('Windows platform is rejected with clear reason', { skip: process.platform === 'win32' }, () => {
  const orig = process.platform;
  Object.defineProperty(process, 'platform', { value: 'win32', configurable: true });
  try {
    const sb = makeSandbox();
    try {
      const adoptRes = adopt({ cwd: sb.cwd, home: sb.home });
      assert.strictEqual(adoptRes.ok, false);
      assert.strictEqual(adoptRes.reason, 'windows-not-supported');
      const unadoptRes = unadopt({ cwd: sb.cwd, home: sb.home });
      assert.strictEqual(unadoptRes.ok, false);
      assert.strictEqual(unadoptRes.reason, 'windows-not-supported');
    } finally { sb.cleanup(); }
  } finally {
    Object.defineProperty(process, 'platform', { value: orig, configurable: true });
  }
});

// ─── C fix: project-marker mkdir ─────────────────────────────────────────

test('isProjectRoot detects each marker', () => {
  for (const marker of PROJECT_MARKERS) {
    const cwd = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-marker-'));
    try {
      assert.strictEqual(isProjectRoot(cwd), false, `bare cwd should not be a project`);
      const markerPath = path.join(cwd, marker);
      // Some markers are directories (.git, .code-graph), others are files.
      if (marker.startsWith('.')) fs.mkdirSync(markerPath);
      else fs.writeFileSync(markerPath, '');
      assert.strictEqual(isProjectRoot(cwd), true, `${marker} should make cwd a project`);
    } finally {
      fs.rmSync(cwd, { recursive: true, force: true });
    }
  }
});

test('adopt auto-creates memory dir when cwd has a project marker', () => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-adopt-home-'));
  const cwd = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-adopt-cwd-'));
  try {
    // Add a project marker so adopt is allowed to create the memory dir.
    fs.writeFileSync(path.join(cwd, 'package.json'), '{}');
    // The memory dir does NOT exist yet — pre-fix behavior errored 'no-memory-dir'.
    const dir = memoryDir(cwd, home);
    assert.strictEqual(fs.existsSync(dir), false);

    const res = adopt({ cwd, home });
    assert.strictEqual(res.ok, true, `expected ok, got ${JSON.stringify(res)}`);
    assert.strictEqual(fs.existsSync(dir), true, 'memory dir auto-created');
    assert.strictEqual(fs.existsSync(path.join(dir, TARGET_NAME)), true, 'plugin file written');
  } finally {
    fs.rmSync(home, { recursive: true, force: true });
    fs.rmSync(cwd, { recursive: true, force: true });
  }
});

// ─── D fix: slug collision marker ────────────────────────────────────────

test('adopt writes adopted-by marker as first line of plugin file', () => {
  const sb = makeSandbox();
  try {
    adopt({ cwd: sb.cwd, home: sb.home });
    const target = path.join(sb.dir, TARGET_NAME);
    const firstLine = fs.readFileSync(target, 'utf8').split('\n', 1)[0];
    assert.match(firstLine, /^<!-- adopted-by: .* -->$/, `expected adopted-by marker, got: ${firstLine}`);
    assert.ok(firstLine.includes(sb.cwd), `marker should embed absolute cwd: ${firstLine}`);
  } finally { sb.cleanup(); }
});

test('adopt detects slug collision when same memory dir is re-adopted from a different cwd', () => {
  // Simulate two real cwds whose paths slugify to the same string. Here we
  // skip real path encoding and just write a file pretending it came from a
  // different cwd, then re-adopt — collision detection reads the prior marker.
  const sb = makeSandbox();
  try {
    adopt({ cwd: sb.cwd, home: sb.home });
    const target = path.join(sb.dir, TARGET_NAME);
    // Tamper: rewrite the marker to look like a different cwd adopted first.
    const body = fs.readFileSync(target, 'utf8').split('\n').slice(1).join('\n');
    fs.writeFileSync(target, '<!-- adopted-by: /imaginary/other-project -->\n' + body);

    const res = adopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(res.ok, true);
    assert.strictEqual(res.collisionWith, '/imaginary/other-project',
      `expected collisionWith to surface prior cwd, got ${res.collisionWith}`);
  } finally { sb.cleanup(); }
});

test('adopt collisionWith is null when re-adopting from same cwd (idempotent)', () => {
  const sb = makeSandbox();
  try {
    adopt({ cwd: sb.cwd, home: sb.home });
    const res = adopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(res.ok, true);
    assert.strictEqual(res.collisionWith, null);
  } finally { sb.cleanup(); }
});

test('needsRefresh ignores the adopted-by marker when bytewise comparing', () => {
  // Critical: the marker we add to target makes target ≠ template byte-for-byte.
  // needsRefresh must skip the leading marker line before compare; otherwise
  // every SessionStart would re-write the file and burn IO on a no-op.
  const sb = makeSandbox();
  try {
    adopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(needsRefresh({ cwd: sb.cwd, home: sb.home }), false,
      'needsRefresh should be false right after adopt — marker must not trigger drift');
  } finally { sb.cleanup(); }
});
