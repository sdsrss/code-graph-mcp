#!/usr/bin/env python3
"""Decline-only check: is each run of <new> byte-equal to <a> or to <b>?

usage: decline.py <new> <a> <b>   (e.g. head4 head3 head2)
"""
import collections, json, os, sys
ROOT = "/var/tmp/d435/results"
new, a, b = sys.argv[1:4]
neither = []
for corpus in ("flask", "leveldb", "md", "fx2"):
    inp = {json.loads(l)["id"]: json.loads(l) for l in open(f"{ROOT}/inputs-{corpus}.jsonl")}
    c = collections.Counter(); bycls = collections.defaultdict(collections.Counter)
    for sf in sorted(os.listdir(f"{ROOT}/{new}/{corpus}")):
        def rows(arm):
            return {json.loads(l)["id"]: l for l in open(f"{ROOT}/{arm}/{corpus}/{sf}", encoding="utf-8")}
        N, A, B = rows(new), rows(a), rows(b)
        for rid, line in N.items():
            if line == A.get(rid):
                k = f"= {a}"
            elif line == B.get(rid):
                k = f"= {b} only"
            else:
                k = "neither"
                neither.append((corpus, sf, rid, inp[rid]["class"], inp[rid]["sym"], inp[rid].get("file"), A.get(rid), B.get(rid), line))
            c[k] += 1
            bycls[inp[rid]["class"]][k] += 1
    print(f"{corpus}: runs {sum(c.values())} " + ", ".join(f"{k}: {v}" for k, v in sorted(c.items())))
    for cls, cc in sorted(bycls.items()):
        if cc.get(f"= {b} only") or cc.get("neither"):
            print(f"    {cls}: " + ", ".join(f"{k}: {v}" for k, v in sorted(cc.items())))
print(f"\nNEITHER: {len(neither)}")
for corpus, sf, rid, cls, sym, fp, la, lb, ln in neither:
    print(f"## {corpus} {sf} {rid} class={cls} sym={sym!r} file={fp}")
    for tag, l in ((a, la), (b, lb), (new, ln)):
        print(f"   {tag:6} {(l or '<missing>').strip()[:700]}")
