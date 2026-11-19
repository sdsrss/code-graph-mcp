"""baseonly.py <base-bin> <head-bin> <corpus> <work> : list base-only python refs whose oracle verdict is in argv[5:] grouped by target name."""
import os, sys, shutil, sqlite3
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from abdiff import run, key
from reforacle import Corpus, classify
from collections import Counter
base, head, corp, work = map(os.path.abspath, sys.argv[1:5]); want = set(sys.argv[5:])
n = os.path.basename(corp)
rb, _, _, _ = run(base, corp, os.path.join(work, n + "-b"))
rh, _, _, _ = run(head, corp, os.path.join(work, n + "-h"))
left = Counter(key(r) for r in rb) - Counter(key(r) for r in rh)
files = [r[1] for r in rb if r[15] == "python"]
corpus = Corpus(os.path.join(work, n + "-b"), sorted(set(f for f in files if f.endswith(".py"))))
out = Counter()
for r in rb:
    k = key(r)
    if left[k] <= 0: continue
    left[k] -= 1
    rel, sp, st, sn, sq, s0, s1, tp, tt, tn, tq, t0, t1, conf, meta, lang = r
    if lang != "python": continue
    v, info = classify(corpus, sp, st, sn, s0, s1, tp, tn, tq, tt, t0, t1, rel)
    if v in want:
        out[(rel, v, tn, sp)] += 1
for (rel, v, tn, sp), c in sorted(out.items()):
    print(f"{c:4} {rel} {v} name={tn} src={sp}")
shutil.rmtree(os.path.join(work, n + "-b")); shutil.rmtree(os.path.join(work, n + "-h"))
