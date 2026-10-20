#!/usr/bin/env python3
"""Compare results.

  diff.py arms <armA> <armB> [--root R]      byte-equality per (corpus, surface, input)
  diff.py oracle <arm> [--root R]            MCP vs CLI verdicts within one arm

Breakouts (from gen.py tags): D435 = classes D1/D2/D3/DF; D445 = B with n_exact > 10;
D446 = B whose exact rows are all test code (is_test_symbol); D447 = Q with >= 2
definitions and none in production (is_test flag or test path).
"""
import argparse, collections, json, os, re

CORPORA = ("flask", "leveldb", "md", "fx2", "express", "hono", "gin", "gson", "cgm")
DIRS = ("callers", "callees", "both")

def load(path):
    out = {}
    if not os.path.exists(path):
        return out
    for l in open(path, encoding="utf-8"):
        r = json.loads(l)
        out[r["id"]] = (l, r)
    return out

def inputs(root, corpus):
    return {json.loads(l)["id"]: json.loads(l) for l in open(f"{root}/results/inputs-{corpus}.jsonl", encoding="utf-8")}

def breakouts(rec):
    b = []
    t = rec.get("tags", {})
    if rec["class"] in ("D1", "D2", "D3", "DF"):
        b.append("D435")
    if rec["class"] == "B" and t.get("n_exact", 0) > 10:
        b.append("D445")
    if rec["class"] == "B" and t.get("all_test"):
        b.append("D446")
    if rec["class"] == "Q" and t.get("q_test_multi"):
        b.append("D447")
    return b

def bare(s):
    s = (s or "").strip()
    return s.rsplit(".", 1)[-1] if "." in s else s

def cand_names(lst):
    return sorted({c.get("name", "") for c in (lst or []) if isinstance(c, dict)})

def classify_text(msg):
    if "Ambiguous symbol" in msg:
        return "ambiguous"
    if "Did you mean" in msg or "No exact match for" in msg:
        return "near_list"
    if "test/bench paths" in msg:
        return "test_refusal"
    if "not found in file" in msg or "not defined in" in msg or "is not in the index" in msg or "Symbol not found in file" in msg:
        return "file_miss"
    if "not found" in msg or "No call graph results" in msg:
        return "not_found"
    return "other_error"

def norm_mcp(surface, rec):
    """-> dict(verdict, symbol, note, cands, items, truncated)"""
    res = rec["result"]
    v = {"verdict": None, "symbol": None, "note": None, "cands": None, "items": None, "truncated": False}
    if rec["is_error"]:
        msg = res if isinstance(res, str) else json.dumps(res)
        v["verdict"] = classify_text(msg)
        v["msg"] = msg
        return v
    if not isinstance(res, dict):
        v["verdict"] = "other_error"; v["msg"] = str(res); return v
    v["truncated"] = any(k.startswith("_trunc") or k.startswith("_array_trunc") for k in res)
    if "error" in res:
        kind = classify_text(res["error"])
        cands = cand_names(res.get("suggestions") or res.get("candidates"))
        v["verdict"] = kind; v["cands"] = cands; v["msg"] = res["error"]
        return v
    if "candidates" in res and "suggestion" in res:
        v["verdict"] = "near_list"; v["cands"] = cand_names(res["candidates"]); v["msg"] = res["suggestion"]
        return v
    v["verdict"] = "answer"
    v["note"] = res.get("note")
    if surface.startswith("gcg"):
        v["symbol"] = res.get("function")
        items = set()
        for side in ("callers", "callees"):
            part = res.get(side) or []
            if isinstance(part, dict):  # rollup mode (large graphs): no node list to compare
                v["truncated"] = True
                continue
            for n in part:
                items.add((n.get("name"), n.get("file_path"), n.get("depth"), n.get("direction")))
        v["items"] = items
    elif surface == "fr":
        v["symbol"] = res.get("symbol")
        v["items"] = {(r.get("file_path"), r.get("name"), r.get("relation"), r.get("start_line")) for r in res.get("references") or []}
    else:
        v["symbol"] = res.get("name")
        v["items"] = {(res.get("node_id"), res.get("file_path"))}
    return v

NOTE_RE = re.compile(r"(Resolved '[^']*' → '[^']*'|No definition is named '[^']*'; answering for '[^']*')")

