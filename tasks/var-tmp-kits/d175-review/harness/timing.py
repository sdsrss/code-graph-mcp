"""timing.py <corpus-src> <edit> <trials> [arms]: per arm, copy + full index; then
alternate arms: apply edit -> incremental (measured), revert -> incremental."""
import sys, os, re, statistics, subprocess, time, resource
sys.path.insert(0, "/var/tmp/d175-review/harness")
from corp import *
BIN = {"head": f"{R}/bin/cgm-head", "base": f"{R}/bin/cgm-base", "b41": f"{R}/bin/cgm-b41"}
def sub(path, old, new):
    def f(root):
        p = os.path.join(root, path); s = open(p).read()
        assert old in s, (path, old); open(p, "w").write(s.replace(old, new, 1))
    return f
def append(path, text):
    def f(root):
        open(os.path.join(root, path), "a").write(text)
    return f
def create(path, text):
    def f(root):
        open(os.path.join(root, path), "w").write(text)
    return f
def remove(path):
    def f(root):
        os.remove(os.path.join(root, path))
    return f
I = "src/flask/__init__.py"
EDITS = {
  "flask_new_reexport": [append(I, "from .helpers import get_root_path as get_root_path\n")],
  "flask_retarget_redirect": [sub(I, "from .helpers import redirect as redirect", "from werkzeug.utils import redirect as redirect")],
  "flask_retarget_Flask": [sub(I, "from .app import Flask as Flask", "from .sansio.app import App as Flask")],
  "flask_init_comment": [append(I, "\n# edit\n")],
  "hono_types_touch": [append("src/client/types.ts", "\n// edit\n")],
  "hono_iface_to_class": [sub("src/client/types.ts", "export interface Response extends ClientResponse<unknown> {}", "export class Response {}")],
  "hono_new_response_def": [create("src/zz_response.ts", "export function Response() {\n  return 1\n}\n")],
  "flask_plain": [append("src/flask/helpers.py", "\n# edit\n")],
  "flask_body": [sub("src/flask/helpers.py", "def get_debug_flag() -> bool:\n", "def get_debug_flag() -> bool:\n    _x = 1\n")],
  "flask_tests_init": [create("tests/__init__.py", "")],
  "nx_add_graph": [append("networkx/__init__.py", "from networkx.classes.graph import Graph\n")],
  "nx_plain": [append("networkx/algorithms/cycles.py", "\n# edit\n")],
  "nx_utils_init_delete": [remove("networkx/utils/__init__.py")],
  "std_plain": [append("argparse.py", "\n# edit\n")],
  "std_del_json_init": [remove("json/__init__.py")],
  "std_logging_body": [sub("logging/__init__.py", "def getLogger(name=None):\n", "def getLogger(name=None):\n    _x = 1\n")],
}
corpus, edit, trials = sys.argv[1], sys.argv[2], int(sys.argv[3])
arms = sys.argv[4].split(",") if len(sys.argv) > 4 else ["head", "base"]
cname = os.path.basename(corpus.rstrip("/")) if not corpus.rstrip("/").endswith("/A") else os.path.basename(os.path.dirname(corpus.rstrip("/")))
roots = {}
for a in arms:
    roots[a] = f"{R}/work/time/{a}/{cname}"
    copy(corpus, roots[a]); index(BIN[a], roots[a])
originals = {}
def snapshot(root):
    return {p: open(os.path.join(root, p)).read() if os.path.exists(os.path.join(root, p)) else None for p in touched}
touched = set()
for f in EDITS[edit]:
    touched.add(f.__closure__[0].cell_contents if f.__closure__ else None)
ENV = dict(env, RUST_LOG="info")
def run(a):
    r0 = resource.getrusage(resource.RUSAGE_CHILDREN); t0 = time.time()
    r = subprocess.run([BIN[a], "incremental-index", "--no-embed"], cwd=roots[a], env=ENV, capture_output=True, text=True)
    wall = time.time() - t0; r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
    cpu = (r1.ru_utime - r0.ru_utime) + (r1.ru_stime - r0.ru_stime)
    out = r.stdout + r.stderr
    fan = [int(x) for x in re.findall(r"fan-out round: re-extracting (\d+)", out)]
    pre = [int(x) for x in re.findall(r"Phase 0-pre: re-extracting (\d+)", out)]
    m = re.search(r"\[incremental\] (\d+) files changed, (\d+) deleted", out)
    elapsed = re.search(r"\[incremental\].*?, ([\d.]+)s", out)
    return wall, cpu, fan, pre, m.groups() if m else None
res = {a: [] for a in arms}
import shutil
backup = {}
for a in arms:
    backup[a] = {}
order = []
for t in range(trials):
    seq = arms if t % 2 == 0 else list(reversed(arms))
    for a in seq:
        # save originals of every file the edit can touch
        saved = {}
        for dirpath in ["src/client/types.ts","src/zz_response.ts","argparse.py","json/__init__.py","logging/__init__.py","src/flask/__init__.py","src/flask/helpers.py","tests/__init__.py","networkx/__init__.py","networkx/algorithms/cycles.py","networkx/utils/__init__.py"]:
            p = os.path.join(roots[a], dirpath)
            saved[p] = open(p).read() if os.path.exists(p) else None
        for f in EDITS[edit]: f(roots[a])
        res[a].append(run(a))
        for p, c in saved.items():
            if c is None:
                if os.path.exists(p): os.remove(p)
            else:
                open(p, "w").write(c)
        run(a)
for a in arms:
    w = [x[0] * 1000 for x in res[a]]; c = [x[1] * 1000 for x in res[a]]
    print(f"{edit} [{a}] n={len(w)} wall ms min/med/max {min(w):.0f}/{statistics.median(w):.0f}/{max(w):.0f}  cpu ms min/med/max {min(c):.0f}/{statistics.median(c):.0f}/{max(c):.0f}  fanout={res[a][0][2]} pre={res[a][0][3]} changed={res[a][0][4]}")
