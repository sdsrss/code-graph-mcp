#!/usr/bin/env python3
"""localreexp.py <root>: for HEAD-style edges bound through a re-export (attr calls without amb whose
target lies outside the module's files, and from-imports whose target lies outside the module's files),
report those where the module imports the name only inside a function body."""
import sqlite3, json, ast, os, sys, collections
root = sys.argv[1]; os.chdir(root)
con = sqlite3.connect("file:.code-graph/index.db?mode=ro", uri=True)
paths = {r[0] for r in con.execute("select path from files where language='python'")}
def modfiles(m, importer):
    if m.startswith('.'):
        lvl = len(m) - len(m.lstrip('.')); base = os.path.dirname(importer)
        for _ in range(lvl - 1): base = os.path.dirname(base)
        rest = m.lstrip('.'); p = os.path.join(base, rest.replace('.', '/')) if rest else base
    else:
        p = m.replace('.', '/')
    return [f for f in (p + '.py', p + '/__init__.py') if f in paths]
cache = {}
def scopes(f):
    if f in cache: return cache[f]
    try: t = ast.parse(open(f, encoding='utf-8', errors='replace').read())
    except SyntaxError: cache[f] = (set(), set()); return cache[f]
    top, local = set(), set()
    def walk(n, infunc):
        for c in ast.iter_child_nodes(n):
            if isinstance(c, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda)):
                walk(c, True)
            elif isinstance(c, ast.ImportFrom):
                for a in c.names: (local if infunc else top).add(a.asname or a.name)
            else:
                walk(c, infunc)
    walk(t, False); cache[f] = (top, local); return cache[f]
c = collections.Counter(); ex = []
rows = con.execute("""select sf.path, sn.name, tf.path, tn.name, e.relation, e.metadata from edges e
 join nodes sn on sn.id=e.source_id join files sf on sf.id=sn.file_id join nodes tn on tn.id=e.target_id join files tf on tf.id=tn.file_id
 where sf.language='python' and tn.name<>'<module>' and ((e.relation='calls' and json_extract(e.metadata,'$.attr')=1 and json_extract(e.metadata,'$.amb') is null)
   or (e.relation='imports' and json_extract(e.metadata,'$.python_module') is not null and json_extract(e.metadata,'$.is_module_import') is null))""").fetchall()
for sp, sn, tp, tn, rel, meta in rows:
    m = json.loads(meta); mod = m.get('v') if rel == 'calls' else m.get('python_module')
    mf = modfiles(mod, sp)
    if not mf or tp in mf: continue
    top, local = set(), set()
    for f in mf:
        a, b = scopes(f); top |= a; local |= b
    if tn in top: c[(rel, 'top-level import')] += 1
    elif tn in local: c[(rel, 'FUNCTION-LOCAL only')] += 1; ex.append((rel, sp, sn, mod, tp, tn))
    else: c[(rel, 'no import of that name (name chain)')] += 1
for k, v in sorted(c.items()): print(v, k)
for e in ex[:20]: print("  ", e)
