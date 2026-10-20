#!/usr/bin/env python3
"""0.174.0 transitions: classify every run that differs between two arms.

usage: rel174.py <armA> <armB> [--root R]
Expected classes:
  ext   call-graph surface -> the new external-only message,
        input name held only as <external> rows, and the old answer (if any)
        was a different symbol
  ctor  leveldb: ambiguous_exact -> answer (a class in a header) or
        ambiguous_exact with fewer candidates
  refs  cli_refs answer -> answer, new list equal to MCP find_references'
Anything else is printed as UNEXPECTED.
"""
import argparse, collections, json, os, sqlite3, sys

HARNESS = "/var/tmp/d446/harness"
ns = {}
src = open(f"{HARNESS}/diff.py", encoding="utf-8").read().replace("\nmain()\n", "\n")
exec(compile(src, "diff.py", "exec"), ns)
load, inputs, norm_mcp, norm_cli, amb_kind = ns["load"], ns["inputs"], ns["norm_mcp"], ns["norm_cli"], ns["amb_kind"]
# 053b50ba reworded the message; either text marks it.
EXT_MSGS = ("is defined outside this project", "has no definition in the index")


def verdict(surface, rec, sym):
    s = surface[4:]
    v = norm_mcp(s, rec) if surface.startswith("mcp_") else norm_cli(s, rec)
    return v, amb_kind(v, sym)


def text(rec):
    if "stdout" in rec:
        return (rec.get("stdout") or "") + (rec.get("stderr") or "")
    r = rec.get("result")
    return r if isinstance(r, str) else json.dumps(r)


def refs_list(rec):
    """(file, line, relation) triples of a CLI refs or MCP find_references answer."""
    try:
        d = json.loads(rec["stdout"]) if "stdout" in rec else rec["result"]
    except Exception:
        return None
    if not isinstance(d, dict):
        return None
    out = []
    for r in d.get("references") or []:
        out.append((r.get("file_path") or r.get("file"), r.get("line"), r.get("relation")))
    return sorted(out, key=lambda t: tuple(str(x) for x in t)), d.get("total_references")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("a"); ap.add_argument("b"); ap.add_argument("--root", default="/var/tmp/d446")
    a = ap.parse_args()
    grand = collections.Counter()
    for corpus in ns["CORPORA"]:
        da, dbd = f"{a.root}/results/{a.a}/{corpus}", f"{a.root}/results/{a.b}/{corpus}"
        if not (os.path.isdir(da) and os.path.isdir(dbd)):
            continue
        db = sqlite3.connect(f"file:{a.root}/corpora/{corpus}/.code-graph/index.db?mode=ro", uri=True)
        rows = collections.defaultdict(set)
        for name, path in db.execute("SELECT n.name, f.path FROM nodes n JOIN files f ON f.id = n.file_id"):
            rows[name].add(path)
        inp = inputs(a.root, corpus)
        cnt = collections.Counter(); unexpected = []; ext_inputs = set(); ctor_inputs = set(); refs_inputs = set()
        before_kinds = collections.Counter(); ctor_kinds = collections.Counter(); refs_agree = collections.Counter()
        total = 0
        mcp_fr_b = None
        for fn in sorted(os.listdir(da)):
            A, B = load(f"{da}/{fn}"), load(f"{dbd}/{fn}")
            surface = fn[:-6]
            for rid in A:
                total += 1
                ra, rb = A[rid][1], B[rid][1]
                if json.dumps(ra, sort_keys=True) == json.dumps(rb, sort_keys=True):
                    continue
                sym = inp[rid]["sym"]
                va, ka = verdict(surface, ra, sym)
                vb, kb = verdict(surface, rb, sym)
                is_cg = "_cg_" in surface or "_gcg_" in surface
                bare = sym.strip().replace("::", ".").strip(".").rsplit(".", 1)[-1]
                if is_cg and any(m in text(rb) for m in EXT_MSGS):
                    asked = sym if sym in rows else bare
                    ext_only = rows.get(asked) == {"<external>"}
                    switched = ka != "answer" or (va.get("symbol") or "") not in (sym, bare)
                    if ext_only and switched:
                        cnt["ext"] += 1; ext_inputs.add(rid); before_kinds[(surface.split("_")[0], ka)] += 1
                        continue
                    unexpected.append((surface, rid, sym, ka, kb, f"ext_only={ext_only} switched={switched} before={va.get('symbol')}"))
                    continue
                if corpus == "leveldb" and ka == "ambiguous_exact" and kb in ("answer", "ambiguous_exact"):
                    cnt["ctor"] += 1; ctor_inputs.add(rid); ctor_kinds[kb] += 1
                    continue
                if surface == "cli_refs" and ka == kb == "answer":
                    if mcp_fr_b is None or mcp_fr_b[0] != corpus:
                        mcp_fr_b = (corpus, load(f"{dbd}/mcp_fr.jsonl"))
                    fr = mcp_fr_b[1].get(rid)
                    cl, fl = refs_list(rb), (refs_list(fr[1]) if fr else None)
                    if cl and fl:
                        same_list = cl[0] == fl[0]
                        same_total = cl[1] == fl[1]
                        subset = set(fl[0]) <= set(cl[0])
                        refs_agree["list equal" if same_list else ("total equal, MCP list a subset" if same_total and subset else "DIFFERENT")] += 1
                    else:
                        refs_agree["unparsed"] += 1
                    cnt["refs"] += 1; refs_inputs.add(rid)
                    continue
                unexpected.append((surface, rid, sym, ka, kb, ""))
        changed = sum(cnt.values()) + len(unexpected)
        print(f"===== {corpus}: {a.a} -> {a.b}: runs {total}, changed {changed}")
        print(f"   ext {cnt['ext']} runs / {len(ext_inputs)} inputs; before: {dict(before_kinds)}")
        print(f"   ctor {cnt['ctor']} runs / {len(ctor_inputs)} inputs; after: {dict(ctor_kinds)}")
        print(f"   refs {cnt['refs']} runs / {len(refs_inputs)} inputs; vs MCP fr: {dict(refs_agree)}")
        print(f"   UNEXPECTED {len(unexpected)}")
        for u in unexpected[:15]:
            print("     ", u)
        grand.update(cnt); grand["unexpected"] += len(unexpected); grand["runs"] += total; grand["changed"] += changed
    print("TOTAL", dict(grand))


main()
