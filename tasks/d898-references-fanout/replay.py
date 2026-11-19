#!/usr/bin/env python3
"""Replay one history jump incrementally and compare with a fresh index (D#898).

usage: replay.py <binary> <git-repo> <old-ref> <new-ref> <work-dir> [--keep]

Indexes <old-ref>'s tree, overwrites it with <new-ref>'s tree (files missing
from the new tree are deleted), runs one `incremental-index`, then indexes a
fresh copy of <new-ref>'s tree. Prints the incremental run's wall time and the
edge differences grouped by relation and confidence, and writes them to
<work-dir>/diff.json together with the index the run started from
(<work-dir>/old.db), so `classify.py` can tell why each edge is missing.
"""
import json, os, shutil, sqlite3, subprocess, sys, time
from collections import Counter

ENV = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_DISABLE_MODEL_DOWNLOAD="1")
DUMP = """SELECT e.relation, sf.path||':'||COALESCE(sn.qualified_name, sn.name)||':'||sn.type,
 tf.path||':'||COALESCE(tn.qualified_name, tn.name)||':'||tn.type, e.confidence, COALESCE(e.metadata,'')
 FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
 JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id"""


def sh(cmd, **kw):
    return subprocess.run(cmd, check=True, **kw)


def extract(repo, ref, dest):
    os.makedirs(dest, exist_ok=True)
    arch = subprocess.run(["git", "-C", repo, "archive", ref], check=True, stdout=subprocess.PIPE).stdout
    sh(["tar", "-x", "-C", dest], input=arch)


def index(binary, d, log=False):
    """Wall seconds; with `log`, also the fan-out round's info lines."""
    env = dict(ENV, RUST_LOG="info") if log else ENV
    args = [binary, "incremental-index", "--no-embed"] + ([] if log else ["--quiet"])
    t = time.monotonic()
    r = subprocess.run(args, cwd=d, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    if r.returncode != 0:
        raise SystemExit(f"index failed in {d}: {r.stderr[-800:]}")
    secs = time.monotonic() - t
    if log:
        for line in r.stderr.splitlines():
            if "fan-out round" in line:
                print("  ", line.split("[index] ", 1)[-1][:160])
    return secs


def dump(d):
    con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
    rows = [" | ".join(map(str, r)) for r in con.execute(DUMP)]
    con.close()
    return Counter(rows)


def main():
    binary, repo, old, new, work = sys.argv[1:6]
    binary, repo, work = map(os.path.abspath, (binary, repo, work))
    if os.path.exists(work):
        raise SystemExit(f"{work} exists")
    inc, fresh, nxt = (os.path.join(work, x) for x in ("inc", "fresh", "next"))
    extract(repo, old, inc)
    sh(["git", "init", "-q"], cwd=inc)
    index(binary, inc)
    shutil.copy(os.path.join(inc, ".code-graph", "index.db"), os.path.join(work, "old.db"))
    # Overwrite with the new tree: delete what it lacks, copy the rest over.
    extract(repo, new, nxt)
    sh(["rsync", "-a", "--delete", "--exclude", "/.git/", "--exclude", "/.code-graph/", nxt + "/", inc + "/"])
    shutil.rmtree(nxt)
    secs = index(binary, inc, log=True)
    extract(repo, new, fresh)
    sh(["git", "init", "-q"], cwd=fresh)
    index(binary, fresh)
    a, b = dump(inc), dump(fresh)
    only_inc = sorted((a - b).elements())
    only_fresh = sorted((b - a).elements())
    shutil.copy(os.path.join(fresh, ".code-graph", "index.db"), os.path.join(work, "fresh.db"))
    shutil.copy(os.path.join(inc, ".code-graph", "index.db"), os.path.join(work, "inc.db"))
    with open(os.path.join(work, "diff.json"), "w") as fh:
        json.dump({"inc_only": only_inc, "fresh_only": only_fresh, "secs": secs}, fh)
    print(f"incremental {secs:.2f}s; edges inc {sum(a.values())} fresh {sum(b.values())}; "
          f"inc-only {len(only_inc)} fresh-only {len(only_fresh)}")
    for side, rows in (("inc-only", only_inc), ("fresh-only", only_fresh)):
        c = Counter((r.split(" | ")[0], r.split(" | ")[3]) for r in rows)
        for (rel, conf), n in sorted(c.items()):
            print(f"  {side} {rel} {conf}: {n}")
    if "--keep" not in sys.argv:
        shutil.rmtree(inc)
        shutil.rmtree(fresh)


if __name__ == "__main__":
    main()
