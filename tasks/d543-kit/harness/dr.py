"""dr.py <bin> <relpath> [corpus]: index, delete <relpath>, index, restore it, index; diff against a fresh index."""
import sys, os, shutil
sys.path.insert(0, "/var/tmp/d543/harness")
from corp import *
binp, rel = sys.argv[1], sys.argv[2]
corpus = sys.argv[3] if len(sys.argv) > 3 else f"{R}/corpus/repo"
tag = os.path.basename(binp) + "-" + rel.replace("/", "_")
w = f"{R}/work/dr/{tag}"; inc = w + "/inc"; fr = w + "/fresh"
copy(corpus, inc); index(binp, inc)
p = os.path.join(inc, rel); keep = open(p, "rb").read()
os.remove(p); index(binp, inc)
a1 = dump(inc); copy(inc, fr); index(binp, fr); diff(a1, dump(fr), 10, "after delete")
open(p, "wb").write(keep); index(binp, inc)
copy(inc, fr); index(binp, fr)
oa, of = diff(dump(inc), dump(fr), 40, "after restore")
import collections
print("by relation only-fresh:", collections.Counter(k[8] for k in of))
print("by source file only-fresh:", collections.Counter(k[0] for k in of).most_common(10))
