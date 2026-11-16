import os, sys, subprocess, time, resource, shutil
R = '/var/tmp/agent-tmp-r12-review/perf'
BINS = {'base': '/var/tmp/agent-tmp-converge-r12/bin/v0193', 'head': '/var/tmp/agent-tmp-converge-r12/bin/rel'}
env = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE='1')
order = sys.argv[1].split(',')
for arm in order:
    root = os.path.join(R, 'run_' + arm)
    if not os.path.isdir(root):
        shutil.copytree(os.path.join(R, 'src'), root, symlinks=True)
    cg = os.path.join(root, '.code-graph')
    if os.path.isdir(cg): shutil.rmtree(cg)
    t = time.time()
    p = subprocess.run(['/usr/bin/env', 'bash', '-c', f'{BINS[arm]} incremental-index --no-embed 2>&1 | tail -3'], cwd=root, env=env, capture_output=True, text=True)
    full = time.time() - t
    # one-file edit: touch a Go file whose methods have receiver calls
    f = os.path.join(root, 'p001', 'context.go')
    with open(f, 'a') as fh: fh.write('\n// edit\n')
    tt = time.time() + 5; os.utime(f, (tt, tt))
    t = time.time()
    q = subprocess.run([BINS[arm], 'incremental-index', '--no-embed'], cwd=root, env=env, capture_output=True, text=True)
    inc = time.time() - t
    print(arm, f'full={full:.1f}s inc1={inc:.2f}s', p.stdout.strip().splitlines()[0] if p.stdout else '', flush=True)
print('maxrss children (KB):', resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss)
