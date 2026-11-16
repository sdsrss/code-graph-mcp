"""corpus_inc.py <corpus> <rel_file> <python-expr transforming text> : incremental vs rebuild on a real corpus copy (HEAD)."""
import os, sys, shutil, subprocess, sqlite3, time
R = '/var/tmp/agent-tmp-r12-review/cinc'
BIN = os.environ.get('BIN', '/var/tmp/agent-tmp-converge-r12/bin/rel')
env = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE='1')
corpus, rel, expr = sys.argv[1], sys.argv[2], sys.argv[3]
src = f'/var/tmp/agent-tmp-r12-review/{os.environ.get("ARM", "head")}/{corpus}'
inc, ctl = os.path.join(R, corpus + '.inc'), os.path.join(R, corpus + '.ctl')
for d in (inc, ctl):
    if os.path.isdir(d): shutil.rmtree(d)
shutil.copytree(src, inc, symlinks=True)          # carries HEAD's full index
def dump(root):
    c = sqlite3.connect(os.path.join(root, '.code-graph/index.db'))
    q = """SELECT e.relation, sf.path||':'||COALESCE(sn.qualified_name, sn.name)||'@'||sn.start_line, tf.path||':'||COALESCE(tn.qualified_name, tn.name)||'@'||tn.start_line, e.confidence
           FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
           JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id"""
    r = set(c.execute(q)); c.close(); return r
steps = expr.split(';;')
for i, ex in enumerate(steps):
    f = os.path.join(inc, rel)
    text = open(f).read()
    new = eval(ex, {'t': text})
    open(f, 'w').write(new); tt = time.time() + 10 * (i + 1); os.utime(f, (tt, tt))
    subprocess.run([BIN, 'incremental-index', '--no-embed'], cwd=inc, env=env, capture_output=True)
    if os.path.isdir(ctl): shutil.rmtree(ctl)
    shutil.copytree(inc, ctl, symlinks=True, ignore=shutil.ignore_patterns('.code-graph'))
    subprocess.run([BIN, 'incremental-index', '--no-embed'], cwd=ctl, env=env, capture_output=True)
    a, b = dump(inc), dump(ctl)
    print(f'step {i+1}: incremental == rebuild: {a == b}  (edges {len(a)} vs {len(b)})')
    for x in sorted(a - b)[:15]: print('  only inc:', x)
    for x in sorted(b - a)[:15]: print('  only rebuild:', x)
