"""fullcmp.py <binA> <binB> <corpus>...: full index of each corpus by each binary, compare every edge (with labels/metadata)."""
import sys, os
sys.path.insert(0, "/var/tmp/d520/harness")
from corp import *
a, b, corpora = sys.argv[1], sys.argv[2], sys.argv[3:]
for c in corpora:
    rows = {}
    for x in (a, b):
        w = f"{R}/work/full/{os.path.basename(c)}/{os.path.basename(x)}"
        copy(c, w); t, cpu, _ = index(x, w); rows[x] = dump(w)
    diff(rows[a], rows[b], 20, f"{os.path.basename(c)} {os.path.basename(a)} vs {os.path.basename(b)}")
