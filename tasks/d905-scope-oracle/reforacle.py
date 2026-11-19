#!/usr/bin/env python3
"""reforacle.py <corpus-dir> [--rel references|calls] [--dump N]

Classifies every Python edge of one relation in <corpus-dir>/.code-graph/index.db
against Python's own scoping rules (ast), per occurrence of the target's name
inside the source node:

  ok           the name resolves to that very definition (same file def/class,
               or a project import chain ending at it)
  wrong-local  every occurrence is a local of the source (param, assignment,
               comprehension / lambda / except / with / walrus / match target)
  wrong-var    the name is a module-level variable / module object, not a def
  wrong-ext    the name is imported from a module outside the project
  wrong-builtin the name is a builtin nowhere bound in the file
  wrong-other  the name resolves to a different project definition than the
               target (a namesake elsewhere, a method by bare name, ...)
  unknown      unbound in the file (star import from outside, dynamic globals)
  unmatched    no occurrence of the name found in the source node
"""
import ast, builtins, os, sqlite3, sys
from collections import Counter, defaultdict

BUILTINS = set(dir(builtins))
FUNC = (ast.FunctionDef, ast.AsyncFunctionDef)
SCOPES = FUNC + (ast.Lambda, ast.ClassDef, ast.ListComp, ast.SetComp, ast.DictComp, ast.GeneratorExp)


class Scope:
    def __init__(self, node, parent):
        self.node, self.parent = node, parent
        self.bound = {}  # name -> (kind, detail)
        self.defs = defaultdict(list)  # name -> every def/class line bound in this scope
        self.kinds = defaultdict(set)  # name -> every kind bound in this scope
        self.glob, self.nonloc = set(), set()

    @property
    def is_class(self):
        return isinstance(self.node, ast.ClassDef)


def bind_target(t, scope, kind):
    if isinstance(t, ast.Name):
        scope.bound.setdefault(t.id, kind)
        scope.kinds[t.id].add(kind[0])
    elif isinstance(t, (ast.Tuple, ast.List)):
        for e in t.elts:
            bind_target(e, scope, kind)
    elif isinstance(t, ast.Starred):
        bind_target(t.value, scope, kind)