def norm_cli(surface, rec):
    v = {"verdict": None, "symbol": None, "note": None, "cands": None, "items": None, "truncated": False}
    notes = NOTE_RE.findall(rec["stderr"])
    v["note"] = notes[0] if notes else None
    try:
        o = json.loads(rec["stdout"]) if rec["stdout"].strip() else None
    except Exception:
        o = None
    if not isinstance(o, dict):
        v["verdict"] = "other_error"; v["msg"] = (rec["stdout"] + rec["stderr"])[:300]; return v
    if "error" in o:
        kind = classify_text(o["error"])
        cands = cand_names(o.get("suggestions") or o.get("candidates"))
        # CLI fuzzy candidate lists reuse "Ambiguous symbol 'X': N matches" with no
        # file/same-file clause; treat as a near list when the names differ from X.
        v["verdict"] = kind; v["cands"] = cands; v["msg"] = o["error"]
        return v
    v["verdict"] = "answer"
    v["symbol"] = o.get("symbol")
    if surface.startswith("cg"):
        v["items"] = {(n.get("name"), n.get("file_path"), n.get("depth"), n.get("direction")) for n in o.get("results") or []}
    else:
        v["items"] = {(r.get("file_path"), r.get("name"), r.get("relation"), r.get("start_line")) for r in o.get("references") or []}
    return v

def amb_kind(v, sym):
    """Split ambiguous into exact (every candidate is the asked name or its bare part) vs near."""
    if v["verdict"] not in ("ambiguous", "near_list"):
        return v["verdict"]
    s = sym.strip().replace("::", ".").strip(".")
    names = set(v["cands"] or [])
    same = bool(names) and all(n in (s, bare(s)) for n in names)
    if v["verdict"] == "near_list":
        # A "No exact match ... Did you mean" list whose candidates all carry the
        # asked name (or its last part) is kept apart from a real near list.
        return "near_list_same_name" if same else "near_list"
    return "ambiguous_exact" if same else "near_list"

def compare(m, c, sym):
    mk, ck = amb_kind(m, sym), amb_kind(c, sym)
    if mk != ck:
        return f"verdict {mk} vs {ck}"
    if mk == "ambiguous_exact":
        cnt = lambda v: (re.search(r"': (\d+) (?:matches|definitions)", v.get("msg") or "") or [None, None])[1]
        if cnt(m) != cnt(c):
            return f"ambiguity count {cnt(m)} vs {cnt(c)}"
    if mk == "answer":
        if bare((m["symbol"] or "").replace("::", ".")) != bare((c["symbol"] or "").replace("::", ".")):
            return "symbol differs"
        if m["truncated"]:
            return None  # set not comparable; verdict+symbol agree
        if m["items"] != c["items"]:
            return "items differ"
    return None

