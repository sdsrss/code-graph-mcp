#!/usr/bin/env python3
"""inc_bench.py <rounds> <dir> <bin> <relpath> <old-substring> <new-substring> [<dir2> <bin2>]:
toggle an edit of <relpath> (replace old with new, then back), incremental-index after each; report apply-run
wall/cpu medians and the fan-out log lines of the first apply run. With dir2/bin2 alternate the two arms."""
import os, subprocess, sys, time, resource, statistics, re
n = int(sys.argv[1]); arms = [(sys.argv[2], sys.argv[3])]
rel, old, new = sys.argv[4], sys.argv[5].encode().decode('unicode_escape'), sys.argv[6].encode().decode('unicode_escape')
if len(sys.argv) > 8: arms.append((sys.argv[7], sys.argv[8]))
home = "/var/tmp/d370a-review/fxhome"
env = dict(os.environ, HOME=home, CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_NO_AUTO_UPDATE="1", RUST_LOG="info")
def edit(d, a, b):
    p = os.path.join(d, rel); s = open(p).read()
    assert a in s, (p, a)
    open(p, "w").write(s.replace(a, b, 1)); t = time.time() + 2; os.utime(p, (t, t))
def run(d, b):
    r0 = resource.getrusage(resource.RUSAGE_CHILDREN); t = time.time()
    r = subprocess.run([b, "incremental-index", "--no-embed", "--json"], cwd=d, env=env, capture_output=True, text=True)
    w = (time.time() - t) * 1000; r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
    assert r.returncode == 0, r.stderr[-800:]
    fan = [re.sub(r'^.*\[index\] ', '', l) for l in r.stderr.splitlines() if 'fan-out round' in l]
    return w, (r1.ru_utime - r0.ru_utime) + (r1.ru_stime - r0.ru_stime), fan, r.stdout.strip()
res = {b: {'apply': [], 'revert': []} for _, b in arms}; logs = {}
for i in range(n):
    for d, b in (arms if i % 2 == 0 else arms[::-1]):
        edit(d, old, new); w, c, fan, out = run(d, b); res[b]['apply'].append((w, c))
        if i == 0: logs[b] = (fan, out)
        edit(d, new, old); w, c, fan2, _ = run(d, b); res[b]['revert'].append((w, c))
        if i == 0: logs[b + ' revert'] = (fan2, '')
for b, v in res.items():
    for k in ('apply', 'revert'):
        ws = sorted(x[0] for x in v[k]); cs = sorted(x[1] for x in v[k])
        print(f"{os.path.basename(b)} {k}: wall ms median {statistics.median(ws):.0f} ({ws[0]:.0f}-{ws[-1]:.0f}); cpu s median {statistics.median(cs):.3f}")
for b, (fan, out) in logs.items():
    print(f"  [{os.path.basename(b.split()[0])}{' revert' if 'revert' in b else ''}] {out[:160]}")
    for f in fan: print("     ", f[:200])
