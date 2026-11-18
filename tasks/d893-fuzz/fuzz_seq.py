#!/usr/bin/env python3
"""Random-sequence differential test: incremental index vs a fresh index.

usage: fuzz_seq.py <binary> <first-seed> <count> <out.jsonl> [--steps N] [--jobs J] [--gen gen.py]

Each seed builds a small Python project out of renamed / plain / relative /
function-local imports, re-exports and bare calls, then applies N random edit
steps. After every step the working copy is refreshed with `incremental-index`
and a copy of the same state is indexed from scratch; every edge (relation,
both endpoints, confidence, metadata, duplicates kept) must match. One JSON
line per seed: every step's diff (`diffs`, the first also as `step` /
`inc_only` / `fresh_only`) and per step a hash of the incremental and the
fresh edge sets with the confidence column dropped (`hashes`), or ok.

Workflow: run it for a base and a head binary over the same seeds, then
`cmp.py base.jsonl head.jsonl` lists seeds only one of them diverges on, and
`classify_new.py base.jsonl head.jsonl <base-binary>` replays each head-only
seed on base with renamed imports turned into `pass` (`masked` = base then
diverges the same way). `show.py` prints a seed's states, `repro.py` replays
one and prints the edges matching a pattern. Work dirs go under $FUZZ_WORK.
"""
import hashlib, json, os, random, shutil, sqlite3, subprocess, sys, tempfile
from collections import Counter
from multiprocessing import Pool

ROOT = os.environ.get("FUZZ_WORK", "/var/tmp/agent-tmp-fuzz-work")
ENV = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_DISABLE_MODEL_DOWNLOAD="1")

DEF_FILES = ["app/m.py", "app/n.py", "other.py", "app/sub/impl.py", "tests/helpers.py"]
CALLERS = ["app/views.py", "app/rev.py", "use.py", "app/fl.py"]
INITS = {
    "app/__init__.py": ["", "from .m import a\n", "from .n import a\n", "from .m import a as b\n",
                        "from .sub import a\n"],
    "app/sub/__init__.py": ["", "from .impl import a\n", "from .impl import a as b\n",
                            "from .impl import *\n"],
    "tests/__init__.py": [""],
}
NAMES = ["a", "b", "c"]


def def_file(rng):
    out = []
    for nm in NAMES:
        r = rng.random()
        if r < 0.45:
            out.append(f"def {nm}():\n    return {rng.randint(0, 9)}\n")
        elif r < 0.5:
            out.append(f"{nm} = {rng.randint(0, 9)}\n")
        elif r < 0.55:
            out.append(f"def outer_{nm}():\n    def {nm}():\n        return 1\n    return {nm}\n")
    if rng.random() < 0.2:
        out.append("class K:\n    def a(self):\n        return 1\n")
    rng.shuffle(out)
    return "\n\n".join(out)


def imports_for(path, rng):
    pool = [
        "from app.m import a", "from app.m import a as b", "from app.n import a",
        "from app.n import a as b", "from app.m import b", "from app.m import a as c",
        "from app import a", "from app.sub import a", "from app.sub import a as b",
        "import app.m", "from app import m", "from other import b", "from other import a as c",
        "from other import a", "from tests.helpers import a as b", "from tests.helpers import a",
        "from app.sub.impl import a as b", "from app.n import a as a",
    ]
    if path.startswith("app/"):
        pool += ["from .m import a as c", "from .n import a", "from .m import a as b",
                 "from .sub import a", "from . import m", "from .sub.impl import a"]
    k = rng.randint(0, 4)
    return [rng.choice(pool) for _ in range(k)]


def caller_file(path, rng):
    lines = imports_for(path, rng)
    if rng.random() < 0.15:
        lines = ["try:\n    from app.m import a as b\nexcept ImportError:\n    b = None"] + lines
    body = []
    for nm in NAMES:
        if rng.random() < 0.6:
            body.append(f"def go_{nm}():\n    return {nm}()\n")
    if rng.random() < 0.3:
        body.append("def go_p(b):\n    return b()\n")
    if rng.random() < 0.25:
        alias = rng.choice(["b", "c", "a"])
        src = rng.choice(["app.m", "app.n", "other"])
        body.append(f"def go_local():\n    from {src} import a as {alias}\n    return {alias}()\n")
    if rng.random() < 0.2:
        body.append("m.a()\n")
    if rng.random() < 0.2:
        body.append(f"{rng.choice(NAMES)}()\n")
    if rng.random() < 0.2:
        body.append(f"def {rng.choice(NAMES)}():\n    return 5\n")
    return "\n".join(lines) + "\n\n\n" + "\n\n".join(body)


def gen_file(path, rng):
    if path in INITS:
        return rng.choice(INITS[path])
    if path in DEF_FILES:
        return def_file(rng)
    return caller_file(path, rng)


ALL = list(INITS) + DEF_FILES + CALLERS


def initial(rng):
    state = {}
    for p in ALL:
        if p in INITS or rng.random() < 0.85:
            state[p] = gen_file(p, rng)
    return state


