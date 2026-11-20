#!/usr/bin/env python3
"""parity909.py <binary> <work>: D#909 shapes, each a sequence of trees; after
every step the incremental index must equal a fresh index of the same tree
(every edge: relation, both ends, confidence, metadata). Prints per case the
python edges of the last state so a reader sees the rule acted."""
import os, shutil, sqlite3, subprocess, sys

ENV = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_DISABLE_MODEL_DOWNLOAD="1")
DUMP = """SELECT e.relation||' '||e.confidence||' '||sf.path||':'||COALESCE(sn.qualified_name,sn.name)||'@'||sn.start_line||' -> '||
 tf.path||':'||COALESCE(tn.qualified_name,tn.name)||'@'||tn.start_line||' '||COALESCE(e.metadata,'')
 FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
 JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id WHERE e.relation IN ('calls','references')"""

CASES = {
    "round 2: a default call both scopes rule out, beside another file's namesake": [
        {"a.py": "def make():\n    pass\n\n\ndef f(x=make()):\n    def make():\n        pass\n    return x\n",
         "b.py": "def make():\n    pass\n"},
        {"a.py": "def f(x=make()):\n    def make():\n        pass\n    return x\n",
         "b.py": "def make():\n    pass\n"},
        {"a.py": "def f(x=make()):\n    return x\n",
         "b.py": "def make():\n    pass\n"},
        {"a.py": "def make():\n    pass\n\n\ndef f(x=make()):\n    def make():\n        pass\n    return x\n",
         "b.py": "def other():\n    pass\n"},
        {"a.py": "def make():\n    pass\n\n\ndef f(x=make()):\n    def make():\n        pass\n    return x\n",
         "b.py": "def make():\n    pass\n"},
    ],
    "sibling nested defs: added, own removed, cross-file namesake removed": [
        {"a.py": "def a(app):\n    def index():\n        pass\n    app.add(view_func=index)\n",
         "b.py": "def index():\n    pass\n"},
        {"a.py": "def a(app):\n    def index():\n        pass\n    app.add(view_func=index)\n\n\n"
                 "def b(app):\n    def index():\n        pass\n    app.add(view_func=index)\n"},
        {"a.py": "def a(app):\n    app.add(view_func=index)\n\n\n"
                 "def b(app):\n    def index():\n        pass\n    app.add(view_func=index)\n"},
        {"b.py": None},
        {"a.py": "def a(app):\n    def index():\n        pass\n    app.add(view_func=index)\n"},
    ],
    "nested class shadows a module class that moves away": [
        {"a.py": "class Row:\n    pass\n\n\ndef a():\n    class Row:\n        pass\n    x: Row = Row()\n    return x\n\n\n"
                 "def b():\n    y: Row = Row()\n    return y\n"},
        {"a.py": "def a():\n    class Row:\n        pass\n    x: Row = Row()\n    return x\n\n\n"
                 "def b():\n    y: Row = Row()\n    return y\n",
         "lib.py": "class Row:\n    pass\n"},
        {"a.py": "from lib import Row\n\n\ndef a():\n    class Row:\n        pass\n    x: Row = Row()\n    return x\n\n\n"
                 "def b():\n    y: Row = Row()\n    return y\n"},
        {"a.py": "from lib import Row\n\n\ndef b():\n    y: Row = Row()\n    return y\n"},
    ],
    "method namesake, nested def removed, module def elsewhere": [
        {"a.py": "class C:\n    def h(self):\n        pass\n\n\ndef a():\n    def h():\n        pass\n    return h\n"},
        {"a.py": "class C:\n    def h(self):\n        pass\n\n\ndef a():\n    return h\n"},
        {"b.py": "def h():\n    pass\n"},
        {"a.py": "class C:\n    def h(self):\n        pass\n\n\ndef h():\n    pass\n\n\ndef a():\n    return h\n"},
    ],
    "a def its function declares global, then the declaration removed": [
        {"a.py": "def h():\n    pass\n\n\ndef f():\n    global h\n    def h():\n        pass\n\n\ndef g():\n    return h\n"},
        {"a.py": "def h():\n    pass\n\n\ndef f():\n    def h():\n        pass\n\n\ndef g():\n    return h\n"},
        {"a.py": "def h():\n    pass\n\n\ndef f():\n    global h\n    def h():\n        pass\n\n\ndef g():\n    return h\n"},
    ],
    "a call in a default to a class the def nests, module class removed": [
        {"a.py": "class Row:\n    pass\n\n\ndef f(x=Row()):\n    class Row:\n        pass\n    return x\n"},
        {"a.py": "def f(x=Row()):\n    class Row:\n        pass\n    return x\n", "b.py": "class Row:\n    pass\n"},
        {"b.py": None},
    ],
    "decorator a function nests, sibling added and own removed": [
        {"a.py": "def a():\n    def deco(f):\n        return f\n    @deco\n    def g():\n        pass\n    return g\n"},
        {"a.py": "def a():\n    def deco(f):\n        return f\n    @deco\n    def g():\n        pass\n    return g\n\n\n"
                 "def b():\n    def deco(f):\n        return f\n    return deco\n"},
        {"a.py": "def a():\n    @deco\n    def g():\n        pass\n    return g\n\n\n"
                 "def b():\n    def deco(f):\n        return f\n    return deco\n",
         "c.py": "def deco(f):\n    return f\n"},
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
