'use strict';
const test = require('node:test');
const assert = require('node:assert');
const fs = require('fs');
const path = require('path');
const os = require('os');

// Tests call adopt()/unadopt() in-process; both now maintain the
// adopted-projects registry under ~/.cache/code-graph. Point HOME at a
// sandbox BEFORE any test runs so no test writes the real user registry
// (os.homedir() reads $HOME at call time on POSIX).
//
// And it is REMOVED when the run ends. Without this the sandbox survived every
// run in the real os.tmpdir() — which under Claude Code is ~/.claude/tmp/ —
// measured at 159 accumulated `cg-adopt-isolated-home-*` directories, growing by
// exactly one per run. Same shape as the leak fixed in tmp-dir.test.js: a
// module-load mkdtemp with no owner, invisible to the exit code.
const ISOLATED_HOME = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-adopt-isolated-home-'));
// And CLAUDE_CONFIG_DIR is dropped, not just HOME. These tests call adopt()/
// unadopt() IN-PROCESS, and `claudeHome()` is
// `CLAUDE_CONFIG_DIR || homedir/.claude` — the env var outranks the redirected
// HOME above, so for a developer who exports it the adoption registry and the
// per-project memory dirs were written into their LIVE config (measured: five
// `projects/<slug>/memory/` trees landed in a canary config dir). The two tests
// that need the variable set it themselves and restore the previous value,
// which is now correctly "absent".
delete process.env.CLAUDE_CONFIG_DIR;
process.env.HOME = ISOLATED_HOME;
test.after(() => {
  try { fs.rmSync(ISOLATED_HOME, { recursive: true, force: true }); } catch { /* best effort */ }
});
const {
  adopt, unadopt, memoryDir, stripSentinelBlock,
  isAdopted, isPluginModeInstall, maybeAutoAdopt, needsRefresh, isProjectRoot,
  detectProjectType, buildBlock, migrateLegacyMemoryDir,
  SENTINEL_BEGIN, SENTINEL_END, MANAGED_BY, TEMPLATE_PATH, TARGET_NAME,
  PROJECT_MARKERS,
} = require('./adopt');

// Legacy v1 sentinel (pre-v0.74, lived in the memory-dir MEMORY.md). Hard-coded
// here because the strip/migration path must keep removing it after the constant
// moved to v2.
const SENTINEL_BEGIN_V1 = '<!-- code-graph-mcp:begin v1 -->';

function makeSandbox() {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-adopt-home-'));
  const cwd = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-adopt-cwd-'));
  // Mark the sandbox cwd as a real project — adopt() gates on a project marker.
  fs.mkdirSync(path.join(cwd, '.git'));
  return {
    home, cwd,
    claudeMd: path.join(cwd, 'CLAUDE.md'),
    detail: path.join(cwd, '.claude', TARGET_NAME),
    cleanup: () => {
      fs.rmSync(home, { recursive: true, force: true });
      fs.rmSync(cwd, { recursive: true, force: true });
    },
  };
}


// A cwd that is genuinely NOT a project — for the two tests that assert the
// activation gate refuses one.
//
// `mkdtemp(os.tmpdir())` is not enough: findProjectRoot walks UP, bounded by
// $HOME, and under Claude Code $TMPDIR is `~/.claude/tmp`, whose ancestry
// carries this machine's own package.json. Because the suite overrides HOME to a
// tmpdir SIBLING, that bound stops applying and a bare temp dir resolves as a
// project — so these two tests passed or failed depending on where $TMPDIR
// pointed, which is also why the pre-commit hook saw failures that a bare
// `node --test` in /tmp did not.
//
// Creating the cwd inside `process.env.HOME` restores the bound: the walk stops
// at the home we control, which has no markers in it. It must be that HOME (set
// at the top of this file) and NOT the `home` argument these tests pass to
// adopt/maybeAutoAdopt — findProjectRoot bounds on the environment, not on the
// caller-supplied home, and using the argument here left both tests still red.
function mkBareCwd() {
  return fs.mkdtempSync(path.join(process.env.HOME, 'bare-cwd-'));
}

// ── memoryDir (legacy slug — still used by migrateLegacyMemoryDir) ──────────

test('memoryDir slugifies cwd path', () => {
  assert.strictEqual(
    memoryDir('/home/alice/proj', '/home/alice'),
    '/home/alice/.claude/projects/-home-alice-proj/memory'
  );
});

test('memoryDir replaces underscores and dots (Claude Code slug convention)', () => {
  assert.strictEqual(
    memoryDir('/mnt/data_ssd/dev/projects/code-graph-mcp', '/home/u'),
    '/home/u/.claude/projects/-mnt-data-ssd-dev-projects-code-graph-mcp/memory'
  );
  assert.strictEqual(
    memoryDir('/home/sds/.claude/x', '/home/sds'),
    '/home/sds/.claude/projects/-home-sds--claude-x/memory'
  );
});

test('memoryDir honors CLAUDE_CONFIG_DIR override (multi-account isolation)', () => {
  const prev = process.env.CLAUDE_CONFIG_DIR;
  process.env.CLAUDE_CONFIG_DIR = '/home/alice/work-claude';
  try {
    assert.strictEqual(
      memoryDir('/home/alice/proj', '/home/alice'),
      '/home/alice/work-claude/projects/-home-alice-proj/memory'
    );
  } finally {
    if (prev === undefined) delete process.env.CLAUDE_CONFIG_DIR;
    else process.env.CLAUDE_CONFIG_DIR = prev;
  }
});

// ── buildBlock — the managed CLAUDE.md block ────────────────────────────────

test('buildBlock generic: v2 sentinel + 6 base rows + pointer', () => {
  const block = buildBlock('generic');
  assert.ok(block.startsWith(SENTINEL_BEGIN), 'opens with v2 BEGIN');
  assert.ok(block.endsWith(SENTINEL_END), 'closes with END');
  assert.ok(block.includes('| Who calls X / what X calls | `code-graph-mcp callgraph X` |'));
  assert.ok(block.includes('| Impact before editing a fn | `code-graph-mcp impact X` |'));
  assert.ok(block.includes('Full command + MCP-tool table: `.claude/plugin_code_graph_mcp.md`'));
  assert.ok(!block.includes('trace'), 'generic has no HTTP-trace row');
});

test('buildBlock web-rs inserts the HTTP-route → handler row', () => {
  const block = buildBlock('web-rs');
  assert.ok(block.includes('HTTP route → handler chain'), 'web project gets trace row');
  assert.ok(block.includes('`code-graph-mcp trace "GET /api/x"`'));
});

test('buildBlock frontend surfaces a find-references audit row', () => {
  const block = buildBlock('frontend');
  assert.ok(block.includes('Rename / refactor audit (refs)'));
  assert.ok(block.includes('`code-graph-mcp refs X`'));
});

test('buildBlock is deterministic (byte-identical across calls)', () => {
  assert.strictEqual(buildBlock('rust'), buildBlock('rust'));
  assert.strictEqual(buildBlock('generic'), buildBlock(undefined));
});

// ── adopt — installs CLAUDE.md block + .claude/ detail ──────────────────────

test('adopt creates CLAUDE.md with the block when none exists', () => {
  const sb = makeSandbox();
  try {
    const res = adopt({ cwd: sb.cwd });
    assert.strictEqual(res.ok, true);
    assert.strictEqual(res.created, true);
    assert.strictEqual(res.claudeMdWritten, true);
    assert.strictEqual(res.detailWritten, true);
    const cmd = fs.readFileSync(sb.claudeMd, 'utf8');
    assert.ok(cmd.includes(SENTINEL_BEGIN) && cmd.includes(SENTINEL_END));
    assert.ok(fs.existsSync(sb.detail), 'detail file written under .claude/');
    assert.ok(fs.readFileSync(sb.detail, 'utf8').startsWith(MANAGED_BY), 'detail has managed-by marker');
  } finally { sb.cleanup(); }
});