class Analyzer(ast.NodeVisitor):
    """Builds scopes and records every Name(Load) occurrence with its scope and owner."""

    def __init__(self):
        self.occ = defaultdict(list)  # name -> [(scope, owner)]
        self.callocc = defaultdict(list)  # bare-name calls only
        self.scope = None
        self.owner = None  # innermost def/class node owning the occurrence (None = module)
        self.module_scope = None
        self.imports = {}  # module-level name -> ('from', module, level, orig) | ('import', dotted)
        self.star = []  # (module, level)
        self.global_names = set()

    def func_scope(self):
        s = self.scope
        while s and isinstance(s.node, (ast.ListComp, ast.SetComp, ast.DictComp, ast.GeneratorExp)):
            s = s.parent
        return s

    def visit_Module(self, node):
        self.scope = self.module_scope = Scope(node, None)
        self.generic_visit(node)

    def _def(self, node, kind):
        # decorators, defaults, annotations, bases evaluate in the enclosing scope but
        # code-graph attributes decorators to the decorated def: owner = node.
        outer_owner = self.owner
        for d in node.decorator_list:
            self.owner = node if isinstance(d, ast.Name) else outer_owner
            self.visit(d)
        self.owner = outer_owner
        self.scope.bound.setdefault(node.name, (kind, node.lineno))
        self.scope.defs[node.name].append(node.lineno)
        self.scope.kinds[node.name].add(kind)
        if isinstance(node, ast.ClassDef):
            for b in node.bases + [k.value for k in node.keywords]:
                self.visit(b)
        else:
            a = node.args
            for d in a.defaults + [x for x in a.kw_defaults if x]:
                self.visit(d)
            self.owner = node
            for arg in a.posonlyargs + a.args + a.kwonlyargs + [x for x in (a.vararg, a.kwarg) if x]:
                if arg.annotation:
                    self.visit(arg.annotation)
            if node.returns:
                self.visit(node.returns)
            self.owner = outer_owner
        s = Scope(node, self.scope)
        if not isinstance(node, ast.ClassDef):
            a = node.args
            for arg in a.posonlyargs + a.args + a.kwonlyargs + [x for x in (a.vararg, a.kwarg) if x]:
                s.bound[arg.arg] = ("param", None)
        for tp in getattr(node, "type_params", []) or []:
            s.bound[tp.name] = ("param", None)
        prev_scope, prev_owner = self.scope, self.owner
        self.scope, self.owner = s, node
        for st in node.body:
            self.visit(st)
        self.scope, self.owner = prev_scope, prev_owner

    def visit_FunctionDef(self, node):
        self._def(node, "def")

    visit_AsyncFunctionDef = visit_FunctionDef

    def visit_ClassDef(self, node):
        self._def(node, "class")

    def visit_Lambda(self, node):
        a = node.args
        for d in a.defaults + [x for x in a.kw_defaults if x]:
            self.visit(d)
        s = Scope(node, self.scope)
        for arg in a.posonlyargs + a.args + a.kwonlyargs + [x for x in (a.vararg, a.kwarg) if x]:
            s.bound[arg.arg] = ("param", None)
        prev = self.scope
        self.scope = s
        self.visit(node.body)
        self.scope = prev

    def _comp(self, node, elts):
        gens = node.generators
        self.visit(gens[0].iter)  # first iterable evaluates in the enclosing scope
        s = Scope(node, self.scope)
        prev = self.scope
        self.scope = s
        for i, g in enumerate(gens):
            bind_target(g.target, s, ("comp", None))
            if i:
                self.visit(g.iter)
            for c in g.ifs:
                self.visit(c)
        for e in elts:
            self.visit(e)
        self.scope = prev

    def visit_ListComp(self, node):
        self._comp(node, [node.elt])

    visit_SetComp = visit_GeneratorExp = visit_ListComp

    def visit_DictComp(self, node):
        self._comp(node, [node.key, node.value])

    def visit_Call(self, node):
        if isinstance(node.func, ast.Name):
            self.callocc[node.func.id].append((self.scope, self.owner))
        self.generic_visit(node)

    def visit_Global(self, node):
        self.scope.glob.update(node.names)
        self.global_names.update(node.names)

    def visit_Assign(self, node):
        self.visit(node.value)
        v = node.value
        for t in node.targets:
            if isinstance(t, ast.Name) and ((isinstance(v, ast.Name) and v.id == t.id)
                                            or (isinstance(v, ast.Attribute) and v.attr == t.id)):
                self.scope.bound.setdefault(t.id, ("alias", None))
                self.scope.kinds[t.id].add("alias")
            else:
                self.visit(t)

    def visit_Nonlocal(self, node):
        self.scope.nonloc.update(node.names)

    def visit_Name(self, node):
        if isinstance(node.ctx, (ast.Store, ast.Del)):
            self.scope.bound.setdefault(node.id, ("var", None))
            self.scope.kinds[node.id].add("var")
        else:
            self.occ[node.id].append((self.scope, self.owner))

    def visit_NamedExpr(self, node):
        self.visit(node.value)
        fs = self.func_scope()
        fs.bound.setdefault(node.target.id, ("var", None))

    def visit_ExceptHandler(self, node):
        if node.type:
            self.visit(node.type)
        if node.name:
            self.scope.bound.setdefault(node.name, ("var", None))
        for st in node.body:
            self.visit(st)

    def visit_Import(self, node):
        for a in node.names:
            nm = a.asname or a.name.split(".")[0]
            self.scope.bound.setdefault(nm, ("import", ("import", a.name if a.asname else a.name.split(".")[0])))
            self.scope.kinds[nm].add("import")

    def visit_ImportFrom(self, node):
        for a in node.names:
            if a.name == "*":
                if self.scope is self.module_scope:
                    self.star.append((node.module or "", node.level))
                continue
            nm = a.asname or a.name
            self.scope.bound.setdefault(nm, ("import", ("from", node.module or "", node.level, a.name)))
            self.scope.kinds[nm].add("import")

    def visit_MatchAs(self, node):
        if node.pattern:
            self.visit(node.pattern)
        if node.name:
            self.scope.bound.setdefault(node.name, ("var", None))

    def visit_MatchStar(self, node):
        if node.name:
            self.scope.bound.setdefault(node.name, ("var", None))

    def visit_MatchMapping(self, node):
        self.generic_visit(node)
        if node.rest:
            self.scope.bound.setdefault(node.rest, ("var", None))


