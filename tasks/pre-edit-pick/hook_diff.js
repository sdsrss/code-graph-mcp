// The real pre-edit-guide.js of two trees, run on every Edit hunk of the corpus with a
// stub binary that answers `impact` for any name: which name does each one ask about?
// usage: node hook_diff.js <edits.jsonl> <scriptsDirA> <scriptsDirB> <workDir>
const fs = require('fs'); const path = require('path'); const { spawnSync } = require('child_process');
const [corpus, dirA, dirB, work] = process.argv.slice(2);
const proj = path.join(work, 'proj'); const tmp = path.join(work, 'tmp'); const home = path.join(work, 'home');
for (const d of [path.join(proj, '.code-graph'), path.join(proj, 'src'), tmp, home]) fs.mkdirSync(d, { recursive: true });
fs.writeFileSync(path.join(proj, '.code-graph', 'index.db'), '');
const preload = (dir) => {
  const f = path.join(work, `preload-${path.basename(path.dirname(dir))}.js`);
  fs.writeFileSync(f, `'use strict';
const cp = require('child_process');
cp.execFileSync = (bin, args) => JSON.stringify({ direct_callers: 1, total_callers: 1, affected_files: 1,
  risk: 'low', callers: [{ name: 'c', file: 'src/use.x', depth: 1 }], test_callers: [] });
require(${JSON.stringify(path.join(dir, 'find-binary.js'))}).findBinary = () => '/nonexistent/bin';
`);
  return f;
};
const pre = { A: preload(dirA), B: preload(dirB) };
function run(arm, dir, d) {
  for (const f of fs.readdirSync(path.join(tmp, 'code-graph-mcp'), { withFileTypes: true }).filter(() => true) || []) {
    if (f.name.startsWith('.cg-impact-')) fs.rmSync(path.join(tmp, 'code-graph-mcp', f.name), { force: true });
  }
  const file = path.join(proj, 'src', path.basename(d.file_path || 'x.txt'));
  const r = spawnSync(process.execPath, ['--require', pre[arm], path.join(dir, 'pre-edit-guide.js')], {
    cwd: proj, encoding: 'utf8',
    input: JSON.stringify({ tool_name: 'Edit', tool_input: { file_path: file, old_string: d.old, new_string: d.new } }),
    env: { ...process.env, HOME: home, USERPROFILE: home, TMPDIR: tmp, TMP: tmp, TEMP: tmp,
      CLAUDE_CONFIG_DIR: path.join(home, '.claude'), CODE_GRAPH_QUIET_HOOKS: '' },
  });
  const m = /\[code-graph:impact\] (\S+?)\(\)/.exec(r.stdout || '');
  return { name: m ? m[1] : null, status: r.status, err: (r.stderr || '').slice(0, 200), ms: 0 };
}
fs.mkdirSync(path.join(tmp, 'code-graph-mcp'), { recursive: true });
const seen = new Set(); const cls = {}; const rows = [];
for (const line of fs.readFileSync(corpus, 'utf8').split('\n')) {
  if (!line) continue; const d = JSON.parse(line); const key = d.id + ':' + d.k;
  if (seen.has(key)) continue; seen.add(key);
  const a = run('A', dirA, d), b = run('B', dirB, d);
  if (a.status !== 0 || b.status !== 0) { cls.nonzero = (cls.nonzero || 0) + 1; rows.push(['NONZERO', a.status, b.status, a.err, b.err]); continue; }
  const k = a.name === b.name ? (a.name ? 'same_summary' : 'same_none') : !a.name ? 'added' : !b.name ? 'removed' : 'switched';
  cls[k] = (cls[k] || 0) + 1;
  if (k !== 'same_summary' && k !== 'same_none') rows.push([k, d.proj.replace('-home-ai-dev-', ''), path.extname(d.file_path), a.name, '->', b.name, key]);
}
console.log(JSON.stringify(cls));
for (const r of rows) console.log(r.join(' '));