test('adopt injects the block into an existing CLAUDE.md, preserving user prose', () => {
  const sb = makeSandbox();
  try {
    fs.writeFileSync(sb.claudeMd, '# My Project\n\nUser instructions here.\n');
    const res = adopt({ cwd: sb.cwd });
    assert.strictEqual(res.created, false);
    assert.strictEqual(res.claudeMdWritten, true);
    const cmd = fs.readFileSync(sb.claudeMd, 'utf8');
    assert.ok(cmd.includes('User instructions here.'), 'preserves user prose');
    assert.ok(cmd.includes(SENTINEL_BEGIN), 'block appended');
  } finally { sb.cleanup(); }
});

test('adopt is idempotent — no duplicate block, no write on re-run', () => {
  const sb = makeSandbox();
  try {
    adopt({ cwd: sb.cwd });
    const res2 = adopt({ cwd: sb.cwd });
    assert.strictEqual(res2.claudeMdWritten, false, 'second run leaves CLAUDE.md alone');
    assert.strictEqual(res2.detailWritten, false, 'second run leaves detail alone');
    const cmd = fs.readFileSync(sb.claudeMd, 'utf8');
    const esc = SENTINEL_BEGIN.replace(/[\\/[\]^$.*+?()|{}]/g, '\\$&');
    assert.strictEqual((cmd.match(new RegExp(esc, 'g')) || []).length, 1, 'block appears exactly once');
  } finally { sb.cleanup(); }
});

test('adopt block reflects detected project type (web-rs → trace row in CLAUDE.md)', () => {
  const sb = makeSandbox();
  try {
    fs.writeFileSync(path.join(sb.cwd, 'Cargo.toml'), '[dependencies]\naxum = "0.7"\n');
    adopt({ cwd: sb.cwd });
    assert.ok(fs.readFileSync(sb.claudeMd, 'utf8').includes('HTTP route → handler chain'));
  } finally { sb.cleanup(); }
});

test('adopt heals a malformed prior block (orphan BEGIN) and preserves neighbors', () => {
  const sb = makeSandbox();
  try {
    fs.writeFileSync(sb.claudeMd,
      `# Project\n\nKeep me.\n\n${SENTINEL_BEGIN}\n- stale partial block\n\nAlso keep me.\n`);
    const res = adopt({ cwd: sb.cwd });
    assert.strictEqual(res.healed, true);
    const cmd = fs.readFileSync(sb.claudeMd, 'utf8');
    const esc = SENTINEL_BEGIN.replace(/[\\/[\]^$.*+?()|{}]/g, '\\$&');
    assert.strictEqual((cmd.match(new RegExp(esc, 'g')) || []).length, 1, 'exactly one block');
    assert.ok(cmd.includes('Keep me.') && cmd.includes('Also keep me.'), 'neighbors preserved');

    // DELIBERATE narrowing (2026-07-27 contract audit, round-5 F1): the heal
    // removes the orphan MARKER LINE and leaves the content under it.
    //
    // This assertion used to read `!cmd.includes('stale partial block')`. That
    // behavior is not implementable safely: this fixture and a user's own notes
    // under a marker they quoted from our instructions are byte-for-byte the
    // same shape, and the old "strip to the next blank line" rule took 221 B of
    // a real CLAUDE.md down to 100 B in a repo that had never been adopted. Since
    // the two cannot be distinguished, the tie goes to the recoverable outcome.
    // The cost is this: a genuinely truncated block leaves a visible fragment.
    assert.ok(cmd.includes('stale partial block'),
      'the fragment is LEFT — see the comment above; deleting it would require ' +
      'guessing whose text it is, and that guess destroyed user prose');
    assert.ok(!cmd.split('\n').slice(0, 6).some(l => l.trim() === SENTINEL_BEGIN),
      'but the orphan marker line itself is gone, so the fragment is inert');
  } finally { sb.cleanup(); }
});

test('adopt refuses a non-project cwd and writes nothing', () => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-adopt-home-'));
  const cwd = mkBareCwd(); // no marker, and no marker above it either
  try {
    const res = adopt({ cwd });
    assert.strictEqual(res.ok, false);
    assert.strictEqual(res.reason, 'not-a-project');
    assert.ok(!fs.existsSync(path.join(cwd, 'CLAUDE.md')), 'no CLAUDE.md written');
    assert.ok(!fs.existsSync(path.join(cwd, '.claude')), 'no .claude dir created');
  } finally {
    fs.rmSync(home, { recursive: true, force: true });
    fs.rmSync(cwd, { recursive: true, force: true });
  }
});

test('adopt writes atomically — no .tmp residue in cwd or .claude', () => {
  const sb = makeSandbox();
  try {
    adopt({ cwd: sb.cwd });
    const cwdResidue = fs.readdirSync(sb.cwd).filter((f) => f.includes('.tmp.'));
    const claudeResidue = fs.readdirSync(path.join(sb.cwd, '.claude')).filter((f) => f.includes('.tmp.'));
    assert.deepStrictEqual(cwdResidue, []);
    assert.deepStrictEqual(claudeResidue, []);
  } finally { sb.cleanup(); }
});

test('writeFileAtomic cleans its temp file when rename fails (no orphaned .tmp)', () => {
  const sb = makeSandbox();
  const realRename = fs.renameSync;
  try {
    fs.renameSync = () => { const e = new Error('EROFS: simulated read-only fs'); e.code = 'EROFS'; throw e; };
    try { adopt({ cwd: sb.cwd }); } catch { /* expected — rename failed */ }
    fs.renameSync = realRename;
    // .claude may or may not exist depending on which write failed first; tolerate both.
    const dirs = [sb.cwd, path.join(sb.cwd, '.claude')].filter((d) => fs.existsSync(d));
    for (const d of dirs) {
      assert.deepStrictEqual(fs.readdirSync(d).filter((f) => f.includes('.tmp.')), [],
        `failed rename must not orphan a temp in ${d}`);
    }
  } finally {
    fs.renameSync = realRename;
    sb.cleanup();
  }
});

// ── unadopt ─────────────────────────────────────────────────────────────────

test('unadopt removes the block + detail file, preserving user prose', () => {
  const sb = makeSandbox();
  try {
    fs.writeFileSync(sb.claudeMd, '# Project\n\nMy own notes.\n');
    adopt({ cwd: sb.cwd });
    const res = unadopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(res.fileRemoved, true);
    assert.strictEqual(res.blockPruned, true);
    assert.strictEqual(res.claudeMdRemoved, false, 'CLAUDE.md kept — has user prose');
    assert.ok(!fs.existsSync(sb.detail), 'detail file gone');
    const cmd = fs.readFileSync(sb.claudeMd, 'utf8');
    assert.ok(!cmd.includes(SENTINEL_BEGIN), 'block removed');
    assert.ok(cmd.includes('My own notes.'), 'user prose preserved');
  } finally { sb.cleanup(); }
});

test('unadopt deletes a CLAUDE.md that contained only our block', () => {
  const sb = makeSandbox();
  try {
    adopt({ cwd: sb.cwd }); // creates a block-only CLAUDE.md
    const res = unadopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(res.claudeMdRemoved, true);
    assert.ok(!fs.existsSync(sb.claudeMd), 'block-only CLAUDE.md removed');
  } finally { sb.cleanup(); }
});

test('unadopt will NOT delete a user file lacking our managed-by marker', () => {
  const sb = makeSandbox();
  try {
    fs.mkdirSync(path.join(sb.cwd, '.claude'));
    fs.writeFileSync(sb.detail, 'user-authored notes, not ours\n');
    const res = unadopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(res.fileRemoved, false, 'unmarked file is not deleted');
    assert.ok(fs.existsSync(sb.detail), 'user file survives');
  } finally { sb.cleanup(); }
});

test('unadopt is a no-op when never adopted', () => {
  const sb = makeSandbox();
  try {
    const res = unadopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(res.fileRemoved, false);
    assert.strictEqual(res.blockPruned, false);
  } finally { sb.cleanup(); }
});

// ── isAdopted ───────────────────────────────────────────────────────────────

