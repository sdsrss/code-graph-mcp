const fs = require('fs'); const path = require('path');
const SI = require('/home/ai/dev/code-graph-mcp/claude-plugin/scripts/stop-impact.js');
const want = new Set(process.argv.slice(3)); const seen = new Set();
for (const line of fs.readFileSync(process.argv[2], 'utf8').split('\n')) {
  if (!line) continue; const d = JSON.parse(line); const key = d.id + ':' + d.k;
  if (seen.has(key)) continue; seen.add(key);
  const ext = path.extname(d.file_path).toLowerCase();
  const oS = SI.allSignatures(d.old, ext), nS = SI.allSignatures(d.new, ext);
  if (!oS || !nS) continue;
  for (const n of want) if (oS.has(n) && SI.signatureChanged(oS.get(n), nS.get(n) || [])) {
    console.log('=====', n, path.basename(d.file_path)); console.log('  old:', oS.get(n).join(' | ')); console.log('  new:', (nS.get(n) || ['(gone)']).join(' | '));
  }
}
