import sys, os
# gen_metas2.py MAXBYTES OUTDIR: the longest chain of classes, each with its own unbound metaclass name, that fits MAXBYTES
cap, out = int(sys.argv[1]), sys.argv[2]
os.makedirs(out, exist_ok=True)
tail = "def t_call():\n    return A0.m(None)\n"
parts = ["class A0(metaclass=M0):\n    def m(self): ...\n"]; size = len(parts[0]) + len(tail); i = 1
while True:
    s = f"class A{i}(A{i-1},metaclass=M{i}):...\n"
    if size + len(s) > cap: break
    parts.append(s); size += len(s); i += 1
parts.append(tail)
open(os.path.join(out, "big.py"), "w").write("".join(parts))
print(out, i, "classes", size, "bytes")
