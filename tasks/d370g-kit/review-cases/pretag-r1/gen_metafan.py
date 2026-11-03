import sys, os
# gen_metafan.py K MAXBYTES OUTDIR: a chain of K classes with distinct unbound metaclass names, then classes with the chain's last 63 classes as bases until MAXBYTES
k, cap, out = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3]
os.makedirs(out, exist_ok=True)
parts = ["class A0(metaclass=M0):\n    def m(self): ...\n"]
for i in range(1, k):
    parts.append(f"class A{i}(A{i-1},metaclass=M{i}):...\n")
tail = "def t_call():\n    return A0.m(None)\n"
size = sum(map(len, parts)) + len(tail)
bases = ",".join(f"A{i}" for i in range(k - 63, k))
j = 0
while True:
    s = f"class X{j}({bases}):...\n"
    if size + len(s) > cap: break
    parts.append(s); size += len(s); j += 1
parts.append(tail)
open(os.path.join(out, "big.py"), "w").write("".join(parts))
print(out, k, "chain +", j, "fan classes", size, "bytes")
