#!/usr/bin/env python3
"""Dump nodes and edges of a code-graph index as sorted multisets.
usage: dump.py DB OUTPREFIX [--nolines]
writes OUTPREFIX.nodes / OUTPREFIX.edges (one TSV row per item, sorted)."""
import sqlite3, sys
def esc(x):
    return str(x).replace("\\","\\\\").replace("\n","\\n").replace("\t","\\t")
db, out = sys.argv[1], sys.argv[2]
nolines = '--nolines' in sys.argv
c = sqlite3.connect(f'file:{db}?mode=ro', uri=True)
def key(path, typ, name, q, sl):
    k = esc(f"{path}|{typ}|{q or name}")
    return k if nolines else f"{k}@{sl}"
nodes = {}
rows = []
for nid, path, typ, name, q, sl, el, sig, test in c.execute(
    "SELECT n.id, f.path, n.type, n.name, n.qualified_name, n.start_line, n.end_line, n.signature, n.is_test FROM nodes n JOIN files f ON f.id=n.file_id"):
    nodes[nid] = key(path, typ, name, q, sl)
    rows.append(f"{nodes[nid]}\t{esc(name)}\t{el if not nolines else ''}\t{esc(sig)}\t{test}")
rows.sort(key=lambda r: r.encode())
open(out + '.nodes', 'w').write('\n'.join(rows) + '\n')
erows = []
for s, t, rel, meta, conf in c.execute("SELECT source_id, target_id, relation, metadata, confidence FROM edges"):
    erows.append(f"{nodes.get(s,'?'+str(s))}\t{rel}\t{nodes.get(t,'?'+str(t))}\t{conf}\t{esc(meta or '')}")
erows.sort(key=lambda r: r.encode())
open(out + '.edges', 'w').write('\n'.join(erows) + '\n')
pend = []
try:
    for s, tn, lang, meta in c.execute("SELECT source_id, target_name, source_language, metadata FROM pending_unresolved_calls"):
        pend.append(f"{nodes.get(s,'?')}\t{tn}\t{lang}\t{meta or ''}")
except Exception:
    pass
pend.sort(key=lambda r: r.encode())
open(out + '.pending', 'w').write('\n'.join(pend) + '\n')
iv = c.execute("SELECT value FROM meta WHERE key LIKE '%index_version%'").fetchall()
print(f"{db}: {len(rows)} nodes, {len(erows)} edges, {len(pend)} pending, index_version={iv}")
