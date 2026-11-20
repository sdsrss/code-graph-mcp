import sys, collections, statistics
d = collections.defaultdict(list); edges = {}
for l in open(sys.argv[1]):
    p = l.split()
    if len(p) < 5 or p[3] == "": continue
    if not p[3].isdigit(): continue
    d[(p[0], p[1])].append(int(p[3])); edges[(p[0], p[1])] = p[-1]
names = []
for (n, a) in d:
    if n not in names: names.append(n)
for n in names:
    row = [f"{n:32}"]
    for a in ["v0.203.0", "c52b115c", "f06f5e96"]:
        v = d.get((n, a))
        row.append(f"{a[:8]} {statistics.mean(v)/1000:7.2f}s (n={len(v)}, {min(v)/1000:.2f}-{max(v)/1000:.2f}) {edges.get((n,a),'')}" if v else f"{a[:8]} -")
    print(" | ".join(row))
