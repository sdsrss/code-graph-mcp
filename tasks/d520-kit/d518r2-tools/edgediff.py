import sys, collections
def load(p):
    c = collections.Counter()
    for l in open(p):
        f = l.rstrip("\n").split("\t")
        if len(f) < 13: continue
        if f[10] == "imports" and '"attr":' in f[11]: continue  # synthetic
        c[tuple(f)] += 1
    return c
a, b = load(sys.argv[1]), load(sys.argv[2])
lost, gained = a - b, b - a
print("lost", sum(lost.values()), "gained", sum(gained.values()))
for name, d in (("lost", lost), ("gained", gained)):
    print(name, "by (relation,confidence):", collections.Counter((k[10], k[12]) for k in d.elements()).most_common())
    print(name, "by source file:", collections.Counter(k[0] for k in d.elements()).most_common(5))
