#!/usr/bin/env python3
"""Generate D#435/D#445/D#446/D#447 inputs from a corpus index (read-only).

usage: gen.py <corpus_dir> <corpus_name>  > inputs.jsonl
Seed 435. Each line: {"id","class","sym","file"(opt),"tags"}.
"""
import json, random, sqlite3, sys

PASCAL_TEST_EXTS = ["cs", "vb", "fs", "java", "kt", "scala", "swift", "php"]
SPEC_TEST_EXTS = ["scala", "kt"]
STEMS = [("Test", PASCAL_TEST_EXTS), ("Tests", PASCAL_TEST_EXTS), ("Spec", SPEC_TEST_EXTS)]
INFIX = ["go", "rs", "py", "dart"]

def is_test_path(p):  # mirrors src/domain.rs is_test_path @ 29d3b812
    lo = p.lower()
    if lo.startswith("tests/") or lo.startswith("test/") or "/tests/" in lo or "/test/" in lo:
        return True
    if any(p.endswith(f"{s}.{e}") for s, es in STEMS for e in es):
        return True
    if any(p.endswith(f"_test.{e}") for e in INFIX):
        return True
    if p.endswith(".py") and (p.startswith("test_") or "/test_" in p or p.endswith("conftest.py")):
        return True
    return (p.startswith("benches/") or p.startswith("bench/") or "__tests__/" in p
            or p.endswith("/tests.rs")
            or any(p.endswith(s) for s in [".test.ts", ".test.js", ".test.tsx", ".test.jsx",
                                           ".spec.ts", ".spec.js", ".spec.tsx", ".spec.jsx"]))

def is_test_symbol(name, p):  # mirrors src/domain.rs is_test_symbol
    return (name.startswith("test_") or name.endswith("Test") or name.endswith("Tests")
            or is_test_path(p))

HEAD = ("h1", "h2", "h3", "h4", "h5", "h6")

def gen_md(rnd, rows, by_name, names, bnames, emit):
    """md corpus: X controls, HD (dotted heading names) and HDF (each with a file)."""
    def mutate(w):
        i = rnd.randrange(len(w))
        k = rnd.choice("sdit")
        abc = "abcdefghijklmnopqrstuvwxyz"
        if k == "s":
            return w[:i] + rnd.choice(abc) + w[i + 1:]
        if k == "d" and len(w) > 2:
            return w[:i] + w[i + 1:]
        if k == "t" and i < len(w) - 1:
            return w[:i] + w[i + 1] + w[i] + w[i + 2:]
        return w[:i] + rnd.choice(abc) + w[i:]
    pool = [n for n in bnames if len(n) >= 4 and not n.startswith("-")]
    seen = set()
    k = 0
    while k < 200:
        src = rnd.choice(pool)
        w = mutate(src)
        if w in names or w in seen or not w.strip() or w.startswith("-"):
            continue
        seen.add(w); k += 1
        emit("X", w, kind="typo", src=src)
    k = 0
    long = [n for n in pool if len(n) >= 6]
    while k < 200:
        w = rnd.choice(long)
        cut = w[:max(3, int(len(w) * rnd.uniform(0.5, 0.85)))]
        if cut in names or cut in seen or not cut.strip():
            continue
        seen.add(cut); k += 1
        emit("X", cut, kind="prefix", src=w)
    heads = [r for r in rows if r[3] in HEAD and "." in r[1]]
    by_hname = {}
    for r in heads:
        by_hname.setdefault(r[1], []).append(r)
    for nm in sorted(by_hname):
        rs = by_hname[nm]
        emit("HD", nm, n_nodes=len(rs), node_ids=[r[0] for r in rs],
             files=sorted({r[4] for r in rs}), n_all_named=len(by_name.get(nm, [])))
    for nm in sorted(by_hname):
        per_file = {}
        for r in by_hname[nm]:
            per_file.setdefault(r[4], []).append(r[0])
        for fp in sorted(per_file):
            emit("HDF", nm, file=fp, node_ids=per_file[fp], n_in_file=len(per_file[fp]))


