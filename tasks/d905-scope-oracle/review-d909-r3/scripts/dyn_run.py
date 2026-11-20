"""dyn_run.py <dir> <out.json>: run every .py file of <dir> statement by statement
(each top-level statement exec'd on its own, exceptions swallowed) as a module named
by its path, then call each module-level function twice (no args, and what it returns
if callable) and instantiate each module-level class and call its methods. Records
every Python-level call between code of these files: call-site file:line -> callee
file:firstline name qualname."""
import ast, json, os, sys, types
D = os.path.abspath(sys.argv[1]); OUT = sys.argv[2]
sys.path.insert(0, D)
events = set()
def rel(fn):
    return os.path.relpath(fn, D) if fn.startswith(D + os.sep) else None
def prof(frame, event, arg):
    if event != "call":
        return
    c = frame.f_code; cf = rel(c.co_filename)
    b = frame.f_back
    if cf is None or b is None:
        return
    bf = rel(b.f_code.co_filename)
    if bf is None:
        return
    events.add((bf, b.f_lineno, cf, c.co_firstlineno, c.co_name, getattr(c, "co_qualname", c.co_name)))
files = sorted(os.path.relpath(os.path.join(r, f), D) for r, _, fs in os.walk(D) for f in fs if f.endswith(".py") and ".code-graph" not in r)
sys.setrecursionlimit(200)
for p in files:
    modname = p[:-3].replace(os.sep, ".")
    if modname.endswith(".__init__"):
        modname = modname[: -len(".__init__")]
    if modname in sys.modules:
        mod = sys.modules[modname]
    else:
        mod = types.ModuleType(modname); mod.__file__ = os.path.join(D, p); sys.modules[modname] = mod
        if p.endswith("__init__.py"):
            mod.__path__ = [os.path.dirname(os.path.join(D, p))]
    src = open(os.path.join(D, p)).read()
    try:
        tree = ast.parse(src)
    except SyntaxError:
        continue
    sys.setprofile(prof)
    try:
        for st in tree.body:
            try:
                exec(compile(ast.Module([st], []), os.path.join(D, p), "exec"), mod.__dict__)
            except BaseException:
                pass
        for _ in range(2):
            for k, v in list(mod.__dict__.items()):
                try:
                    if isinstance(v, types.FunctionType) and rel(v.__code__.co_filename) == p:
                        r = v()
                        if callable(r) and not isinstance(r, type):
                            try: r()
                            except BaseException: pass
                    elif isinstance(v, type) and getattr(v, "__module__", None) == modname:
                        try: o = v()
                        except BaseException: o = None
                        for mk, mv in list(vars(v).items()):
                            if isinstance(mv, types.FunctionType) and o is not None and mk != "__init__":
                                try: getattr(o, mk)()
                                except BaseException: pass
                except BaseException:
                    pass
    finally:
        sys.setprofile(None)
json.dump(sorted(events), open(OUT, "w"))
