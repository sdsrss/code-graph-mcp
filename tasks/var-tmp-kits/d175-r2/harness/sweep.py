#!/usr/bin/env python3
"""Run every case of a case file on several arms in parallel; summarize.
usage: sweep.py CASEFILE OUTJSON [--arms a,b,c] [--jobs N] [--filter substr]"""
import sys, os, json, runpy, multiprocessing as mp
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import run as R

def work(item):
    casefile, name, arm = item
    cases = runpy.run_path(casefile)["CASES"]
    bad = R.run_case(name, cases[name], arm, verbose=False)
    out = []
    for i, extra, missing in bad:
        out.append({"step": i,
                    "extra": sorted(f"{n}x {R.fmt(r)}" for r, n in extra.items()),
                    "missing": sorted(f"{n}x {R.fmt(r)}" for r, n in missing.items())})
    # clean the work dir
    import shutil
    shutil.rmtree(os.path.join(R.WORK, name, arm), ignore_errors=True)
    return name, arm, out

def main():
    args = sys.argv[1:]
    arms = ["head", "a-rel", "base174"]
    jobs = 10
    filt = None
    if "--arms" in args:
        i = args.index("--arms"); arms = args[i + 1].split(","); del args[i:i + 2]
    if "--jobs" in args:
        i = args.index("--jobs"); jobs = int(args[i + 1]); del args[i:i + 2]
    if "--filter" in args:
        i = args.index("--filter"); filt = args[i + 1]; del args[i:i + 2]
    casefile, outjson = os.path.abspath(args[0]), args[1]
    names = list(runpy.run_path(casefile)["CASES"])
    if filt:
        names = [n for n in names if filt in n]
    items = [(casefile, n, a) for n in names for a in arms]
    res = {}
    with mp.Pool(jobs) as p:
        for name, arm, out in p.imap_unordered(work, items):
            res.setdefault(name, {})[arm] = out
    with open(outjson, "w") as f:
        json.dump(res, f, indent=1, sort_keys=True)
    # summary
    def key(out):
        return json.dumps(out, sort_keys=True)
    counts = {}
    for name in names:
        r = res[name]
        sig = "".join("X" if r[a] else "." for a in arms)
        counts[sig] = counts.get(sig, 0) + 1
        if r[arms[0]] or (len(arms) > 1 and key(r[arms[0]]) != key(r[arms[1]])):
            nd = {a: sum(len(s["extra"]) + len(s["missing"]) for s in r[a]) for a in arms}
            print(f"{sig} {name}: " + " ".join(f"{a}={nd[a]}" for a in arms))
    print("signature counts (" + ",".join(arms) + "):", counts)

if __name__ == "__main__":
    main()
