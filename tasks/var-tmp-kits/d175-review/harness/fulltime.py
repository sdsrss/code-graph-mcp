import sys, os, statistics
sys.path.insert(0, "/var/tmp/d175-review/harness")
from corp import *
corpus, n = sys.argv[1], int(sys.argv[2])
arms = sys.argv[3].split(",") if len(sys.argv) > 3 else ["head", "base"]
BIN = {"head": f"{R}/bin/cgm-head", "base": f"{R}/bin/cgm-base", "b41": f"{R}/bin/cgm-b41"}
res = {a: [] for a in arms}
for i in range(n):
    for a in (arms if i % 2 == 0 else arms[::-1]):
        d = f"{R}/work/full/{a}"; copy(corpus, d)
        wall, cpu, _ = index(BIN[a], d); res[a].append((wall, cpu))
for a in arms:
    w = [x[0] for x in res[a]]; c = [x[1] for x in res[a]]
    print(f"{os.path.basename(corpus.rstrip('/'))} [{a}] n={n} cpu s min/med/max {min(c):.2f}/{statistics.median(c):.2f}/{max(c):.2f} wall s med {statistics.median(w):.2f}")
