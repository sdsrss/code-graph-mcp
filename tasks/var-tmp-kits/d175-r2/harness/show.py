#!/usr/bin/env python3
"""Print a fresh index's edges (filtered to non-<module>-contains noise) for a case's init tree.
usage: show.py CASEFILE case [arm] [--all]"""
import sys, os, runpy
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import run as R

args = sys.argv[1:]
allrel = "--all" in args
args = [a for a in args if a != "--all"]
cases = runpy.run_path(args[0])["CASES"]
case = cases[args[1]]
arm = args[2] if len(args) > 2 else "head"
d = os.path.join(R.WORK, "show", args[1], arm)
R.fresh_dir(d)
R.write_tree(d, case["init"])
R.index(arm, d)
for r, n in sorted(R.dump(d).items()):
    if not allrel and r[4] in ("contains",):
        continue
    print(f"{n}x {R.fmt(r)}")