def resolve_scope(scope, name):
    """Scope where `name` read in `scope` is bound: ('local', kind) | ('module', None)."""
    s, first = scope, True
    while s is not None:
        if s.parent is None:
            return ("module", None)
        if name in s.glob:
            return ("module", None)
        if s.is_class and not first:
            s = s.parent
            continue
        if name in s.nonloc:
            s, first = s.parent, False
            continue
        if name in s.bound:
            if s.is_class:
                return ("class", s)
            return ("local", (s.bound[name], s))
        s, first = s.parent, False
    return ("module", None)


class Corpus:
    def __init__(self, root, files):
        self.root = root
        self.files = files  # rel path list (python)
        self.mod2file = {}
        for p in files:
            parts = p[:-3].split("/")
            if parts[-1] == "__init__":
                parts = parts[:-1]
            for i in range(len(parts)):
                self.mod2file.setdefault(".".join(parts[i:]), []).append(p)
        self.cache = {}

    def analyze(self, path):
        if path not in self.cache:
            try:
                with open(os.path.join(self.root, path), "rb") as f:
                    tree = ast.parse(f.read())
                an = Analyzer()
                an.visit(tree)
                self.cache[path] = an
            except (SyntaxError, ValueError, RecursionError):
                self.cache[path] = None
        return self.cache[path]

    def module_file(self, from_path, module, level):
        if level:
            base = from_path.split("/")[:-1]
            if level > 1:
                base = base[: len(base) - (level - 1)]
            cand = "/".join(base + (module.split(".") if module else []))
            for c in (cand + ".py", cand + "/__init__.py"):
                if c in self.files_set:
                    return c
            return None
        hits = self.mod2file.get(module, [])
        if len(hits) == 1:
            return hits[0]
        if hits:
            # prefer the shortest path (closest to a source root)
            return sorted(hits, key=len)[0]
        return None

    @property
    def files_set(self):
        if not hasattr(self, "_fs"):
            self._fs = set(self.files)
        return self._fs

    def module_binding(self, path, name, depth=0):
        """Binding of a module-level name: ('def'|'class', path) | ('var', path) |
        ('ext', mod) | ('builtin',) | ('unbound',) | ('module', mod)."""
        if depth > 12:
            return ("unbound",)
        an = self.analyze(path)
        if an is None:
            return ("unbound",)
        ms = an.module_scope
        kinds = ms.kinds.get(name, set())
        for k in ("def", "class"):
            if k in kinds:
                return (k, path)
        if "import" in kinds:
            kind, extra = ms.bound[name]
            if kind != "import":
                return ("unbound",)  # an import beside another binding: undecided
            return self.import_binding(path, name, extra, depth)
        if "alias" in kinds or name in an.global_names:
            return ("unbound",)
        if kinds:
            return ("var", path)
        return self.unbound_binding(path, an, name, depth)

    def import_binding(self, path, name, imp, depth):
            if imp[0] == "import":
                return ("module", imp[1])
            _, mod, level, orig = imp
            f = self.module_file(path, mod, level)
            if f is None:
                # `from pkg import submodule`?
                sub = self.module_file(path, (mod + "." if mod else "") + orig, level)
                if sub is not None:
                    return ("module", sub)
                return ("ext", ("." * level) + mod)
            sub = self.module_file(path, (mod + "." if mod else "") + orig, level)
            r = self.module_binding(f, orig, depth + 1)
            if r[0] == "unbound" and sub is not None:
                return ("module", sub)
            return r

    def unbound_binding(self, path, an, name, depth):
        for mod, level in an.star:
            f = self.module_file(path, mod, level)
            if f is None:
                continue
            r = self.module_binding(f, name, depth + 1)
            if r[0] not in ("unbound", "builtin"):
                return r
        if an.star and any(self.module_file(path, m, l) is None for m, l in an.star):
            return ("unbound",)
        if name in BUILTINS:
            return ("builtin",)
        return ("unbound",)


def owner_matches(owner, stype, sname, sstart, send):
    if stype in ("function", "method"):
        return isinstance(owner, FUNC) and owner.name == sname and sstart <= owner.lineno <= send
    if stype == "class":
        return isinstance(owner, ast.ClassDef) and owner.name == sname and sstart <= owner.lineno <= send
    if stype == "module":
        return owner is None or isinstance(owner, ast.ClassDef)
    return False


