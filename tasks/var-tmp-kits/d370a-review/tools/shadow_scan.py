#!/usr/bin/env python3
"""shadow_scan.py <root>: count `m.f()` calls where m is bound by a plain module import at file level
(import m / import a.b as m) but rebound locally in the enclosing function (param, assignment, loop, with, except)
or rebound at module level by assignment/def/class."""
import ast, os, sys, collections
root = sys.argv[1]
tot = collections.Counter(); ex = []
for dp, dn, fn in os.walk(root):
    if '/.git' in dp or '/.code-graph' in dp: continue
    for f in fn:
        if not f.endswith('.py'): continue
        p = os.path.join(dp, f)
        try: tree = ast.parse(open(p, encoding='utf-8', errors='replace').read())
        except Exception: continue
        mods = set(); frm = set()
        for n in ast.walk(tree):
            if isinstance(n, ast.Import):
                for a in n.names:
                    if a.asname: mods.add(a.asname)
                    elif '.' not in a.name: mods.add(a.name)
            elif isinstance(n, ast.ImportFrom):
                for a in n.names: frm.add(a.asname or a.name)
        mods -= frm
        if not mods: continue
        # module-level rebinding
        modlevel = set()
        for n in tree.body:
            for t in ast.walk(n) if isinstance(n, (ast.Assign, ast.AugAssign, ast.AnnAssign)) else []:
                if isinstance(t, ast.Name) and isinstance(t.ctx, ast.Store): modlevel.add(t.id)
            if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)): modlevel.add(n.name)
        def local_binds(fn):
            b = set()
            for a in fn.args.args + fn.args.kwonlyargs + fn.args.posonlyargs: b.add(a.arg)
            if fn.args.vararg: b.add(fn.args.vararg.arg)
            if fn.args.kwarg: b.add(fn.args.kwarg.arg)
            g = set()
            for n in ast.walk(fn):
                if isinstance(n, ast.Global): g |= set(n.names)
                if isinstance(n, ast.Name) and isinstance(n.ctx, ast.Store): b.add(n.id)
                if isinstance(n, ast.ExceptHandler) and n.name: b.add(n.name)
            return b - g
        def visit(node, shadow):
            for c in ast.iter_child_nodes(node):
                s = shadow
                if isinstance(c, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda)):
                    s = shadow | (local_binds(c) if not isinstance(c, ast.Lambda) else {a.arg for a in c.args.args})
                if isinstance(c, ast.Call) and isinstance(c.func, ast.Attribute) and isinstance(c.func.value, ast.Name) and c.func.value.id in mods:
                    nm = c.func.value.id
                    tot['attr_calls'] += 1
                    if nm in s:
                        tot['shadowed_local'] += 1
                        if len(ex) < 15: ex.append(f"{p[len(root)+1:]}:{c.lineno} {nm}.{c.func.attr}() [local]")
                    elif nm in modlevel:
                        tot['shadowed_module_level'] += 1
                        if len(ex) < 15: ex.append(f"{p[len(root)+1:]}:{c.lineno} {nm}.{c.func.attr}() [module-level rebind]")
                visit(c, s)
        visit(tree, set())
print(dict(tot))
for e in ex: print("  ", e)
