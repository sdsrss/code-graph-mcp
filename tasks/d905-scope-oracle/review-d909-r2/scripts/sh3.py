#!/usr/bin/env python3
"""sh3.py <cases.py> <work> [substr...] -- index each case with v0203, pre, rep; print edges per arm."""
import importlib.util, os, shutil, sqlite3, subprocess, sys
R = "/var/tmp/d909-review2"
ARMS = os.environ.get("ARMS", "v0203 pre rep").split()
ENV = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_DISABLE_MODEL_DOWNLOAD="1")
DUMP = """SELECT e.relation||' '||sn.name||'@'||sn.start_line||' -> '||tn.name||'@'||tn.start_line||'('||substr(tn.type,1,2)||')'||CASE WHEN tf.path<>sf.path THEN '['||tf.path||']' ELSE '' END||CASE WHEN e.confidence<>'extracted' THEN ' '||e.confidence ELSE '' END
 FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
 JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id WHERE e.relation IN ('calls','references')"""

def index(arm, files, d):
    assert os.path.abspath(d).startswith(R + "/"), d
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d)
    for p, body in files.items():
        f = os.path.join(d, p)
        os.makedirs(os.path.dirname(f), exist_ok=True)
        open(f, "w").write(body)
    subprocess.run(["git", "init", "-q"], cwd=d, check=True)
    r = subprocess.run([f"{R}/bin/{arm}", "incremental-index", "--no-embed", "--quiet"], cwd=d, env=ENV, capture_output=True, text=True)
    if r.returncode:
        raise SystemExit(f"{d}: {r.stderr[-500:]}")
    con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
    out = sorted(x for (x,) in con.execute(DUMP))
    con.close()
    return out

def main():
    spec = importlib.util.spec_from_file_location("cases", sys.argv[1])
    mod = importlib.util.module_from_spec(spec); spec.loader.exec_module(mod)
    work = os.path.abspath(sys.argv[2]); pick = sys.argv[3:]
    for name, files in mod.CASES.items():
        if pick and not any(p in name for p in pick):
            continue
        res = {a: set(index(a, files, os.path.join(work, a))) for a in ARMS}
        print(f"=== {name}")
        for p, body in files.items():
            for i, line in enumerate(body.splitlines(), 1):
                print(f"   {p}:{i:3} {line}")
        allr = sorted(set().union(*res.values()))
        for x in allr:
            tag = "".join("X" if x in res[a] else "." for a in ARMS)
            print(f"   [{tag}] {x}")
    for a in ARMS:
        shutil.rmtree(os.path.join(work, a), ignore_errors=True)

if __name__ == "__main__":
    main()
