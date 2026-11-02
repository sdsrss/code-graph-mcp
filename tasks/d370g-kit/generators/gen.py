import sys
kind, n = sys.argv[1], int(sys.argv[2])
out = ["class K0:", "    def m0(self): pass"]
for i in range(1, n + 1):
    if kind == "wide":
        bases = ", ".join(f"K{j}" for j in range(i - 1, -1, -1))
    else:
        bases = ", ".join(f"K{j}" for j in (i - 1, i - 2) if j >= 0)
    out += [f"class K{i}({bases}): pass"]
out += ["def use(o):", f"    K{n}.m0(o)"]
print("\n".join(out))
