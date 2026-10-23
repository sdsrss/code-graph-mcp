// Replays pre-edit-guide.js's symbol pick and header check on each dumped edit.
const fs = require('fs');
const src = fs.readFileSync('/home/ai/dev/code-graph-mcp/claude-plugin/scripts/pre-edit-guide.js', 'utf8');
const arr = src.slice(src.indexOf('const fnPatterns = ['), src.indexOf('];', src.indexOf('const fnPatterns = [')) + 2);
const fnPatterns = eval(arr.replace('const fnPatterns =', '(') .replace(/\];$/, '])'));
function headerAt(text, at) {
  const rest = text.slice(at, at + 400);
  const ends = [rest.indexOf('{'), rest.indexOf('=>'), rest.search(/:\s*(?:\n|$)/)].filter((i) => i >= 0);
  return ends.length ? rest.slice(0, Math.min(...ends)) : rest.split('\n', 1)[0];
}
for (const line of fs.readFileSync(process.argv[2], 'utf8').split('\n').filter(Boolean)) {
  const d = JSON.parse(line);
  const oldStr = d.old || '', newStr = d.new || '';
  const scanned = oldStr.length > 8192 ? oldStr.slice(0, 8192) : oldStr;
  let symbol = null, symbolAt = -1, patIdx = -1;
  for (const [k, pat] of fnPatterns.entries()) {
    const m = scanned.match(new RegExp(pat.source, `${pat.flags}d`));
    if (m) { symbol = m[1] || m[2]; const nameAt = (m.indices[1] || m.indices[2])[0];
      symbolAt = Math.max(m.index, scanned.lastIndexOf('\n', nameAt - 1) + 1); patIdx = k; break; }
  }
  const bodyOnly = symbol ? newStr.includes(headerAt(scanned, symbolAt)) : null;
  const wt = (d.file_path || '').includes('/.claude/worktrees/');
  console.log([d.hook ? 'hook' : 'MISS', wt ? 'WT' : 'main', d.name, 'picked=' + symbol, 'pat=' + patIdx, 'bodyOnly=' + bodyOnly].join('\t'));
}
