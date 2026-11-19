#!/usr/bin/env python3
"""symjudge.py <corpus-copy-dir> <baseonly.tsv>

An independent judge (not reforacle): for each base-only Python calls/references
edge, find every Load of the target's name inside the source node's line range
and resolve it with Python's scoping (function / class / module scopes,
global / nonlocal declarations, class-body LOAD_NAME, header expressions in
the enclosing scope). Verdict: RIGHT if any occurrence can bind the target
definition, WRONG if occurrences exist and none can, NOOCC if no occurrence.
"""
import ast, os, sqlite3, sys
from collections import Counter, defaultdict

FUNCS = (ast.FunctionDef, ast.AsyncFunctionDef)
COMPS = (ast.ListComp, ast.SetComp, ast.DictComp, ast.GeneratorExp)


class Scope:
    def __init__(self, kind, parent, node):
        self.kind, self.parent, self.node = kind, parent, node
        self.locals = set()
        self.defs = defaultdict(set)  # name -> def start lines bound in this scope
        self.glob, self.nonloc = set(), set()
        self.children = []
        if parent:
            parent.children.append(self)


def start_of(n):
    return min([n.lineno] + [d.lineno for d in getattr(n, "decorator_list", [])])


class Builder:
    def __init__(self):
        self.occ = []  # (name, lineno, is_call, scope)
        self.module = None

    def bind(self, scope, name):
        scope.locals.add(name)

    def targets(self, t, scope):
        for n in ast.walk(t):
            if isinstance(n, ast.Name) and isinstance(n.ctx, (ast.Store, ast.Del)):
                self.bind(scope, n.id)
            elif isinstance(n, (ast.MatchAs, ast.MatchStar)) and n.name:
                self.bind(scope, n.name)
            elif isinstance(n, ast.MatchMapping) and n.rest:
                self.bind(scope, n.rest)

    def run(self, tree):
        self.module = Scope("module", None, tree)
        self.calls = set()
        for n in ast.walk(tree):
            if isinstance(n, ast.Call) and isinstance(n.func, ast.Name):
                self.calls.add(id(n.func))
        for st in tree.body:
            self.visit(st, self.module)
        return self.module

    def expr(self, e, scope):
        """Visit an expression evaluated in `scope`."""
        if e is None:
            return
        if isinstance(e, ast.Name):
            if isinstance(e.ctx, ast.Load):
                self.occ.append((e.id, e.lineno, id(e) in self.calls, scope))
            else:
                self.bind(scope, e.id)
            return
        if isinstance(e, ast.Lambda):
            self.args_outer(e.args, scope)
            s = Scope("function", scope, e)
            self.args_inner(e.args, s)
            self.expr(e.body, s)
            return
        if isinstance(e, COMPS):
            gens = e.generators
            self.expr(gens[0].iter, scope)
            s = Scope("function", scope, e)
            for i, g in enumerate(gens):
                if i:
                    self.expr(g.iter, s)
                self.targets(g.target, s)
                for c in g.ifs:
                    self.expr(c, s)
            if isinstance(e, ast.DictComp):
                self.expr(e.key, s)
                self.expr(e.value, s)
            else:
                self.expr(e.elt, s)
            return
        if isinstance(e, ast.NamedExpr):
            tgt = scope
            while tgt.kind == "function" and isinstance(tgt.node, COMPS):
                tgt = tgt.parent
            self.bind(tgt, e.target.id)
            self.expr(e.value, scope)
            return
        for c in ast.iter_child_nodes(e):
            if isinstance(c, ast.expr):
                self.expr(c, scope)
            elif isinstance(c, ast.comprehension):
                pass
            else:
                for cc in ast.walk(c):
                    if isinstance(cc, ast.expr):
                        self.expr(cc, scope)
                        break

    def args_outer(self, a, scope):
        for d in a.defaults + [d for d in a.kw_defaults if d is not None]:
            self.expr(d, scope)

    def args_inner(self, a, s):
        for x in a.posonlyargs + a.args + a.kwonlyargs + [a.vararg, a.kwarg]:
            if x is not None:
                self.bind(s, x.arg)

    def annotations(self, a, scope):
        for x in a.posonlyargs + a.args + a.kwonlyargs + [a.vararg, a.kwarg]:
            if x is not None and x.annotation is not None:
                self.expr(x.annotation, scope)

    def visit(self, st, scope):
        if isinstance(st, FUNCS) or isinstance(st, ast.ClassDef):
            for d in st.decorator_list:
                self.expr(d, scope)
            for tp in getattr(st, "type_params", []) or []:
                for c in ast.iter_child_nodes(tp):
                    if isinstance(c, ast.expr):
                        self.expr(c, scope)
            self.bind(scope, st.name)
            scope.defs[st.name].add(start_of(st))
            if isinstance(st, ast.ClassDef):
                for b in st.bases + [k.value for k in st.keywords]:
                    self.expr(b, scope)
                s = Scope("class", scope, st)
            else:
                self.args_outer(st.args, scope)
                self.annotations(st.args, scope)
                self.expr(st.returns, scope)
                s = Scope("function", scope, st)
                self.args_inner(st.args, s)
            for b in st.body:
                self.visit(b, s)
            return
        if isinstance(st, ast.Global):
            scope.glob.update(st.names)
            return
        if isinstance(st, ast.Nonlocal):
            scope.nonloc.update(st.names)
            return
        if isinstance(st, (ast.Import, ast.ImportFrom)):
            for al in st.names:
                nm = al.asname or al.name.split(".")[0]
                if nm != "*":
                    self.bind(scope, nm)
            return
        if isinstance(st, ast.TypeAlias):
            self.bind(scope, st.name.id)
            self.expr(st.value, scope)
            return
        # generic statement: bind its targets, visit its expressions and nested statements
        for field, val in ast.iter_fields(st):
            vals = val if isinstance(val, list) else [val]
            for v in vals:
                if isinstance(v, ast.stmt):
                    self.visit(v, scope)
                elif isinstance(v, ast.expr):
                    self.expr(v, scope)
                elif isinstance(v, ast.excepthandler):
                    if v.name:
                        self.bind(scope, v.name)
                    self.expr(v.type, scope)
                    for b in v.body:
                        self.visit(b, scope)
                elif isinstance(v, ast.withitem):
                    self.expr(v.context_expr, scope)
                    self.expr(v.optional_vars, scope)
                elif isinstance(v, ast.match_case):
                    self.targets(v.pattern, scope)
                    for n in ast.walk(v.pattern):
                        if isinstance(n, ast.expr):
                            self.expr(n, scope)
                    self.expr(v.guard, scope)
                    for b in v.body:
                        self.visit(b, scope)
                elif isinstance(v, ast.comprehension):
                    pass


