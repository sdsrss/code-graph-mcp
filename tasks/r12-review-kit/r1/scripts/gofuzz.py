import os, sys, random, shutil, sqlite3, subprocess, time
R = '/var/tmp/agent-tmp-r12-review/fx/fuzz'
BIN = {'base': '/var/tmp/agent-tmp-converge-r12/bin/v0193', 'head': os.environ.get('HEADBIN', '/var/tmp/agent-tmp-converge-r12/bin/rel')}
env = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE='1')
SNIPS = [
 'type Foo struct{}', 'type Foo interface{ Bar() }', 'type Foo int', 'type Bar struct{}',
 'type T1 struct{}', 'type T1 struct{ T2 }', 'type T1 struct{ *T3 }', 'type T2 struct{}', 'type T2 struct{ T3 }', 'type T3 struct{}',
 'func (t *T1) Foo() {}', 'func (t *T2) Foo() {}', 'func (t T3) Foo() {}', 'func (t *T1) Bar() {}', 'func (t *T3) Bar() {}',
 'func Foo() {}', 'func Bar() {}',
 'func (t *T1) Call() { t.Foo(); t.Bar() }',
 'func (t *T2) Call2() { t.x.Foo(); v := t; v.Bar() }',
 'func (t *T3) Call3() { for _, t := range nil { t.Foo() }; t.Bar() }',
 'func use() { var x T1; x.Foo(); y := new(T2); y.Bar(); Foo() }',
 'func use2(p *T3) { p.Foo(); p.Bar().Foo() }',
 'func (b Bar) Use3() { b.Foo(); b.Bar() }',
]
PATHS = ['a/a1.go', 'a/a2.go', 'a/a3.go', 'b/b1.go', 'b/b2.go', 'c/c1.go']
def content(path, rng):
    pkg = path.split('/')[0]
    picks = rng.sample(SNIPS, rng.randint(1, 5))
    return f'package {pkg}\n\n' + '\n'.join(picks) + '\n'
def write(root, path, body):
    f = os.path.join(root, path)
    if body is None:
        if os.path.exists(f): os.remove(f)
        return
    os.makedirs(os.path.dirname(f), exist_ok=True)
    open(f, 'w').write(body)
    t = time.time() + 5
    os.utime(f, (t, t))
def index(root, arm):
    r = subprocess.run([BIN[arm], 'incremental-index', '--no-embed'], cwd=root, env=env, capture_output=True, text=True)
    if r.returncode: print('INDEX FAIL', r.stdout, r.stderr)
def dump_inh(root):
    c = sqlite3.connect(os.path.join(root, '.code-graph/index.db'))
    q = """SELECT sf.path||':'||sn.name, tf.path||':'||tn.name FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
           JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id WHERE e.relation='inherits'"""
    rows = set(c.execute(q)); c.close(); return rows
def dump(root):
    c = sqlite3.connect(os.path.join(root, '.code-graph/index.db'))
    q = """SELECT e.relation, sf.path||':'||COALESCE(sn.qualified_name, sn.name)||'@'||sn.start_line, tf.path||':'||COALESCE(tn.qualified_name, tn.name)||':'||tn.type, e.confidence
           FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
           JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id WHERE e.relation IN ('calls')"""
    rows = set(c.execute(q)); c.close(); return rows
def fresh(root, tree, arm):
    if os.path.isdir(root): shutil.rmtree(root)
    os.makedirs(root)
    subprocess.run(['git', 'init', '-q', root])
    write(root, 'go.mod', 'module example.com/app\n')
    for p, b in tree.items(): write(root, p, b)
    index(root, arm)
def run(seed, arm, steps):
    rng = random.Random(seed)
    tree = {p: content(p, rng) for p in rng.sample(PATHS, rng.randint(2, len(PATHS)))}
    inc = os.path.join(R, f'{arm}.inc'); ctl = os.path.join(R, f'{arm}.ctl')
    fresh(inc, tree, arm)
    hist = [dict(tree)]
    for s in range(steps):
        p = rng.choice(PATHS)
        body = None if (p in tree and rng.random() < 0.3) else content(p, rng)
        write(inc, p, body)
        if body is None: tree.pop(p, None)
        else: tree[p] = body
        hist.append({p: body})
        index(inc, arm)
        fresh(ctl, tree, arm)
        a, b = dump(inc), dump(ctl)
        if a != b:
            print('  inherits-diverged-too:', dump_inh(inc) != dump_inh(ctl))
            return hist, a - b, b - a
    return None
arm = sys.argv[1]; n = int(sys.argv[2]); start = int(sys.argv[3]) if len(sys.argv) > 3 else 0
bad = 0
for seed in ([int(x) for x in os.environ['SEEDS'].split(',')] if os.environ.get('SEEDS') else range(start, start + n)):
    r = run(seed, arm, 4)
    if r:
        bad += 1
        hist, only_inc, only_ctl = r
        print(f'seed {seed}: DIVERGED after {len(hist)-1} edits')
        for i, h in enumerate(hist):
            for p, b in h.items(): print(f'  step{i} {p}:', repr(b))
        print('  only incremental:', sorted(only_inc)); print('  only rebuild:', sorted(only_ctl))
print(f'{arm}: {bad}/{n} diverged')
