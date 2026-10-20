#!/usr/bin/env python3
"""D#446 transitions: every run that differs between two arms, by verdict pair.

usage: d446.py <armA> <armB> [--root R]
Expected: only mcp_fr test_refusal -> answer|ambiguous_exact and mcp_gan/gani
not_found -> answer|ambiguous_exact; all other surfaces byte-equal.
Also lists inputs whose armB fr verdict is still test_refusal, and checks each
changed gan answer is a node named the lookup name in test code.
"""
import argparse, collections, json, os, sqlite3, sys
sys.argv_saved = list(sys.argv)
HERE = os.path.dirname(os.path.abspath(__file__))
ns = {}
src = open(f"{HERE}/diff.py", encoding="utf-8").read().replace("\nmain()\n", "\n")
exec(compile(src, "diff.py", "exec"), ns)
gen_src = open(f"{HERE}/gen.py", encoding="utf-8").read().replace("\nmain()\n", "\n")
gns = {}
exec(compile(gen_src, "gen.py", "exec"), gns)
is_test_symbol = gns["is_test_symbol"]
load, inputs, norm_mcp, norm_cli, amb_kind = ns["load"], ns["inputs"], ns["norm_mcp"], ns["norm_cli"], ns["amb_kind"]

EXPECTED = {("mcp_fr", "test_refusal", "answer"), ("mcp_fr", "test_refusal", "ambiguous_exact"),
            ("mcp_gan", "not_found", "answer"), ("mcp_gan", "not_found", "ambiguous_exact"),
            ("mcp_gani", "not_found", "answer"), ("mcp_gani", "not_found", "ambiguous_exact")}

def verdict(surface, rec, sym):
    s = surface[4:] if surface.startswith("mcp_") else surface[4:]
    v = norm_mcp(s, rec) if surface.startswith("mcp_") else norm_cli(s, rec)
    return v, amb_kind(v, sym)

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("a"); ap.add_argument("b"); ap.add_argument("--root", default="/var/tmp/d446")
    a = ap.parse_args()
    grand = collections.Counter()
    for corpus in ns["CORPORA"]:
        da, db = f"{a.root}/results/{a.a}/{corpus}", f"{a.root}/results/{a.b}/{corpus}"
        if not (os.path.isdir(da) and os.path.isdir(db)):
            continue
        inp = inputs(a.root, corpus)
        conn = sqlite3.connect(f"file:{a.root}/corpora/{corpus}/.code-graph/index.db?mode=ro", uri=True)
        trans = collections.Counter(); unexpected = []; still = []; bad_node = []
        changed_inputs = collections.defaultdict(set)
        total = same = 0
        for s in sorted(set(os.listdir(da)) | set(os.listdir(db))):
            A, B = load(f"{da}/{s}"), load(f"{db}/{s}")
            surf = s[:-6]
            for rid in sorted(set(A) | set(B)):
                total += 1
                rec = inp[rid]
                la, ra = A[rid]; lb, rb = B[rid]
                if surf == "mcp_fr":
                    vb, kb = verdict(surf, rb, rec["sym"])
                    if kb == "test_refusal":
                        still.append((rid, rec["sym"], rec.get("file"), (vb.get("msg") or "")[:160]))
                if la == lb:
                    same += 1; continue
                va, ka = verdict(surf, ra, rec["sym"]); vb, kb = verdict(surf, rb, rec["sym"])
                key = (surf, ka, kb)
                trans[key] += 1
                changed_inputs[rid].add(surf)
                if key not in EXPECTED:
                    unexpected.append((surf, rid, rec["sym"], rec.get("file"), la[:300], lb[:300]))
                if surf in ("mcp_gan", "mcp_gani") and kb == "answer":
                    body = rb["result"]
                    nid, fp, name = body.get("node_id"), body.get("file_path"), body.get("name")
                    row = conn.execute("SELECT n.name, f.path, n.type FROM nodes n JOIN files f ON f.id=n.file_id WHERE n.id=?", (nid,)).fetchone()
                    if not row or row[1] != fp or not is_test_symbol(row[0], row[1]):
                        bad_node.append((surf, rid, rec["sym"], nid, fp, name, row))
        print(f"===== {corpus}: {a.a} -> {a.b}: runs {total}, byte-equal {same}, changed {total - same} ({len(changed_inputs)} inputs)")
        for k, n in sorted(trans.items()):
            mark = "" if k in EXPECTED else "   <-- UNEXPECTED"
            print(f"   {k[0]:10s} {k[1]:16s} -> {k[2]:16s} {n}{mark}")
            grand[k] += n
        cls = collections.Counter(inp[r]["class"] for r in changed_inputs)
        print(f"   changed inputs by class: {dict(sorted(cls.items()))}")
        print(f"   {a.b} fr still test_refusal: {len(still)}")
        for e in still[:5]:
            print(f"      {e}")
        print(f"   gan answers not a test-code node named so: {len(bad_node)}")
        for e in bad_node[:5]:
            print(f"      {e}")
        for e in unexpected[:8]:
            print(f"   UNEXPECTED {e[0]} {e[1]} {e[2]!r} file={e[3]}\n      A {e[4]}\n      B {e[5]}")
        with open(f"{a.root}/results/d446-changed-{a.a}-{a.b}-{corpus}.json", "w") as f:
            json.dump({r: sorted(v) for r, v in changed_inputs.items()}, f)
    print("===== all corpora")
    for k, n in sorted(grand.items()):
        print(f"   {k[0]:10s} {k[1]:16s} -> {k[2]:16s} {n}")

main()
