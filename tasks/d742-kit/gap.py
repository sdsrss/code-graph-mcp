# gap.py DIR...: in each indexed corpus, JS/TS bare calls left pending although the
# caller's file imports that name from a relative specifier (the D#741/D#742 gap).
import os, re, sqlite3, sys, collections
IMP = re.compile(r"""import\s+(?:type\s+)?(?:(\w+)\s*,?\s*)?(?:\{([^}]*)\})?\s*from\s*['"](\.{1,2}/[^'"]+)['"]""", re.S)
def local_imports(src):
    out = {}
    for m in IMP.finditer(src):
        default, named, spec = m.group(1), m.group(2), m.group(3)
        if default: out[default] = (spec, "default", default)
        for part in (named or "").split(","):
            part = part.strip()
            if not part or part.startswith("type "): continue
            if " as " in part:
                exp, loc = [x.strip() for x in part.split(" as ", 1)]
            else:
                exp = loc = part
            out[loc] = (spec, "named", exp)
    return out
for d in sys.argv[1:]:
    db = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
    rows = db.execute("""SELECT f.path, f.language, n.name, p.target_name FROM pending_unresolved_calls p
        JOIN nodes n ON n.id=p.source_id JOIN files f ON f.id=n.file_id
        WHERE p.metadata IS NULL AND f.language IN ('javascript','typescript','tsx')""").fetchall()
    cache = {}; hits = []
    for path, lang, caller, name in rows:
        if path not in cache:
            try: cache[path] = local_imports(open(os.path.join(d, path), encoding="utf-8", errors="replace").read())
            except OSError: cache[path] = {}
        if name in cache[path]:
            spec, kind, exp = cache[path][name]
            hits.append((path, caller, name, spec, kind, exp))
    kinds = collections.Counter()
    for path, caller, name, spec, kind, exp in hits:
        tgt_ext = os.path.splitext(spec)[1]
        k = "renamed-import" if exp != name else kind
        if path.endswith((".js", ".mjs", ".cjs", ".jsx")) and (tgt_ext in (".ts", ".tsx") ): k += "+js->ts"
        kinds[k] += 1
    print(f"{os.path.basename(d)}: pending bare JS/TS rows {len(rows)}, through a relative import {len(hits)} {dict(kinds)}")
    for h in hits[:8]: print("   ", h)

def classify(d):
    db = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
    files = {p: l for p, l in db.execute("SELECT path, language FROM files")}
    rows = db.execute("""SELECT f.path, f.language, n.name, p.target_name FROM pending_unresolved_calls p
        JOIN nodes n ON n.id=p.source_id JOIN files f ON f.id=n.file_id
        WHERE p.metadata IS NULL AND f.language IN ('javascript','typescript','tsx')""").fetchall()
    cache = {}; out = collections.Counter(); ex = collections.defaultdict(list)
    for path, lang, caller, name in rows:
        if path not in cache:
            try: cache[path] = local_imports(open(os.path.join(d, path), encoding="utf-8", errors="replace").read())
            except OSError: cache[path] = {}
        if name not in cache[path]: continue
        spec, kind, exp = cache[path][name]
        base = os.path.normpath(os.path.join(os.path.dirname(path), spec))
        cands = [base] + [base + e for e in (".ts", ".tsx", ".js", ".mjs", ".cjs", ".jsx", ".mts", ".cts")] + [base + "/index" + e for e in (".ts", ".tsx", ".js", ".mjs", ".jsx")]
        if base.endswith(".js"): cands += [base[:-3] + ".ts", base[:-3] + ".tsx"]
        tgt = next((c for c in cands if c in files), None)
        if tgt is None: k = "file not indexed"
        else:
            defined = db.execute("SELECT COUNT(*) FROM nodes n JOIN files f ON f.id=n.file_id WHERE f.path=? AND n.name=?", (tgt, exp)).fetchone()[0]
            tl = files[tgt]
            if kind == "default": k = f"default import ({lang}->{tl})"
            elif defined and tl == lang: k = f"defined there, same language ({lang})"
            elif defined: k = f"defined there, {lang}->{tl}"
            else: k = f"not defined there (re-export or none), {lang}->{tl}"
        out[k] += 1; ex[k].append((path, caller, name, spec, tgt))
    print(f"== {os.path.basename(d)}")
    for k, v in out.most_common():
        print(f"  {v:4d} {k}   e.g. {ex[k][0]}")