def all_scopes(s):
    yield s
    for c in s.children:
        yield from all_scopes(c)


def global_defs(module, name):
    out = set(module.defs.get(name, ()))
    for s in all_scopes(module):
        if s is not module and name in s.glob:
            out |= s.defs.get(name, set())
    return out


def nonlocal_rebinders(owner, name):
    """defs of `name` in nested function scopes whose `nonlocal name` reaches owner."""
    out = set()
    for s in all_scopes(owner):
        if s is owner or name not in s.nonloc:
            continue
        p = s.parent
        while p is not None:
            if p.kind == "function" and name in p.locals and name not in p.nonloc:
                break
            p = p.parent
        if p is owner:
            out |= s.defs.get(name, set())
    return out


def resolve(name, scope, module):
    s = scope
    if name in s.glob or s.kind == "module":
        return global_defs(module, name)
    if name in s.locals and name not in s.nonloc:
        if s.kind == "class":
            return set(s.defs.get(name, ())) | global_defs(module, name)
        return set(s.defs.get(name, ())) | nonlocal_rebinders(s, name)
    # free
    p = s.parent
    while p is not None:
        if p.kind == "module":
            return global_defs(module, name)
        if p.kind == "class":
            p = p.parent
            continue
        if name in p.glob:
            return global_defs(module, name)
        if name in p.locals and name not in p.nonloc:
            return set(p.defs.get(name, ())) | nonlocal_rebinders(p, name)
        p = p.parent
    return global_defs(module, name)


def main():
    root, tsv = sys.argv[1], sys.argv[2]
    con = sqlite3.connect(f"file:{root}/.code-graph/index.db?mode=ro", uri=True)
    cache = {}
    verdicts = Counter()
    out = []
    for line in open(tsv):
        rel, sf, st, sq, s0, tf, tt, tq, t0, conf, meta, lang = line.rstrip("\n").split("\t")
        if lang != "python" or rel not in ("calls", "references"):
            verdicts[("other", rel)] += 1
            continue
        s0, t0 = int(s0), int(t0)
        srow = con.execute("SELECT n.end_line FROM nodes n JOIN files f ON f.id=n.file_id WHERE f.path=? AND COALESCE(n.qualified_name,n.name)=? AND n.type=? AND n.start_line=?", (sf, sq, st, s0)).fetchone()
        trow = con.execute("SELECT n.name FROM nodes n JOIN files f ON f.id=n.file_id WHERE f.path=? AND COALESCE(n.qualified_name,n.name)=? AND n.type=? AND n.start_line=?", (tf, tq, tt, t0)).fetchone()
        s1 = srow[0] if srow else 10 ** 9
        if st == "module":
            s1 = 10 ** 9
        name = trow[0]
        if sf != tf:
            verdicts[(rel, "cross-file")] += 1
            out.append(f"{rel} CROSSFILE {sf}:{sq}@{s0} -> {tf}:{tq}@{t0}")
            continue
        if sf not in cache:
            src = open(os.path.join(root, sf), encoding="utf-8", errors="replace").read()
            b = Builder()
            try:
                mod = b.run(ast.parse(src))
            except SyntaxError:
                cache[sf] = None
            else:
                cache[sf] = (b, mod)
        if cache[sf] is None:
            verdicts[(rel, "parse-fail")] += 1
            continue
        b, mod = cache[sf]
        occs = [(ln, sc) for (nm, ln, is_call, sc) in b.occ
                if nm == name and s0 <= ln <= s1 and (is_call if rel == "calls" else True)]
        if not occs:
            v = "NOOCC"
        else:
            v = "WRONG"
            for ln, sc in occs:
                if t0 in resolve(name, sc, mod):
                    v = "RIGHT"
                    break
        verdicts[(rel, v)] += 1
        out.append(f"{rel} {v} {sf}:{sq}({st})@{s0}-{s1} -> {tq}({tt})@{t0} occ={[o[0] for o in occs][:6]}")
    print(dict(sorted(verdicts.items())))
    for o in out:
        if os.environ.get("ALL") or not o.split()[1] == "WRONG":
            print("  ", o)
    return out


if __name__ == "__main__":
    main()
