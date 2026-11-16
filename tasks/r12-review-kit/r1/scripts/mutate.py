"""mutate.py <file> <old> <new> <test filters...>: apply one exact replacement, run the lib tests, restore."""
import sys, subprocess, shutil, os, time
SRC = '/var/tmp/agent-tmp-r12-review/src'
f, old, new = sys.argv[1], sys.argv[2], sys.argv[3]
filters = sys.argv[4:]
path = os.path.join(SRC, f)
orig = open(path).read()
n = orig.count(old)
if n != 1:
    print(f'SKIP: {n} matches'); sys.exit(2)
backup = path + '.mutorig'
shutil.copy2(path, backup)
try:
    open(path, 'w').write(orig.replace(old, new))
    env = dict(os.environ, CARGO_TARGET_DIR='/var/tmp/agent-tmp-r12-review/target')
    r = subprocess.run(['cargo', 'test', '--no-default-features', '--lib', '-q', '--', *filters], cwd=SRC, env=env, capture_output=True, text=True)
    out = r.stdout + r.stderr
    fails = [l for l in out.splitlines() if l.startswith('    ') and '::' in l and 'test' in l][:8]
    summ = [l for l in out.splitlines() if l.startswith('test result')]
    if 'error[' in out or 'could not compile' in out:
        print('COMPILE ERROR'); print('\n'.join(l for l in out.splitlines() if 'error' in l)[:1500])
    print('KILLED' if r.returncode else 'SURVIVED', summ, fails)
finally:
    shutil.copy2(backup, path); os.remove(backup)
    t = time.time(); os.utime(path, (t, t))
    assert open(path).read() == orig
