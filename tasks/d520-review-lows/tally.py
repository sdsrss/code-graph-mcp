import re, glob, sys, os, runpy
def steps(dirr, arm, pattern="rv-*.log"):
    out = set()
    for f in sorted(glob.glob(f"{dirr}/{pattern}")):
        cur = None
        for line in open(f):
            m = re.match(r"^== (\S+):", line)
            if m: cur = (os.path.basename(f), m.group(1)); continue
            m = re.match(rf"^   \[{arm}\] step(\d+) (\+inc=|FAIL)", line)
            if m: out.add((cur, int(m.group(1))))
    return out
v175 = steps("/var/tmp/d518r2/logs", "v175")
pre = steps("/var/tmp/d521/logs", "pre176")
low = steps("/var/tmp/d521/logs", "low7")
total = 0
for s in ["twofile","attr","attr2","roots","big","big2","d370","fk","review175"]:
    sc = runpy.run_path(f"/var/tmp/d520-review/scn/{s}.py")["SCENARIOS"]
    total += sum(len(v) - 1 for v in sc.values())
print("steps", total, "v175", len(v175), "pre176", len(pre), "low7", len(low))
print("low7 only (vs v175):", sorted(low - v175))
print("pre176 only (vs v175):", sorted(pre - v175))
print("low7 not pre176:", sorted(low - pre))
print("pre176 not low7:", sorted(pre - low))
