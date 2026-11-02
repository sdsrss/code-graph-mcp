import sys
kind, n = sys.argv[1], int(sys.argv[2])
out = ["class K0:", "    def m0(self): pass"]
if kind in ("wide", "fib"):
    for i in range(1, n + 1):
        js = range(i - 1, -1, -1) if kind == "wide" else [j for j in (i - 1, i - 2) if j >= 0]
        out.append(f"class K{i}({', '.join(f'K{j}' for j in js)}): pass")
    out += ["def use(o):", f"    K{n}.m0(o)"]
else:  # capwide: a chain of 62, then n classes deriving from all of it
    for i in range(1, 62):
        out.append(f"class K{i}(K{i-1}): pass")
    bases = ", ".join(f"K{j}" for j in range(61, -1, -1))
    for x in range(n):
        out.append(f"class X{x}({bases}): pass")
    out.append("def use(o):")
    out += [f"    X{x}.m0(o)" for x in range(n)]
print("\n".join(out))
