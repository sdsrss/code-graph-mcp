// Classifies every Edit hunk by pre-edit-guide's current pick/header rule vs stop-impact's per-language reader.
const fs = require('fs');
const path = require('path');
const SI = require('/home/ai/dev/code-graph-mcp/claude-plugin/scripts/stop-impact.js');
const src = fs.readFileSync('/home/ai/dev/code-graph-mcp/claude-plugin/scripts/pre-edit-guide.js', 'utf8');
const a = src.indexOf('const fnPatterns = [');
const fnPatterns = eval('(' + src.slice(a + 'const fnPatterns ='.length, src.indexOf('];', a) + 1) + ')');
function headerAt(text, at) {
  const rest = text.slice(at, at + 400);
  const ends = [rest.indexOf('{'), rest.indexOf('=>'), rest.search(/:\s*(?:\n|$)/)].filter((i) => i >= 0);
  return ends.length ? rest.slice(0, Math.min(...ends)) : rest.split('\n', 1)[0];
}
function oldRule(oldStr, newStr) {
  if (!oldStr || oldStr.length < 10) return { exit: 'short' };
  const scanned = oldStr.length > 8192 ? oldStr.slice(0, 8192) : oldStr;
  let symbol = null, symbolAt = -1;
  for (const pat of fnPatterns) {
    const m = scanned.match(new RegExp(pat.source, `${pat.flags}d`));
    if (m) { symbol = m[1] || m[2]; const nameAt = (m.indices[1] || m.indices[2])[0];
      symbolAt = Math.max(m.index, scanned.lastIndexOf('\n', nameAt - 1) + 1); break; }
  }
  if (!symbol || symbol.length < 3) return { exit: 'nosym' };
  if (/^(if|for|while|switch|catch|else|return|new|get|set|try)$/i.test(symbol)) return { exit: 'kw', symbol };
  if (newStr.includes(headerAt(scanned, symbolAt))) return { exit: 'body', symbol };
  return { exit: null, symbol };
}
const counts = {};
const inc = (k) => { counts[k] = (counts[k] || 0) + 1; };
const samples = { B1: [], B2: [], Adis: [] };
const seen = new Set();
for (const line of fs.readFileSync(process.argv[2], 'utf8').split('\n')) {
  if (!line) continue;
  const d = JSON.parse(line);
  const key = d.id + ':' + d.k;
  if (seen.has(key)) continue; seen.add(key);
  const ext = path.extname(d.file_path).toLowerCase();
  const r = oldRule(d.old, d.new);
  inc('all');
  const known = SI.allSignatures('', ext) !== null;
  if (!known) { inc('ext_unread'); continue; }
  inc('ext_read');
  const oS = SI.allSignatures(d.old, ext), nS = SI.allSignatures(d.new, ext);
  const changed = [];
  if (oS && nS) for (const [name, sigs] of oS) if (SI.signatureChanged(sigs, nS.get(name) || [], name)) changed.push(name);
  inc('old_exit_' + (r.exit || 'proceed'));
  if (r.exit === 'body') {
    if (changed.includes(r.symbol)) { inc('B1_same_symbol_changed'); samples.B1.push({ ...d, symbol: r.symbol }); }
    else if (changed.length) { inc('B2_other_changed'); samples.B2.push({ ...d, symbol: r.symbol, alt: changed[0] }); }
    else inc('B3_none');
  } else if (r.exit === null) {
    if (changed.includes(r.symbol)) inc('A_reader_agrees');
    else if (oS === null || nS === null) inc('A_reader_null');
    else { inc('A_reader_disagrees'); samples.Adis.push({ ...d, symbol: r.symbol, changed }); }
  }
}
console.log(counts);
fs.writeFileSync(process.argv[3], JSON.stringify(samples));
