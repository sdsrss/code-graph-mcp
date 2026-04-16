'use strict';
const test = require('node:test');
const assert = require('node:assert');
const fs = require('fs');
const path = require('path');
const os = require('os');
const {
  adopt, unadopt, memoryDir, stripSentinelBlock,
  SENTINEL_BEGIN, SENTINEL_END, INDEX_LINE, TEMPLATE_PATH,
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

test('adopt fails gracefully when memory dir missing', () => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-adopt-home-'));
  const cwd = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-adopt-cwd-'));
  try {
    const res = adopt({ cwd, home });
    assert.strictEqual(res.ok, false);
    assert.strictEqual(res.reason, 'no-memory-dir');
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
