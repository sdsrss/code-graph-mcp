#!/usr/bin/env python3
"""Score classes that name one known node (E, HD, HDF, DC, DCW) against node-id oracles.

usage: node_score.py <arm>... [--table]
Right: the target set T is one node -> an answer for that node (find_references:
symbol is the typed or stored name and the reference set equals
find_references(node_id); get_call_graph: same symbol rule and node set equals
CLI callgraph --node-id; get_ast_node: node_id in T). T of several nodes -> an
ambiguity whose candidates are all in T. External import nodes have no call
graph (CLI --node-id: rc 1 "No call graph results"), so for them a get_call_graph
not-found / empty answer naming the typed input is counted as "nograph".
"""
import collections, json, re, sqlite3, sys
ROOT = "/var/tmp/d435"
argv = sys.argv; sys.argv = ["x"]
exec(open(f"{ROOT}/harness/diff.py").read().split("def main():")[0])
sys.argv = argv
arms = [a for a in sys.argv[1:] if not a.startswith("--")]
CLASSES = ("E", "HD", "HDF", "DC", "DCW")
norm = lambda s: (s or "").replace("::", ".").strip()

def oracle(corpus):
    fr, gan, cg = {}, {}, {}
    for l in open(f"{ROOT}/results/oracle-nodes/{corpus}/mcp-head-baafe5ee.jsonl"):
        r = json.loads(l); k, n = r["id"].split("|"); n = int(n)
        (fr if k == "fr" else gan)[n] = r
    for l in open(f"{ROOT}/results/oracle-nodes/{corpus}/cli-cg-head-baafe5ee.tsv"):
        n, rc, js = l.rstrip("\n").split("\t", 2)
        o = json.loads(js) if js.strip() else {}
        cg[int(n)] = (int(rc), {(x.get("name"), x.get("file_path"), x.get("depth"), x.get("direction")) for x in o.get("results") or []})
    return fr, gan, cg

def targets(rec, db):
    t = rec["tags"]; c = rec["class"]
    if c == "E":
        if t["n_project"]:
            ids = [r[0] for r in db.execute("SELECT n.id FROM nodes n JOIN files f ON f.id=n.file_id WHERE n.name=? AND f.path<>'<external>' AND n.type<>'module'", (rec["sym"],))]
            return ids, "E-project"
        return t["ext_ids"], "E-ext" + ("-dotted" if t["dotted"] else "")
    if c in ("HD", "HDF"):
        return t["node_ids"], c
    return [t["node_id"]], c + ("" if c == "DC" else "-" + t.get("variant", ""))

def cand_ids(res):
    lst = (res.get("suggestions") or res.get("candidates") or []) if isinstance(res, dict) else []
    return {x.get("node_id") for x in lst if isinstance(x, dict)}

def judge(surface, rec, r, T, names, fr_o, cg_o, ext):
    v = norm_mcp(surface, r); k = amb_kind(v, rec["sym"]); res = r["result"]
    if k in ("ambiguous_exact", "near_list", "near_list_same_name"):
        ok = len(T) > 1 and cand_ids(res) and cand_ids(res) <= set(T)
        return ("right" if ok else "wrong-" + k), k
    if k != "answer":
        if surface == "gcg_both" and ext and k == "not_found" and len(T) == 1 and cg_o[T[0]][0] == 1:
            return "nograph", k
        return "refused-" + k, k
    if surface == "gan":
        ok = len(T) == 1 and res.get("node_id") == T[0]
        return ("right" if ok else "wrong-answer"), f"{res.get('qualified_name') or res.get('name')}#{res.get('node_id')}" + (" +note" if res.get("note") else "")
    sym = v["symbol"]
    okname = norm(sym) in {norm(rec["sym"])} | {norm(x) for x in names}
    if len(T) != 1:
        return "wrong-answer(merged)", sym
    if surface == "fr":
        okitems = v["truncated"] or v["items"] == norm_mcp("fr", fr_o[T[0]])["items"]
    else:
        rc, items = cg_o[T[0]]
        okitems = v["truncated"] or v["items"] == items
        if ext and rc == 1 and okname and not v["items"]:
            return "nograph", sym
    return ("right" if (okname and okitems) else "wrong-answer"), sym + (" +note" if v.get("note") else "")

def main():
    table = "--table" in sys.argv
    for corpus in ("flask", "leveldb", "md"):
        inp = [json.loads(l) for l in open(f"{ROOT}/results/inputs-{corpus}.jsonl")]
        recs = [r for r in inp if r["class"] in CLASSES]
        if not recs:
            continue
        db = sqlite3.connect(f"file:{ROOT}/corpora/{corpus}/.code-graph/index.db?mode=ro", uri=True)
        fr_o, gan_o, cg_o = oracle(corpus)
        res = {}
        for arm in arms:
            for sf in ("fr", "gcg_both", "gan"):
                rows = load(f"{ROOT}/results/{arm}/{corpus}/mcp_{sf}.jsonl")
                for rec in recs:
                    if rec["id"] not in rows:
                        continue
                    T, grp = targets(rec, db)
                    names = set()
                    for n in T:
                        row = db.execute("SELECT name, qualified_name FROM nodes WHERE id=?", (n,)).fetchone()
                        names.update(x for x in row if x)
                    res[(rec["id"], arm, sf)] = (grp,) + judge(sf, rec, rows[rec["id"]][1], T, names, fr_o, cg_o, grp.startswith("E-ext"))
        print(f"===== {corpus}")
        groups = sorted({v[0] for v in res.values()})
        for g in groups:
            for sf in ("fr", "gcg_both", "gan"):
                line = []
                for arm in arms:
                    c = collections.Counter(v[1] for (rid, a, s), v in res.items() if a == arm and s == sf and v[0] == g)
                    line.append(f"{arm}: " + ", ".join(f"{k}={n}" for k, n in sorted(c.items())))
                n = sum(1 for (rid, a, s), v in res.items() if a == arms[0] and s == sf and v[0] == g)
                print(f"  {g:16} {sf:9} n={n:3} | " + " | ".join(line))
        if table:
            print(f"--- per input ({' / '.join(arms)})")
            for rec in recs:
                cells = []
                for sf in ("fr", "gcg_both", "gan"):
                    vals = [res.get((rec["id"], a, sf)) for a in arms]
                    cells.append(f"{sf}: " + " / ".join(f"{v[1]}<{str(v[2])[:40]}>" if v else "-" for v in vals))
                print(f"  {rec['class']} {rec['sym']!r}{' file=' + rec['file'] if 'file' in rec else ''}\n      " + "\n      ".join(cells))
main()
