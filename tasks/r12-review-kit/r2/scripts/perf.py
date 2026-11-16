"""perf.py <corpus_src> <edit_rel_path> <full_rounds> <edit_rounds> arm=bin ...
Interleaved arms. Full index: fresh .code-graph each round. Edit: append a comment line, then incremental-index.
Prints wall and user+sys CPU per run, and means."""
import os, sys, shutil, subprocess, time, resource, statistics, json
W = '/var/tmp/agent-tmp-r12-review2/perfrun'
src, edit, nfull, nedit = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])
arms = dict(a.split('=', 1) for a in sys.argv[5:])
env = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE='1')
tag = os.path.basename(src.rstrip('/'))
roots = {}
for arm in arms:
    root = os.path.join(W, f'{tag}.{arm}')
    if os.path.isdir(root): shutil.rmtree(root)
    shutil.copytree(src, root, symlinks=True)
    cg = os.path.join(root, '.code-graph')
    if os.path.isdir(cg): shutil.rmtree(cg)
    roots[arm] = root
def run(arm):
    r0 = resource.getrusage(resource.RUSAGE_CHILDREN); t = time.time()
    p = subprocess.run([arms[arm], 'incremental-index', '--no-embed'], cwd=roots[arm], env=env, capture_output=True, text=True)
    wall = time.time() - t; r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
    if p.returncode: print('FAIL', arm, p.stderr[-500:])
    return wall, (r1.ru_utime - r0.ru_utime) + (r1.ru_stime - r0.ru_stime)
res = {a: {'full': [], 'edit': []} for a in arms}
for i in range(nfull):
    for arm in (list(arms) if i % 2 == 0 else list(arms)[::-1]):
        cg = os.path.join(roots[arm], '.code-graph')
        if os.path.isdir(cg): shutil.rmtree(cg)
        res[arm]['full'].append(run(arm))
        print(f'full {i} {arm} wall={res[arm]["full"][-1][0]:.2f} cpu={res[arm]["full"][-1][1]:.2f} load={os.getloadavg()[0]:.1f}', flush=True)
for i in range(nedit):
    for arm in (list(arms) if i % 2 == 0 else list(arms)[::-1]):
        f = os.path.join(roots[arm], edit)
        if os.environ.get('EDITMODE') == 'prepend':
            body = open(f).read(); open(f, 'w').write(f'// edit {i}\n' + body)
        else:
            with open(f, 'a') as fh: fh.write(f'\n// edit {i}\n')
        t = time.time() + 2; os.utime(f, (t, t))
        res[arm]['edit'].append(run(arm))
        print(f'edit {i} {arm} wall={res[arm]["edit"][-1][0]:.2f} cpu={res[arm]["edit"][-1][1]:.2f} load={os.getloadavg()[0]:.1f}', flush=True)
for arm in arms:
    for k in ('full', 'edit'):
        xs = res[arm][k]
        if xs: print(f'{arm} {k}: wall mean={statistics.mean(x[0] for x in xs):.3f} min={min(x[0] for x in xs):.3f} max={max(x[0] for x in xs):.3f}  cpu mean={statistics.mean(x[1] for x in xs):.3f} min={min(x[1] for x in xs):.3f}  n={len(xs)}')
