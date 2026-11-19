#!/usr/bin/env python3
"""abdiff.py <base-bin> <head-bin> <corpora-dir> <work-dir> [names...]

Indexes each corpus from scratch with both binaries (a copy each) and reports:
  - every edge (relation, endpoints, confidence, metadata) only one side has;
  - for each base-only Python calls/references edge, the oracle's verdict
    (reforacle.classify on the base copy, which holds the same sources);
  - Python definitions (function/method/class) with no incoming edge, per side,
    and samples of those only head leaves edge-less;
  - wall time and index size.
"""
import os, shutil, sqlite3, subprocess, sys, time
from collections import Counter, defaultdict

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from reforacle import Corpus, classify  # noqa: E402

ENV = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_DISABLE_MODEL_DOWNLOAD="1")
ROWS = """SELECT e.relation, sf.path, sn.type, sn.name, COALESCE(sn.qualified_name, sn.name), sn.start_line, sn.end_line,
 tf.path, tn.type, tn.name, COALESCE(tn.qualified_name, tn.name), tn.start_line, tn.end_line,
 e.confidence, COALESCE(e.metadata,''), sf.language
 FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
 JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id"""
ORPHANS = """SELECT f.path, n.type, COALESCE(n.qualified_name, n.name), n.start_line FROM nodes n JOIN files f ON f.id=n.file_id
 WHERE f.language='python' AND n.type IN ('function','method','class')
 AND NOT EXISTS (SELECT 1 FROM edges e WHERE e.target_id=n.id)"""


def key(r):
    rel, sp, st, sn, sq, s0, s1, tp, tt, tn, tq, t0, t1, conf, meta, lang = r
    # Start lines too: same-named nested definitions of one file share path,
    # name and type, and without the line a removed edge to one of them was
    # counted against the row of another (D#909: the oracle then judged the
    # kept, right edge).
    return (rel, f"{sp}:{sq}:{st}@{s0}", f"{tp}:{tq}:{tt}@{t0}", conf, meta)


def run(binary, src, dest):
    if os.path.exists(dest):
        shutil.rmtree(dest)
    shutil.copytree(src, dest, symlinks=True)
    t = time.monotonic()
    r = subprocess.run([binary, "incremental-index", "--no-embed", "--quiet"], cwd=dest, env=ENV,
                       stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    secs = time.monotonic() - t
    if r.returncode != 0:
        raise SystemExit(f"index failed in {dest}: {r.stderr[-800:]}")
    db = os.path.join(dest, ".code-graph", "index.db")
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    rows = list(con.execute(ROWS))
    orphans = set(con.execute(ORPHANS))
    con.close()
    size = sum(os.path.getsize(os.path.join(dest, ".code-graph", f))
               for f in os.listdir(os.path.join(dest, ".code-graph")) if f.startswith("index.db"))
    return rows, orphans, size, secs


def main():
    base, head, corp, work = map(os.path.abspath, sys.argv[1:5])
    names = sys.argv[5:] or ["std", "nltk", "flask", "cgm", "daagu"]
    os.makedirs(work, exist_ok=True)
    for n in names:
        rb, ob, sb, tb = run(base, os.path.join(corp, n), os.path.join(work, f"{n}-base"))
        rh, oh, sh, th = run(head, os.path.join(corp, n), os.path.join(work, f"{n}-head"))
        kb = Counter(key(r) for r in rb)
        kh = Counter(key(r) for r in rh)
        base_only = kb - kh
        head_only = kh - kb
        print(f"== {n}: edges {sum(kb.values())} -> {sum(kh.values())}; base-only {sum(base_only.values())} "
              f"head-only {sum(head_only.values())}; python orphans {len(ob)} -> {len(oh)} "
              f"(new {len(oh - ob)}, revived {len(ob - oh)}); index MB {sb / 1e6:.1f} -> {sh / 1e6:.1f}; "
              f"secs {tb:.2f} -> {th:.2f}", flush=True)
        by_rel = Counter((k[0], k[3]) for k in base_only.elements())
        print("   base-only by relation/conf:", dict(sorted(by_rel.items())))
        for k in sorted(head_only.elements())[:10]:
            print("   HEAD-ONLY", k)
        # oracle verdicts for base-only python calls/references edges
        files = sorted({r[1] for r in rb if r[15] == "python"} | {r[7] for r in rb})
        corpus = Corpus(os.path.join(work, f"{n}-base"), [f for f in files if f.endswith(".py")])
        verdicts = Counter()
        samples = defaultdict(list)
        left = Counter(base_only)
        for r in rb:
            k = key(r)
            if left[k] <= 0:
                continue
            left[k] -= 1
            rel, sp, st, sn, sq, s0, s1, tp, tt, tn, tq, t0, t1, conf, meta, lang = r
            if lang != "python" or rel not in ("calls", "references"):
                verdicts[(rel, "other-lang-or-relation")] += 1
                if len(samples[(rel, "other-lang-or-relation")]) < 6:
                    samples[(rel, "other-lang-or-relation")].append(str(k))
                continue
            v, info = classify(corpus, sp, st, sn, s0, s1, tp, tn, tq, tt, t0, t1, rel)
            verdicts[(rel, v)] += 1
            if len(samples[(rel, v)]) < 6:
                samples[(rel, v)].append(f"{sp}:{sq} -> {tp}:{tq}({tt}) {conf} [{info}]")
        print("   oracle on base-only:", dict(sorted(verdicts.items())))
        for (rel, v), ss in sorted(samples.items(), key=str):
            if v in ("ok", "unknown", "unmatched", "parse-fail", "other-lang-or-relation"):
                for s in ss:
                    print(f"     {rel} {v}: {s}")
        newly = sorted(oh - ob)
        for o in newly[:12]:
            print("   NEW ORPHAN", o)
        shutil.rmtree(os.path.join(work, f"{n}-base"))
        shutil.rmtree(os.path.join(work, f"{n}-head"))


if __name__ == "__main__":
    main()
