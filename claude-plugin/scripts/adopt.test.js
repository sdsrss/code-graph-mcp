'use strict';
const test = require('node:test');
const assert = require('node:assert');
const fs = require('fs');
const path = require('path');
const os = require('os');
const {
  adopt, unadopt, memoryDir, SENTINEL_BEGIN, SENTINEL_END, INDEX_LINE, TEMPLATE_PATH,
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
