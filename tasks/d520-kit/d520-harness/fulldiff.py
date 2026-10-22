"""fulldiff.py <binA> <binB> <corpus>...: full index each corpus with both; edge differences with the
"attr" key stripped from B's call metadata (so only real target/label moves show); synthetic imports counted."""
import sys, os, collections, json
sys.path.insert(0, "/var/tmp/d520/harness")
from corp import *
a, b, corpora = sys.argv[1], sys.argv[2], sys.argv[3:]
SHOWN = int(os.environ.get("SHOWN", "0"))
def strip(k):
    meta = k[9]
    if '"attr"' in meta:
        m = json.loads(meta); m.pop("attr", None)
        meta = json.dumps(m, separators=(",", ":"), sort_keys=True) if m else ""
    return k[:9] + (meta,) + k[10:]
for c in corpora:
    rows = {}
    for x in (a, b):
        w = f"{R}/work/full/{os.path.basename(c)}/{os.path.basename(x)}"
        if not os.path.exists(w + "/.code-graph/index.db"):
            copy(c, w); index(x, w)
        rows[x] = dump(w)
    ra = rows[a]
    synth = sum(v for k, v in rows[b].items() if k[8] == "imports" and '"attr"' in k[9])
    rb = collections.Counter()
    for k, v in rows[b].items():
        if k[8] == "imports" and '"attr"' in k[9]: continue
        rb[strip(k)] += v
    oa, ob = ra - rb, rb - ra
    kind = lambda k: (k[8], k[10])
    ca = collections.Counter(kind(k) for k in oa.elements()); cb = collections.Counter(kind(k) for k in ob.elements())
    print(f"== {os.path.basename(c)}: edges {sum(ra.values())} -> {sum(rows[b].values())} (synthetic imports {synth}); removed {sum(oa.values())} added {sum(ob.values())}")
    for k in sorted(set(ca) | set(cb)): print(f"   {k}: -{ca.get(k,0)} +{cb.get(k,0)}")
    for k in sorted(oa)[:SHOWN]: print("     -", fmt(k))
    for k in sorted(ob)[:SHOWN]: print("     +", fmt(k))
