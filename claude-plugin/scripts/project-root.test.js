'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const { indexBuildInProgress, INDEXING_STALE_MS, indexKey, worktreePrefix } = require('./project-root');

function project(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-project-root-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(path.join(root, '.code-graph'));
  return root;
}

function writeStatus(root, body) {
  const file = path.join(root, '.code-graph', 'indexing-status.json');
  fs.writeFileSync(file, typeof body === 'string' ? body : JSON.stringify(body));
  return file;
}

test('indexBuildInProgress: a fresh indexing or finalizing status means the index is partial', (t) => {
  const root = project(t);
  writeStatus(root, { s: 'indexing', d: 3, t: 50 });
  assert.equal(indexBuildInProgress(root), true);
  writeStatus(root, { s: 'finalizing', d: 50, t: 50 });
  assert.equal(indexBuildInProgress(root), true);
});

test('indexBuildInProgress: no status file, or one a killed server left behind, is not a build', (t) => {
  const root = project(t);
  assert.equal(indexBuildInProgress(root), false, 'no file');
  const file = writeStatus(root, { s: 'indexing', d: 3, t: 50 });
  const old = (Date.now() - INDEXING_STALE_MS - 1000) / 1000;
  fs.utimesSync(file, old, old);
  assert.equal(indexBuildInProgress(root), false, 'stale file');
});

test('indexBuildInProgress: anything but a live indexing/finalizing record with files is not a build', (t) => {
  const root = project(t);
  for (const body of [{ s: 'indexing', d: 0, t: 0 }, { s: 'done', d: 5, t: 5 }, {}, 'not json']) {
    writeStatus(root, body);
    assert.equal(indexBuildInProgress(root), false, JSON.stringify(body));
  }
  assert.equal(indexBuildInProgress(null), false);
});

test('INDEXING_STALE_MS mirrors statusline.js and the Rust INDEXING_STATUS_STALE_SECS', () => {
  assert.equal(INDEXING_STALE_MS, 120000);
  const statusline = fs.readFileSync(path.join(__dirname, 'statusline.js'), 'utf8');
  assert.match(statusline, /const INDEXING_STALE_MS = 120000;/);
  const rust = fs.readFileSync(path.join(__dirname, '..', '..', 'src', 'indexer', 'pipeline', 'mod.rs'), 'utf8');
  assert.match(rust, /pub const INDEXING_STATUS_STALE_SECS: u64 = 120;/);
});

test('indexBuildInProgress: "starting" is a build; a status whose writer has exited is not', (t) => {
  // E2E 2026-10-16 round 2: the MCP server now says "starting" before its tree
  // walk (no file count yet), and names its pid so a killed server's leftover
  // file stops reading as a build at once instead of after the stale window.
  const { spawnSync } = require('node:child_process');
  const root = project(t);
  writeStatus(root, { s: 'starting', pid: process.pid });
  assert.equal(indexBuildInProgress(root), true, 'starting, live writer');
  const exited = spawnSync(process.execPath, ['-e', 'process.stdout.write(String(process.pid))'], { encoding: 'utf8' });
  const deadPid = Number(exited.stdout);
  assert.ok(deadPid > 0);
  writeStatus(root, { s: 'indexing', d: 3, t: 50, pid: deadPid });
  assert.equal(indexBuildInProgress(root), false, 'writer gone');
  writeStatus(root, { s: 'indexing', d: 3, t: 50, pid: process.pid });
  assert.equal(indexBuildInProgress(root), true, 'control: same file, live writer');
});

// D#560: a worktree with no index of its own reads the main checkout's index,
// which stores the main checkout's layout. A key that runs through the worktree
// names the worktree's copy; the index knows it by the path from the worktree.
function linkedDir(root, rel, gitdir) {
  const dir = path.join(root, ...rel.split('/'));
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, '.git'), `gitdir: ${gitdir}\n`);
  return dir;
}

