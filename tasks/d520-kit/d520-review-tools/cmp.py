#!/usr/bin/env python3
"""cmp.py <base.edges> <head.edges> [-v]: compare edge dumps after dropping synthetic
imports (imports + "attr") and the "attr" key from calls metadata. Prints counts by
(relation, confidence) of base-only and head-only, and by source-file language."""
import sys, json
from collections import Counter
def load(p):
    out = Counter(); syn = 0
    for line in open(p):
        line = line.rstrip("\n")
        if not line: continue
        f = line.split("\t")
        sf, sn, sl, tf, tn, tl, rel, meta, conf = f
        if rel == "imports" and '"attr":' in meta:
            syn += 1; continue
        if meta.startswith("{") and '"attr"' in meta:
            m = json.loads(meta); m.pop("attr", None); meta = json.dumps(m, separators=(",", ":"), sort_keys=True)
        elif meta.startswith("{"):
            meta = json.dumps(json.loads(meta), separators=(",", ":"), sort_keys=True)
        out[(sf, sn, sl, tf, tn, tl, rel, meta, conf)] += 1
    return out, syn
a, sa = load(sys.argv[1]); b, sb = load(sys.argv[2])
ao = a - b; bo = b - a
print(f"base={sum(a.values())} (+{sa} synthetic) head={sum(b.values())} (+{sb} synthetic) base-only={sum(ao.values())} head-only={sum(bo.values())}")
def ext(p): return p.rsplit(".", 1)[-1] if "." in p.rsplit("/", 1)[-1] else "-"
for lab, c in (("base-only", ao), ("head-only", bo)):
    k = Counter()
    for r, n in c.items(): k[(ext(r[0]), r[6], r[8])] += n
    print(lab, dict(sorted(k.items())))
if "-v" in sys.argv:
    for r in sorted(ao.elements()): print("-", "\t".join(r))
    for r in sorted(bo.elements()): print("+", "\t".join(r))
