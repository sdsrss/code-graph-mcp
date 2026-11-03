import sys, os
# gen_metas.py N OUTDIR: a chain of N classes, each with its own (unbound, so "imported") metaclass name
n, out = int(sys.argv[1]), sys.argv[2]
os.makedirs(out, exist_ok=True)
lines = ["class A0(metaclass=M0):\n    def m(self): ...\n"]
for i in range(1, n):
    lines.append(f"class A{i}(A{i-1},metaclass=M{i}):...\n")
lines.append("def t_call():\n    return A0.m(None)\n")
open(os.path.join(out, "big.py"), "w").write("".join(lines))
print(out, sum(map(len, lines)), "bytes")
