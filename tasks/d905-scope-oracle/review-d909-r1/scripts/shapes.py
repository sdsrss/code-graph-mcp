#!/usr/bin/env python3
"""shapes.py <cases.py> <work> [case-name-substring...]

For each case (dict name -> {path: source}) index a fresh copy with base and
head, dump every calls/references edge as `rel src@line -> tgt@line conf`,
and print base-only / head-only rows and the head edges.
"""
import importlib.util, os, shutil, sqlite3, subprocess, sys

BIN = {"base": os.environ.get("BASE_BIN", "/var/tmp/d909-review/bin/base"), "head": os.environ.get("HEAD_BIN", "/var/tmp/d909-review/bin/head")}
ENV = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_DISABLE_MODEL_DOWNLOAD="1")
DUMP = """SELECT e.relation||' '||sf.path||':'||sn.name||'@'||sn.start_line||' -> '||tf.path||':'||tn.name||'@'||tn.start_line||'('||tn.type||') '||e.confidence
 FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
 JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id WHERE e.relation IN ('calls','references')"""


def index(arm, files, d):
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d)
    subprocess.run(["git", "init", "-q"], cwd=d, check=True)
    for p, body in files.items():
        f = os.path.join(d, p)
        os.makedirs(os.path.dirname(f), exist_ok=True)
        with open(f, "w") as fh:
            fh.write(body)
    assert os.path.abspath(d).startswith("/var/tmp/d909-review/"), d
    r = subprocess.run([BIN[arm], "incremental-index", "--no-embed", "--quiet"], cwd=d, env=ENV,
                       capture_output=True, text=True)
    if r.returncode:
        raise SystemExit(f"{d}: {r.stderr[-500:]}")
    con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
    out = sorted(x for (x,) in con.execute(DUMP))
    con.close()
    return out


def main():
    spec = importlib.util.spec_from_file_location("cases", sys.argv[1])
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    work = os.path.abspath(sys.argv[2])
    pick = sys.argv[3:]
    for name, files in mod.CASES.items():
        if pick and not any(p in name for p in pick):
            continue
        b = index("base", files, os.path.join(work, "b"))
        h = index("head", files, os.path.join(work, "h"))
        print(f"=== {name}")
        for p, body in files.items():
            for i, line in enumerate(body.splitlines(), 1):
                print(f"   {p}:{i:3} {line}")
        for x in sorted(set(b) - set(h)):
            print("   BASE-ONLY", x)
        for x in sorted(set(h) - set(b)):
            print("   HEAD-ONLY", x)
        for x in h:
            if x in set(b):
                print("   both     ", x)
    shutil.rmtree(os.path.join(work, "b"), ignore_errors=True)
    shutil.rmtree(os.path.join(work, "h"), ignore_errors=True)


if __name__ == "__main__":
    main()
