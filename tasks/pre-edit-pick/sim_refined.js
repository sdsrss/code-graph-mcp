// Current vs refined pick over every hunk (spec tasks/specs/pre-edit-pick-by-reader.md, rev 2):
// reader verdict null -> current rule; a changed definition named like the current pick -> it;
// current rule proceeds and the pick is not provably a non-definition -> current pick;
// another changed definition -> the first; else no summary. Provably not a definition: the
// pick's line is a comment, or the language keys every definition with a keyword the reader reads.
const fs = require('fs'); const path = require('path');
const SI = require('/home/ai/dev/code-graph-mcp/claude-plugin/scripts/stop-impact.js');
const src = fs.readFileSync(process.argv[3] || '/home/ai/dev/code-graph-mcp/claude-plugin/scripts/pre-edit-guide.js', 'utf8');
const a = src.indexOf('const fnPatterns = [');
const fnPatterns = eval('(' + src.slice(a + 'const fnPatterns ='.length, src.indexOf('];', a) + 1) + ')');
const COMMENT_LINE = /^\s*(?:\/\/|#|\*|\/\*|--|;)/;
const KEYWORD_ONLY = new Set(['.rs', '.py', '.pyi', '.go', '.php', '.scala', '.sc', '.rb', '.lua']);
function headerAt(text, at) {
  const rest = text.slice(at, at + 400);
  const ends = [rest.indexOf('{'), rest.indexOf('=>'), rest.search(/:\s*(?:\n|$)/)].filter((i) => i >= 0);
  return ends.length ? rest.slice(0, Math.min(...ends)) : rest.split('\n', 1)[0];
}
function cur(oldStr, newStr) {
  if (!oldStr || oldStr.length < 10) return null;
  const scanned = oldStr.length > 8192 ? oldStr.slice(0, 8192) : oldStr;
  let symbol = null, at = -1;
  for (const pat of fnPatterns) {
    const m = scanned.match(new RegExp(pat.source, `${pat.flags}d`));
    if (m) { symbol = m[1] || m[2]; const nameAt = (m.indices[1] || m.indices[2])[0];
      at = Math.max(m.index, scanned.lastIndexOf('\n', nameAt - 1) + 1); break; }
  }
  if (!symbol || symbol.length < 3 || /^(if|for|while|switch|catch|else|return|new|get|set|try)$/i.test(symbol)) return null;
  const lineStart = scanned.lastIndexOf('\n', at - 1) + 1;
  const lineEnd = scanned.indexOf('\n', at);
  return { symbol, at, line: scanned.slice(lineStart, lineEnd === -1 ? undefined : lineEnd), proceed: !newStr.includes(headerAt(scanned, at)) };
}
const cls = {}; const inc = (k) => { cls[k] = (cls[k] || 0) + 1; }; const ex = {}; const seen = new Set();
for (const line of fs.readFileSync(process.argv[2], 'utf8').split('\n')) {
  if (!line) continue; const d = JSON.parse(line); const key = d.id + ':' + d.k;
  if (seen.has(key)) continue; seen.add(key);
  const ext = path.extname(d.file_path).toLowerCase();
  const c = cur(d.old, d.new);
  const oldDec = c && c.proceed ? c.symbol : null;
  let newDec = oldDec;
  if (c && SI.allSignatures('', ext) !== null) {
    const oS = SI.allSignatures(d.old, ext), nS = SI.allSignatures(d.new, ext);
    if (oS !== null && nS !== null) {
      const changed = [...oS].filter(([n, s]) => SI.signatureChanged(s, nS.get(n) || [])).map(([n]) => n);
      const provablyNot = COMMENT_LINE.test(c.line) || KEYWORD_ONLY.has(ext);
      if (changed.includes(c.symbol)) newDec = c.symbol;
      else if (c.proceed && !oS.has(c.symbol) && !provablyNot) newDec = c.symbol;
      else if (c.proceed && oS.has(c.symbol) && !provablyNot) newDec = c.symbol;
      else newDec = changed[0] || null;
    }
  }
  const k = oldDec === newDec ? (oldDec ? 'same_summary' : 'same_none') : !oldDec ? 'added' : !newDec ? 'removed' : 'switched';
  inc(k); (ex[k] = ex[k] || []).push([d.proj.replace('-home-ai-dev-', ''), ext, oldDec, '->', newDec].join(' '));
}
console.log(cls);
for (const k of ['added', 'removed', 'switched']) { console.log('##', k); for (const r of ex[k] || []) console.log('  ' + r); }
