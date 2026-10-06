'use strict';
// Shapes 1–24 of tasks/specs/steering-channel.md (design r4): when SessionStart
// writes .claude/rules/code-graph.md, and when it must not.
const test = require('node:test');
const assert = require('node:assert');
const fs = require('fs');
const path = require('path');
const os = require('os');
const { execFileSync } = require('child_process');

// In-process calls record into the adopted-projects registry under HOME.
const ISOLATED_HOME = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-rules-home-'));
delete process.env.CLAUDE_CONFIG_DIR;
process.env.HOME = ISOLATED_HOME;
process.env.USERPROFILE = ISOLATED_HOME;
test.after(() => fs.rmSync(ISOLATED_HOME, { recursive: true, force: true }));

const { maybeWriteRulesFile, RULES_MARKER } = require('./rules-file');
const {
  unadopt, buildRulesFile, detectProjectType, readAdoptedProjects, MANAGED_BY, SENTINEL_BEGIN, SENTINEL_END,
  RULES_REL,
} = require('./adopt');

// isPluginModeInstall keys on a /.claude/plugins/ path segment.
const PLUGIN_SCRIPTS = path.join(path.sep, 'x', '.claude', 'plugins', 'code-graph-mcp', 'scripts');
const ENV = {};

const git = (cwd, ...args) => execFileSync('git', args, {
  cwd, stdio: ['ignore', 'pipe', 'pipe'], encoding: 'utf8',
  env: { ...process.env, GIT_CONFIG_NOSYSTEM: '1', HOME: ISOLATED_HOME, USERPROFILE: ISOLATED_HOME },
});

/** A git top-level with an index dir — the one shape that gets the file. */
function mkRepo(t, { indexDir = true } = {}) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-rules-repo-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  git(dir, 'init', '-q');
  // As the index build does (0.164.0: the `.code-graph/` rule goes to info/exclude).
  fs.appendFileSync(path.join(dir, '.git', 'info', 'exclude'), '/.code-graph/\n');
  if (indexDir) fs.mkdirSync(path.join(dir, '.code-graph'));
  return fs.realpathSync(dir);
}

const run = (cwd, extra = {}) => maybeWriteRulesFile({
  cwd, home: ISOLATED_HOME, env: ENV, scriptPath: PLUGIN_SCRIPTS, ...extra,
});
const rulesAt = (dir) => path.join(dir, RULES_REL);
const excludeOf = (dir) => fs.readFileSync(path.join(dir, '.git', 'info', 'exclude'), 'utf8');
const desired = (dir) => buildRulesFile(detectProjectType(dir));

test('1: a git top-level with .code-graph/ gets the file, an exclude line and a registry entry', (t) => {
  const dir = mkRepo(t);
  const r = run(dir);
  assert.equal(r.action, 'created', JSON.stringify(r));
  assert.equal(fs.readFileSync(rulesAt(dir), 'utf8'), desired(dir));
  assert.equal(r.text, desired(dir), 'the creating session is handed the same text');
  assert.match(excludeOf(dir), /^\/\.claude\/rules\/code-graph\.md$/m);
  assert.equal(git(dir, 'status', '--porcelain', '--untracked-files=all'), '', 'git status stays clean');
  assert.ok(readAdoptedProjects(ISOLATED_HOME).includes(dir), 'recorded for the uninstall sweep');
  assert.ok(fs.existsSync(path.join(dir, '.code-graph', RULES_MARKER)));
});