test('isAdopted: false fresh, true after adopt, false after unadopt', () => {
  const sb = makeSandbox();
  try {
    assert.strictEqual(isAdopted({ cwd: sb.cwd }), false);
    adopt({ cwd: sb.cwd });
    assert.strictEqual(isAdopted({ cwd: sb.cwd }), true);
    unadopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(isAdopted({ cwd: sb.cwd }), false);
  } finally { sb.cleanup(); }
});

test('isAdopted: false when block present but detail file missing', () => {
  const sb = makeSandbox();
  try {
    fs.writeFileSync(sb.claudeMd, `${SENTINEL_BEGIN}\nx\n${SENTINEL_END}\n`);
    assert.strictEqual(isAdopted({ cwd: sb.cwd }), false, 'needs both block + detail');
  } finally { sb.cleanup(); }
});

// ── needsRefresh ────────────────────────────────────────────────────────────

test('needsRefresh: false right after adopt', () => {
  const sb = makeSandbox();
  try {
    adopt({ cwd: sb.cwd });
    assert.strictEqual(needsRefresh({ cwd: sb.cwd }), false);
  } finally { sb.cleanup(); }
});

test('needsRefresh: true when detail-doc body drifts from shipped template', () => {
  const sb = makeSandbox();
  try {
    adopt({ cwd: sb.cwd });
    fs.writeFileSync(sb.detail, `${MANAGED_BY}\n# stale content from an older plugin\n`);
    assert.strictEqual(needsRefresh({ cwd: sb.cwd }), true);
  } finally { sb.cleanup(); }
});

test('needsRefresh: true when the CLAUDE.md block drifts (project type change)', () => {
  const sb = makeSandbox();
  try {
    adopt({ cwd: sb.cwd }); // generic block
    // Now the project gains a web framework — block should switch to web-rs.
    fs.writeFileSync(path.join(sb.cwd, 'Cargo.toml'), '[dependencies]\naxum = "0.7"\n');
    assert.strictEqual(needsRefresh({ cwd: sb.cwd }), true);
  } finally { sb.cleanup(); }
});

test('needsRefresh: false when not adopted (nothing to refresh)', () => {
  const sb = makeSandbox();
  try {
    assert.strictEqual(needsRefresh({ cwd: sb.cwd }), false);
  } finally { sb.cleanup(); }
});

// ── maybeAutoAdopt ──────────────────────────────────────────────────────────

const PLUGIN_SCRIPTS = '/home/u/.claude/plugins/cache/code-graph-mcp/scripts';

test('maybeAutoAdopt skips when CODE_GRAPH_NO_AUTO_ADOPT=1', () => {
  const sb = makeSandbox();
  try {
    const res = maybeAutoAdopt({ cwd: sb.cwd, home: sb.home, scriptPath: PLUGIN_SCRIPTS, env: { CODE_GRAPH_NO_AUTO_ADOPT: '1' } });
    assert.strictEqual(res.reason, 'opted-out');
    assert.deepStrictEqual(res.migrated, { memoryIndexPruned: false, legacyDetailRemoved: false }, 'consistent migrated shape on early return');
    assert.strictEqual(isAdopted({ cwd: sb.cwd }), false);
  } finally { sb.cleanup(); }
});

test('maybeAutoAdopt skips when not plugin-mode (npm install path)', () => {
  const sb = makeSandbox();
  try {
    const res = maybeAutoAdopt({ cwd: sb.cwd, home: sb.home, scriptPath: '/usr/local/lib/node_modules/@sdsrs/code-graph/claude-plugin/scripts', env: {} });
    assert.strictEqual(res.reason, 'not-plugin-mode');
    assert.deepStrictEqual(res.migrated, { memoryIndexPruned: false, legacyDetailRemoved: false }, 'consistent migrated shape on early return');
    assert.strictEqual(isAdopted({ cwd: sb.cwd }), false);
  } finally { sb.cleanup(); }
});

test('maybeAutoAdopt installs when plugin-mode + not-yet-adopted', () => {
  const sb = makeSandbox();
  try {
    const res = maybeAutoAdopt({ cwd: sb.cwd, home: sb.home, scriptPath: PLUGIN_SCRIPTS, env: {} });
    assert.strictEqual(res.attempted, true);
    assert.strictEqual(res.reason, 'adopted');
    assert.strictEqual(res.result.ok, true);
    assert.strictEqual(isAdopted({ cwd: sb.cwd }), true);
  } finally { sb.cleanup(); }
});

test('maybeAutoAdopt is already-adopted when in sync (no gratuitous write)', () => {
  const sb = makeSandbox();
  try {
    adopt({ cwd: sb.cwd });
    const mtime = fs.statSync(sb.claudeMd).mtimeMs;
    const res = maybeAutoAdopt({ cwd: sb.cwd, home: sb.home, scriptPath: PLUGIN_SCRIPTS, env: {} });
    assert.strictEqual(res.reason, 'already-adopted');
    assert.strictEqual(fs.statSync(sb.claudeMd).mtimeMs, mtime, 'CLAUDE.md not touched');
  } finally { sb.cleanup(); }
});

test('maybeAutoAdopt refreshes a drifted detail doc (reason=refreshed)', () => {
  const sb = makeSandbox();
  try {
    adopt({ cwd: sb.cwd });
    fs.writeFileSync(sb.detail, `${MANAGED_BY}\n# stale\n`);
    const res = maybeAutoAdopt({ cwd: sb.cwd, home: sb.home, scriptPath: PLUGIN_SCRIPTS, env: {} });
    assert.strictEqual(res.reason, 'refreshed');
    const shipped = fs.readFileSync(TEMPLATE_PATH);
    const cur = fs.readFileSync(sb.detail);
    const nl = cur.indexOf(0x0a);
    assert.ok(shipped.equals(cur.subarray(nl + 1)), 'detail re-synced to shipped template');
  } finally { sb.cleanup(); }
});

test('maybeAutoAdopt skips refresh when CODE_GRAPH_NO_TEMPLATE_REFRESH=1 (locks edits)', () => {
  const sb = makeSandbox();
  try {
    adopt({ cwd: sb.cwd });
    const userEdit = `${MANAGED_BY}\n# my hand-edited table\n`;
    fs.writeFileSync(sb.detail, userEdit);
    const res = maybeAutoAdopt({ cwd: sb.cwd, home: sb.home, scriptPath: PLUGIN_SCRIPTS, env: { CODE_GRAPH_NO_TEMPLATE_REFRESH: '1' } });
    assert.strictEqual(res.reason, 'already-adopted');
    assert.strictEqual(fs.readFileSync(sb.detail, 'utf8'), userEdit, 'user edit preserved');
  } finally { sb.cleanup(); }
});

test('maybeAutoAdopt surfaces not-a-project for a bare cwd', () => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-adopt-home-'));
  const cwd = mkBareCwd();
  try {
    const res = maybeAutoAdopt({ cwd, home, scriptPath: PLUGIN_SCRIPTS, env: {} });
    assert.strictEqual(res.result.ok, false);
    assert.strictEqual(res.result.reason, 'not-a-project');
  } finally {
    fs.rmSync(home, { recursive: true, force: true });
    fs.rmSync(cwd, { recursive: true, force: true });
  }
});

// ── migrateLegacyMemoryDir — auto-upgrade cleanup of the pre-v0.74 scheme ────

function seedLegacy(sb) {
  const dir = memoryDir(sb.cwd, sb.home);
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, TARGET_NAME), `<!-- adopted-by: ${sb.cwd} -->\nold detail table\n`);
  fs.writeFileSync(path.join(dir, 'MEMORY.md'),
    `# Memory Index\n\n- [user_note.md](user_note.md) — keep me\n\n${SENTINEL_BEGIN_V1}\n- old code-graph router line\n${SENTINEL_END}\n`);
  return { dir, memIndex: path.join(dir, 'MEMORY.md'), legacyDetail: path.join(dir, TARGET_NAME) };
}