def gen_fx2(rnd, rows, by_name, names, bnames, emit):
    """Reviewer fixture: X controls (bounded; the corpus has 10 nodes) and a hand list."""
    abc = "abcdefghijklmnopqrstuvwxyz"
    def mutate(w):
        i = rnd.randrange(len(w)); k = rnd.choice("sdit")
        if k == "s":
            return w[:i] + rnd.choice(abc) + w[i + 1:]
        if k == "d" and len(w) > 2:
            return w[:i] + w[i + 1:]
        if k == "t" and i < len(w) - 1:
            return w[:i] + w[i + 1] + w[i] + w[i + 2:]
        return w[:i] + rnd.choice(abc) + w[i:]
    pool = [n for n in bnames if len(n) >= 4]
    seen = set()
    for _ in range(5000):
        if len(seen) >= 60:
            break
        src = rnd.choice(pool); w = mutate(src)
        if w in names or w in seen or not w.strip():
            continue
        seen.add(w)
        emit("X", w, kind="typo", src=src)
    for w in sorted(pool):
        for n in range(3, len(w)):
            cut = w[:n]
            if cut in names or cut in seen:
                continue
            seen.add(cut)
            emit("X", cut, kind="prefix", src=w)
    for sym, fp in (("Widget.launch", None), ("Base.launch", None),
                    ("Widget.launch", "README.md"), ("Widget.launch", "pkg/widget.py"),
                    ("Base.launch", "pkg/widget.py"), ("Base.launch", "README.md")):
        if fp:
            emit("FH", sym, file=fp)
        else:
            emit("FH", sym)