def binding_verdict(b, spath, tpath, tqn, tname, ttype):
    if b[0] in ("def", "class"):
        if tpath == b[1] and tqn == tname and ttype in ("function", "class"):
            return ("ok", b[0])
        sub = "method" if ttype == "method" else ("same-file" if tpath == b[1] else "elsewhere")
        return ("wrong-other", f"{b[0]}->{sub}")
    if b[0] in ("var", "module"):
        return ("wrong-var", b[0])
    if b[0] == "ext":
        return ("wrong-ext", b[1].split(".")[0] or b[1])
    if b[0] == "builtin":
        return ("wrong-builtin", tname)
    return ("unknown", "")


def classify(corpus, spath, stype, sname, sstart, send, tpath, tname, tqn, ttype, t0, t1, rel="references"):
    an = corpus.analyze(spath)
    if an is None:
        return "parse-fail", ""
    pool = an.callocc if rel == "calls" else an.occ
    occs = [(sc, ow) for sc, ow in pool.get(tname, []) if owner_matches(ow, stype, sname, sstart, send)]
    if not occs:
        return "unmatched", ""
    verdicts = []
    for sc, ow in occs:
        where, info = resolve_scope(sc, tname)
        if where == "local":
            (kind, extra), bscope = info
            if kind in ("def", "class"):
                ok = tpath == spath and any(t0 <= ln <= t1 for ln in bscope.defs[tname])
                verdicts.append(("ok", "nested") if ok else ("wrong-other", f"nested-{kind}->elsewhere"))
            elif kind == "import":
                verdicts.append(binding_verdict(corpus.import_binding(spath, tname, extra, 0), spath, tpath, tqn, tname, ttype))
            elif "alias" in bscope.kinds.get(tname, ()):
                verdicts.append(("unknown", "local-alias"))
            else:
                verdicts.append(("wrong-local", kind))
            continue
        if where == "class":
            kind, extra = info.bound[tname]
            ok = ttype == "method" and tpath == spath and tqn == f"{info.node.name}.{tname}"
            verdicts.append(("ok" if ok else "wrong-other", f"class-scope-{kind}"))
            continue
        verdicts.append(binding_verdict(corpus.module_binding(spath, tname), spath, tpath, tqn, tname, ttype))
    order = ["ok", "unknown", "wrong-other", "wrong-var", "wrong-ext", "wrong-builtin", "wrong-local"]
    return min(verdicts, key=lambda v: order.index(v[0]))


Q = """SELECT sf.path, sn.type, sn.name, sn.start_line, sn.end_line,
 tf.path, tn.name, COALESCE(tn.qualified_name, tn.name), tn.type, e.confidence, tn.start_line, tn.end_line
 FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
 JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id
 WHERE e.relation=? AND sf.language='python'"""


def main():
    root = os.path.abspath(sys.argv[1])
    rel = "references"
    dump = 0
    args = sys.argv[2:]
    while args:
        a = args.pop(0)
        if a == "--rel":
            rel = args.pop(0)
        elif a == "--dump":
            dump = int(args.pop(0))
    con = sqlite3.connect(f"file:{root}/.code-graph/index.db?mode=ro", uri=True)
    files = [r[0] for r in con.execute("SELECT path FROM files WHERE language='python'")]
    corpus = Corpus(root, files)
    tally = Counter()
    sub = Counter()
    samples = defaultdict(list)
    for row in con.execute(Q, (rel,)):
        spath, stype, sname, s0, s1, tpath, tname, tqn, ttype, conf, t0, t1 = row
        v, info = classify(corpus, spath, stype, sname, s0, s1, tpath, tname, tqn, ttype, t0, t1, rel)
        tally[(v, conf)] += 1
        sub[(v, info)] += 1
        if len(samples[v]) < dump:
            samples[v].append(f"{spath}:{sname}({stype}) -> {tpath}:{tqn}({ttype}) {conf} [{info}]")
    total = sum(tally.values())
    print(f"{os.path.basename(root)} {rel}: {total} python edges")
    for v in ["ok", "unknown", "unmatched", "parse-fail", "wrong-other", "wrong-var", "wrong-ext", "wrong-builtin", "wrong-local"]:
        n = sum(c for (vv, _), c in tally.items() if vv == v)
        if not n:
            continue
        byconf = ", ".join(f"{cf} {c}" for (vv, cf), c in sorted(tally.items()) if vv == v)
        top = ", ".join(f"{i or '-'} {c}" for (vv, i), c in sub.most_common() if vv == v)
        print(f"  {v:14} {n:6} ({100 * n / total:.1f}%)  [{byconf}]  top: {top[:160]}")
    for v, ss in samples.items():
        for s in ss:
            print(f"    {v}: {s}")


if __name__ == "__main__":
    main()
