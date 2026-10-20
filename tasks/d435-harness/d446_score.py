#!/usr/bin/env python3
"""Node-id oracle for D#446 changes on one arm.

For each input whose find_references changed (d446-changed-<a>-<b>-<corpus>.json):
- fr answered: the node get_ast_node answers for the same input, asked by
  node_id through find_references, must give the same references.
- fr ambiguous: its suggestions' node_ids must equal get_ast_node's.
usage: d446_score.py <armA> <armB> <binary> [--root R]
"""
import argparse, json, os, subprocess, sys
ap = argparse.ArgumentParser()
ap.add_argument("a"); ap.add_argument("b"); ap.add_argument("binary"); ap.add_argument("--root", default="/var/tmp/d446")
a = ap.parse_args()
HERE = os.path.dirname(os.path.abspath(__file__))
def load(p):
    return {json.loads(l)["id"]: json.loads(l) for l in open(p, encoding="utf-8")}
tot = {}
for corpus in ("flask", "leveldb", "express", "hono", "gin", "gson", "cgm"):
    cp = f"{a.root}/results/d446-changed-{a.a}-{a.b}-{corpus}.json"
    if not os.path.exists(cp):
        continue
    changed = json.load(open(cp))
    d = f"{a.root}/results/{a.b}/{corpus}"
    FR, GAN = load(f"{d}/mcp_fr.jsonl"), load(f"{d}/mcp_gan.jsonl")
    calls, pend = [], {}
    res = {"answer_same": 0, "answer_diff": 0, "answer_no_gan": 0, "amb_same": 0, "amb_diff": 0, "other": 0}
    ex = []
    for rid, surfs in changed.items():
        fr, gan = FR[rid], GAN[rid]
        if "mcp_fr" not in surfs:
            # get_ast_node alone changed: fr already answered (exact qualified
            # name) or refused; its answer must be the node gan now answers.
            if "mcp_gan" not in surfs:
                continue
            if fr["is_error"] or not isinstance(fr["result"], dict):
                res["other"] += 1; ex.append(("gan-only, fr error", rid, str(fr["result"])[:160])); continue
            if "error" in fr["result"] or not isinstance(gan["result"], dict) or "node_id" not in gan["result"]:
                fs = sorted(x["node_id"] for x in fr["result"].get("suggestions", []))
                gs = sorted(x["node_id"] for x in (gan["result"].get("suggestions", []) if isinstance(gan["result"], dict) else []))
                if fs and fs == gs:
                    res["amb_same"] += 1
                else:
                    res["amb_diff"] += 1; ex.append(("gan-only verdicts differ", rid, str(fr["result"])[:160], str(gan["result"])[:160]))
                continue
            calls.append({"id": rid, "tool": "find_references", "args": {"node_id": gan["result"]["node_id"]}})
            pend[rid] = fr["result"]
            continue
        if fr["is_error"] or not isinstance(fr["result"], dict):
            res["other"] += 1; continue
        if "error" in fr["result"]:
            fs = sorted(s["node_id"] for s in fr["result"].get("suggestions", []))
            gs = sorted(s["node_id"] for s in (gan["result"].get("suggestions", []) if isinstance(gan["result"], dict) else []))
            if fs == gs and fs:
                res["amb_same"] += 1
            else:
                res["amb_diff"] += 1; ex.append(("amb", rid, fs, gs))
            continue
        if gan["is_error"] or not isinstance(gan["result"], dict) or "node_id" not in gan["result"]:
            res["answer_no_gan"] += 1; ex.append(("nogan", rid, str(gan["result"])[:200])); continue
        nid = gan["result"]["node_id"]
        calls.append({"id": rid, "tool": "find_references", "args": {"node_id": nid}})
        pend[rid] = fr["result"]
    if calls:
        p = subprocess.run([sys.executable, "-I", f"{HERE}/mcpc.py", a.binary, f"{a.root}/corpora/{corpus}"],
                           input="".join(json.dumps(c) + "\n" for c in calls), capture_output=True, text=True)
        for line in p.stdout.splitlines():
            r = json.loads(line)
            got, want = r["result"], pend[r["id"]]
            strip = lambda o: {k: v for k, v in o.items() if k not in ("symbol", "note")} if isinstance(o, dict) else o
            if strip(got) == strip(want):
                res["answer_same"] += 1
            else:
                res["answer_diff"] += 1; ex.append(("diff", r["id"], str(got)[:200], str(want)[:200]))
    print(corpus, res)
    for e in ex[:4]:
        print("   ", e)
