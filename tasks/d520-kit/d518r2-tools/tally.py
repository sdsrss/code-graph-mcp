import re, runpy, sys, collections
arms = ["v175", "base", "head"]
tot = 0; diff = {a: set() for a in arms}; fails = []
for s in "twofile attr attr2 roots big big2 d370 fk review175".split():
    scen = runpy.run_path(f"/var/tmp/d520-review/scn/{s}.py")["SCENARIOS"]
    tot += sum(len(v) - 1 for v in scen.values())
    seen = set()
    for line in open(f"logs/rv-{s}.log"):
        m = re.match(r"== (\S+): (.*)", line)
        if not m: 
            if "FAIL" in line: fails.append((s, line.strip()))
            continue
        name = m.group(1); seen.add(name)
        for part in m.group(2).split():
            a, st = part.split(":", 1)
            if st.startswith("DIFF@"):
                for i in st[5:].split(","):
                    diff[a].add((s, name, i))
    missing = set(scen) - seen
    if missing: print("MISSING", s, len(missing))
print("total steps", tot, "fails", fails[:5])
for a in arms: print(a, len(diff[a]))
print("head-only vs v175:", sorted(diff["head"] - diff["v175"]))
print("head-not-base:", sorted(diff["head"] - diff["base"]))
print("base-not-head:", len(diff["base"] - diff["head"]))
print("v175-not-head:", len(diff["v175"] - diff["head"]))
