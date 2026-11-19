"""aliasscan.py <corpus-name>: base-only python calls/references edges whose target name the
source file binds by an alias-like assignment other than single-target module-level X = X / X = m.X."""
import ast, os, sqlite3, sys
from collections import Counter, defaultdict
R = "/var/tmp/d905-review/keep"
n = sys.argv[1]
Q = """SELECT e.relation, sf.path, sn.type, COALESCE(sn.qualified_name,sn.name), sn.start_line, sn.end_line,
 tf.path, tn.type, tn.name, COALESCE(tn.qualified_name,tn.name), e.confidence, COALESCE(e.metadata,''), sf.language
 FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
 JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id"""
def rows(arm):
    con = sqlite3.connect(f"file:{R}/{n}-{arm}/.code-graph/index.db?mode=ro", uri=True)
    r = list(con.execute(Q)); con.close(); return r
b, h = rows("base"), rows("head")
ch = Counter(h); left = Counter(b) - ch
cache = {}
def analyze(path):
    if path in cache: return cache[path]
    try:
        src = open(os.path.join(R, f"{n}-base", path), "rb").read()
        tree = ast.parse(src)
    except Exception:
        cache[path] = None; return None
    imp_alias = {}  # local alias -> original name
    for node in ast.walk(tree):
        if isinstance(node, ast.ImportFrom):
            for a in node.names:
                if a.asname: imp_alias[a.asname] = a.name
    binds = defaultdict(list)  # name -> [(shape, lineno, module_level)]
    def walk(node, level):
        for ch_ in ast.iter_child_nodes(node):
            lvl = level
            if isinstance(ch_, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda)): lvl = "func"
            elif isinstance(ch_, ast.ClassDef): lvl = "class"
            if isinstance(ch_, ast.Assign):
                pairs = []
                for t in ch_.targets:
                    v = ch_.value
                    if isinstance(t, (ast.Tuple, ast.List)) and isinstance(v, (ast.Tuple, ast.List)) and len(t.elts) == len(v.elts):
                        pairs += [(te, ve, "tuple") for te, ve in zip(t.elts, v.elts)]
                    elif isinstance(t, ast.Name):
                        pairs.append((t, v, "chain" if len(ch_.targets) > 1 else "single"))
                for te, ve, kind in pairs:
                    if not isinstance(te, ast.Name): continue
                    N = te.id
                    if isinstance(ve, ast.Attribute) and ve.attr == N: shape = "attr"
                    elif isinstance(ve, ast.Name) and ve.id == N: shape = "same"
                    elif isinstance(ve, ast.Name) and imp_alias.get(ve.id) == N: shape = "rename-back"
                    else: continue
                    if kind == "single" and level == "module" and shape in ("attr", "same"): continue  # handled by head
                    binds[N].append((f"{kind}/{shape}/{level}", ch_.lineno))
            walk(ch_, lvl)
    walk(tree, "module")
    cache[path] = binds; return binds
hits = Counter(); ex = defaultdict(list)
for r in sorted(left.elements()):
    rel, sp, st, sq, s0, s1, tp, tt, tn, tq, conf, meta, lang = r
    if lang != "python" or rel not in ("calls", "references"): continue
    a = analyze(sp)
    if not a or tn not in a: continue
    shapes = "+".join(sorted({s for s, _ in a[tn]}))
    hits[(rel, shapes)] += 1
    ex[(rel, shapes)].append(f"{sp}:{sq}[{s0}-{s1}] -> {tp}:{tq}({tt}) {conf} binds@{[l for _, l in a[tn]]}")
print(n, dict(hits))
for k, v in ex.items():
    print(" ", k, len(v))
    for e in v[:int(sys.argv[2]) if len(sys.argv) > 2 else 8]: print("     ", e)