def cmd_oracle(root, arm):
    pairs = [(f"gcg_{d}", f"cg_{d}") for d in DIRS] + [("fr", "refs")]
    for corpus in CORPORA:
        if not os.path.isdir(f"{root}/results/{arm}/{corpus}"):
            continue
        inp = inputs(root, corpus)
        base = f"{root}/results/{arm}/{corpus}"
        dist = collections.defaultdict(collections.Counter)
        dis = collections.defaultdict(collections.Counter)
        tot = collections.defaultdict(collections.Counter)
        ex = collections.defaultdict(list)
        calib = collections.Counter()
        calib_ex = collections.defaultdict(list)
        for ms, cs in pairs:
            M = load(f"{base}/mcp_{ms}.jsonl"); C = load(f"{base}/cli_{cs}.jsonl")
            for rid, (_, mr) in M.items():
                rec = inp[rid]
                m = norm_mcp(ms, mr); c = norm_cli(cs, C[rid][1])
                groups = [rec["class"]] + breakouts(rec)
                why = compare(m, c, rec["sym"])
                for g in groups:
                    tot[g][ms] += 1
                    dist[g][(ms, "mcp", amb_kind(m, rec["sym"]))] += 1
                    dist[g][(cs, "cli", amb_kind(c, rec["sym"]))] += 1
                    if why:
                        dis[g][(ms, why)] += 1
                        if len(ex[(g, ms, why)]) < 3:
                            ex[(g, ms, why)].append((rid, rec["sym"], rec.get("file"),
                                                     m.get("symbol") or (m.get("msg") or "")[:110],
                                                     c.get("symbol") or (c.get("msg") or "")[:110],
                                                     m.get("note"), c.get("note")))
                t = rec.get("tags", {})
                if rec["class"] == "B" and t.get("n_nontest") == 1:
                    calib[(ms, "agree" if not why else why)] += 1
                    if why and len(calib_ex[(ms, why)]) < 4:
                        calib_ex[(ms, why)].append((rid, rec["sym"], m.get("symbol") or (m.get("msg") or "")[:100],
                                                    c.get("symbol") or (c.get("msg") or "")[:100]))
        print(f"===== {corpus} ({arm}) =====")
        print("-- calibration: class B with exactly one non-test definition")
        for k, n in sorted(calib.items()):
            print(f"   {k[0]:12s} {k[1]:40s} {n}")
        for k, lst in calib_ex.items():
            for e in lst:
                print(f"      ex {k}: {e}")
        for g in sorted(dist):
            print(f"-- group {g}")
            for ms, cs in pairs:
                mc = {k[2]: n for k, n in dist[g].items() if k[0] == ms and k[1] == "mcp"}
                cc = {k[2]: n for k, n in dist[g].items() if k[0] == cs and k[1] == "cli"}
                d = sum(n for (s, _), n in dis[g].items() if s == ms)
                print(f"   {ms:12s} n={tot[g][ms]:5d} disagree={d:5d}  MCP {dict(sorted(mc.items()))}  CLI {dict(sorted(cc.items()))}")
            for (ms, why), n in sorted(dis[g].items()):
                print(f"      {ms:12s} {why:45s} {n}")
                for e in ex[(g, ms, why)]:
                    print(f"         ex {e}")
        # gan / gani verdicts (no CLI selector counterpart)
        for s in ("gan", "gani"):
            M = load(f"{base}/mcp_{s}.jsonl")
            cnt = collections.defaultdict(collections.Counter)
            for rid, (_, mr) in M.items():
                rec = inp[rid]; m = norm_mcp(s, mr)
                for g in [rec["class"]] + breakouts(rec):
                    cnt[g][amb_kind(m, rec["sym"])] += 1
            print(f"-- MCP {s} verdicts: " + "; ".join(f"{g} {dict(c)}" for g, c in sorted(cnt.items())))

def cmd_arms(root, a, b):
    for corpus in CORPORA:
        da, db = f"{root}/results/{a}/{corpus}", f"{root}/results/{b}/{corpus}"
        if not (os.path.isdir(da) and os.path.isdir(db)):
            continue
        inp = inputs(root, corpus)
        surfaces = sorted(set(os.listdir(da)) | set(os.listdir(db)))
        changed_list = []
        print(f"===== {corpus}: {a} vs {b} =====")
        total = same = 0
        by = collections.defaultdict(collections.Counter)
        ex = collections.defaultdict(list)
        for s in surfaces:
            A, B = load(f"{da}/{s}"), load(f"{db}/{s}")
            for rid in sorted(set(A) | set(B)):
                total += 1
                la, lb = A.get(rid, (None,))[0], B.get(rid, (None,))[0]
                if la == lb:
                    same += 1; continue
                rec = inp[rid]
                for g in [rec["class"]] + breakouts(rec):
                    by[g][s] += 1
                    if len(ex[(g, s)]) < 2:
                        ex[(g, s)].append((rid, rec["sym"], (la or "")[:220], (lb or "")[:220]))
                changed_list.append({"id": rid, "surface": s, "sym": rec["sym"], "class": rec["class"]})
        print(f"   runs {total}  byte-equal {same}  changed {total - same}")
        for g in sorted(by):
            print(f"   {g}: {dict(sorted(by[g].items()))}")
            for s in sorted(by[g]):
                for e in ex[(g, s)]:
                    print(f"      ex {s} {e[0]} {e[1]!r}\n         A {e[2]}\n         B {e[3]}")
        out = f"{root}/results/changed-{a}-vs-{b}-{corpus}.jsonl"
        with open(out, "w", encoding="utf-8") as f:
            for r in changed_list:
                f.write(json.dumps(r, ensure_ascii=False) + "\n")
        print(f"   changed inputs listed in {out}")

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("mode"); ap.add_argument("arms", nargs="+")
    ap.add_argument("--root", default="/var/tmp/d435")
    a = ap.parse_args()
    if a.mode == "oracle":
        cmd_oracle(a.root, a.arms[0])
    else:
        cmd_arms(a.root, a.arms[0], a.arms[1])

main()
