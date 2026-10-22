#!/usr/bin/env python3
"""Incremental-vs-fresh parity harness for code-graph-mcp arms.

usage: run.py CASEFILE [case ...] [--arms head,a-rel,base174] [--keep]
CASEFILE defines CASES = {name: {"init": {path: text}, "steps": [{path: text|None}, ...]}}
After every step, the incremental index is compared with a fresh index of
the same tree. Prints differing edges per arm.
"""
import os, sys, json, shutil, sqlite3, subprocess, collections, runpy, time

BIN = "/var/tmp/d175-r2/bin"
WORK = "/var/tmp/d175-r2/work"
ENV = dict(os.environ, CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_NO_AUTO_UPDATE="1")

DUMP = """
SELECT fs.path, ns.type, COALESCE(ns.qualified_name, ns.name), ns.start_line,
       e.relation, e.confidence, COALESCE(e.metadata, ''),
       ft.path, nt.type, COALESCE(nt.qualified_name, nt.name), nt.start_line
FROM edges e
JOIN nodes ns ON ns.id = e.source_id JOIN files fs ON fs.id = ns.file_id
JOIN nodes nt ON nt.id = e.target_id JOIN files ft ON ft.id = nt.file_id
"""

def fmt(r):
    return f"{r[0]}:{r[2]}({r[1]}@{r[3]}) -{r[4]}[{r[5]}]{r[6]}-> {r[7]}:{r[9]}({r[8]}@{r[10]})"

def dump(d):
    con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
    rows = collections.Counter(con.execute(DUMP).fetchall())
    con.close()
    return rows

def index(arm, d):
    p = subprocess.run([f"{BIN}/{arm}", "incremental-index", "--no-embed", "--quiet"],
                       cwd=d, env=ENV, capture_output=True, text=True)
    if p.returncode != 0:
        raise SystemExit(f"index failed in {d}: {p.stderr[-2000:]}")

def write_tree(d, tree, bump=0):
    for p, t in tree.items():
        a = os.path.join(d, p)
        os.makedirs(os.path.dirname(a), exist_ok=True)
        with open(a, "w") as f:
            f.write(t)
        if bump:
            now = time.time() + bump
            os.utime(a, (now, now))

def fresh_dir(d):
    if os.path.exists(d):
        shutil.rmtree(d)
    os.makedirs(d)
    subprocess.run(["git", "init", "-q"], cwd=d, check=True)

def run_case(name, case, arm, verbose=True):
    base = os.path.join(WORK, name, arm)
    if os.path.exists(base):
        shutil.rmtree(base)
    inc = os.path.join(base, "inc")
    fresh_dir(inc)
    tree = dict(case["init"])
    write_tree(inc, tree)
    index(arm, inc)
    bad = []
    for i, step in enumerate(case["steps"], 1):
        for p, t in step.items():
            a = os.path.join(inc, p)
            if t is None:
                os.remove(a)
                tree.pop(p)
            else:
                write_tree(inc, {p: t}, bump=i * 3)
                tree[p] = t
        index(arm, inc)
        fr = os.path.join(base, f"fresh{i}")
        fresh_dir(fr)
        write_tree(fr, tree)
        index(arm, fr)
        a, b = dump(inc), dump(fr)
        extra = a - b
        missing = b - a
        if extra or missing:
            bad.append((i, extra, missing))
        if not case.get("keep"):
            shutil.rmtree(fr)
    if verbose:
        if not bad:
            print(f"  [{arm}] {name}: parity OK after {len(case['steps'])} step(s)")
        for i, extra, missing in bad:
            print(f"  [{arm}] {name}: step {i}: {sum(extra.values())} extra, {sum(missing.values())} missing")
            for r, n in sorted(extra.items()):
                print(f"      + {n}x {fmt(r)}")
            for r, n in sorted(missing.items()):
                print(f"      - {n}x {fmt(r)}")
    return bad

def main():
    args = sys.argv[1:]
    arms = ["head", "a-rel", "base174"]
    if "--arms" in args:
        i = args.index("--arms")
        arms = args[i + 1].split(",")
        del args[i:i + 2]
    cases = runpy.run_path(args[0])["CASES"]
    names = args[1:] or list(cases)
    for n in names:
        print(f"== {n}")
        for arm in arms:
            run_case(n, cases[n], arm)

if __name__ == "__main__":
    main()
