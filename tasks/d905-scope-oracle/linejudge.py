"""Line-aware static judge of a same-file Python edge, by CPython's scoping.

resolve(tree) -> for every Name occurrence: (line, col, name, ctx, owner_line,
is_call) and the set of definition lines (def/class) it can reach, flow-
insensitively. owner_line = start line (decorators included) of the innermost
enclosing def/async def/lambda-less function node the indexer attributes it to,
0 for the module. Headers (decorators, defaults, annotations, return, bases)
belong to the enclosing scope; a lambda's / comprehension's body is its own
scope but its owner stays the enclosing def."""
import ast

FUNCS = (ast.FunctionDef, ast.AsyncFunctionDef)


class Scope:
    def __init__(self, kind, parent):
        self.kind, self.parent = kind, parent  # kind: module/function/class/lambda/comp
        self.bound = set(); self.defs = {}  # name -> set of def lines bound here
        self.glob = set(); self.nonloc = set()

    def bind(self, name, defline=None):
        self.bound.add(name)
        if defline is not None:
            self.defs.setdefault(name, set()).add(defline)


def def_start(n):
    return min([n.lineno] + [d.lineno for d in n.decorator_list])


class Builder(ast.NodeVisitor):
    def __init__(self):
        self.module = Scope("module", None)
        self.occ = []  # (name, ctx, scope, owner, is_call)
        self.scope = self.module; self.owner = 0
        self.calls = set()

    def target_scope(self, name):
        s = self.scope
        if name in s.glob:
            return self.module
        if name in s.nonloc:
            p = s.parent
            while p is not None and p.kind == "class":
                p = p.parent
            while p is not None and not (p.kind in ("function", "lambda") and name in p.bound | p.nonloc):
                p = p.parent
            return p or self.module
        return s

    def visit_Global(self, n):
        self.scope.glob.update(n.names)

    def visit_Nonlocal(self, n):
        self.scope.nonloc.update(n.names)

    def header(self, n):
        for d in n.decorator_list:
            self.visit(d)
        if isinstance(n, ast.ClassDef):
            for b in n.bases + [k.value for k in n.keywords]:
                self.visit(b)
            return
        a = n.args
        for d in a.defaults + [d for d in a.kw_defaults if d is not None]:
            self.visit(d)
        for arg in a.posonlyargs + a.args + a.kwonlyargs + [x for x in (a.vararg, a.kwarg) if x]:
            if arg.annotation:
                self.visit(arg.annotation)
        if n.returns:
            self.visit(n.returns)

    def visit_FunctionDef(self, n):
        outer_scope, outer_owner = self.scope, self.owner
        self.owner = def_start(n)  # the indexer gives a def its own header
        self.header(n)
        self.target_scope(n.name).bind(n.name, def_start(n))
        self.scope = Scope("function", outer_scope)
        a = n.args
        for arg in a.posonlyargs + a.args + a.kwonlyargs + [x for x in (a.vararg, a.kwarg) if x]:
            self.scope.bind(arg.arg)
        # globals/nonlocals first (they are declarations for the whole body)
        for st in ast.walk(ast.Module(body=n.body, type_ignores=[])):
            if isinstance(st, (ast.Global, ast.Nonlocal)) and self._directly_in(st, n):
                (self.scope.glob if isinstance(st, ast.Global) else self.scope.nonloc).update(st.names)
        for st in n.body:
            self.visit(st)
        self.scope, self.owner = outer_scope, outer_owner

    visit_AsyncFunctionDef = visit_FunctionDef

    def _directly_in(self, st, fn):
        # whether st sits in fn's body and not in a nested def/class/lambda
        stack = list(fn.body)
        while stack:
            x = stack.pop()
            if x is st:
                return True
            if isinstance(x, FUNCS + (ast.ClassDef, ast.Lambda)):
                continue
            stack.extend(ast.iter_child_nodes(x))
        return False

    def visit_ClassDef(self, n):
        self.header(n)
        self.target_scope(n.name).bind(n.name, def_start(n))
        outer = self.scope
        self.scope = Scope("class", outer)
        for st in n.body:
            if isinstance(st, ast.Global):
                self.scope.glob.update(st.names)
        for st in n.body:
            self.visit(st)
        self.scope = outer

    def visit_Lambda(self, n):
        a = n.args
        for d in a.defaults + [d for d in a.kw_defaults if d is not None]:
            self.visit(d)
        outer = self.scope
        self.scope = Scope("lambda", outer)
        for arg in a.posonlyargs + a.args + a.kwonlyargs + [x for x in (a.vararg, a.kwarg) if x]:
            self.scope.bind(arg.arg)
        self.visit(n.body)
        self.scope = outer

    def comp(self, n, elts):
        self.visit(n.generators[0].iter)
        outer = self.scope
        self.scope = Scope("comp", outer)
        for i, g in enumerate(n.generators):
            if i:
                self.visit(g.iter)
            self.visit(g.target)
            for c in g.ifs:
                self.visit(c)
        for e in elts:
            self.visit(e)
        self.scope = outer

    def visit_ListComp(self, n): self.comp(n, [n.elt])
    visit_SetComp = visit_ListComp
    visit_GeneratorExp = visit_ListComp
    def visit_DictComp(self, n): self.comp(n, [n.key, n.value])

    def visit_Import(self, n):
        for a in n.names:
            nm = (a.asname or a.name).split(".")[0]
            self.target_scope(nm).bind(nm)

    visit_ImportFrom = visit_Import

    def visit_Call(self, n):
        if isinstance(n.func, ast.Name):
            self.calls.add(id(n.func))
        self.generic_visit(n)

    def visit_Name(self, n):
        if isinstance(n.ctx, (ast.Store, ast.Del)):
            self.target_scope(n.id).bind(n.id)
        else:
            self.occ.append((n, self.scope, self.owner, id(n) in self.calls))

    def visit_NamedExpr(self, n):
        self.visit(n.value)
        s = self.scope
        while s.kind == "comp":
            s = s.parent
        s.bind(n.target.id)


