#!/usr/bin/env python3
"""Pick headroom-pilot round-2 targets (D#749) from a Python corpus by its AST.

Criteria are fixed before any session runs; the printed truth is then checked
by hand (grep every member) before it is written to the truth file.

  fanout     one def of the name in the corpus, not a dunder, >= 10 characters
             with an underscore (project vocabulary, not a method a library
             object could also have), never read as a value or named in a
             string, 40..90 call sites (whole corpus, tests too)

  transitive a def G (one def of its name) whose upward closure over production
             code (not tests) has 8..25 functions, depth >= 2, >= 4 of them at
             depth >= 2. Each call is resolved to a def by targets():
               bare name   innermost enclosing FUNCTION scope that defines or
                           binds it, then the module: a def there is the target;
                           an assignment or parameter makes it a local value (no
                           target); an import resolves through its module path
                           (outside the project: no target); unbound -> every
                           def of the name
               self./cls.  the same class chain in the same file, else every def
               other       only a unique project-vocabulary name, else every def
                           plus a library method of that name
             A call that may reach a member and something else, a member read
             as a value or named in a string in production code, or a
             module-level call rejects the candidate, so "calls G directly or
             indirectly" has one answer. Calls through an import alias count.

A call site is (file, line of the callee name token). Usage:

    select.py CORPUS [--root-prefix backend/] [--json-out FILE]
"""
import argparse
import ast
import collections
import json
import posixpath
import re
from pathlib import Path

TEST_PART = re.compile(r"(^|/)(tests?|testing)(/|$)|(^|/)test_[^/]*\.py$|_test\.py$|conftest\.py$")


def is_test(path):
    return bool(TEST_PART.search(path))


