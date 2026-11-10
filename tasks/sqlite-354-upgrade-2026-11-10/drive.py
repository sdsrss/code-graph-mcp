"""SQLite 3.51.1 -> 3.54.1 differential (D#753): index each corpus with both
arms, compare the index tables row by row, then run the same CLI queries on
both and compare stdout, stderr and exit code byte for byte (arm root
normalised).

    python3 drive.py index            # copy + index every corpus with both arms
    python3 drive.py tables           # compare index tables
    python3 drive.py queries          # run + compare CLI queries
"""
import concurrent.futures as cf
import json
import os
import random
import shutil
import sqlite3
import subprocess
import sys

W = "/var/tmp/agent-tmp-sqlite-diff"
ARMS = ["base", "head"]
CORPORA = sorted(os.listdir(f"{W}/corpora"))
ENV = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE="1", NO_COLOR="1")
VOLATILE = {"files": {"indexed_at"}}
TABLES = ["files", "nodes", "edges", "meta", "pending_unresolved_calls", "cpp_fields", "js_export_entries"]


def root(arm, c):
    return f"{W}/run-{arm}/{c}"


def run(arm, c, args, timeout=300):
    p = subprocess.run([f"{W}/bin/{arm}", *args], cwd=root(arm, c), env=ENV,
                       capture_output=True, text=True, timeout=timeout)
    norm = lambda s: s.replace(root(arm, c), "<ROOT>").replace(f"{W}/run-{arm}", "<RUN>")
    return p.returncode, norm(p.stdout), norm(p.stderr)


def index():
    def one(arm, c):
        dst = root(arm, c)
        if os.path.exists(dst):
            shutil.rmtree(dst)
        shutil.copytree(f"{W}/corpora/{c}", dst, symlinks=True)
        rc, out, err = run(arm, c, ["incremental-index"], timeout=1800)
        return arm, c, rc, err.strip().splitlines()[-1:] if err.strip() else out.strip().splitlines()[-1:]
    with cf.ThreadPoolExecutor(4) as ex:
        for r in ex.map(lambda ac: one(*ac), [(a, c) for c in CORPORA for a in ARMS]):
            print(*r, flush=True)


def dump(arm, c):
    db = sqlite3.connect(f"file:{root(arm, c)}/.code-graph/index.db?mode=ro", uri=True)
    out = {}
    for t in TABLES:
        cols = [r[1] for r in db.execute(f"PRAGMA table_info({t})")]
        if not cols:
            continue
        keep = [x for x in cols if x not in VOLATILE.get(t, set())]
        sel = ", ".join(keep)
        rows = db.execute(f"SELECT {sel} FROM {t} ORDER BY {sel}").fetchall()
        out[t] = (keep, rows)
    return out


def tables():
    total_diff = 0
    for c in CORPORA:
        a, b = dump("base", c), dump("head", c)
        for t in TABLES:
            if t not in a and t not in b:
                continue
            ra, rb = a.get(t, ([], []))[1], b.get(t, ([], []))[1]
            same = ra == rb
            total_diff += not same
            extra = ""
            if not same:
                sa, sb = set(ra), set(rb)
                extra = f" only-base {len(sa - sb)} only-head {len(sb - sa)}"
            print(f"{c:8} {t:26} rows {len(ra):7} vs {len(rb):7} {'SAME' if same else 'DIFF'}{extra}")
    print("tables differing:", total_diff)


def inputs(c):
    db = sqlite3.connect(f"file:{root('head', c)}/.code-graph/index.db?mode=ro", uri=True)
    top = [r[0] for r in db.execute(
        "SELECT n.name FROM edges e JOIN nodes n ON n.id = e.target_id "
        "WHERE e.relation = 'calls' AND n.type IN ('function','method') "
        "GROUP BY n.name ORDER BY COUNT(*) DESC, n.name LIMIT 25")]
    every = [r[0] for r in db.execute(
        "SELECT DISTINCT name FROM nodes WHERE type IN ('function','method','class','struct') ORDER BY name")]
    rnd = random.Random(753)
    picks = top + rnd.sample(every, min(25, len(every)))
    quals = [r[0] for r in db.execute(
        "SELECT DISTINCT qualified_name FROM nodes WHERE type = 'method' AND qualified_name LIKE '%.%' "
        "ORDER BY qualified_name")]
    picks += rnd.sample(quals, min(8, len(quals)))
    files = [r[0] for r in db.execute("SELECT path FROM files ORDER BY path")]
    dirs = sorted({p.split("/")[0] for p in files if "/" in p})[:6]
    words = rnd.sample(every, min(10, len(every)))
    cmds = []
    for n in dict.fromkeys(picks):
        for sub in ("callgraph", "refs", "impact", "show"):
            cmds.append([sub, n, "--json"])
        cmds.append(["callgraph", n])
        cmds.append(["refs", n])
    for w in words:
        cmds.append(["search", w, "--json"])
        cmds.append(["search", w])
    for d in dirs:
        cmds.append(["overview", d, "--json"])
    for f in rnd.sample(files, min(6, len(files))):
        cmds.append(["deps", f, "--json"])
    for sub in (["map", "--json"], ["map"], ["dead-code", "--json"], ["centrality", "--json"],
                ["cycles", "--json"], ["surprising", "--json"], ["tour", "--json"], ["health-check", "--json"]):
        cmds.append(sub)
    return cmds


def queries():
    report = {}
    os.makedirs(f"{W}/qdiff", exist_ok=True)
    for c in CORPORA:
        cmds = inputs(c)
        def both(cmd):
            return cmd, run("base", c, cmd, timeout=120), run("head", c, cmd, timeout=120)
        diffs = []
        with cf.ThreadPoolExecutor(6) as ex:
            for cmd, a, b in ex.map(both, cmds):
                if a != b:
                    diffs.append({"cmd": cmd, "base": a, "head": b})
        report[c] = (len(cmds), len(diffs))
        json.dump(diffs, open(f"{W}/qdiff/{c}.json", "w"), indent=1)
        print(f"{c:8} commands {len(cmds):4} differing {len(diffs)}"
              + (f"  e.g. {[d['cmd'] for d in diffs[:4]]}" if diffs else ""), flush=True)
    print("total", sum(v[0] for v in report.values()), "differing", sum(v[1] for v in report.values()))


if __name__ == "__main__":
    {"index": index, "tables": tables, "queries": queries}[sys.argv[1]]()