test('the file carries the block text, no detail-doc pointer, and says what to do once the plugin is gone', () => {
  const text = buildRulesFile('generic');
  assert.equal(text.split('\n', 1)[0], MANAGED_BY);
  assert.match(text, /## Code Graph \(repo-wide AST index\)/);
  assert.match(text, /\| Who calls X \/ what X calls \| `code-graph-mcp callgraph X` \|/);
  assert.doesNotMatch(text, /plugin_code_graph_mcp\.md/);
  assert.match(text, /If `code-graph-mcp` is not found,\s+the plugin has been removed: ignore this file\./);
  assert.ok(!text.includes(SENTINEL_BEGIN) && !text.includes(SENTINEL_END),
    'a whole file we own is marked by its first line, not by CLAUDE.md sentinels');
});

test('2 + 3: unchanged when current, rewritten when older', (t) => {
  const dir = mkRepo(t);
  run(dir);
  const exclude = excludeOf(dir);
  assert.equal(run(dir).action, 'unchanged');
  fs.writeFileSync(rulesAt(dir), `${MANAGED_BY}\nolder text\n`);
  assert.equal(run(dir).action, 'updated');
  assert.equal(fs.readFileSync(rulesAt(dir), 'utf8'), desired(dir));
  assert.equal(excludeOf(dir), exclude, 'the exclude line is written once');
});

test('4: a file at the path without our first line is left alone', (t) => {
  const dir = mkRepo(t);
  fs.mkdirSync(path.dirname(rulesAt(dir)), { recursive: true });
  fs.writeFileSync(rulesAt(dir), '# my own code-graph notes\n');
  const r = run(dir);
  assert.equal(r.action, 'refused');
  assert.equal(r.reason, 'foreign-file');
  assert.equal(fs.readFileSync(rulesAt(dir), 'utf8'), '# my own code-graph notes\n');
});

test('5 + 6 + 7: any symlink on the path is refused', (t) => {
  for (const shape of ['.claude', 'rules', 'file']) {
    const dir = mkRepo(t);
    const elsewhere = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-rules-target-'));
    t.after(() => fs.rmSync(elsewhere, { recursive: true, force: true }));
    if (shape === '.claude') {
      fs.symlinkSync(elsewhere, path.join(dir, '.claude'));
    } else if (shape === 'rules') {
      fs.mkdirSync(path.join(dir, '.claude'));
      fs.symlinkSync(elsewhere, path.join(dir, '.claude', 'rules'));
    } else {
      fs.mkdirSync(path.join(dir, '.claude', 'rules'), { recursive: true });
      fs.writeFileSync(path.join(elsewhere, 'x.md'), `${MANAGED_BY}\nold\n`);
      fs.symlinkSync(path.join(elsewhere, 'x.md'), rulesAt(dir));
    }
    const r = run(dir);
    assert.equal(r.action, 'refused', `${shape}: ${JSON.stringify(r)}`);
    assert.equal(r.reason, 'symlink');
    assert.deepEqual(fs.readdirSync(elsewhere).filter((f) => f !== 'x.md'), [], `${shape}: nothing written through the link`);
  }
});

test('8: a tracked path is refused', (t) => {
  const dir = mkRepo(t);
  fs.mkdirSync(path.dirname(rulesAt(dir)), { recursive: true });
  fs.writeFileSync(rulesAt(dir), `${MANAGED_BY}\nteam copy\n`);
  git(dir, 'add', RULES_REL);
  const r = run(dir);
  assert.equal(r.action, 'refused');
  assert.equal(r.reason, 'tracked');
  assert.equal(fs.readFileSync(rulesAt(dir), 'utf8'), `${MANAGED_BY}\nteam copy\n`);
});

test('9 + 24: no git work tree, or no .code-graph/, writes nothing', (t) => {
  const bare = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-rules-nogit-'));
  t.after(() => fs.rmSync(bare, { recursive: true, force: true }));
  fs.mkdirSync(path.join(bare, '.code-graph'));
  assert.equal(run(bare).action, 'skipped');
  assert.equal(fs.existsSync(path.join(bare, '.claude')), false);

  const noIndex = mkRepo(t, { indexDir: false });
  assert.equal(run(noIndex).action, 'skipped');
  assert.equal(fs.existsSync(path.join(noIndex, '.claude')), false);
});

test('10: a git top-level at $HOME writes nothing', (t) => {
  const dir = mkRepo(t);
  const r = run(dir, { home: dir });
  assert.equal(r.action, 'skipped');
  assert.equal(r.reason, 'home-or-root');
  assert.equal(fs.existsSync(path.join(dir, '.claude')), false);
});

test('11: a session in a subdirectory writes nothing', (t) => {
  const dir = mkRepo(t);
  const sub = path.join(dir, 'src');
  fs.mkdirSync(path.join(sub, '.code-graph'), { recursive: true });
  const r = run(sub);
  assert.equal(r.action, 'skipped');
  assert.equal(r.reason, 'not-top-level');
  assert.equal(fs.existsSync(path.join(dir, '.claude')), false);
  assert.equal(fs.existsSync(path.join(sub, '.claude')), false);
});

test('12 + 13 + 14: an npm root that would publish the file is refused', (t) => {
  const cases = [
    [{ name: 'p', version: '1.0.0' }, 'refused'],
    [{ name: 'p', version: '1.0.0', private: true }, 'created'],
    [{ name: 'p', version: '1.0.0', files: ['dist', 'README.md'] }, 'created'],
    [{ name: 'p', version: '1.0.0', files: ['dist', '.claude'] }, 'refused'],
    [{ name: 'p', version: '1.0.0', files: ['./.claude/rules'] }, 'refused'],
    [{ name: 'p', version: '1.0.0', files: ['**/*.md'] }, 'refused'],
    [{ name: 'p', version: '1.0.0', files: ['.'] }, 'refused'],
    [{ name: 'p', version: '1.0.0', files: [''] }, 'refused'],
  ];
  for (const [pkg, want] of cases) {
    const dir = mkRepo(t);
    fs.writeFileSync(path.join(dir, 'package.json'), JSON.stringify(pkg));
    const r = run(dir);
    assert.equal(r.action, want, `${JSON.stringify(pkg)} → ${JSON.stringify(r)}`);
    if (want === 'refused') {
      assert.equal(r.reason, 'npm-publishable');
      assert.equal(fs.existsSync(rulesAt(dir)), false);
    }
  }
  // An unreadable package.json is not proof of "private".
  const dir = mkRepo(t);
  fs.writeFileSync(path.join(dir, 'package.json'), '{ not json');
  assert.equal(run(dir).action, 'refused');
});

test('12b: our file is taken out when the root later becomes publishable', (t) => {
  const dir = mkRepo(t);
  assert.equal(run(dir).action, 'created');
  fs.writeFileSync(path.join(dir, 'package.json'), JSON.stringify({ name: 'p', version: '1.0.0' }));
  const r = run(dir);
  assert.equal(r.action, 'refused');
  assert.equal(fs.existsSync(rulesAt(dir)), false, 'npm publish would have shipped it');
  // Not a user removal: a root that goes private again gets it back.
  fs.writeFileSync(path.join(dir, 'package.json'), JSON.stringify({ name: 'p', private: true }));
  assert.equal(run(dir).action, 'created');
});

test('15: a CLAUDE.md that already holds our block gets no second copy', (t) => {
  const dir = mkRepo(t);
  fs.writeFileSync(path.join(dir, 'CLAUDE.md'), `# notes\n\n${SENTINEL_BEGIN}\nx\n${SENTINEL_END}\n`);
  const r = run(dir);
  assert.equal(r.action, 'skipped');
  assert.equal(r.reason, 'claude-md-block');
  assert.equal(fs.existsSync(path.join(dir, '.claude')), false);
});

test('16: a file the user deleted is not written back', (t) => {
  const dir = mkRepo(t);
  run(dir);
  fs.unlinkSync(rulesAt(dir));
  const r = run(dir);
  assert.equal(r.action, 'skipped');
  assert.equal(r.reason, 'removed-by-user');
  assert.equal(fs.existsSync(rulesAt(dir)), false);
});

test('17: no file when the exclude line cannot be written', (t) => {
  const dir = mkRepo(t);
  const info = path.join(dir, '.git', 'info');
  fs.rmSync(info, { recursive: true, force: true });
  fs.writeFileSync(info, 'a file where git expects a directory');
  const r = run(dir);
  assert.equal(r.action, 'refused');
  assert.equal(r.reason, 'exclude-unwritable');
  assert.equal(fs.existsSync(rulesAt(dir)), false, 'an unexcluded file would show in git status');
});

test('18: a path git already ignores gets no second exclude line', (t) => {
  const dir = mkRepo(t);
  fs.writeFileSync(path.join(dir, '.gitignore'), '.claude/\n');
  const before = excludeOf(dir);
  assert.equal(run(dir).action, 'created');
  assert.equal(excludeOf(dir), before);
});

test('19 + 20: the opt-out and a non-plugin install write nothing', (t) => {
  const dir = mkRepo(t);
  assert.equal(run(dir, { env: { CODE_GRAPH_NO_AUTO_ADOPT: '1' } }).action, 'skipped');
  assert.equal(run(dir, { scriptPath: path.join(path.sep, 'checkout', 'claude-plugin', 'scripts') }).action, 'skipped');
  assert.equal(fs.existsSync(path.join(dir, '.claude')), false);
});

test('21: a linked worktree gets its own file; the exclude line goes to the common dir', (t) => {
  const main = mkRepo(t);
  fs.writeFileSync(path.join(main, 'a.txt'), 'a\n');
  git(main, 'add', 'a.txt');
  git(main, '-c', 'user.email=t@x', '-c', 'user.name=t', 'commit', '-qm', 'a');
  const wt = path.join(path.dirname(main), path.basename(main) + '-wt');
  t.after(() => fs.rmSync(wt, { recursive: true, force: true }));
  git(main, 'worktree', 'add', '-q', wt);
  fs.mkdirSync(path.join(wt, '.code-graph'));
  const r = run(fs.realpathSync(wt));
  assert.equal(r.action, 'created', JSON.stringify(r));
  assert.ok(fs.existsSync(rulesAt(wt)));
  assert.match(excludeOf(main), /^\/\.claude\/rules\/code-graph\.md$/m);
  assert.equal(git(wt, 'status', '--porcelain', '--untracked-files=all'), '');
});

test('22: unadopt removes our file, the emptied dirs and the registry entry', (t) => {
  const dir = mkRepo(t);
  run(dir);
  const r = unadopt({ cwd: dir, home: ISOLATED_HOME });
  assert.equal(r.rulesRemoved, true);
  assert.equal(fs.existsSync(path.join(dir, '.claude')), false, '.claude/ we created is gone');
  assert.ok(!readAdoptedProjects(ISOLATED_HOME).includes(dir));
});

test('22b: unadopt keeps a .claude/ that holds anything else', (t) => {
  const dir = mkRepo(t);
  run(dir);
  fs.writeFileSync(path.join(dir, '.claude', 'settings.json'), '{}');
  assert.equal(unadopt({ cwd: dir, home: ISOLATED_HOME }).rulesRemoved, true);
  assert.ok(fs.existsSync(path.join(dir, '.claude', 'settings.json')));
  assert.equal(fs.existsSync(path.join(dir, '.claude', 'rules')), false);
});

test('23: unadopt leaves a user file at the path', (t) => {
  const dir = mkRepo(t);
  fs.mkdirSync(path.dirname(rulesAt(dir)), { recursive: true });
  fs.writeFileSync(rulesAt(dir), '# mine\n');
  const r = unadopt({ cwd: dir, home: ISOLATED_HOME });
  assert.equal(r.rulesRemoved, false);
  assert.equal(fs.readFileSync(rulesAt(dir), 'utf8'), '# mine\n');
});
