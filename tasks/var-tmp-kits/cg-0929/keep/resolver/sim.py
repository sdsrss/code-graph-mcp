#!/usr/bin/env python3
"""Re-tier judged call edges under candidate confidence rules (D10B).

sim.py ARM_DIR CORPUS LANG [--shapes SHAPES_TSV] [--rules R0,R1,...] [--rows FILTER]

R0 re-implements classify_edge_confidence (resolve.rs CONF_CASE/CONF_WHERE) over the
index and must reproduce the stored label of every judged edge; candidate rules add
same-file edges whose every call site of that name in the caller has a listed shape.
"""
import argparse, collections, json, sqlite3, sys

EXEMPT_Q = {"self", "stype", "rtype", "super", "path", "imp"}
FAMILY = {"python": ("python",), "javascript": ("javascript", "typescript", "tsx"), "rust": ("rust",)}


def load(arm, corpus, lang, shapes_path):
    db = sqlite3.connect(f"{arm}/{corpus}/.code-graph/index.db")
    judged = json.load(open(f"{arm}/out/{corpus}-{lang}-judged.json"))
    nodes = {}
    by_key = {}
    by_file = collections.defaultdict(list)
    for nid, name, ntype, path, flang, sl, el in db.execute(
            "SELECT n.id, n.name, n.type, f.path, f.language, n.start_line, n.end_line FROM nodes n JOIN files f ON f.id = n.file_id"):
        nodes[nid] = (name, ntype, path, flang, sl, el)
        by_key.setdefault((name, f"{path}:{sl}"), []).append(nid)
        by_file[path].append(nid)
    namecount = collections.Counter((n[0], n[3]) for n in nodes.values())
    imports = set()
    for s, t in db.execute("SELECT ie.source_id, ie.target_id FROM edges ie WHERE ie.relation = 'imports'"):
        imports.add((nodes[s][2], t))
    edges = {}
    for s, t, conf, meta in db.execute("SELECT source_id, target_id, confidence, metadata FROM edges WHERE relation = 'calls'"):
        edges.setdefault((s, t), []).append((conf, meta))
    shapes = collections.defaultdict(set)  # (caller id, callee name) -> shapes
    unplaced = 0
    if shapes_path:
        for line in open(shapes_path):
            path, scope, callee, ln, shape = line.rstrip("\n").split("\t")
            ln = int(ln)
            best = None
            for nid in by_file.get(path, ()):
                name, ntype, _p, _l, sl, el = nodes[nid]
                if sl <= ln <= el and (name == scope or name == scope.split(".")[-1]) and ntype in ("function", "method", "module"):
                    if best is None or (nodes[best][5] - nodes[best][4]) > (el - sl):
                        best = nid
            if best is None and scope == "<module>":
                best = next((nid for nid in by_file.get(path, ()) if nodes[nid][0] == "<module>"), None)
            if best is None:
                unplaced += 1
                continue
            shapes[(best, callee)].add(shape)
    return db, judged, nodes, by_key, namecount, imports, edges, shapes, unplaced


def q_of(meta):
    if not meta:
        return None, {}
    try:
        m = json.loads(meta)
    except Exception:
        return None, {}
    return (m.get("q") if isinstance(m, dict) else None), (m if isinstance(m, dict) else {})


RANK = {"extracted": 0, "inferred": 1, "ambiguous": 2}


def best(tiers):
    return min(tiers, key=RANK.get)


def base_tier(s, t, nodes, namecount, imports, edges, allowed=None, shapes=None):
    """Best tier over the pair's rows; with `allowed`, same-file rows whose call
    shapes are all in it are relabelled too. Returns (tier, any_row_eligible)."""
    out = []
    elig_any = False
    for conf, meta in edges[(s, t)]:
        tier, elig = row_tier(s, t, meta, nodes, namecount, imports)
        if not elig and allowed is not None and nodes[s][2] == nodes[t][2]:
            sh = shapes.get((s, nodes[t][0]), set())
            if sh and sh <= allowed:
                tier = relabel(s, t, meta, nodes, namecount, imports)
        elig_any |= elig
        out.append(tier)
    return best(out), elig_any


def row_tier(s, t, meta, nodes, namecount, imports):
    src, tgt = nodes[s], nodes[t]
    q, m = q_of(meta)
    same_file = src[2] == tgt[2]
    eligible = (not same_file) or (tgt[3] == "rust" and q in ("member", "chain") and ("rt" not in m or "amb" in m))
    if not eligible:
        return "extracted", False
    return relabel(s, t, meta, nodes, namecount, imports), True


