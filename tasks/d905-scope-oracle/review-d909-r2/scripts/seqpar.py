#!/usr/bin/env python3
"""seqpar.py <cases.py> <work> <arm>...: each case a list of steps {path: body|None};
after every step incremental vs fresh (line-keyed edges, all relations)."""
import importlib.util, os, shutil, sqlite3, subprocess, sys
R = "/var/tmp/d909-review2"
ENV = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_DISABLE_MODEL_DOWNLOAD="1")
DUMP = """SELECT e.relation||' '||e.confidence||' '||sf.path||':'||COALESCE(sn.qualified_name,sn.name)||'@'||sn.start_line||' -> '||
 tf.path||':'||COALESCE(tn.qualified_name,tn.name)||'@'||tn.start_line||' '||COALESCE(e.metadata,'')
 FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
 JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id"""

def index(binary, d):
    r = subprocess.run([binary, "incremental-index", "--no-embed", "--quiet"], cwd=d, env=ENV, capture_output=True, text=True)
    if r.returncode:
        raise SystemExit(f"{d}: {r.stderr[-500:]}")
    con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
    out = sorted(x for (x,) in con.execute(DUMP))
    con.close()
    return out

def apply(d, step, stamp):
    for p, body in step.items():
        f = os.path.join(d, p)
        if body is None:
            os.remove(f)
        else:
            os.makedirs(os.path.dirname(f), exist_ok=True)
            open(f, "w").write(body)
            os.utime(f, (stamp, stamp))

def main():
    spec = importlib.util.spec_from_file_location("c", sys.argv[1]); mod = importlib.util.module_from_spec(spec); spec.loader.exec_module(mod)
    work = os.path.abspath(sys.argv[2]); assert work.startswith(R + "/")
    for arm in sys.argv[3:]:
        binary = f"{R}/bin/{arm}"
        bad = 0; total = 0
        for name, steps in mod.CASES.items():
            inc = os.path.join(work, "inc"); shutil.rmtree(inc, ignore_errors=True); os.makedirs(inc)
            subprocess.run(["git", "init", "-q"], cwd=inc, check=True)
            stamp = 1_700_000_000
            for i, step in enumerate(steps):
                stamp += 10
                apply(inc, step, stamp)
                got = index(binary, inc)
                fresh = os.path.join(work, "fresh"); shutil.rmtree(fresh, ignore_errors=True)
                shutil.copytree(inc, fresh, ignore=shutil.ignore_patterns(".code-graph"))
                want = index(binary, fresh)
                total += 1
                if got != want:
                    bad += 1
                    print(f"[{arm}] DIVERGE {name} step {i}: inc-only {sorted(set(got) - set(want))} fresh-only {sorted(set(want) - set(got))}")
        print(f"[{arm}] {len(mod.CASES)} cases, {total} steps, diverging steps: {bad}")
    shutil.rmtree(os.path.join(work, "inc"), ignore_errors=True); shutil.rmtree(os.path.join(work, "fresh"), ignore_errors=True)

if __name__ == "__main__":
    main()
