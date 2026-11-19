#!/usr/bin/env python3
"""fulldiff.py <base-bin> <head-bin> <corpora-dir> <work-dir> [names...]

Indexes each corpus from scratch with both binaries (a copy each, so neither
sees the other's index) and compares every edge: relation, both endpoints,
confidence, metadata, duplicates kept. Also reports the pending table (calls
rows, dropped-references rows), the index file size and the wall time.
"""
import os, shutil, sqlite3, subprocess, sys, time
from collections import Counter

ENV = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_DISABLE_MODEL_DOWNLOAD="1")
DUMP = """SELECT e.relation, sf.path||':'||COALESCE(sn.qualified_name, sn.name)||':'||sn.type,
 tf.path||':'||COALESCE(tn.qualified_name, tn.name)||':'||tn.type, e.confidence, COALESCE(e.metadata,'')
 FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
 JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id"""
MARK = '{"rel":"references"}'


def run(binary, src, dest):
    shutil.copytree(src, dest, symlinks=True)
    t = time.monotonic()
    r = subprocess.run([binary, "incremental-index", "--no-embed", "--quiet"], cwd=dest, env=ENV,
                       stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    secs = time.monotonic() - t
    if r.returncode != 0:
        raise SystemExit(f"index failed in {dest}: {r.stderr[-800:]}")
    db = os.path.join(dest, ".code-graph", "index.db")
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    edges = Counter(" | ".join(map(str, r)) for r in con.execute(DUMP))
    calls, refs = con.execute("SELECT SUM(metadata IS NOT ?1), SUM(metadata IS ?1) FROM pending_unresolved_calls",
                              (MARK,)).fetchone()
    con.close()
    size = sum(os.path.getsize(os.path.join(dest, ".code-graph", f)) for f in os.listdir(os.path.join(dest, ".code-graph"))
               if f.startswith("index.db"))
    shutil.rmtree(dest)
    return edges, calls or 0, refs or 0, size, secs


def main():
    base, head, corp, work = map(os.path.abspath, sys.argv[1:5])
    names = sys.argv[5:] or ["std", "nltk", "flask", "cgm", "daagu"]
    os.makedirs(work, exist_ok=True)
    for n in names:
        eb, cb, rb, sb, tb = run(base, os.path.join(corp, n), os.path.join(work, f"{n}-base"))
        eh, ch, rh, sh, th = run(head, os.path.join(corp, n), os.path.join(work, f"{n}-head"))
        print(f"{n}: edges {sum(eb.values())} {sum(eh.values())} base-only {sum((eb - eh).values())} "
              f"head-only {sum((eh - eb).values())}; pending calls {cb} {ch}, references rows {rb} {rh}; "
              f"index MB {sb / 1e6:.1f} {sh / 1e6:.1f}; secs {tb:.2f} {th:.2f}", flush=True)
        for r in sorted((eb - eh).elements())[:5]:
            print("   base-only", r)
        for r in sorted((eh - eb).elements())[:5]:
            print("   head-only", r)


if __name__ == "__main__":
    main()
