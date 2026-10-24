// The line each named hunk's pick sits on (the first-match rule of the given pre-edit-guide.js), old
// and new, for classifying a differential's rows by hand.
// usage: node hunk_lines.js <edits.jsonl> <pre-edit-guide.js> <name>@<id:k>...
const fs = require('fs'); const path = require('path');
const [corpus, guide, ...rows] = process.argv.slice(2);
const src = fs.readFileSync(guide, 'utf8');
const arr = src.slice(src.indexOf('const fnPatterns = ['), src.indexOf('];', src.indexOf('const fnPatterns = [')) + 2);
const fnPatterns = new Function(`${arr}; return fnPatterns;`)();
const want = new Map(rows.map((a) => { const i = a.indexOf('@'); return [a.slice(i + 1), a.slice(0, i)]; }));
const lineAt = (text, at) => text.slice(text.lastIndexOf('\n', at - 1) + 1, (text.indexOf('\n', at) + 1 || text.length + 1) - 1);
for (const l of fs.readFileSync(corpus, 'utf8').split('\n')) {
  if (!l) continue; const d = JSON.parse(l); const key = d.id + ':' + d.k;
  if (!want.has(key)) continue; const name = want.get(key); want.delete(key);
  const scanned = d.old.slice(0, 8192); let at = -1; let picked = null;
  for (const pat of fnPatterns) {
    const m = scanned.match(new RegExp(pat.source, `${pat.flags}d`));
    if (m) { picked = m[1] || m[2]; at = (m.indices[1] || m.indices[2])[0]; break; }
  }
  if (picked !== name) at = d.old.search(new RegExp(`\\b${name}\\b`));
  console.log(`== ${name} ${path.extname(d.file_path)} ${key}${picked === name ? '' : ` (pick ${picked}; first ${name})`}`);
  console.log(`  old: ${at < 0 ? '(absent)' : lineAt(d.old, at).trim().slice(0, 160)}`);
}
