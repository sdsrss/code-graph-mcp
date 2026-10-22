import sys, statistics
sys.path.insert(0, "/var/tmp/d520/harness")
from corp import *
corpus, n, bins = sys.argv[1], int(sys.argv[2]), sys.argv[3:]
res = {b: [] for b in bins}
for i in range(n):
    for b in (bins if i % 2 == 0 else bins[::-1]):
        w = f"{R}/work/cpu/{os.path.basename(corpus)}/{os.path.basename(b)}"
        copy(corpus, w); t, cpu, _ = index(b, w); res[b].append(cpu)
for b in bins:
    v = sorted(res[b]); print(f"{os.path.basename(b)}: median CPU {statistics.median(v):.2f} s  min {v[0]:.2f} max {v[-1]:.2f}")
