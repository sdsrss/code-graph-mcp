#!/usr/bin/env python3
"""Classify every changed (surface, input) between two arms by its MCP transition.

usage: transitions.py <armA> <armB> [--root R]
Writes <root>/results/transitions-<A>-<B>-<corpus>.jsonl and prints counts,
plus flagged inputs (possible regressions) in full.
"""
import argparse, collections, json, os, re, sqlite3, sys
sys.argv_saved = sys.argv
ROOT = "/var/tmp/d435"
src = open(f"{ROOT}/harness/diff.py").read().split("def main():")[0]
sys_argv = sys.argv
exec(src)
sys.argv = sys_argv

def canon(s):
    return (s or "").strip().replace("::", ".").lstrip(".") if (s or "").strip().startswith("::") else (s or "").strip().replace("::", ".")

def summarize(surface, rec, sym, quals):
    v = norm_mcp(surface, rec)
    k = amb_kind(v, sym)
    out = {"kind": k}
    if k == "answer":
        out["symbol"] = v["symbol"]
        out["note"] = v["note"]
        if surface.startswith("gan"):
            res = rec["result"]
            out["qualified_name"] = res.get("qualified_name")
            out["note"] = res.get("note")
    elif k in ("ambiguous_exact", "near_list", "near_list_same_name"):
        m = re.search(r"': (\d+) (?:matches|definitions)", v.get("msg") or "")
        out["count"] = int(m.group(1)) if m else len(v.get("cands") or [])
        out["cands"] = v.get("cands")
    else:
        out["msg"] = (v.get("msg") or "")[:160]
    return out

def same_symbol(typed, answered, quals, qn=None):
    t = canon(typed)
    a = (answered or "").replace("::", ".")
    if a == t or (qn and qn.replace("::", ".") == t):
        return True
    # exact qualified input answered under its last part
    if t in quals and a == bare(t):
        return True
    return False

def label(b, h):
    lb = b["kind"]; lh = h["kind"]
    if lb == "answer": lb += "(note)" if b.get("note") else ""
    if lh == "answer": lh += "(note)" if h.get("note") else ""
    s = f"{lb} -> {lh}"
    if b["kind"] == h["kind"] == "answer":
        s += " [same symbol]" if b["symbol"] == h["symbol"] else " [symbol changed]"
    if b["kind"] == h["kind"] == "ambiguous_exact" and b.get("count") != h.get("count"):
        s += f" [count {b.get('count')}->{h.get('count')}]"
    return s

def main():
    ap = argparse.ArgumentParser(); ap.add_argument("a"); ap.add_argument("b")
    a = ap.parse_args()
    for corpus in CORPORA:
        if not os.path.exists(f"{ROOT}/results/changed-{a.a}-vs-{a.b}-{corpus}.jsonl"):
            continue
        db = sqlite3.connect(f"file:{ROOT}/corpora/{corpus}/.code-graph/index.db?mode=ro", uri=True)
        quals = {r[0] for r in db.execute("SELECT DISTINCT qualified_name FROM nodes WHERE qualified_name IS NOT NULL")}
        names = {r[0] for r in db.execute("SELECT DISTINCT name FROM nodes")}
        inp = inputs(ROOT, corpus)
        changed = [json.loads(l) for l in open(f"{ROOT}/results/changed-{a.a}-vs-{a.b}-{corpus}.jsonl")]
        cnt = collections.Counter(); bycls = collections.defaultdict(collections.Counter)
        flags = collections.defaultdict(list)
        out = []
        cache = {}
        def rec_of(arm, surface, rid):
            key = (arm, surface)
            if key not in cache:
                cache[key] = load(f"{ROOT}/results/{arm}/{corpus}/{surface}")
            return cache[key][rid][1]
        for ch in changed:
            s = ch["surface"]; rid = ch["id"]; rec = inp[rid]
            if not s.startswith("mcp_"):
                flags["CLI output changed"].append((s, rid, rec["sym"])); continue
            surf = s[4:-6]
            b = summarize(surf, rec_of(a.a, s, rid), rec["sym"], quals)
            h = summarize(surf, rec_of(a.b, s, rid), rec["sym"], quals)
            lab = label(b, h)
            cnt[(surf, lab)] += 1
            for g in [rec["class"]] + breakouts(rec):
                bycls[g][lab] += 1
            row = {"id": rid, "class": rec["class"], "surface": surf, "sym": rec["sym"], "file": rec.get("file"),
                   "label": lab, "base": b, "head": h}
            out.append(row)
            # regression flags
            if b["kind"] == "answer" and h["kind"] != "answer":
                flags["answer became non-answer"].append(row)
            if b["kind"] == h["kind"] == "answer" and b["symbol"] != h["symbol"]:
                if same_symbol(rec["sym"], b["symbol"], quals, b.get("qualified_name")) and not same_symbol(rec["sym"], h["symbol"], quals, h.get("qualified_name")):
                    flags["right symbol became another"].append(row)
            if h["kind"] == "answer":
                differs = not same_symbol(rec["sym"], h["symbol"], quals, h.get("qualified_name"))
                if differs and not h.get("note"):
                    flags["HEAD note missing where answered symbol differs"].append(row)
                if not differs and h.get("note"):
                    flags["HEAD note present where answered symbol equals typed"].append(row)
                if h.get("note") and h["symbol"] and h["symbol"] not in h["note"] and (h.get("qualified_name") or "") not in h["note"]:
                    flags["HEAD note names another symbol than the answer"].append(row)
        with open(f"{ROOT}/results/transitions-{a.a}-{a.b}-{corpus}.jsonl", "w", encoding="utf-8") as f:
            for r in out:
                f.write(json.dumps(r, ensure_ascii=False, default=list) + "\n")
        print(f"===== {corpus}: {len(out)} changed MCP (surface,input) pairs =====")
        for (surf, lab), n in sorted(cnt.items(), key=lambda x: (x[0][0], -x[1])):
            print(f"   {surf:12s} {n:5d}  {lab}")
        print("  -- by class / breakout")
        for g in sorted(bycls):
            print(f"   {g}: " + "; ".join(f"{l}={n}" for l, n in sorted(bycls[g].items(), key=lambda x: -x[1])))
        print("  -- flags")
        for k, rows in flags.items():
            print(f"   {k}: {len(rows)}")
            for r in rows:
                print("      ", json.dumps(r, ensure_ascii=False, default=list)[:600])

main()
