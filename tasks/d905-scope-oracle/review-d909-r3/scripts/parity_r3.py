#!/usr/bin/env python3
"""parity_r3.py <binary> <work>: edit sequences around a call both scope rules rule out;
after every step incremental == fresh (relation, both ends with lines, confidence, metadata)."""
import os, shutil, sqlite3, subprocess, sys
ENV = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_DISABLE_MODEL_DOWNLOAD="1")
DUMP = """SELECT e.relation||' '||e.confidence||' '||sf.path||':'||COALESCE(sn.qualified_name,sn.name)||'@'||sn.start_line||' -> '||
 tf.path||':'||COALESCE(tn.qualified_name,tn.name)||'@'||tn.start_line||' '||COALESCE(e.metadata,'')
 FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
 JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id"""
DIS = "def make():\n    pass\n\n\ndef f(x=make()):\n    def make():\n        pass\n    return x\n"
DIS2 = "from lib import make\n\n\ndef make():\n    pass\n\n\ndef f(x=make()):\n    def make():\n        pass\n    return x\n"
LIB = "def make():\n    pass\n"
NOLOCAL = "def f(x=make()):\n    return x\n"
CASES = {
 "pending row, then the file gains the disagreement, then lib changes": [
   {"m.py": NOLOCAL}, {"lib.py": LIB}, {"m.py": DIS}, {"lib.py": LIB + "\n\ndef other():\n    pass\n"}, {"lib.py": None},
   {"lib.py": LIB}, {"m.py": NOLOCAL}, {"lib.py": None}, {"m.py": DIS}],
 "disagreement, lib namesake added/renamed/moved/removed": [
   {"m.py": DIS}, {"lib.py": LIB}, {"lib.py": "def make2():\n    pass\n"}, {"lib.py": LIB},
   {"lib.py": None, "pkg/__init__.py": "", "pkg/x.py": LIB}, {"pkg/x.py": None}, {"lib.py": LIB}],
 "disagreement with an import of the name, import target edited": [
   {"m.py": DIS2, "lib.py": LIB}, {"lib.py": "def make2():\n    pass\n"}, {"lib.py": LIB}, {"lib.py": None}, {"lib.py": LIB},
   {"m.py": DIS}, {"m.py": DIS2}],
 "module def moves out of the file and back": [
   {"m.py": DIS, "use.py": "from m import make\n\n\ndef u():\n    return make()\n"},
   {"m.py": "def f(x=make()):\n    def make():\n        pass\n    return x\n", "lib.py": LIB},
   {"m.py": DIS, "lib.py": None}, {"use.py": "from m import f\n\n\ndef u():\n    return f()\n"}],
 "another file's caller of the name, duplicates appear and vanish": [
   {"m.py": DIS, "use.py": "def u():\n    return make()\n"}, {"lib.py": LIB}, {"pkg/__init__.py": "", "pkg/x.py": LIB},
   {"lib.py": None}, {"pkg/x.py": None}, {"m.py": None}, {"m.py": DIS}],
 "nested-class disagreement in a method default, class in a fn": [
   {"m.py": "def F():\n    def make():\n        pass\n    class K:\n        def m(self, x=make()):\n            def make():\n                pass\n            return x\n    return K\n"},
   {"lib.py": LIB}, {"m.py": "def F():\n    class K:\n        def m(self, x=make()):\n            def make():\n                pass\n            return x\n    return K\n"},
   {"lib.py": None}, {"lib.py": LIB},
   {"m.py": "def F():\n    def make():\n        pass\n    class K:\n        def m(self, x=make()):\n            def make():\n                pass\n            return x\n    return K\n"}],
 "global rebinding in another function toggled, lib namesake": [
   {"m.py": "def h():\n    pass\n\n\ndef k():\n    global h\n    h = None\n\n\ndef g():\n    def h():\n        pass\n    return h()\n", "lib.py": "def h():\n    pass\n"},
   {"m.py": "def h():\n    pass\n\n\ndef k():\n    h = None\n\n\ndef g():\n    def h():\n        pass\n    return h()\n"},
   {"lib.py": None}, {"m.py": "def k():\n    global h\n    h = None\n\n\ndef g(x=h()):\n    def h():\n        pass\n    return h()\n"},
   {"lib.py": "def h():\n    pass\n"}],
 "lambda default disagreement, file renamed": [
   {"m.py": "def make():\n    pass\n\n\ndef f(cb=lambda: make()):\n    def make():\n        pass\n    return cb\n", "lib.py": LIB},
   {"m.py": None, "m2.py": "def make():\n    pass\n\n\ndef f(cb=lambda: make()):\n    def make():\n        pass\n    return cb\n"},
   {"lib.py": None}, {"m2.py": None, "m.py": "def make():\n    pass\n\n\ndef f(cb=lambda: make()):\n    def make():\n        pass\n    return cb\n", "lib.py": LIB}],
}

def write(d, delta, stamp):
    for p, body in delta.items():
        f = os.path.join(d, p)
        if body is None:
            if os.path.exists(f): os.remove(f)
            continue
        os.makedirs(os.path.dirname(f), exist_ok=True)
        open(f, "w").write(body); os.utime(f, (stamp, stamp))

def index(binary, d):
    r = subprocess.run([binary, "incremental-index", "--no-embed", "--quiet"], cwd=d, env=ENV, capture_output=True, text=True)
    if r.returncode: raise SystemExit(f"index failed {d}: {r.stderr[-400:]}")
    con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
    rows = sorted(x for (x,) in con.execute(DUMP)); con.close(); return rows

binary, work = os.path.abspath(sys.argv[1]), os.path.abspath(sys.argv[2])
assert work.startswith("/var/tmp/agent-tmp-d909-review3/")
bad = 0; steps = 0
for name, seq in CASES.items():
    inc = os.path.join(work, "inc"); shutil.rmtree(inc, ignore_errors=True); os.makedirs(inc)
    subprocess.run(["git", "init", "-q"], cwd=inc, check=True)
    state = {}; stamp = 1_700_000_000
    for i, delta in enumerate(seq):
        stamp += 10; state.update(delta); state = {k: v for k, v in state.items() if v is not None}
        write(inc, delta, stamp)
        a = index(binary, inc)
        fr = os.path.join(work, "fresh"); shutil.rmtree(fr, ignore_errors=True); os.makedirs(fr)
        subprocess.run(["git", "init", "-q"], cwd=fr, check=True)
        write(fr, state, stamp); b = index(binary, fr)
        steps += 1
        if a != b:
            bad += 1
            print(f"DIVERGE [{name}] step {i}: inc-only {sorted(set(a)-set(b))} fresh-only {sorted(set(b)-set(a))}")
    print(f"[{name}] last state calls: {[x for x in a if x.startswith('calls')]}")
shutil.rmtree(os.path.join(work, "inc"), ignore_errors=True); shutil.rmtree(os.path.join(work, "fresh"), ignore_errors=True)
print(f"steps {steps} diverging {bad}")
