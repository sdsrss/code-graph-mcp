#!/usr/bin/env python3
"""Score class DC answers per arm against the node-id oracle (results/dc-oracle)."""
import collections, json, sys
ROOT = "/var/tmp/d435"
sys_argv = sys.argv; sys.argv = ["x"]
exec(open(f"{ROOT}/harness/diff.py").read().split("def main():")[0])
sys.argv = sys_argv
arms = [a for a in sys.argv[1:] if not a.startswith("--")] or ["base", "head", "head2"]
inp = {r["id"]: r for r in (json.loads(l) for l in open(f"{ROOT}/results/inputs-leveldb.jsonl")) if r["class"] == "DC"}
orc = {}
for l in open(f"{ROOT}/results/dc-oracle/mcp-head-355892f3.jsonl"):
    r = json.loads(l); kind, n = r["id"].split("|"); orc[(kind, int(n))] = r
cgo = {}
for l in open(f"{ROOT}/results/dc-oracle/cli-cg-head-355892f3.tsv"):
    n, js = l.rstrip("\n").split("\t", 1)
    o = json.loads(js)
    cgo[int(n)] = {(x.get("name"), x.get("file_path"), x.get("depth"), x.get("direction")) for x in o.get("results") or []}
def norm(s): return (s or "").replace("::", ".")
def judge(surface, rec, r):
    t = rec["tags"]; n = t["node_id"]
    v = norm_mcp(surface, r)
    k = amb_kind(v, rec["sym"])
    if k != "answer":
        return k, None, False
    sym = v["symbol"]
    if surface == "gan":
        return k, f"{r['result'].get('qualified_name')}#{r['result'].get('node_id')}", r["result"].get("node_id") == n
    okname = norm(sym) in (norm(t["stored_q"]), norm(t["node_name"]))
    if surface == "fr":
        ov = norm_mcp("fr", orc[("fr", n)])
        okitems = v["truncated"] or v["items"] == ov["items"]
    else:
        okitems = v["truncated"] or v["items"] == cgo[n]
    return k, sym + (" +note" if v.get("note") else ""), okname and okitems
rows = collections.defaultdict(dict)
for arm in arms:
    for sf in ["fr", "gcg_both", "gan"]:
        for l in open(f"{ROOT}/results/{arm}/leveldb/mcp_{sf}.jsonl"):
            r = json.loads(l)
            if r["id"] in inp:
                rows[(r["id"], sf)][arm] = judge(sf, inp[r["id"]], r)
# summary
print("per tool / arm: right / answered-wrong / non-answer (by form)")
for sf in ["fr", "gcg_both", "gan"]:
    for arm in arms:
        c = collections.Counter()
        for (rid, s), d in rows.items():
            if s != sf: continue
            k, sym, ok = d[arm]
            form = inp[rid]["tags"]["form"]
            c[(form, "right" if ok else ("wrong-answer" if k == "answer" else f"non-answer:{k}"))] += 1
        forms = ["dots", "colons", "stored"]
        out = []
        for f in forms:
            tot = sum(v for (ff, _), v in c.items() if ff == f)
            parts = ", ".join(f"{lab}={v}" for (ff, lab), v in sorted(c.items()) if ff == f)
            out.append(f"{f}(n={tot}): {parts}")
        print(f"  {sf:9} {arm:5} | " + " | ".join(out))
if "--table" in sys.argv:
    print("\nper input (tool: base / head / head2)")
    for rid in sorted(inp, key=lambda x: int(x.rsplit(':', 1)[1])):
        rec = inp[rid]
        print(f"{rec['sym']!r} [{rec['tags']['form']}] stored={rec['tags']['stored_q']!r} #{rec['tags']['node_id']} {rec['tags']['node_type']}")
        for sf in ["fr", "gcg_both", "gan"]:
            d = rows[(rid, sf)]
            cells = []
            for arm in arms:
                k, sym, ok = d[arm]
                cells.append(f"{'OK ' if ok else 'XX '}{k if k != 'answer' else sym}")
            print(f"     {sf:9} " + " / ".join(c[:60] for c in cells))
