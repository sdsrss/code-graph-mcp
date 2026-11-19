"""varshape.py <corpus> : for every Python references edge whose name the
source file binds at module level only by a value (oracle wrong-var 'var'),
print the module-level binding statements of that name, grouped by shape."""
import ast, os, sqlite3, sys
from collections import Counter, defaultdict
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from reforacle import Corpus, classify, Q
root = os.path.abspath(sys.argv[1])
con = sqlite3.connect(f"file:{root}/.code-graph/index.db?mode=ro", uri=True)
files = [r[0] for r in con.execute("SELECT path FROM files WHERE language='python'")]
corpus = Corpus(root, files)
shapes = Counter(); ex = defaultdict(list)
def binds(path, name):
    tree = ast.parse(open(os.path.join(root, path), 'rb').read())
    out = []
    for node in ast.walk(tree):
        if isinstance(node, (ast.Assign, ast.AnnAssign, ast.AugAssign)):
            tg = node.targets if isinstance(node, ast.Assign) else [node.target]
            for t in tg:
                for n in ast.walk(t):
                    if isinstance(n, ast.Name) and n.id == name:
                        v = node.value
                        if isinstance(v, ast.Name) and v.id == name: s = 'X = X'
                        elif isinstance(v, ast.Attribute) and v.attr == name: s = 'X = m.X'
                        elif isinstance(v, ast.Name): s = 'X = Y'
                        elif isinstance(v, ast.Attribute): s = 'X = m.Y'
                        elif isinstance(v, ast.Call): s = 'X = call()'
                        else: s = 'X = other'
                        out.append((s, ast.get_source_segment(open(os.path.join(root, path)).read(), node)[:90]))
        if isinstance(node, ast.Global) and name in node.names:
            out.append(('global', f'global {name}'))
    return out
for row in con.execute(Q, ("references",)):
    spath, stype, sname, s0, s1, tpath, tname, tqn, ttype, conf, t0, t1 = row
    v, info = classify(corpus, spath, stype, sname, s0, s1, tpath, tname, tqn, ttype, t0, t1, "references")
    if v != 'wrong-var' or info != 'var': continue
    b = binds(spath, tname)
    key = ' + '.join(sorted({s for s, _ in b})) or 'none-found'
    shapes[key] += 1
    if len(ex[key]) < 3: ex[key].append(f"{spath}:{sname} -> {tpath}:{tqn} {conf} | {b[0][1] if b else ''}")
print(os.path.basename(root))
for k, c in shapes.most_common():
    print(f"  {c:5} {k}")
    for e in ex[k]: print("        ", e)
