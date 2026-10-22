"""ediff.py <labA> <binA> <labB> <binB> <corpus-src>...: fresh index each corpus with both
binaries, compare edges (without confidence) and label moves; dump differences to
/var/tmp/d175-review/ediff/<corpus>.<labA>-<labB>.txt"""
import sys, os, collections, json
sys.path.insert(0, "/var/tmp/d175-review/harness")
from corp import *
la, ba, lb, bb = sys.argv[1:5]
os.makedirs(f"{R}/ediff", exist_ok=True)
for src in sys.argv[5:]:
    c = os.path.basename(src.rstrip("/")) if not src.rstrip("/").endswith("/A") else os.path.basename(os.path.dirname(src.rstrip("/")))
    a, b = f"{R}/work/ed/{la}/{c}", f"{R}/work/ed/{lb}/{c}"
    copy(src, a); copy(src, b)
    ta = index(ba, a); tb = index(bb, b)
    ra, rb = dump(a), dump(b)
    ka = collections.Counter(); kb = collections.Counter()
    for k, n in ra.items(): ka[k[:-1]] += n
    for k, n in rb.items(): kb[k[:-1]] += n
    ca = collections.defaultdict(list); cb = collections.defaultdict(list)
    for k, n in ra.items(): ca[k[:-1]] += [k[-1]] * n
    for k, n in rb.items(): cb[k[:-1]] += [k[-1]] * n
    moves = collections.Counter(); movelist = []
    for k in ca:
        if k in cb and sorted(ca[k]) != sorted(cb[k]):
            for x, y in zip(sorted(ca[k]), sorted(cb[k])):
                if x != y: moves[(x, y, k[7], k[8])] += 1; movelist.append((x, y, k))
    oa, ob = ka - kb, kb - ka
    print(f"{c}: edges {la}={sum(ra.values())} {lb}={sum(rb.values())} set-equal={not oa and not ob} only-{la}={sum(oa.values())} only-{lb}={sum(ob.values())} cpu {la}={ta[1]:.2f} {lb}={tb[1]:.2f}")
    for k, n in sorted(moves.items(), key=lambda x: -x[1]): print(f"   label {k[0]}->{k[1]} target={k[2]} rel={k[3]}: {n}")
    with open(f"{R}/ediff/{c}.{la}-{lb}.txt", "w") as f:
        for k in sorted(oa): f.write(f"-{la} x{oa[k]} " + json.dumps(k) + "\n")
        for k in sorted(ob): f.write(f"+{lb} x{ob[k]} " + json.dumps(k) + "\n")
        for x, y, k in movelist: f.write(f"label {x}->{y} " + json.dumps(k) + "\n")
