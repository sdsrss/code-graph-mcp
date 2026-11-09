"""Pick headroom-pilot targets from SCIP gold (criteria fixed before any session)."""
import ast, json, re, subprocess, sys
from collections import Counter, defaultdict
from pathlib import Path

root, gold_path = Path(sys.argv[1]), sys.argv[2]
gold = json.load(open(gold_path))
defs = Counter()
for p in root.rglob("*.py"):
    try:
        tree = ast.parse(p.read_text(errors="replace"))
    except SyntaxError:
        continue
    for n in ast.walk(tree):
        if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef)):
            defs[n.name] += 1
sites = defaultdict(set)
shapes = defaultdict(Counter)
for g in gold:
    key = (g["callee"], g["callee_at"])
    sites[key].add(g["site"])
    shapes[key][g["shape"]] += 1
rows = []
for (name, at), s in sites.items():
    if name.startswith("__") or not (6 <= len(s) <= 15) or defs[name] < 3:
        continue
    if shapes[(name, at)].most_common(1)[0][0] == "bare":
        continue
    hits = subprocess.run(["git", "-C", str(root), "grep", "-cE", rf"\.{re.escape(name)}\(", "--", "*.py"],
                          capture_output=True, text=True).stdout
    h = sum(int(l.rsplit(":", 1)[1]) for l in hits.splitlines())
    if h >= 4 * len(s) and h <= 80:
        rows.append((h / len(s), name, at, len(s), h, defs[name], dict(shapes[(name, at)])))
for r in sorted(rows, reverse=True):
    print(f"{r[1]:<22} {r[2]:<40} gold={r[3]:<3} grep={r[4]:<3} defs={r[5]:<3} {r[6]}")