def relabel(s, t, meta, nodes, namecount, imports):
    tgt = nodes[t]
    q, m = q_of(meta)
    dup = namecount[(tgt[0], tgt[3])] > 1
    imported = (nodes[s][2], t) in imports
    not_exempt = q is None or q not in EXEMPT_Q or "amb" in m
    return "ambiguous" if dup and not imported and not_exempt else "inferred"


RULES = {
    # shape sets whose SAME-FILE edges get relabelled (every call site of the name in the caller must match)
    "R1_selfattr": {"self+attr"},
    "R1_relimp": {"relimp", "relimp+attr"},
    "R1_absimp": {"absimp", "absimp+attr"},
    "R1_self_rel": {"self+attr", "relimp", "relimp+attr"},
    "R1_all": {"self+attr", "relimp", "relimp+attr", "absimp", "absimp+attr"},
    "R1_this": {"this+attr"},
    "R1_ident": {"ident", "ident+attr"},
    "R1_callrecv": {"callrecv", "callrecv+attr"},
}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("arm"); ap.add_argument("corpus"); ap.add_argument("lang")
    ap.add_argument("--shapes")
    ap.add_argument("--rules", default=",".join(RULES))
    ap.add_argument("--rows", help="print judged same-file rows for this shape-set rule")
    a = ap.parse_args()
    db, judged, nodes, by_key, namecount, imports, edges, shapes, unplaced = load(a.arm, a.corpus, a.lang, a.shapes)
    rows = []
    miss = 0
    for j in judged:
        s = by_key.get((j["caller"], j["caller_at"]), [])
        t = by_key.get((j["callee"], j["callee_at"]), [])
        pair = next(((x, y) for x in s for y in t if (x, y) in edges), None)
        if pair is None:
            miss += 1
            continue
        rows.append((j, pair))
    # R0 must reproduce the stored labels AND the oracle's tier counts.
    r0_mismatch = 0
    oracle = collections.Counter((j["tier"], j["verdict"] == "correct") for j, _ in rows)
    sim0 = collections.Counter()
    for j, (s, t) in rows:
        tier, _ = base_tier(s, t, nodes, namecount, imports, edges)
        if tier != j["tier"] or tier != best([c for c, _m in edges[(s, t)]]):
            r0_mismatch += 1
        sim0[(tier, j["verdict"] == "correct")] += 1
    print(f"== {a.corpus}/{a.lang}: judged {len(judged)}, joined {len(rows)}, unjoined {miss}, shape rows unplaced {unplaced}")
    print(f"R0 mismatches vs stored/oracle label: {r0_mismatch}; R0==oracle counts: {sim0 == oracle}")
    def table(c):
        return "  ".join(f"{t} {c[(t, True)]}/{c[(t, True)] + c[(t, False)]}" for t in ("extracted", "inferred", "ambiguous"))
    print(f"R0: {table(sim0)}")
    # shape census of same-file, not-already-relabelled judged edges
    census = collections.Counter()
    for j, (s, t) in rows:
        tier, elig = base_tier(s, t, nodes, namecount, imports, edges)
        if nodes[s][2] == nodes[t][2] and not elig:
            q = "/".join(sorted({str(q_of(m)[0]) for _c, m in edges[(s, t)]}))
            key = ("|".join(sorted(shapes.get((s, nodes[t][0]), {"?"}))), q)
            census[(key, j["verdict"] == "correct")] += 1
    print("same-file extracted edges by (call shapes, q): correct/total")
    keys = sorted({k for k, _ in census}, key=lambda k: -(census[(k, True)] + census[(k, False)]))
    for k in keys:
        print(f"  {k[0]:<40} q={str(k[1]):<8} {census[(k, True)]}/{census[(k, True)] + census[(k, False)]}")
    for rn in a.rules.split(","):
        allowed = RULES[rn]
        c = collections.Counter()
        moved = collections.Counter()
        for j, (s, t) in rows:
            tier, elig = base_tier(s, t, nodes, namecount, imports, edges)
            new, _ = base_tier(s, t, nodes, namecount, imports, edges, allowed, shapes)
            sh = shapes.get((s, nodes[t][0]), set())
            ok = j["verdict"] == "correct"
            c[(new, ok)] += 1
            if new != tier:
                moved[(tier, new, ok)] += 1
            if a.rows == rn and new != tier:
                print(f"    {tier}->{new} {'OK ' if ok else 'BAD'} {j['caller']}@{j['caller_at']} -> {j['callee']}@{j['callee_at']} {sorted(sh)} {[m for _c, m in edges[(s, t)]]}")
        mv = "  ".join(f"{a_}->{b_}: {moved[(a_, b_, True)]} correct / {moved[(a_, b_, False)]} wrong"
                       for a_, b_ in sorted({(x, y) for x, y, _ in moved}))
        print(f"{rn}: {table(c)}   moved: {mv or 'none'}")


if __name__ == "__main__":
    main()