test('indexKey: a key through a linked worktree of the root drops the worktree prefix', (t) => {
  const root = project(t);
  linkedDir(root, '.claude/worktrees/wt', path.join(root, '.git', 'worktrees', 'wt'));
  assert.equal(indexKey(root, '.claude/worktrees/wt/src/a.js'), 'src/a.js');
  assert.equal(indexKey(root, '.claude/worktrees/wt/top.js'), 'top.js');
  // Any other key is the index's own.
  assert.equal(indexKey(root, 'src/a.js'), 'src/a.js');
  assert.equal(indexKey(root, '.claude/worktrees/other/src/a.js'), '.claude/worktrees/other/src/a.js');
  assert.equal(indexKey(root, 'main.js'), 'main.js');
  assert.equal(indexKey(root, ''), '');
});

test('indexKey: a submodule, a worktree of another repo, and a key leaving the root keep their key', (t) => {
  const root = project(t);
  linkedDir(root, 'vendor/lib', path.join(root, '.git', 'modules', 'lib'));
  linkedDir(root, 'nested/wt', path.join(os.tmpdir(), 'elsewhere', '.git', 'worktrees', 'wt'));
  // Under a hidden directory too, where the worktree rule looks at it: only a
  // worktree whose main checkout is this root is mapped (review of 0.178.0).
  linkedDir(root, '.cache/otherwt', path.join(os.tmpdir(), 'elsewhere', '.git', 'worktrees', 'otherwt'));
  assert.equal(indexKey(root, 'vendor/lib/x.js'), 'vendor/lib/x.js');
  assert.equal(indexKey(root, 'nested/wt/x.js'), 'nested/wt/x.js');
  assert.equal(indexKey(root, '.cache/otherwt/src/a.js'), '.cache/otherwt/src/a.js');
  // A worktree of this root outside it (`git worktree add ../wt`): its files
  // are outside the project, and `..` must not read as a hidden directory.
  const outside = linkedDir(path.dirname(root), path.basename(root) + '-outwt', path.join(root, '.git', 'worktrees', 'outwt'));
  t.after(() => fs.rmSync(outside, { recursive: true, force: true }));
  const outKey = `../${path.basename(outside)}/src/a.js`;
  assert.equal(indexKey(root, outKey), outKey);
  assert.equal(indexKey(root, '../wt/src/a.js'), '../wt/src/a.js');
});

// The indexer skips hidden directories, so a worktree under one is never in
// the index; a worktree elsewhere in the project is indexed under its own path
// unless ignored, and stripping its prefix asked for main's copy instead
// (review of D#560: 13 callers became 12).
test('indexKey: a worktree outside hidden directories keeps its prefix', (t) => {
  const root = project(t);
  linkedDir(root, 'wt2', path.join(root, '.git', 'worktrees', 'wt2'));
  linkedDir(root, '.wt/x', path.join(root, '.git', 'worktrees', 'x'));
  assert.equal(indexKey(root, 'wt2/src/a.js'), 'wt2/src/a.js');
  assert.equal(indexKey(root, '.wt/x/src/a.js'), 'src/a.js');
});

test('indexKey: a root reached through a symlink still matches its worktrees', (t) => {
  const real = project(t);
  linkedDir(real, '.claude/worktrees/wt', path.join(fs.realpathSync(real), '.git', 'worktrees', 'wt'));
  const link = path.join(os.tmpdir(), `cg-project-root-link-${process.pid}`);
  fs.symlinkSync(real, link);
  t.after(() => fs.rmSync(link, { force: true }));
  assert.equal(indexKey(link, '.claude/worktrees/wt/src/a.js'), 'src/a.js');
});

test('worktreePrefix: the worktree a session directory sits in, or none', (t) => {
  const root = project(t);
  linkedDir(root, '.claude/worktrees/wt', path.join(root, '.git', 'worktrees', 'wt'));
  assert.equal(worktreePrefix(root, '.claude/worktrees/wt'), '.claude/worktrees/wt/');
  assert.equal(worktreePrefix(root, '.claude/worktrees/wt/src'), '.claude/worktrees/wt/');
  assert.equal(worktreePrefix(root, 'src'), '');
  assert.equal(worktreePrefix(root, ''), '');
});

test('indexKey: the key keeps its own separators', { skip: path.sep !== '\\' && 'Windows separators' }, (t) => {
  const root = project(t);
  linkedDir(root, '.claude/worktrees/wt', path.join(root, '.git', 'worktrees', 'wt'));
  assert.equal(indexKey(root, '.claude\\worktrees\\wt\\src\\a.js'), 'src\\a.js');
});
