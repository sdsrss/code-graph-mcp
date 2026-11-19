#!/usr/bin/env python3
"""seq909.py <arm> <work>: edit sequences around D#909; after every step the
incremental index must equal a fresh index (every edge with both ends' start
lines, confidence, metadata)."""
import os, shutil, sqlite3, subprocess, sys

BIN = {"base": "/var/tmp/d909-review/bin/base", "head": "/var/tmp/d909-review/bin/head"}
ENV = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_DISABLE_MODEL_DOWNLOAD="1")
DUMP = """SELECT e.relation||' '||e.confidence||' '||sf.path||':'||COALESCE(sn.qualified_name,sn.name)||'@'||sn.start_line||' -> '||
 tf.path||':'||COALESCE(tn.qualified_name,tn.name)||'@'||tn.start_line||' '||COALESCE(e.metadata,'')
 FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
 JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id"""

ROWA = "def a(x: Row) -> Row:\n    class Row:\n        pass\n    return Row()\n\n\ndef b():\n    return Row()\n"
CASES = {
    "P1 header annotation, module class appears/moves": [
        {"a.py": "from lib import Row\n\n\n" + ROWA, "lib.py": "class Row:\n    pass\n"},
        {"a.py": ROWA},
        {"a.py": "class Row:\n    pass\n\n\n" + ROWA},
        {"a.py": ROWA + "\n\nclass Row:\n    pass\n"},
        {"lib.py": None},
        {"a.py": "from lib import Row\n\n\n" + ROWA, "lib.py": "class Row:\n    pass\n"},
    ],
    "P2 global rebinding": [
        {"a.py": "def h():\n    pass\n\n\ndef f():\n    global h\n    def h():\n        pass\n\n\ndef g():\n    return h\n"},
        {"a.py": "def f():\n    global h\n    def h():\n        pass\n\n\ndef g():\n    return h\n", "b.py": "def h():\n    pass\n"},
        {"a.py": "from b import h\n\n\ndef f():\n    global h\n    def h():\n        pass\n\n\ndef g():\n    return h\n"},
        {"b.py": None},
    ],
    "P3 nested classes vs module class in another file": [
        {"a.py": "def a():\n    class Row:\n        pass\n    return Row()\n\n\ndef b():\n    return Row()\n", "lib.py": "class Row:\n    pass\n"},
        {"a.py": "from lib import Row\n\n\ndef a():\n    class Row:\n        pass\n    return Row()\n\n\ndef b():\n    return Row()\n"},
        {"a.py": "def a():\n    class Row:\n        pass\n    return Row()\n\n\ndef b():\n    class Row:\n        pass\n    return Row()\n\n\ndef c():\n    return Row()\n"},
        {"lib.py": None},
        {"lib.py": "class Row:\n    pass\n"},
    ],
    "P4 method namesake, nested def toggled, other-file def": [
        {"a.py": "class C:\n    def index(self):\n        pass\n    x = staticmethod(index)\n\n\ndef t(app):\n    def index():\n        pass\n    app.add(view_func=index)\n\n\ndef u(app):\n    app.add(view_func=index)\n"},
        {"b.py": "def index():\n    pass\n"},
        {"a.py": "class C:\n    def index(self):\n        pass\n    x = staticmethod(index)\n\n\ndef t(app):\n    app.add(view_func=index)\n\n\ndef u(app):\n    app.add(view_func=index)\n"},
        {"a.py": "from b import index\n\n\nclass C:\n    def index(self):\n        pass\n\n\ndef t(app):\n    def index():\n        pass\n    app.add(view_func=index)\n"},
        {"b.py": None},
    ],
    "P5 first-statement method header toggled": [
        {"a.py": "class Row:\n    pass\n\n\nclass C:\n    def m(self, x: Row):\n        pass\n    def Row(self):\n        pass\n"},
        {"a.py": "class Row:\n    pass\n\n\nclass C:\n    y = 1\n    def m(self, x: Row):\n        pass\n    def Row(self):\n        pass\n"},
        {"a.py": "class C:\n    def m(self, x: Row):\n        pass\n    def Row(self):\n        pass\n", "lib.py": "class Row:\n    pass\n"},
    ],
    "P6 decorator and closure": [
        {"a.py": "def deco(f):\n    return f\n\n\ndef a():\n    def deco(f):\n        return f\n    @deco\n    def g():\n        pass\n    def inner():\n        return deco\n    return g, inner\n"},
        {"a.py": "def a():\n    def deco(f):\n        return f\n    @deco\n    def g():\n        pass\n    def inner():\n        return deco\n    return g, inner\n", "d.py": "def deco(f):\n    return f\n"},
        {"a.py": "from d import deco\n\n\ndef a():\n    @deco\n    def g():\n        pass\n    def inner():\n        return deco\n    return g, inner\n\n\ndef b():\n    def deco(f):\n        return f\n    return deco\n"},
    ],
    "P7 class body in function, nested def and method": [
        {"a.py": "def helper():\n    pass\n\n\ndef outer():\n    def helper():\n        pass\n    class C:\n        x = staticmethod(helper)\n        def helper(self):\n            pass\n        def m(self):\n            return helper\n    return C\n"},
        {"a.py": "def outer():\n    def helper():\n        pass\n    class C:\n        x = staticmethod(helper)\n        def helper(self):\n            pass\n        def m(self):\n            return helper\n    return C\n", "h.py": "def helper():\n    pass\n"},
        {"a.py": "from h import helper\n\n\ndef outer():\n    class C:\n        x = staticmethod(helper)\n        def helper(self):\n            pass\n        def m(self):\n            return helper\n    return C\n"},
    ],
}


def index(binary, d):
    assert d.startswith("/var/tmp/d909-review/")
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
            open(f, "w").write(body)


def main():
    binary, work = BIN[sys.argv[1]], os.path.abspath(sys.argv[2])
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
        print(f"{name}: {len(steps)} steps")
    print("diverging steps:", bad)


if __name__ == "__main__":
    main()