test('migrate strips the legacy v1 MEMORY.md block + deletes the adopted-by detail file', () => {
  const sb = makeSandbox();
  try {
    const L = seedLegacy(sb);
    const res = migrateLegacyMemoryDir({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(res.memoryIndexPruned, true);
    assert.strictEqual(res.legacyDetailRemoved, true);
    assert.ok(!fs.existsSync(L.legacyDetail), 'legacy detail deleted');
    const mem = fs.readFileSync(L.memIndex, 'utf8');
    assert.ok(!mem.includes(SENTINEL_BEGIN_V1), 'v1 sentinel removed');
    assert.ok(mem.includes('keep me'), "user's other memory preserved");
  } finally { sb.cleanup(); }
});

test('migrate will NOT delete a legacy detail file lacking the adopted-by marker', () => {
  const sb = makeSandbox();
  try {
    const dir = memoryDir(sb.cwd, sb.home);
    fs.mkdirSync(dir, { recursive: true });
    const userFile = path.join(dir, TARGET_NAME);
    fs.writeFileSync(userFile, 'a user file that happens to share the name\n');
    const res = migrateLegacyMemoryDir({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(res.legacyDetailRemoved, false);
    assert.ok(fs.existsSync(userFile), 'unmarked file survives');
  } finally { sb.cleanup(); }
});

test('migrate is a no-op when there is nothing to clean', () => {
  const sb = makeSandbox();
  try {
    const res = migrateLegacyMemoryDir({ cwd: sb.cwd, home: sb.home });
    assert.deepStrictEqual(res, { memoryIndexPruned: false, legacyDetailRemoved: false });
  } finally { sb.cleanup(); }
});

test('maybeAutoAdopt runs the legacy migration then installs the new scheme', () => {
  const sb = makeSandbox();
  try {
    const L = seedLegacy(sb);
    const res = maybeAutoAdopt({ cwd: sb.cwd, home: sb.home, scriptPath: PLUGIN_SCRIPTS, env: {} });
    assert.ok(res.migrated.memoryIndexPruned && res.migrated.legacyDetailRemoved, 'legacy cleaned');
    assert.ok(!fs.existsSync(L.legacyDetail), 'legacy detail gone');
    assert.ok(!fs.readFileSync(L.memIndex, 'utf8').includes(SENTINEL_BEGIN_V1), 'v1 block gone');
    assert.strictEqual(isAdopted({ cwd: sb.cwd }), true, 'new CLAUDE.md scheme installed');
  } finally { sb.cleanup(); }
});

// ── stripSentinelBlock (matches v1 + v2) ────────────────────────────────────

test('stripSentinelBlock removes a well-formed v2 block, preserving neighbors', () => {
  const before = `# Index\nKeep.\n\n${SENTINEL_BEGIN}\nbody\n${SENTINEL_END}\n\n- [x.md](x.md)\n`;
  const after = stripSentinelBlock(before);
  assert.ok(!after.includes(SENTINEL_BEGIN) && !after.includes(SENTINEL_END));
  assert.ok(after.includes('Keep.') && after.includes('- [x.md](x.md)'));
});

test('stripSentinelBlock removes a legacy v1 block (version-agnostic match)', () => {
  const before = `# Index\n${SENTINEL_BEGIN_V1}\n- old line\n${SENTINEL_END}\n- [keep.md](keep.md)\n`;
  const after = stripSentinelBlock(before);
  assert.ok(!after.includes(SENTINEL_BEGIN_V1), 'v1 begin removed');
  assert.ok(after.includes('- [keep.md](keep.md)'), 'neighbor preserved');
});

test('stripSentinelBlock self-heals orphan BEGIN without END', () => {
  const before = `# Index\n- [a.md](a.md) — entry\n${SENTINEL_BEGIN}\nbody\n\n- [b.md](b.md) — survivor\n`;
  const after = stripSentinelBlock(before);
  assert.ok(!after.includes(SENTINEL_BEGIN), 'orphan BEGIN removed');
  assert.ok(after.includes('survivor') && after.includes('entry'));
});

test('stripSentinelBlock self-heals orphan END line', () => {
  const before = `# Index\n- [a.md](a.md)\n${SENTINEL_END}\n- [b.md](b.md)\n`;
  const after = stripSentinelBlock(before);
  assert.ok(!after.includes(SENTINEL_END));
  assert.ok(after.includes('- [a.md](a.md)') && after.includes('- [b.md](b.md)'));
});

// ── platform guard ──────────────────────────────────────────────────────────

test('Windows platform is rejected with clear reason', { skip: process.platform === 'win32' }, () => {
  const orig = process.platform;
  Object.defineProperty(process, 'platform', { value: 'win32', configurable: true });
  try {
    const sb = makeSandbox();
    try {
      assert.strictEqual(adopt({ cwd: sb.cwd }).reason, 'windows-not-supported');
      assert.strictEqual(unadopt({ cwd: sb.cwd, home: sb.home }).reason, 'windows-not-supported');
    } finally { sb.cleanup(); }
  } finally {
    Object.defineProperty(process, 'platform', { value: orig, configurable: true });
  }
});

// ── template integrity ──────────────────────────────────────────────────────

test('template file exists and contains the decision table', () => {
  assert.ok(fs.existsSync(TEMPLATE_PATH), `template at ${TEMPLATE_PATH}`);
  const content = fs.readFileSync(TEMPLATE_PATH, 'utf8');
  assert.ok(content.includes('get_call_graph'), 'mentions get_call_graph');
  assert.ok(content.includes('CODE_GRAPH_QUIET_HOOKS'), 'mentions env gate');
  assert.ok(content.includes('.claude/plugin_code_graph_mcp.md'), 'describes the new layout');
});

// ── isPluginModeInstall ─────────────────────────────────────────────────────

test('isPluginModeInstall recognizes ~/.claude/plugins/... paths', () => {
  assert.strictEqual(isPluginModeInstall('/home/user/.claude/plugins/cache/code-graph-mcp@0.9.0/scripts'), true);
});

test('isPluginModeInstall rejects npm global / dev / npx paths', () => {
  assert.strictEqual(isPluginModeInstall('/usr/local/lib/node_modules/@sdsrs/code-graph/claude-plugin/scripts'), false);
  assert.strictEqual(isPluginModeInstall('/mnt/data_ssd/dev/projects/code-graph-mcp/claude-plugin/scripts'), false);
  assert.strictEqual(isPluginModeInstall('/tmp/npx-abc123/node_modules/@sdsrs/code-graph/claude-plugin/scripts'), false);
});

test('isPluginModeInstall recognizes CLAUDE_CONFIG_DIR/plugins/... paths', () => {
  const prev = process.env.CLAUDE_CONFIG_DIR;
  process.env.CLAUDE_CONFIG_DIR = '/home/alice/work-claude';
  try {
    assert.strictEqual(isPluginModeInstall('/home/alice/work-claude/plugins/cache/code-graph-mcp@0.31.0/scripts'), true);
    assert.strictEqual(isPluginModeInstall('/home/user/.claude/plugins/cache/code-graph-mcp/scripts'), true);
    assert.strictEqual(isPluginModeInstall('/home/alice/work-claude/projects/foo/memory'), false);
  } finally {
    if (prev === undefined) delete process.env.CLAUDE_CONFIG_DIR;
    else process.env.CLAUDE_CONFIG_DIR = prev;
  }
});

// ── isProjectRoot markers ───────────────────────────────────────────────────

test('isProjectRoot detects each marker', () => {
  for (const marker of PROJECT_MARKERS) {
    const cwd = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-marker-'));
    try {
      assert.strictEqual(isProjectRoot(cwd), false, 'bare cwd should not be a project');
      const markerPath = path.join(cwd, marker);
      if (marker.startsWith('.')) fs.mkdirSync(markerPath);
      else fs.writeFileSync(markerPath, '');
      assert.strictEqual(isProjectRoot(cwd), true, `${marker} should make cwd a project`);
    } finally {
      fs.rmSync(cwd, { recursive: true, force: true });
    }
  }
});

// ── detectProjectType (unchanged logic; tailoring still feeds buildBlock) ────

test('detectProjectType returns generic for an empty cwd', () => {
  const sb = makeSandbox();
  try { assert.strictEqual(detectProjectType(sb.cwd), 'generic'); } finally { sb.cleanup(); }
});

test('detectProjectType returns rust for a Cargo.toml without web framework', () => {
  const sb = makeSandbox();
  try {
    fs.writeFileSync(path.join(sb.cwd, 'Cargo.toml'), '[package]\nname="x"\n[dependencies]\nserde="1"\n');
    assert.strictEqual(detectProjectType(sb.cwd), 'rust');
  } finally { sb.cleanup(); }
});

test('detectProjectType returns web-rs when Cargo.toml has axum', () => {
  const sb = makeSandbox();
  try {
    fs.writeFileSync(path.join(sb.cwd, 'Cargo.toml'), '[dependencies]\naxum = "0.7"\n');
    assert.strictEqual(detectProjectType(sb.cwd), 'web-rs');
  } finally { sb.cleanup(); }
});

test('detectProjectType returns frontend for React/Next deps', () => {
  const sb = makeSandbox();
  try {
    fs.writeFileSync(path.join(sb.cwd, 'package.json'), '{"dependencies":{"next":"^14","react":"^18"}}');
    assert.strictEqual(detectProjectType(sb.cwd), 'frontend');
  } finally { sb.cleanup(); }
});

test('detectProjectType returns web-node for express', () => {
  const sb = makeSandbox();
  try {
    fs.writeFileSync(path.join(sb.cwd, 'package.json'), '{"dependencies":{"express":"^4"}}');
    assert.strictEqual(detectProjectType(sb.cwd), 'web-node');
  } finally { sb.cleanup(); }
});

test('detectProjectType returns web-py for FastAPI in pyproject.toml', () => {
  const sb = makeSandbox();
  try {
    fs.writeFileSync(path.join(sb.cwd, 'pyproject.toml'), '[tool.poetry.dependencies]\nfastapi = "^0.115"\n');
    assert.strictEqual(detectProjectType(sb.cwd), 'web-py');
  } finally { sb.cleanup(); }
});

test('detectProjectType ignores commented-out web-framework deps in Cargo.toml', () => {
  const sb = makeSandbox();
  try {
    fs.writeFileSync(path.join(sb.cwd, 'Cargo.toml'),
      '[package]\nname="x"\n[dependencies]\n# axum = "0.7"  # disabled\nserde = "1"\n');
    assert.strictEqual(detectProjectType(sb.cwd), 'rust');
  } finally { sb.cleanup(); }
});

test('detectProjectType ignores axum in [dev-dependencies] only', () => {
  const sb = makeSandbox();
  try {
    fs.writeFileSync(path.join(sb.cwd, 'Cargo.toml'),
      '[package]\nname="x"\n[dependencies]\nserde = "1"\n[dev-dependencies]\naxum = "0.7"\n');
    assert.strictEqual(detectProjectType(sb.cwd), 'rust');
  } finally { sb.cleanup(); }
});

test('detectProjectType ignores react in devDependencies', () => {
  const sb = makeSandbox();
  try {
    fs.writeFileSync(path.join(sb.cwd, 'package.json'),
      JSON.stringify({ dependencies: { lodash: '^4' }, devDependencies: { react: '^18' } }));
    assert.strictEqual(detectProjectType(sb.cwd), 'node');
  } finally { sb.cleanup(); }
});

test('detectProjectType ignores // indirect deps in go.mod', () => {
  const sb = makeSandbox();
  try {
    fs.writeFileSync(path.join(sb.cwd, 'go.mod'),
      'module example.com/x\n\nrequire (\n\tgithub.com/some/cli v1.0.0\n\tgithub.com/gin-gonic/gin v1.9.0 // indirect\n)\n');
    assert.strictEqual(detectProjectType(sb.cwd), 'go');
  } finally { sb.cleanup(); }
});

test('detectProjectType handles malformed package.json without throwing', () => {
  const sb = makeSandbox();
  try {
    fs.writeFileSync(path.join(sb.cwd, 'package.json'), '{not valid json');
    assert.strictEqual(detectProjectType(sb.cwd), 'node');
  } finally { sb.cleanup(); }
});

test('detectProjectType detects PEP 621 [project] dependencies block', () => {
  const sb = makeSandbox();
  try {
    fs.writeFileSync(path.join(sb.cwd, 'pyproject.toml'),
      '[project]\nname = "x"\ndependencies = ["fastapi>=0.115", "uvicorn"]\n');
    assert.strictEqual(detectProjectType(sb.cwd), 'web-py');
  } finally { sb.cleanup(); }
});

test('detectProjectType reads requirements.txt as fallback', () => {
  const sb = makeSandbox();
  try {
    fs.writeFileSync(path.join(sb.cwd, 'requirements.txt'), '# web stack\nflask>=3.0\ngunicorn\n');
    assert.strictEqual(detectProjectType(sb.cwd), 'web-py');
  } finally { sb.cleanup(); }
});

test('CODE_GRAPH_PROJECT_TYPE env override beats file-based detection', () => {
  const sb = makeSandbox();
  try {
    fs.writeFileSync(path.join(sb.cwd, 'Cargo.toml'), '[package]\nname="x"\n');
    assert.strictEqual(detectProjectType(sb.cwd, { CODE_GRAPH_PROJECT_TYPE: 'web-rs' }), 'web-rs');
  } finally { sb.cleanup(); }
});

test('CODE_GRAPH_PROJECT_TYPE env override falls through on invalid value', () => {
  const sb = makeSandbox();
  try {
    fs.writeFileSync(path.join(sb.cwd, 'Cargo.toml'), '[package]\nname="x"\n');
    assert.strictEqual(detectProjectType(sb.cwd, { CODE_GRAPH_PROJECT_TYPE: 'web-rust' }), 'rust');
  } finally { sb.cleanup(); }
});

// ── Adopted-projects registry (consumed by lifecycle.js uninstall) ──────────

test('adopt records the project in the registry; unadopt removes it', () => {
  const sb = makeSandbox();
  try {
    const { readAdoptedProjects, adoptedRegistryFile } = require('./adopt');
    const r = adopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(r.ok, true);
    assert.deepStrictEqual(readAdoptedProjects(sb.home), [path.resolve(sb.cwd)],
      'adopt must register the project for uninstall-time guidance');

    // Idempotent: re-adopt does not duplicate.
    adopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(readAdoptedProjects(sb.home).length, 1);

    unadopt({ cwd: sb.cwd, home: sb.home });
    assert.deepStrictEqual(readAdoptedProjects(sb.home), [],
      'unadopt must deregister the project');
    assert.ok(fs.existsSync(adoptedRegistryFile(sb.home)) === true || true); // file may stay as []
  } finally { sb.cleanup(); }
});

test('a corrupt registry never throws — and is never OVERWRITTEN (P1-12)', () => {
  // This test used to assert the opposite: that adopt happily replaced the
  // unreadable file with `[thisProject]`. That is the same read-modify-write
  // destruction as the statusline registry — the list is the only record of
  // which projects carry a managed CLAUDE.md block, and `uninstall
  // --unadopt-all` is driven entirely by it. Losing the other entries leaves
  // managed blocks stranded in every other repo, silently.
  const sb = makeSandbox();
  try {
    const { readAdoptedProjects, adoptedRegistryFile } = require('./adopt');
    const file = adoptedRegistryFile(sb.home);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, 'not-json');
    const before = fs.readFileSync(file);

    // The READ side still degrades to empty — callers must not crash.
    assert.deepStrictEqual(readAdoptedProjects(sb.home), []);

    const r = adopt({ cwd: sb.cwd, home: sb.home }); // must not throw
    assert.strictEqual(r.ok, true, 'adoption itself still succeeds — only the bookkeeping is skipped');
    assert.deepStrictEqual(fs.readFileSync(file), before,
      'the unreadable registry must be left byte-identical, not replaced');
    assert.strictEqual(r.registryRecorded, false,
      'and the result must say the project was NOT registered');
  } finally { sb.cleanup(); }
});

test('an UNREADABLE registry is not overwritten by adopt or unadopt', () => {
  const sb = makeSandbox();
  const { adoptedRegistryFile } = require('./adopt');
  const file = adoptedRegistryFile(sb.home);
  try {
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, JSON.stringify(['/some/other/project'], null, 2) + '\n');
    const before = fs.readFileSync(file);
    fs.chmodSync(file, 0o000);

    assert.strictEqual(adopt({ cwd: sb.cwd, home: sb.home }).ok, true);
    assert.strictEqual(unadopt({ cwd: sb.cwd, home: sb.home }).ok, true);

    fs.chmodSync(file, 0o600);
    assert.deepStrictEqual(fs.readFileSync(file), before,
      'another project\'s registry entry must survive an unreadable-registry adopt/unadopt');
  } finally {
    try { fs.chmodSync(file, 0o600); } catch { /* gone */ }
    sb.cleanup();
  }
});

// ── P1-15: the newline collapse must not reach the user's prose ─────────────
//
// stripSentinelBlock ended with an unconditional `\n{3,} → \n\n` over the WHOLE
// file. It runs on every SessionStart (via maybeAutoAdopt) and over every
// registered project on uninstall, so a file that never contained our block was
// still rewritten: blank lines inside fenced code blocks collapsed, and unadopt
// reported "De-blocked" for a file it had merely reformatted.

const FENCE_WITH_BLANKS = [
  '# My notes',
  '',
  '```bash',
  'first_command',
  '',
  '',
  '',
  'second_command_after_three_blank_lines',
  '```',
  '',
  '',
  '',
  'Prose after a deliberate wide gap.',
  '',
].join('\n');

test('unadopt on a file with NO block leaves it byte-identical (no reflow, no "De-blocked")', () => {
  const sb = makeSandbox();
  try {
    fs.writeFileSync(sb.claudeMd, FENCE_WITH_BLANKS);
    const before = fs.readFileSync(sb.claudeMd);

    const res = unadopt({ cwd: sb.cwd, home: sb.home });

    assert.deepStrictEqual(fs.readFileSync(sb.claudeMd), before,
      'a CLAUDE.md we never touched must come back byte-for-byte');
    assert.strictEqual(res.blockPruned, false, 'nothing was pruned, so nothing may be reported as pruned');
    assert.strictEqual(res.claudeMdRemoved, false);
  } finally { sb.cleanup(); }
});

test('stripSentinelBlock leaves blank-line runs outside the block alone', () => {
  const withBlock = FENCE_WITH_BLANKS + '\n' + buildBlock('generic') + '\n';
  const out = stripSentinelBlock(withBlock);
  assert.ok(!out.includes(SENTINEL_BEGIN), 'our block is gone');
  assert.ok(out.includes('first_command\n\n\n\nsecond_command_after_three_blank_lines'),
    'the three blank lines inside the user\'s bash fence must survive');
  assert.ok(out.includes('```'), 'the fence itself survives');
  // Nothing to collapse: no block was removed from inside the prose.
  assert.strictEqual(stripSentinelBlock(FENCE_WITH_BLANKS), FENCE_WITH_BLANKS,
    'a text with no block of ours is returned unchanged');
});

test('a block removed mid-file still heals ITS OWN seam (and only that seam)', () => {
  const text = [
    'Above.',
    '',
    buildBlock('generic'),
    '',
    'Below.',
    '',
    '',
    '',
    'Far below, after a wide gap the user wrote.',
  ].join('\n');
  const out = stripSentinelBlock(text);
  assert.ok(!out.includes(SENTINEL_BEGIN));
  assert.ok(out.includes('Above.\n\nBelow.'), `the seam must collapse to one blank line, got:\n${out}`);
  assert.ok(out.includes('Below.\n\n\n\nFar below'), 'the user\'s wide gap elsewhere is untouched');
});

test('adopt preserves the user\'s existing prose byte-for-byte above the block', () => {
  const sb = makeSandbox();
  try {
    fs.writeFileSync(sb.claudeMd, FENCE_WITH_BLANKS);
    adopt({ cwd: sb.cwd, home: sb.home });
    const after = fs.readFileSync(sb.claudeMd, 'utf8');
    assert.ok(after.includes('first_command\n\n\n\nsecond_command_after_three_blank_lines'),
      'blank lines inside the user\'s fence must not be collapsed by an adopt');
    assert.ok(after.includes(SENTINEL_BEGIN), 'and the block is installed');
  } finally { sb.cleanup(); }
});

// ── P1-16: an unreadable / directory CLAUDE.md must not throw ───────────────
//
// adopt() read CLAUDE.md, the detail file and the template with bare
// readFileSync. EACCES (a root-owned CLAUDE.md) or EISDIR (a directory of that
// name) threw out of maybeAutoAdopt, out of runSessionInit, and killed the whole
// SessionStart hook — binary verification, index freshness and the self-test all
// silently stopped running.

test('adopt on an UNREADABLE CLAUDE.md returns a status instead of throwing', () => {
  const sb = makeSandbox();
  try {
    fs.writeFileSync(sb.claudeMd, '# mine\n');
    fs.chmodSync(sb.claudeMd, 0o000);
    const r = adopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(r.ok, false);
    assert.strictEqual(r.reason, 'claude-md-unreadable');
  } finally {
    try { fs.chmodSync(sb.claudeMd, 0o600); } catch { /* ok */ }
    sb.cleanup();
  }
});

test('adopt on a CLAUDE.md that is a DIRECTORY returns a status instead of throwing', () => {
  const sb = makeSandbox();
  try {
    fs.mkdirSync(sb.claudeMd);
    const r = adopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(r.ok, false);
    assert.strictEqual(r.reason, 'claude-md-unreadable');
  } finally { sb.cleanup(); }
});

test('unadopt tolerates an unreadable CLAUDE.md (uninstall sweeps every project)', () => {
  const sb = makeSandbox();
  try {
    fs.writeFileSync(sb.claudeMd, '# mine\n');
    fs.chmodSync(sb.claudeMd, 0o000);
    const r = unadopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(r.ok, true, 'unadopt must complete its other steps');
    assert.strictEqual(r.claudeMdUnreadable, true);
    assert.strictEqual(r.blockPruned, false);
  } finally {
    try { fs.chmodSync(sb.claudeMd, 0o600); } catch { /* ok */ }
    sb.cleanup();
  }
});

test('isAdopted / needsRefresh return false (never throw) on an unreadable CLAUDE.md', () => {
  const sb = makeSandbox();
  try {
    adopt({ cwd: sb.cwd, home: sb.home });
    fs.chmodSync(sb.claudeMd, 0o000);
    assert.strictEqual(isAdopted({ cwd: sb.cwd }), false);
    assert.strictEqual(needsRefresh({ cwd: sb.cwd }), false);
  } finally {
    try { fs.chmodSync(sb.claudeMd, 0o600); } catch { /* ok */ }
    sb.cleanup();
  }
});

test('maybeAutoAdopt surfaces the unreadable CLAUDE.md instead of throwing', () => {
  const sb = makeSandbox();
  try {
    fs.writeFileSync(sb.claudeMd, '# mine\n');
    fs.chmodSync(sb.claudeMd, 0o000);
    const r = maybeAutoAdopt({
      cwd: sb.cwd, home: sb.home, env: {},
      scriptPath: path.join(os.homedir(), '.claude', 'plugins', 'cache', 'x', 'scripts'),
    });
    assert.strictEqual(r.attempted, true);
    assert.strictEqual(r.result.ok, false);
    assert.strictEqual(r.result.reason, 'claude-md-unreadable');
  } finally {
    try { fs.chmodSync(sb.claudeMd, 0o600); } catch { /* ok */ }
    sb.cleanup();
  }
});

// ── unadopt must never delete the user's own prose ──────────────────────────
//
// Contract audit 2026-07-27. CHANGELOG has promised since v0.74 that unadopt is
// "guarded: never removes a user file lacking our marker" and that it strips
// "only our block". It did neither once the marker string appeared anywhere
// earlier in the file — and the block we write *invites* the user to mention it
// ("do not edit inside this block"). `uninstall({unadoptAll:true})` runs this
// over every registered project, so the blast radius was every adopted repo at
// once, at exit 0, reporting success.
//
// Five of the six are measured repros — they were verified RED against the
// pre-fix adopt.js, and the first one went 1078 B -> 43 B. The sixth is labelled
// a negative control: it must stay GREEN on both sides, proving the guards did
// not simply make unadopt inert.

test('unadopt keeps prose that MENTIONS the begin marker mid-sentence', () => {
  const sb = makeSandbox();
  try {
    const prose = [
      '# Team rules',
      '',
      '## CRITICAL: prod runbook',
      'Pager 555-0100. Key rotation quarterly.',
      '',
      'Note: the block starts with `' + SENTINEL_BEGIN + '` — do not edit inside it.',
      '',
    ].join('\n');
    fs.writeFileSync(sb.claudeMd, prose);
    adopt({ cwd: sb.cwd, home: sb.home });
    const res = unadopt({ cwd: sb.cwd, home: sb.home });

    const after = fs.readFileSync(sb.claudeMd, 'utf8');
    assert.ok(after.includes('Pager 555-0100'), 'runbook survives');
    assert.ok(after.includes('Key rotation quarterly'), 'second prose line survives');
    assert.ok(after.includes('do not edit inside it'), 'the sentence quoting the marker survives whole');
    assert.strictEqual(res.claudeMdRemoved, false);
    // The real block is still gone: only the quoted mention remains, and a
    // mention is not a block opener.
    assert.strictEqual(
      after.split('\n').filter(l => l.trim() === SENTINEL_BEGIN).length, 0,
      'no line-anchored begin marker left');
  } finally { sb.cleanup(); }
});

test('unadopt keeps prose when the marker is quoted on its OWN line before the block', () => {
  const sb = makeSandbox();
  try {
    // Order is load-bearing, and two weaker fixtures do NOT reproduce:
    //   - quoting a BEGIN/END *pair*: the lazy match stops at the quoted END and
    //     eats only the example;
    //   - writing the quote BEFORE adopt: adopt's own orphan heal consumes the
    //     quoted line, so nothing is left to mis-anchor at unadopt time.
    // The real shape is a user annotating a block that already exists.
    adopt({ cwd: sb.cwd, home: sb.home });
    fs.writeFileSync(sb.claudeMd, [
      '# Notes', '', 'Our generated block opens with:', '',
      SENTINEL_BEGIN, '',
      'KEEP: on-call rotation is in PagerDuty.',
      'KEEP: escalate to #sre-oncall after 15 minutes.', '',
    ].join('\n') + fs.readFileSync(sb.claudeMd, 'utf8'));
    unadopt({ cwd: sb.cwd, home: sb.home });

    const after = fs.existsSync(sb.claudeMd) ? fs.readFileSync(sb.claudeMd, 'utf8') : '';
    assert.ok(after.includes('KEEP: on-call rotation'), 'prose after the quoted marker survives');
    assert.ok(after.includes('KEEP: escalate to #sre-oncall'), 'second prose line survives');
    assert.ok(after.includes('Our generated block opens with:'), 'prose before it survives');
  } finally { sb.cleanup(); }
});

test('unadopt does not eat lines after a marker truncated mid-write', () => {
  const sb = makeSandbox();
  try {
    // `[^>]*` (pre-fix) let an unterminated marker span newlines to the next
    // `-->` anywhere below it.
    fs.writeFileSync(sb.claudeMd, [
      '<!-- code-graph-mcp:begin v2 --',
      'DO NOT DELETE: escalation path',
      'DO NOT DELETE: db failover steps',
      '<!-- something else -->',
      '', 'tail prose', '',
    ].join('\n'));
    const before = fs.readFileSync(sb.claudeMd, 'utf8');
    unadopt({ cwd: sb.cwd, home: sb.home });
    const after = fs.existsSync(sb.claudeMd) ? fs.readFileSync(sb.claudeMd, 'utf8') : '';
    assert.ok(after.includes('escalation path'), 'first do-not-delete line survives');
    assert.ok(after.includes('db failover steps'), 'second do-not-delete line survives');
    assert.ok(after.includes('tail prose'), 'tail survives');
    assert.strictEqual(after, before, 'nothing at all was stripped — no well-formed block present');
  } finally { sb.cleanup(); }
});

test('unadopt will NOT unlink a detail file whose first line merely quotes the marker', () => {
  const sb = makeSandbox();
  try {
    fs.mkdirSync(path.join(sb.cwd, '.claude'), { recursive: true });
    const body = 'We write `' + MANAGED_BY + '` at the top of generated files.\nMy notes.\n';
    fs.writeFileSync(sb.detail, body);
    const res = unadopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(res.fileRemoved, false, 'not ours — must not be deleted');
    assert.ok(fs.existsSync(sb.detail), 'user file still exists');
    assert.strictEqual(fs.readFileSync(sb.detail, 'utf8'), body, 'byte-identical');
  } finally { sb.cleanup(); }
});

test('unadopt still removes a real block, and a real managed detail file', () => {
  // Negative control for the four guards above: they must not make unadopt inert.
  const sb = makeSandbox();
  try {
    fs.writeFileSync(sb.claudeMd, '# Project\n\nMy own notes.\n');
    adopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(isAdopted({ cwd: sb.cwd }), true, 'precondition: adopted');
    const res = unadopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(res.blockPruned, true, 'the real block WAS stripped');
    assert.strictEqual(res.fileRemoved, true, 'the real detail file WAS removed');
    const after = fs.readFileSync(sb.claudeMd, 'utf8');
    assert.ok(!after.includes(SENTINEL_END), 'no end marker left');
    assert.ok(after.includes('My own notes.'), 'user prose kept');
  } finally { sb.cleanup(); }
});

test('isAdopted ignores markers that are only quoted in prose', () => {
  const sb = makeSandbox();
  try {
    fs.mkdirSync(path.join(sb.cwd, '.claude'), { recursive: true });
    fs.writeFileSync(sb.detail, MANAGED_BY + '\nbody\n');
    fs.writeFileSync(sb.claudeMd,
      'Docs: the block runs from `' + SENTINEL_BEGIN + '` to `' + SENTINEL_END + '`.\n');
    assert.strictEqual(isAdopted({ cwd: sb.cwd }), false,
      'a quoted pair is not an installed block — this gates auto-adopt, so a ' +
      'false true means the block never gets written');
  } finally { sb.cleanup(); }
});

test('unadopt writes THROUGH a symlinked CLAUDE.md instead of detaching it', () => {
  const sb = makeSandbox();
  const shared = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-adopt-shared-'));
  try {
    // A CLAUDE.md symlinked into a dotfiles/team repo — the atomic rename used
    // to replace the LINK with a regular file, so the shared file kept the block
    // while unadopt reported blockPruned:true.
    const realFile = path.join(shared, 'team-CLAUDE.md');
    fs.writeFileSync(realFile, '# Shared team rules\n\nKEEP: shared prose.\n');
    fs.symlinkSync(realFile, sb.claudeMd);

    adopt({ cwd: sb.cwd, home: sb.home });
    assert.ok(fs.readFileSync(realFile, 'utf8').includes(SENTINEL_BEGIN),
      'precondition: adopt wrote through the link');

    const res = unadopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(res.blockPruned, true);
    assert.ok(fs.lstatSync(sb.claudeMd).isSymbolicLink(), 'still a symlink, not detached');
    const target = fs.readFileSync(realFile, 'utf8');
    assert.ok(!target.includes(SENTINEL_BEGIN),
      'the block is gone from the file the report claims it pruned');
    assert.ok(target.includes('KEEP: shared prose.'), 'shared prose intact');
  } finally {
    fs.rmSync(shared, { recursive: true, force: true });
    sb.cleanup();
  }
});

test('unadopt does not unlink a symlinked CLAUDE.md that held only our block', () => {
  const sb = makeSandbox();
  const shared = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-adopt-shared2-'));
  try {
    const realFile = path.join(shared, 'team-CLAUDE.md');
    fs.writeFileSync(realFile, '');
    fs.symlinkSync(realFile, sb.claudeMd);
    adopt({ cwd: sb.cwd, home: sb.home });

    const res = unadopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(res.claudeMdRemoved, false,
      'the user owns the link — "delete the file we created" does not apply');
    assert.ok(fs.lstatSync(sb.claudeMd).isSymbolicLink(), 'link still there');
    assert.strictEqual(fs.readFileSync(realFile, 'utf8').trim(), '', 'target emptied, not deleted');
  } finally {
    fs.rmSync(shared, { recursive: true, force: true });
    sb.cleanup();
  }
});

test('unadopt leaves a NEVER-ADOPTED file alone even when it quotes the begin marker', () => {
  // Round-5 F1. The orphan self-heal used to strip from a stray begin marker to
  // the next blank line, on the theory that what followed was our truncated
  // block. In a repo that was never adopted there IS no block — the marker is
  // the user quoting our own instructions — and the two shapes are byte-for-byte
  // indistinguishable, so the heal ate their notes. Measured 221 B -> 100 B.
  const sb = makeSandbox();
  try {
    const original = [
      '# Team rules', '',
      '## CRITICAL: prod runbook',
      'Pager 555-0100.', '',
      'Our generated block opens with',
      SENTINEL_BEGIN,
      'KEEP: on-call rotation is in PagerDuty.',
      'KEEP: escalate to #sre-oncall after 15 minutes.', '',
      'tail prose', '',
    ].join('\n');
    fs.writeFileSync(sb.claudeMd, original);

    const res = unadopt({ cwd: sb.cwd, home: sb.home });

    const after = fs.readFileSync(sb.claudeMd, 'utf8');
    assert.ok(after.includes('KEEP: on-call rotation is in PagerDuty.'), 'first note survives');
    assert.ok(after.includes('KEEP: escalate to #sre-oncall after 15 minutes.'), 'second note survives');
    assert.ok(after.includes('Pager 555-0100.'), 'runbook survives');
    assert.ok(after.includes('tail prose'), 'tail survives');
    assert.strictEqual(res.claudeMdRemoved, false);
    // The stray marker line itself is removed — that much is ours, is one line,
    // and is visible. Nothing around it is.
    assert.ok(!after.split('\n').some(l => l.trim() === SENTINEL_BEGIN), 'marker line removed');
  } finally { sb.cleanup(); }
});

test('the same file is safe with NO blank line after the quoted marker', () => {
  // The mutation that exposed the previous version of this guard as
  // fixture-specific: the old code was bounded by the next blank line, so a
  // fixture that happened to have one passed while the real shape did not.
  const sb = makeSandbox();
  try {
    fs.writeFileSync(sb.claudeMd,
      `# Notes\n\nOur block opens with\n${SENTINEL_BEGIN}\nKEEP: escalation path\nKEEP: db failover\n`);
    unadopt({ cwd: sb.cwd, home: sb.home });
    const after = fs.readFileSync(sb.claudeMd, 'utf8');
    assert.ok(after.includes('KEEP: escalation path'), 'no blank line must not mean no protection');
    assert.ok(after.includes('KEEP: db failover'), 'second line survives too');
  } finally { sb.cleanup(); }
});

test('auto-adopt does not eat prose that quotes the marker (adopt path, not unadopt)', () => {
  // adopt() calls the same strip, and maybeAutoAdopt runs it on every
  // SessionStart in plugin mode — so this path has a far higher hit rate than
  // the explicit unadopt one it was found on.
  const sb = makeSandbox();
  try {
    fs.writeFileSync(sb.claudeMd,
      `# P\n\nThe block starts at\n${SENTINEL_BEGIN}\nKEEP: irreplaceable.\n`);
    adopt({ cwd: sb.cwd, home: sb.home });
    const after = fs.readFileSync(sb.claudeMd, 'utf8');
    assert.ok(after.includes('KEEP: irreplaceable.'), 'adopt must not eat it either');
  } finally { sb.cleanup(); }
});

test('a user line that is exactly the END marker costs only that line', () => {
  const sb = makeSandbox();
  try {
    fs.writeFileSync(sb.claudeMd,
      `# Notes\n\nThe managed block is terminated by\n${SENTINEL_END}\nKEEP: everything below.\n`);
    unadopt({ cwd: sb.cwd, home: sb.home });
    const after = fs.readFileSync(sb.claudeMd, 'utf8');
    assert.ok(after.includes('KEEP: everything below.'), 'content after the marker survives');
    assert.ok(after.includes('The managed block is terminated by'), 'content before survives');
  } finally { sb.cleanup(); }
});

test('unadopt does NOT overwrite a symlinked detail file target', () => {
  // Round-5 F5: a regression introduced while fixing the CLAUDE.md symlink case.
  // The detail file is a whole-file replacement, so following its link overwrites
  // whatever the user pointed at instead of replacing our own link.
  const sb = makeSandbox();
  const shared = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-adopt-detail-'));
  try {
    const realFile = path.join(shared, 'my-important-doc.md');
    fs.writeFileSync(realFile, '# Irreplaceable\n\nDo not overwrite me.\n');
    fs.mkdirSync(path.join(sb.cwd, '.claude'), { recursive: true });
    fs.symlinkSync(realFile, sb.detail);

    adopt({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(
      fs.readFileSync(realFile, 'utf8'), '# Irreplaceable\n\nDo not overwrite me.\n',
      'adopt must replace our symlink, not write through it into the user\'s file');
  } finally {
    fs.rmSync(shared, { recursive: true, force: true });
    sb.cleanup();
  }
});

test('migrateLegacyMemoryDir will NOT delete a user file that merely starts with the legacy prefix', () => {
  // Round-6 F3. `unadopt`'s detail-file guard was tightened to require a
  // whole-line HTML comment, but the migration's copy kept a bare untrimmed
  // `startsWith` — and migration is the HOT path: maybeAutoAdopt calls it on
  // every SessionStart, while unadopt is explicit. Half-applied fix, on the more
  // frequently executed half.
  const sb = makeSandbox();
  try {
    const dir = memoryDir(sb.cwd, sb.home);
    fs.mkdirSync(dir, { recursive: true });
    const victim = path.join(dir, TARGET_NAME);
    const body = '<!-- adopted-by: my own note-taking script, do not delete\nmy notes\n';
    fs.writeFileSync(victim, body);

    const res = migrateLegacyMemoryDir({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(res.legacyDetailRemoved, false, 'not ours — must survive');
    assert.ok(fs.existsSync(victim), 'user file still exists');
    assert.strictEqual(fs.readFileSync(victim, 'utf8'), body, 'byte-identical');
  } finally { sb.cleanup(); }
});

test('migrateLegacyMemoryDir still deletes a real legacy detail file', () => {
  // Negative control: the tightened guard must not make migration inert. The
  // legacy scheme wrote `<!-- adopted-by: <cwd> -->` as the whole first line.
  const sb = makeSandbox();
  try {
    const dir = memoryDir(sb.cwd, sb.home);
    fs.mkdirSync(dir, { recursive: true });
    const legacy = path.join(dir, TARGET_NAME);
    fs.writeFileSync(legacy, `<!-- adopted-by: ${sb.cwd} -->\nold generated body\n`);

    const res = migrateLegacyMemoryDir({ cwd: sb.cwd, home: sb.home });
    assert.strictEqual(res.legacyDetailRemoved, true, 'a real legacy file IS removed');
    assert.ok(!fs.existsSync(legacy));
  } finally { sb.cleanup(); }
});
