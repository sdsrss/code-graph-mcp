#!/usr/bin/env python3
"""parity905.py <binary> <work>: D#905 shapes, each a sequence of trees; after
every step the incremental index must equal a fresh index of the same tree
(every edge: relation, both ends, confidence, metadata). Prints per case the
python edges of the last state so a reader sees the rule acted."""
import os, shutil, sqlite3, subprocess, sys

ENV = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_DISABLE_MODEL_DOWNLOAD="1")
DUMP = """SELECT e.relation||' '||e.confidence||' '||sf.path||':'||COALESCE(sn.qualified_name,sn.name)||' -> '||
 tf.path||':'||COALESCE(tn.qualified_name,tn.name)||' '||COALESCE(e.metadata,'')
 FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
 JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id WHERE e.relation IN ('calls','references')"""

CASES = {
    "builtin, namesake added then star import": [
        {"a.py": "def f(xs):\n    return map(str, xs), set(xs)\n", "lib.py": "def g():\n    pass\n"},
        {"lib.py": "def g():\n    pass\n\n\ndef str(x):\n    return x\n\n\ndef set(x):\n    return x\n"},
        {"a.py": "from lib import *\n\n\ndef f(xs):\n    return map(str, xs), set(xs)\n"},
        {"a.py": "def f(xs):\n    return map(str, xs), set(xs)\n"},
    ],
    "comprehension variable, namesake added and removed": [
        {"a.py": "def f(xs):\n    return sum(len(m) for m in xs)\n"},
        {"b.py": "class C:\n    def m(self):\n        pass\n\n\ndef m():\n    pass\n"},
        {"b.py": None},
    ],
    "module variable, then made a def": [
        {"a.py": "m = 1\n\n\ndef f():\n    return use(m)\n", "b.py": "def m():\n    pass\n"},
        {"a.py": "def f():\n    return use(m)\n"},
        {"a.py": "m = 1\n\n\ndef f():\n    return use(m)\n"},
    ],
    "alias and global": [
        {"a.py": "import b\nm = b.m\n\n\ndef f():\n    return use(m)\n", "b.py": "def m():\n    pass\n"},
        {"c.py": "def m():\n    pass\n"},
        {"a.py": "def g():\n    global m\n    m = 1\n\n\ndef f():\n    return use(m)\n"},
    ],
}


def index(binary, d):
    r = subprocess.run([binary, "incremental-index", "--no-embed", "--quiet"], cwd=d, env=ENV, capture_output=True, text=True)
    if r.returncode:
        raise SystemExit(f"{d}: {r.stderr[-500:]}")
    con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
    out = sorted(x for (x,) in con.execute(DUMP))
    con.close()
    return out


def apply(d, step):
    for p, body in step.items():
        f = os.path.join(d, p)
        if body is None:
            os.remove(f)
        else:
            with open(f, "w") as fh:
                fh.write(body)


def main():
    binary, work = os.path.abspath(sys.argv[1]), os.path.abspath(sys.argv[2])
    bad = 0
    for name, steps in CASES.items():
        inc = os.path.join(work, "inc")
        shutil.rmtree(inc, ignore_errors=True)
        os.makedirs(inc)
        subprocess.run("git init -q", shell=True, cwd=inc, check=True)
        for i, step in enumerate(steps):
            apply(inc, step)
            got = index(binary, inc)
            fresh = os.path.join(work, "fresh")
            shutil.rmtree(fresh, ignore_errors=True)
            shutil.copytree(inc, fresh, ignore=shutil.ignore_patterns(".code-graph"))
            want = index(binary, fresh)
            if got != want:
                bad += 1
                print(f"DIVERGE {name} step {i}: inc-only {sorted(set(got) - set(want))} fresh-only {sorted(set(want) - set(got))}")
        print(f"{name}: {len(steps)} steps; last state: {got}")
    print("diverging steps:", bad)


if __name__ == "__main__":
    main()
