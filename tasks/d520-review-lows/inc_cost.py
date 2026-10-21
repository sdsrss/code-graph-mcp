import os, subprocess, time, statistics, sys
P = "/var/tmp/d521/pend"; N = int(sys.argv[1]) if len(sys.argv) > 1 else 15
env = dict(os.environ, HOME="/var/tmp/d518r2/home", CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_NO_UPDATE_CHECK="1")
t = {"pre176": [], "low7": []}
for i in range(N):
    for a in (("pre176", "low7") if i % 2 == 0 else ("low7", "pre176")):
        d = f"{P}/cgmnow-{a}"
        with open(f"{d}/src/storage/db.rs", "a") as f: f.write(f"\n// cost {i}\n")
        t0 = time.time()
        r = subprocess.run([f"/var/tmp/d521/bin/cgm-{a}", "incremental-index", "--no-embed"], cwd=d, env=env, capture_output=True)
        t[a].append((time.time() - t0) * 1000)
        assert r.returncode == 0, r.stderr[-500:]
for a, v in t.items(): print(a, "median %.0f ms" % statistics.median(v), "n", len(v))