def main():
    corpus, cname = sys.argv[1], sys.argv[2]
    rnd = random.Random(435)
    db = sqlite3.connect(f"file:{corpus}/.code-graph/index.db?mode=ro", uri=True)
    rows = db.execute(
        "SELECT n.id, n.name, n.qualified_name, n.type, f.path, n.is_test FROM nodes n "
        "JOIN files f ON f.id = n.file_id WHERE n.type != 'module' AND f.path <> '<external>' "
        "ORDER BY n.id").fetchall()
    by_name, by_q = {}, {}
    for r in rows:
        by_name.setdefault(r[1], []).append(r)
        if r[2] and r[3] not in HEAD:
            by_q.setdefault(r[2], []).append(r)
    names = set(by_name)
    quals = set(by_q)
    out = []
    def emit(cls, sym, file=None, **tags):
        rec = {"id": f"{cname}:{cls}:{len(out)}", "class": cls, "sym": sym, "tags": tags}
        if file is not None:
            rec["file"] = file
        out.append(rec)

    def name_tags(nm):
        rs = by_name.get(nm, [])
        nontest = [r for r in rs if not is_test_symbol(r[1], r[4])]
        return dict(n_exact=len(rs), n_nontest=len(nontest),
                    all_test=bool(rs) and not nontest,
                    n_nontest_flagpath=len([r for r in rs if not (r[5] or is_test_path(r[4]))]))

    def q_tags(q):
        rs = by_q.get(q, [])
        prod = [r for r in rs if not (r[5] or is_test_path(r[4]))]
        return dict(n_q=len(rs), n_q_prod=len(prod),
                    q_test_multi=(not prod and len(rs) >= 2))

    bnames = sorted(n for n in names if not n.startswith("-"))
    for nm in bnames:
        emit("B", nm, **name_tags(nm))
    if cname == "fx2":
        gen_fx2(rnd, rows, by_name, names, bnames, emit)
        for rec in out:
            sys.stdout.write(json.dumps(rec, ensure_ascii=False, sort_keys=True) + "\n")
        counts = {}
        for rec in out:
            counts[rec["class"]] = counts.get(rec["class"], 0) + 1
        sys.stderr.write(f"{cname}: {len(out)} inputs {counts}\n")
        return
    if cname == "md":
        gen_md(rnd, rows, by_name, names, bnames, emit)
        for rec in out:
            sys.stdout.write(json.dumps(rec, ensure_ascii=False, sort_keys=True) + "\n")
        counts = {}
        for rec in out:
            counts[rec["class"]] = counts.get(rec["class"], 0) + 1
        sys.stderr.write(f"{cname}: {len(out)} inputs {counts}\n")
        return
    qlist = sorted(q for q in quals if "." in q and any(r[1] != q for r in by_q[q]) and not q.startswith("-"))
    for q in qlist:
        emit("Q", q, **q_tags(q))
    for nm in bnames:
        emit("BF", nm, file=by_name[nm][0][4], **name_tags(nm))
    for q in qlist:
        emit("QF", q, file=by_q[q][0][4], **q_tags(q))

    # D1: Class.member with no node qualified-named so.
    classes = sorted({r[1] for r in rows if r[3] in ("class", "struct") and "." not in r[1]})
    methods = sorted({r[1] for r in rows if r[3] == "method" and "." not in r[1]})
    cls_file = {}
    for r in rows:
        if r[3] in ("class", "struct"):
            cls_file.setdefault(r[1], r[4])
    d1 = []
    seen = set()
    while len(d1) < 400:
        c, m = rnd.choice(classes), rnd.choice(methods)
        s = f"{c}.{m}"
        if s in quals or s in names or s in seen:
            continue
        seen.add(s)
        d1.append((c, m, s))
    for c, m, s in d1:
        emit("D1", s, bare=m, **name_tags(m))

    # D2: member typos of real qualified names.
    def mutate(w):
        i = rnd.randrange(len(w))
        k = rnd.choice("sdit")
        abc = "abcdefghijklmnopqrstuvwxyz"
        if k == "s":
            return w[:i] + rnd.choice(abc) + w[i + 1:]
        if k == "d" and len(w) > 2:
            return w[:i] + w[i + 1:]
        if k == "t" and i < len(w) - 1:
            return w[:i] + w[i + 1] + w[i] + w[i + 2:]
        return w[:i] + rnd.choice(abc) + w[i:]
    qbase = [q for q in qlist if "." in q and len(q.rsplit(".", 1)[1]) >= 3]
    d2, seen = [], set()
    tries = 0
    while len(d2) < 200 and tries < 100000:
        tries += 1
        q = rnd.choice(qbase)
        head, mem = q.rsplit(".", 1)
        m2 = mutate(mem)
        s = f"{head}.{m2}"
        if m2 == mem or s in quals or s in names or m2 in names or s in seen:
            continue
        seen.add(s)
        d2.append((s, q, m2))
    for s, q, m2 in d2:
        emit("D2", s, from_q=q, bare=m2)

    # D3: hand-picked shapes.
    if cname == "flask":
        hand = ["app.py", "src/flask/app.py", "flask/app.py", "lib.py", "Flask.", "Flask..route",
                ".route", "..route", "flask.app.Flask.route", "flask.Flask", "flask.app.Flask",
                "Flask::route", "::Flask::route", "Flask::", "Scaffold.route", "Flask.route",
                "Blueprint.get_load_dotenv", "a.b.c", "Flask.route.x", "flask.helpers.url_for",
                "App.route", "Flask.test_client", "FlaskClient.open", "Flask.after_request"]
    else:
        hand = ["db/db_impl.cc", "db_impl.cc", "db/db_impl.h", "util/coding.cc", "DBImpl.",
                "DBImpl..Write", ".Write", "..Write", "leveldb.DBImpl.Write", "leveldb::DBImpl::Write",
                "DBImpl::Write", "DBImpl::Writer", "DBImpl.Writer", "leveldb.DB", "DB.Open",
                "a.b.c", "DBImpl.Write.x", "Block.size", "Version.Get", "VersionSet.LogAndApply",
                "TableBuilder.Add", "SkipList.Insert", "DBTest.Empty", "DBTest.Nope"]
    for s in hand:
        emit("D3", s, shape="hand")
    dotted_named = [r for r in rows if "." in r[1]]
    heads = [r for r in dotted_named if r[3] in HEAD]
    gtest = [r for r in dotted_named if r[3] not in HEAD]
    for r in heads[:20]:
        emit("D3", r[1], shape="heading_name", node_type=r[3])
    for r in rnd.sample(gtest, min(20, len(gtest))):
        emit("D3", r[1], shape="dotted_name_node", node_type=r[3])
        # The same gtest suite with a member that does not exist.
        suite = r[1].split(".")[0]
        emit("D3", f"{suite}.NoSuchCase{len(out)}", shape="gtest_suite_missing_case")
    pads = [(" ", ""), ("", " "), ("\t", "\n"), ("", " ")]
    for c, m, s in rnd.sample(d1, 20):
        for pre, post in pads:
            emit("D3", pre + s + post, shape="pad", base=s)

    # DF: D1 with the class's own file.
    for c, m, s in rnd.sample(d1, 100):
        emit("DF", s, file=cls_file[c], bare=m)

    # X: bare typos and prefixes.
    pool = [n for n in bnames if n.isidentifier() and len(n) >= 4]
    seen = set()
    k = 0
    while k < 200:
        w = mutate(rnd.choice(pool))
        if w in names or w in seen or not w:
            continue
        seen.add(w); k += 1
        emit("X", w, kind="typo")
    k = 0
    while k < 200:
        w = rnd.choice([n for n in pool if len(n) >= 6])
        cut = w[:max(3, int(len(w) * rnd.uniform(0.5, 0.85)))]
        if cut in names or cut in seen:
            continue
        seen.add(cut); k += 1
        emit("X", cut, kind="prefix")

    # DC: every node whose qualified_name holds `::` (C++ nested types and their
    # members), typed all-dots, all-`::`, and as stored. Appended last so every
    # earlier class keeps its ids.
    dc_seen = set()
    for r in rows:
        q = r[2]
        if not q or "::" not in q or r[3] in HEAD:
            continue
        forms = [("dots", q.replace("::", ".")), ("colons", q.replace(".", "::")), ("stored", q)]
        for form, s in forms:
            if s in dc_seen:
                continue
            dc_seen.add(s)
            emit("DC", s, form=form, stored_q=q, node_id=r[0], node_name=r[1], node_type=r[3],
                 node_file=r[4], n_q=len(by_q.get(q, [])),
                 q_node_ids=[x[0] for x in by_q.get(q, [])])

    # E: every distinct `<external>` node name (import sentinels), dotted or not.
    ext = db.execute(
        "SELECT n.id, n.name, n.type FROM nodes n JOIN files f ON f.id = n.file_id "
        "WHERE f.path = '<external>' ORDER BY n.id").fetchall()
    ext_by = {}
    for r in ext:
        ext_by.setdefault(r[1], []).append(r)
    for nm in sorted(ext_by):
        emit("E", nm, dotted="." in nm, ext_ids=[r[0] for r in ext_by[nm]],
             ext_types=sorted({r[2] for r in ext_by[nm]}),
             n_project=len(by_name.get(nm, [])), n_project_q=len(by_q.get(nm, [])))
    # DCW: DC inputs with ", " written without the space and with two spaces.
    for rec in [r for r in out if r["class"] == "DC" and ", " in r["sym"]]:
        for variant, rep in (("nospace", ","), ("twospace", ",  ")):
            emit("DCW", rec["sym"].replace(", ", rep), variant=variant, dc_sym=rec["sym"], **rec["tags"])
    # EF: every dotted `<external>` name with a file that imports it and one that does not.
    files = [r[0] for r in db.execute("SELECT path FROM files WHERE path <> '<external>' ORDER BY path")]
    for nm in sorted(n for n in ext_by if "." in n):
        ids = [r[0] for r in ext_by[nm]]
        importers = sorted({r[0] for r in db.execute(
            "SELECT DISTINCT f.path FROM edges e JOIN nodes s ON s.id = e.source_id "
            "JOIN files f ON f.id = s.file_id WHERE e.target_id IN (%s)" % ",".join("?" * len(ids)), ids)})
        if importers:
            emit("EF", nm, file=importers[0], imports=True, ext_ids=ids)
        other = next((f for f in files if f not in importers and f.endswith(".py")), None)
        if other:
            emit("EF", nm, file=other, imports=False, ext_ids=ids)

    for rec in out:
        sys.stdout.write(json.dumps(rec, ensure_ascii=False, sort_keys=True) + "\n")
    counts = {}
    for rec in out:
        counts[rec["class"]] = counts.get(rec["class"], 0) + 1
    sys.stderr.write(f"{cname}: {len(out)} inputs {counts}\n")

main()
