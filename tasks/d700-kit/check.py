"""check.py CASES BASE_REPO HEAD_REPO: judge D#700(2b) against CPython.

CASES is gen.py's output; BASE_REPO / HEAD_REPO are copies of it indexed by the
two binaries. For each case: CPython's answer (which `go` the one call ran),
and whether each arm binds `main` to the file's own `go`. A call HEAD stops
binding into its file while CPython ran the file's `go` is WRONG (a correct
edge lost); any HEAD edge from `main` that BASE lacks is ADDED (the rule must
only remove). Summaries group the cases by the kinds of extra occurrences.
"""
import collections, json, os, sqlite3, subprocess, sys

cases_dir, base_dir, head_dir = sys.argv[1:4]
meta = json.load(open(os.path.join(cases_dir, "cases.json")))

def truth(name):
    # A case inside a package runs as that module (`-m pkg2.c00001`), so its
    # imports of the package resolve as they would in the project.
    args = ["-m", name[:-3].replace("/", ".")] if "/" in name else [name]
    p = subprocess.run([sys.executable, "-E", "-s", *args], cwd=cases_dir, capture_output=True, text=True, timeout=20)
    out = [l for l in p.stdout.split("\n") if l]
    return out[-1].split()[0] if out else "NONE"

def edges(repo):
    db = sqlite3.connect(f"file:{repo}/.code-graph/index.db?mode=ro", uri=True)
    out = collections.defaultdict(set)
    for sp, tp, tn, meta_ in db.execute("""SELECT sf.path, tf.path, t.name, coalesce(e.metadata, '')
        FROM edges e JOIN nodes s ON s.id = e.source_id JOIN files sf ON sf.id = s.file_id
        JOIN nodes t ON t.id = e.target_id JOIN files tf ON tf.id = t.file_id
        WHERE e.relation = 'calls' AND s.name = 'main'"""):
        # `"loc"` is HEAD's new mark on an unchanged call: compare without it.
        try:
            m = json.loads(meta_) if meta_ else None
        except ValueError:
            m = None
        if isinstance(m, dict):
            m.pop("loc", None)
            meta_ = json.dumps(m, sort_keys=True, separators=(",", ":"))
        out[sp].add((tp, tn, meta_))
    return out

base, head = edges(base_dir), edges(head_dir)
tally = collections.Counter(); ex = collections.defaultdict(list)
for name, m in sorted(meta.items()):
    t = truth(name)
    b = {x for x in base[name] if x[0] == name and x[1] == "go"}
    h = {x for x in head[name] if x[0] == name and x[1] == "go"}
    added = head[name] - base[name]
    kinds = ("pkg:" if m.get("pkg") else "") + ("+".join(sorted({k for k, _, _ in m["occ"]})) or "none")
    if added:
        verdict = "ADDED"
    elif b and not h:
        verdict = "WRONG" if t == "LOCAL" else "declined-ok"
    elif h:
        verdict = "kept-correct" if t == "LOCAL" else "kept-wrong"
    else:
        verdict = "no-same-file-edge"
    tally[(verdict, kinds)] += 1
    if verdict in ("WRONG", "ADDED") and len(ex[verdict]) < 40:
        ex[verdict].append((name, t, m["import"], m["call"], m["occ"], sorted(added)[:3]))
for k, n in sorted(tally.items()):
    print(f"{n:6d} {k[0]:18s} {k[1]}")
for v in ("WRONG", "ADDED"):
    for e in ex[v]:
        print(v, json.dumps(e))