class Corpus:
    def __init__(self, root, prefix=""):
        self.defs = collections.defaultdict(list)     # name -> [(file, qual, line)]
        self.calls = collections.defaultdict(list)    # name -> [(file, line, caller qual, caller name, recv)]
        self.reads = collections.Counter()            # name read not as a callee (all files)
        self.strings = collections.Counter()          # word inside a string literal (all files)
        self.prod_reads = collections.Counter()
        self.prod_strings = collections.Counter()
        self.binds = collections.defaultdict(dict)    # (file, function qual or "") -> name -> bind
        self.fn_scopes = set()                        # (file, qual) of every def
        self.files = set()
        for p in sorted(Path(root).rglob("*.py")):
            rel = p.relative_to(root).as_posix()
            if rel.startswith(prefix):
                try:
                    tree = ast.parse(p.read_text(errors="replace"))
                except (SyntaxError, ValueError):
                    continue
                self.files.add(rel)
                self._file(rel, tree)

    # -- scanning ---------------------------------------------------------
    def _bind(self, rel, scope, node):
        b = self.binds[(rel, scope)]
        if isinstance(node, ast.ImportFrom):
            for a in node.names:
                b[a.asname or a.name] = ("import", node.module or "", a.name, node.level)
        elif isinstance(node, ast.Import):
            for a in node.names:
                b[(a.asname or a.name).split(".")[0]] = ("value",)
        elif isinstance(node, ast.arguments):
            for a in node.posonlyargs + node.args + node.kwonlyargs + [x for x in (node.vararg, node.kwarg) if x]:
                b[a.arg] = ("value",)
        else:
            for n in ast.walk(node):
                if isinstance(n, ast.Name) and isinstance(n.ctx, ast.Store):
                    b[n.id] = ("value",)

    def _file(self, rel, tree):
        prod = not is_test(rel)
        callee_nodes = {id(n.func) for n in ast.walk(tree) if isinstance(n, ast.Call)}
        for n in ast.walk(tree):
            if isinstance(n, ast.Constant) and isinstance(n.value, str):
                for w in re.findall(r"[A-Za-z_]\w{5,}", n.value):
                    self.strings[w] += 1
                    self.prod_strings[w] += prod

        def fn_scope(stack):
            return ".".join(s[0] for s in stack) if stack and stack[-1][1] == "def" else (
                "" if not stack else None)  # None: a class body (its names are not visible to methods)

        def expr(node, stack):
            for n in ast.walk(node):
                if isinstance(n, ast.Call):
                    f = n.func
                    name = f.id if isinstance(f, ast.Name) else f.attr if isinstance(f, ast.Attribute) else None
                    if name:
                        line = f.lineno if isinstance(f, ast.Name) else f.end_lineno
                        fns = [s for s in stack if s[1] == "def"]
                        caller = ".".join(s[0] for s in stack) if fns else "<module>"
                        recv = "bare" if isinstance(f, ast.Name) else (
                            f.value.id if isinstance(f.value, ast.Name) and f.value.id in ("self", "cls") else "other")
                        self.calls[name].append((rel, line, caller, fns[-1][0] if fns else None, recv))
                elif isinstance(n, ast.Name) and isinstance(n.ctx, ast.Load) and id(n) not in callee_nodes:
                    self.reads[n.id] += 1
                    self.prod_reads[n.id] += prod
                elif isinstance(n, ast.Attribute) and isinstance(n.ctx, ast.Load) and id(n) not in callee_nodes:
                    self.reads[n.attr] += 1
                    self.prod_reads[n.attr] += prod

        def stmts(body, stack):
            scope = fn_scope(stack)
            for st in body:
                if isinstance(st, (ast.FunctionDef, ast.AsyncFunctionDef)):
                    qual = ".".join([s[0] for s in stack] + [st.name])
                    self.defs[st.name].append((rel, qual, st.lineno))
                    self.fn_scopes.add((rel, qual))
                    self._bind(rel, qual, st.args)
                    for d in st.decorator_list + list(st.args.defaults) + [d for d in st.args.kw_defaults if d]:
                        expr(d, stack)
                    stmts(st.body, stack + [(st.name, "def")])
                elif isinstance(st, ast.ClassDef):
                    for d in st.decorator_list + st.bases + [k.value for k in st.keywords]:
                        expr(d, stack)
                    stmts(st.body, stack + [(st.name, "class")])
                else:
                    if scope is not None:
                        if isinstance(st, (ast.Import, ast.ImportFrom)):
                            self._bind(rel, scope, st)
                        elif isinstance(st, (ast.For, ast.AsyncFor)):
                            self._bind(rel, scope, st.target)
                        elif isinstance(st, (ast.With, ast.AsyncWith)):
                            for it in st.items:
                                if it.optional_vars is not None:
                                    self._bind(rel, scope, it.optional_vars)
                        elif isinstance(st, (ast.Assign, ast.AnnAssign, ast.AugAssign)):
                            self._bind(rel, scope, st)
                    for _field, value in ast.iter_fields(st):
                        if isinstance(value, list) and value and isinstance(value[0], ast.stmt):
                            stmts(value, stack)
                        elif isinstance(value, list):
                            for v in value:
                                if isinstance(v, ast.excepthandler):
                                    if v.name and scope is not None:
                                        self.binds[(rel, scope)][v.name] = ("value",)
                                    if v.type:
                                        expr(v.type, stack)
                                    stmts(v.body, stack)
                                elif isinstance(v, ast.match_case):
                                    stmts(v.body, stack)
                                elif isinstance(v, ast.AST):
                                    expr(v, stack)
                        elif isinstance(value, ast.AST) and not isinstance(value, ast.stmt):
                            expr(value, stack)

        stmts(tree.body, [])

    # -- resolution -------------------------------------------------------
    def unique_ok(self, name):
        return (len(self.defs.get(name, [])) == 1 and not name.startswith("__") and len(name) >= 10
                and "_" in name.strip("_") and self.reads[name] == 0 and self.strings[name] == 0)

    def _module_files(self, f, module, level):
        if level:
            base = posixpath.dirname(f)
            for _ in range(level - 1):
                base = posixpath.dirname(base)
            stem = posixpath.join(base, module.replace(".", "/")) if module else base
            return {stem + ".py", stem + "/__init__.py"}
        tail = module.replace(".", "/")
        return {p for p in self.files if p.endswith("/" + tail + ".py") or p.endswith("/" + tail + "/__init__.py")
                or p in (tail + ".py", tail + "/__init__.py")}

    def _import_target(self, f, bind):
        _kind, module, orig, level = bind
        ds = [(d[0], d[1]) for d in self.defs.get(orig, [])]
        files = self._module_files(f, module, level)
        hit = [d for d in ds if d[0] in files and d[1] == orig]
        if hit:
            return hit
        # a package __init__ that re-exports: accept the project's only def of the name
        if any(p in self.files for p in files) and len(ds) == 1 and "." not in ds[0][1]:
            return ds
        return []

    def targets(self, call, name):
        f, _line, caller_q, _caller, recv = call
        ds = [(d[0], d[1]) for d in self.defs.get(name, [])]
        if recv == "bare":
            parts = caller_q.split(".") if caller_q != "<module>" else []
            for k in range(len(parts), -1, -1):
                prefix = ".".join(parts[:k])
                if k and (f, prefix) not in self.fn_scopes:
                    continue  # a class body: invisible to the functions inside it
                want = f"{prefix}.{name}" if prefix else name
                if (f, want) in ds:
                    return [(f, want)]
                bind = self.binds.get((f, prefix), {}).get(name)
                if bind:
                    return [] if bind[0] == "value" else self._import_target(f, bind)
            return ds
        if recv in ("self", "cls"):
            parts = caller_q.split(".")
            for k in range(len(parts) - 1, 0, -1):
                want = ".".join(parts[:k] + [name])
                if (f, want) in ds:
                    return [(f, want)]
            return ds
        return ds if (len(ds) == 1 and self.unique_ok(name)) else ds + [("<library>", name)]

    def aliases(self, name):
        return {alias for (_f, _s), b in self.binds.items() for alias, v in b.items()
                if v[0] == "import" and v[2] == name and alias != name}

    def closure(self, g_id, limit=40):
        depth = {g_id: 0}
        frontier = [g_id]
        while frontier:
            nxt = []
            for m in frontier:
                name = m[1].rsplit(".", 1)[-1]
                if self.prod_reads[name] or self.prod_strings[name] or name.startswith("__"):
                    return None, f"{name} read as a value or named in a string"
                for used in {name} | self.aliases(name):
                    for c in self.calls.get(used, []):
                        if is_test(c[0]):
                            continue
                        t = self.targets(c, used)
                        if used != name:  # an alias: the call reaches what the alias imports
                            t = t if t else []
                        if m not in t:
                            continue
                        if len(t) > 1:
                            return None, f"ambiguous call of {name} at {c[0]}:{c[1]}"
                        if c[3] is None:
                            return None, f"module-level call of {name}"
                        caller_id = (c[0], c[2])
                        if caller_id not in depth:
                            depth[caller_id] = depth[m] + 1
                            nxt.append(caller_id)
            frontier = nxt
            if len(depth) > limit:
                return None, "too large"
        return depth, None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("corpus")
    ap.add_argument("--root-prefix", default="")
    ap.add_argument("--json-out")
    a = ap.parse_args()
    c = Corpus(a.corpus, a.root_prefix)
    fan = []
    for name, cs in c.calls.items():
        if c.unique_ok(name) and 40 <= len(cs) <= 90:
            fan.append((len(cs), name, c.defs[name][0], sum(1 for x in cs if is_test(x[0]))))
    print("== fanout candidates (sites, name, def, test sites)")
    for r in sorted(fan, reverse=True):
        print("  ", r)
    trans = []
    for g, gdefs in c.defs.items():
        if len(gdefs) != 1 or is_test(gdefs[0][0]) or g.startswith("__"):
            continue
        g_id = (gdefs[0][0], gdefs[0][1])
        depth, _why = c.closure(g_id)
        if depth is None:
            continue
        members = {k: v for k, v in depth.items() if k != g_id}
        if 8 <= len(members) <= 25 and max(members.values(), default=0) >= 2 \
                and sum(1 for v in members.values() if v >= 2) >= 4:
            trans.append((len(members), max(members.values()), g, gdefs[0],
                          sorted(members.items(), key=lambda x: (x[1], x[0]))))
    print("== transitive candidates (members, max depth, G, def)")
    for r in sorted(trans, reverse=True):
        print("  ", r[:4])
    if a.json_out:
        json.dump({"fanout": fan, "transitive": trans}, open(a.json_out, "w"), indent=1, default=str)


if __name__ == "__main__":
    main()
