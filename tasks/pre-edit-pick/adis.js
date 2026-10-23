const fs = require('fs'); const path = require('path');
const SI = require('/home/ai/dev/code-graph-mcp/claude-plugin/scripts/stop-impact.js');
const s = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const cls = {};
for (const d of s.Adis) {
  const ext = path.extname(d.file_path).toLowerCase();
  const oS = SI.allSignatures(d.old, ext), nS = SI.allSignatures(d.new, ext);
  let c;
  if (!oS.has(d.symbol)) c = 'pick_not_a_definition';
  else if (!nS.has(d.symbol)) c = 'BUG?';
  else if (SI.signatureChanged(oS.get(d.symbol), nS.get(d.symbol))) c = 'optional_params_appended';
  else c = 'same_signature_key';
  cls[c] = (cls[c] || 0) + 1;
  if (process.argv[3] === c) {
    const i = d.old.indexOf(d.symbol);
    console.log('-----', d.proj, ext, d.symbol, 'changed=', d.changed.join(','));
    console.log('OLD:', JSON.stringify(d.old.slice(Math.max(0, i - 80), i + 160)));
    const j = d.new.indexOf(d.symbol);
    console.log('NEW:', JSON.stringify(d.new.slice(Math.max(0, j - 80), j + 160)));
  }
}
console.log(cls);
