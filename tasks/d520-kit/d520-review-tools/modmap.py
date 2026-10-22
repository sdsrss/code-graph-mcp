import sys, subprocess, collections
paths = set(subprocess.run(["git", "-C", sys.argv[1], "ls-files", "*.py"], capture_output=True, text=True).stdout.split())
pk = {p[:-len("/__init__.py")] for p in paths if p.endswith("/__init__.py")}
roots = {""}
for p in paths:
    parts = p.split("/")[:-1]
    chain = ["/".join(parts[:i]) for i in range(1, len(parts) + 1)]
    for d in chain:
        if d in pk: break
        roots.add(d)
m = collections.defaultdict(set)
for p in paths:
    s = p[:-len("/__init__.py")] if p.endswith("/__init__.py") else p[:-3]
    for r in roots:
        rel = s if r == "" else (s[len(r) + 1:] if s.startswith(r + "/") else None)
        if rel: m[rel.replace("/", ".")].add(p)
multi = {k: v for k, v in m.items() if len(v) > 1}
print(sys.argv[1], "keys", len(m), "multi-file keys", len(multi))
for k, v in sorted(multi.items())[:15]: print("  ", k, sorted(v))
