// Reader output, v0.179.0 vs HEAD, on every file of the given extensions.
// usage: node reader_diff.js <baseScriptsDir> <headScriptsDir> <exts,comma> <root>...
'use strict';
const fs = require('fs');
const path = require('path');
const [baseDir, headDir, extArg, ...roots] = process.argv.slice(2);
const base = require(path.join(baseDir, 'stop-impact.js'));
const head = require(path.join(headDir, 'stop-impact.js'));
const exts = new Set(extArg.split(','));

function* walk(dir) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    if (e.name === '.git') continue;
    const p = path.join(dir, e.name);
    if (e.isDirectory()) yield* walk(p);
    else if (e.isFile() && exts.has(path.extname(e.name).toLowerCase())) yield p;
  }
}

function read(m, text, ext) {
  const t0 = process.hrtime.bigint();
  const headers = m.definitionHeaders(text, ext);
  const sigs = m.allSignatures(text, ext);
  const ms = Number(process.hrtime.bigint() - t0) / 1e6;
  return { headers, sigs: sigs && [...sigs], ms };
}

const s = { files: 0, bytes: 0, diffFiles: 0, headers: 0, names: 0, perName: 0, perNameDiff: 0,
  receiverOrTypeParam: 0, maxMsBase: 0, maxMsHead: 0, examples: [] };
for (const root of roots) {
  for (const f of walk(root)) {
    const ext = path.extname(f).toLowerCase();
    const text = fs.readFileSync(f, 'utf8');
    s.files++; s.bytes += text.length;
    const a = read(base, text, ext);
    const b = read(head, text, ext);
    s.maxMsBase = Math.max(s.maxMsBase, a.ms);
    s.maxMsHead = Math.max(s.maxMsHead, b.ms);
    const ja = JSON.stringify([a.headers, a.sigs]);
    const jb = JSON.stringify([b.headers, b.sigs]);
    if (ja !== jb) { s.diffFiles++; if (s.examples.length < 5) s.examples.push(f); }
    const defs = (b.headers && b.headers.defs) || [];
    s.headers += defs.length;
    s.receiverOrTypeParam += defs.filter((d) => /\b(?:func\s*\(|fun\s*<)/.test(d.key)).length;
    const names = new Set([...(a.sigs || []).map((e) => e[0]), ...(b.sigs || []).map((e) => e[0])]);
    s.names += names.size;
    for (const n of names) {
      s.perName++;
      if (JSON.stringify(base.extractSignatures(text, n, ext)) !== JSON.stringify(head.extractSignatures(text, n, ext))) {
        s.perNameDiff++;
        if (s.examples.length < 5) s.examples.push(`${f} :: ${n}`);
      }
    }
  }
}
console.log(JSON.stringify(s, null, 1));