def resolve_in(scope, name, module):
    """Def lines the name can reach from `scope`, flow-insensitively."""
    s = scope
    if s.kind == "class":
        if name in s.glob:
            return module.defs.get(name, set())
        if name in s.bound:  # LOAD_NAME: class dict, else globals
            return s.defs.get(name, set()) | module.defs.get(name, set())
        s = s.parent  # free in class: enclosing function, else module
        while s is not None and s.kind == "class":
            s = s.parent
        return resolve_in(s, name, module) if s is not None else module.defs.get(name, set())
    if s.kind == "module":
        return module.defs.get(name, set())
    if name in s.glob:
        return module.defs.get(name, set())
    if name in s.nonloc:
        p = s.parent
        while p is not None and (p.kind == "class" or name not in p.bound):
            p = p.parent
        return (p.defs.get(name, set()) if p is not None and p.kind != "module" else set())
    if name in s.bound:
        return s.defs.get(name, set())
    p = s.parent
    while p is not None and p.kind == "class":
        p = p.parent
    return resolve_in(p, name, module)


def occurrences(src):
    tree = ast.parse(src)
    b = Builder(); b.visit(tree)
    out = []
    for n, scope, owner, is_call in b.occ:
        out.append((n.id, owner, is_call, resolve_in(scope, n.id, b.module)))
    return out


def judge(src, rel, owner, name, t0):
    """'right' if some occurrence of `name` attributed to `owner` (def start
    line, 0 = module) and of the relation's kind reaches the def at t0."""
    occ = [o for o in occurrences(src) if o[0] == name and o[1] == owner and o[2] == (rel == "calls")]
    if not occ:
        return "unmatched"
    return "right" if any(t0 in o[3] for o in occ) else "wrong"