def step(state, rng):
    st = dict(state)
    for _ in range(rng.randint(1, 3)):
        p = rng.choice(ALL)
        r = rng.random()
        if p in st and r < 0.25:
            del st[p]
        else:
            st[p] = gen_file(p, rng)
    return st


def write_state(d, state, prev, stamp):
    for p in (prev or {}):
        if p not in state:
            os.remove(os.path.join(d, p))
    for p, body in state.items():
        if prev is not None and prev.get(p) == body:
            continue
        f = os.path.join(d, p)
        os.makedirs(os.path.dirname(f), exist_ok=True)
        with open(f, "w") as fh:
            fh.write(body)
        os.utime(f, (stamp, stamp))


DUMP = """SELECT e.relation, sf.path||':'||COALESCE(sn.qualified_name, sn.name)||':'||sn.type,
 tf.path||':'||COALESCE(tn.qualified_name, tn.name)||':'||tn.type, e.confidence, COALESCE(e.metadata,'')
 FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
 JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id"""


def dump(d):
    con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
    rows = sorted(" | ".join(map(str, r)) for r in con.execute(DUMP))
    con.close()
    return rows


def index(binary, d):
    r = subprocess.run([binary, "incremental-index", "--no-embed", "--quiet"], cwd=d, env=ENV,
                       stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True, timeout=120)
    if r.returncode != 0:
        raise RuntimeError(f"index failed in {d}: {r.stderr[-400:]}")


_GEN = {}


def load_gen(path):
    """Another generator module's `initial(rng)` / `step(state, rng)`. Loaded in
    the worker: Python 3.14 starts pool workers with forkserver, which
    re-imports this module, so a swap made in main() never reached them and
    `--gen` silently ran this file's generator (found 2026-11-18, D#901)."""
    if path not in _GEN:
        import importlib.util
        spec = importlib.util.spec_from_file_location("gen", path)
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        _GEN[path] = (mod.initial, mod.step)
    return _GEN[path]


def run_seed(args):
    binary, seed, steps, gen = args
    initial_, step_ = load_gen(gen) if gen else (initial, step)
    rng = random.Random(seed)
    states = [initial_(rng)]
    for _ in range(steps):
        states.append(step_(states[-1], rng))
    base = tempfile.mkdtemp(prefix=f"s{seed}-", dir=ROOT)
    try:
        inc = os.path.join(base, "inc")
        os.makedirs(inc)
        subprocess.run(["git", "init", "-q"], cwd=inc, check=True)
        stamp = 1_700_000_000
        prev = None
        diffs, hashes, first = [], [], None
        for i, st in enumerate(states):
            stamp += 10
            write_state(inc, st, prev, stamp)
            prev = st
            index(binary, inc)
            if i == 0:
                continue
            fresh = os.path.join(base, f"fresh{i}")
            os.makedirs(fresh)
            subprocess.run(["git", "init", "-q"], cwd=fresh, check=True)
            write_state(fresh, st, None, stamp)
            index(binary, fresh)
            a, b = dump(inc), dump(fresh)
            shutil.rmtree(fresh)
            # edge sets with the confidence column dropped, per step
            hashes.append([hashlib.sha1("\n".join(strip_conf(a)).encode()).hexdigest()[:12],
                           hashlib.sha1("\n".join(strip_conf(b)).encode()).hexdigest()[:12]])
            if a != b:
                sb, sa = Counter(b), Counter(a)
                only_inc = sorted((sa - sb).elements())
                only_fresh = sorted((sb - sa).elements())
                diffs.append({"step": i, "inc_only": only_inc, "fresh_only": only_fresh})
                if first is None:
                    first = i
        if first is None:
            return {"seed": seed, "ok": True, "hashes": hashes}
        d0 = diffs[0]
        return {"seed": seed, "step": first, "inc_only": d0["inc_only"], "fresh_only": d0["fresh_only"],
                "diffs": diffs, "hashes": hashes, "states": states}
    except Exception as e:  # noqa: BLE001
        return {"seed": seed, "error": str(e)}
    finally:
        shutil.rmtree(base, ignore_errors=True)


def strip_conf(rows):
    out = []
    for r in rows:
        f = r.split(" | ")
        out.append(" | ".join(f[:3] + f[4:]))
    return sorted(out)


def main():
    binary, first, count, out = os.path.abspath(sys.argv[1]), int(sys.argv[2]), int(sys.argv[3]), sys.argv[4]
    steps = int(sys.argv[sys.argv.index("--steps") + 1]) if "--steps" in sys.argv else 3
    jobs = int(sys.argv[sys.argv.index("--jobs") + 1]) if "--jobs" in sys.argv else 8
    gen = os.path.abspath(sys.argv[sys.argv.index("--gen") + 1]) if "--gen" in sys.argv else None
    os.makedirs(ROOT, exist_ok=True)
    bad = err = 0
    with Pool(jobs) as pool, open(out, "w") as fh:
        for res in pool.imap_unordered(run_seed, [(binary, s, steps, gen) for s in range(first, first + count)]):
            if "error" in res:
                err += 1
            elif not res.get("ok"):
                bad += 1
            fh.write(json.dumps(res) + "\n")
    print(f"{binary}: {count} seeds, {bad} diverged, {err} errors")


if __name__ == "__main__":
    main()
