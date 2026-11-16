"""Fixture harness. Usage: fx.py <case.py> [arm...]
case.py defines FILES (dict path->body) and optional EDITS (list of steps; each a dict path->body|None)
and optional CALLERS (list of qualified names to print)."""
import os, sys, shutil, sqlite3, subprocess, time, json
R = '/var/tmp/agent-tmp-r12-review2/fx'
BINS = {'v0193': '/var/tmp/agent-tmp-converge-r12/bin/v0193', 'rel': '/var/tmp/agent-tmp-converge-r12/bin/rel', 'r1': os.environ.get('HEADBIN', '/var/tmp/agent-tmp-converge-r12/bin/r1')}
env = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE='1')
ns = {}
exec(open(sys.argv[1]).read(), ns)
FILES, EDITS = ns['FILES'], ns.get('EDITS', [])
name = os.path.splitext(os.path.basename(sys.argv[1]))[0]
arms = sys.argv[2:] or ['v0193', 'rel', 'r1']
def write(root, files):
    for p, b in files.items():
        f = os.path.join(root, p)
        if b is None:
            if os.path.exists(f): os.remove(f)
            continue
        os.makedirs(os.path.dirname(f), exist_ok=True)
        open(f, 'w').write(b)
        t = time.time() + 5
        os.utime(f, (t, t))
def index(root, arm):
    r = subprocess.run([BINS[arm], 'incremental-index', '--no-embed'], cwd=root, env=env, capture_output=True, text=True)
    if r.returncode: print(r.stdout, r.stderr)
def dump(root):
    c = sqlite3.connect(os.path.join(root, '.code-graph/index.db'))
    q = """SELECT e.relation, sf.path||':'||COALESCE(sn.qualified_name, sn.name), tf.path||':'||COALESCE(tn.qualified_name, tn.name)||':'||tn.type, e.confidence
           FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
           JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id WHERE e.relation IN ('calls','inherits','implements')"""
    rows = sorted(set(c.execute(q)))
    pend = sorted(c.execute("SELECT target_name, metadata FROM pending_unresolved_calls"))
    return rows, pend
def fresh(tag, files, arm):
    root = os.path.join(R, f'{name}.{arm}.{tag}')
    if os.path.isdir(root): shutil.rmtree(root)
    os.makedirs(root)
    subprocess.run(['git','init','-q',root])
    write(root, files)
    index(root, arm)
    return root
def show(rows):
    for r in rows: print('   ', *r)
for arm in arms:
    print(f'=== {arm}')
    root = fresh('inc', FILES, arm)
    rows, pend = dump(root)
    show(rows)
    tree = dict(FILES)
    for i, step in enumerate(EDITS):
        write(root, step)
        for p, b in step.items():
            if b is None: tree.pop(p, None)
            else: tree[p] = b
        index(root, arm)
        inc, ipend = dump(root)
        ctl, cpend = dump(fresh(f'ctl{i}', tree, arm))
        print(f'  -- after edit {i+1} {list(step)}: incremental == rebuild: {inc == ctl}')
        if inc != ctl:
            print('   only incremental:'); show(sorted(set(inc) - set(ctl)))
            print('   only rebuild:'); show(sorted(set(ctl) - set(inc)))
        else:
            show(inc)
