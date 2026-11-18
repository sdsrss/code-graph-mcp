import json, sys
f, seed = sys.argv[1], int(sys.argv[2])
r = next(r for r in map(json.loads, open(f)) if r['seed'] == seed)
sts = r['states']
for i, st in enumerate(sts):
    prev = sts[i-1] if i else {}
    print(f"######## state {i}" + (" (full)" if i == 0 else " (changes)"))
    for p in sorted(set(st) | set(prev)):
        if i and st.get(p) == prev.get(p): continue
        if p not in st: print(f"--- {p}: DELETED"); continue
        print(f"--- {p}:\n{st[p]}")
