// Delivered summaries whose hunk the CURRENT pick rule also picks the same symbol for, classified by the reader.
const fs = require('fs'); const path = require('path');
const SI = require('/home/ai/dev/code-graph-mcp/claude-plugin/scripts/stop-impact.js');
const src = fs.readFileSync('/home/ai/dev/code-graph-mcp/claude-plugin/scripts/pre-edit-guide.js', 'utf8');
const a = src.indexOf('const fnPatterns = [');
const fnPatterns = eval('(' + src.slice(a + 'const fnPatterns ='.length, src.indexOf('];', a) + 1) + ')');
function headerAt(text, at) {
  const rest = text.slice(at, at + 400);
  const ends = [rest.indexOf('{'), rest.indexOf('=>'), rest.search(/:\s*(?:\n|$)/)].filter((i) => i >= 0);
  return ends.length ? rest.slice(0, Math.min(...ends)) : rest.split('\n', 1)[0];
}
function pick(oldStr, newStr) {
  const scanned = oldStr.length > 8192 ? oldStr.slice(0, 8192) : oldStr;
  for (const pat of fnPatterns) {
    const m = scanned.match(new RegExp(pat.source, `${pat.flags}d`));
    if (m) { const symbol = m[1] || m[2]; const nameAt = (m.indices[1] || m.indices[2])[0];
      const at = Math.max(m.index, scanned.lastIndexOf('\n', nameAt - 1) + 1);
      return { symbol, body: newStr.includes(headerAt(scanned, at)) }; }
  }
  return null;
}
const del = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const cls = {}; const seen = new Set(); const rows = [];
for (const line of fs.readFileSync(process.argv[3], 'utf8').split('\n')) {
  if (!line) continue; const d = JSON.parse(line);
  if (d.k !== 0 || !del[d.id] || seen.has(d.id)) continue; seen.add(d.id);
  const sym = del[d.id].symbol; const p = pick(d.old, d.new);
  if (!p || p.symbol !== sym || p.body) { cls.older_rule = (cls.older_rule || 0) + 1; continue; }
  const ext = path.extname(d.file_path).toLowerCase();
  const oS = SI.allSignatures(d.old, ext), nS = SI.allSignatures(d.new, ext);
  let c;
  if (oS === null || nS === null) c = 'reader_none';
  else if (!oS.has(sym)) c = 'not_a_definition';
  else if (SI.signatureChanged(oS.get(sym), nS.get(sym) || [])) c = 'header_changed';
  else c = 'same_key';
  cls[c] = (cls[c] || 0) + 1; rows.push([c, d.ts, d.proj.replace('-home-ai-dev-', ''), ext, sym, del[d.id].direct]);
}
console.log(cls); for (const r of rows) console.log(r.join('\t'));
