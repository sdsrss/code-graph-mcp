#!/usr/bin/env python3
"""dyn.py <cases.py> [substr...]: exec each case's m.py as a module, call every
module-level function twice (no args; TypeError etc. swallowed), and print the
call edges CPython actually made: caller name@firstline -> callee name@firstline."""
import importlib.util, sys, types
spec = importlib.util.spec_from_file_location("cases", sys.argv[1]); mod = importlib.util.module_from_spec(spec); spec.loader.exec_module(mod)
pick = sys.argv[2:]
for name, files in mod.CASES.items():
    if pick and not any(p in name for p in pick):
        continue
    src = files["m.py"]
    edges = set()
    def prof(frame, event, arg):
        if event == "call" and frame.f_code.co_filename == "m.py" and frame.f_back is not None and frame.f_back.f_code.co_filename == "m.py":
            b = frame.f_back.f_code
            c = frame.f_code
            if c.co_name in ("<lambda>", "<listcomp>", "<genexpr>"):
                return
            edges.add(f"{b.co_name if b.co_name != '<module>' else '<module>'}@{b.co_firstlineno} -> {c.co_name}@{c.co_firstlineno}")
    g = {"__name__": "m"}
    sys.setprofile(prof)
    try:
        exec(compile(src, "m.py", "exec"), g)
        fns = [v for k, v in list(g.items()) if isinstance(v, types.FunctionType) and v.__code__.co_filename == "m.py"]
        for _ in range(2):
            for fn in fns:
                try:
                    r = fn()
                    if callable(r):
                        try: r()
                        except Exception: pass
                except Exception as e:
                    pass
    except Exception as e:
        edges.add(f"EXC {type(e).__name__}: {e}")
    finally:
        sys.setprofile(None)
    print(f"=== {name}")
    for e in sorted(edges):
        print("   ", e)
