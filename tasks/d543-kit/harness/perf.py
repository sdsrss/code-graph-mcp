"""perf.py <corpus> <rel> <reps> arms...: per arm, time the delete run and the restore run of <rel>; also a one-file edit of <rel>
with a 2,000-entry vanished record seeded (head arms read it; base ignores it). Prints medians, interleaving arms."""
import sys, os, statistics, sqlite3, json, subprocess
sys.path.insert(0, "/var/tmp/d543/harness")
from corp import *
corpus, rel, reps, arms = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4:]
res = {a: {"del": [], "res": [], "edit": [], "edit_seeded": [], "reextract": []} for a in arms}
name = os.path.basename(corpus.rstrip("/"))
for r in range(reps):
    for a in arms:
        w = f"{R}/work/perf/{name}/{a}"; copy(corpus, w); index(f"{R}/bin/cgm-{a}", w)
        p = os.path.join(w, rel); keep = open(p, "rb").read()
        os.remove(p); t, cpu, _ = index(f"{R}/bin/cgm-{a}", w); res[a]["del"].append((t, cpu))
        open(p, "wb").write(keep)
        e = dict(env, RUST_LOG="info"); import time, resource
        t0 = time.time(); r0 = resource.getrusage(resource.RUSAGE_CHILDREN)
        out = subprocess.run([f"{R}/bin/cgm-{a}", "incremental-index", "--no-embed"], cwd=w, env=e, capture_output=True, text=True)
        r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
        res[a]["res"].append((time.time() - t0, (r1.ru_utime - r0.ru_utime) + (r1.ru_stime - r0.ru_stime)))
        n = [l for l in out.stderr.splitlines() if "Phase 0-pre" in l]
        res[a]["reextract"].append(n[0].split("re-extracting ")[1].split(" ")[0] if n else "0")
        open(p, "ab").write(b"\n"); t, cpu, _ = index(f"{R}/bin/cgm-{a}", w); res[a]["edit"].append((t, cpu))
        con = sqlite3.connect(f"{w}/.code-graph/index.db")
        rec = [[f"gone/{i}/file_{i}.rs", [f"src/dep_{j}.rs" for j in range(6)]] for i in range(2000)]
        con.execute("insert or replace into meta(key, value) values ('vanished_file_dependents', ?)", (json.dumps(rec),)); con.commit(); con.close()
        open(p, "ab").write(b"\n"); t, cpu, _ = index(f"{R}/bin/cgm-{a}", w); res[a]["edit_seeded"].append((t, cpu))
for a in arms:
    f = lambda k: f"{statistics.median(x[0] for x in res[a][k])*1000:.0f}ms wall / {statistics.median(x[1] for x in res[a][k])*1000:.0f}ms cpu"
    print(f"{name} {rel} [{a}] delete {f('del')}; restore {f('res')} (Phase 0-pre re-extract {res[a]['reextract']}); edit {f('edit')}; edit with 2,000-entry record {f('edit_seeded')}")
