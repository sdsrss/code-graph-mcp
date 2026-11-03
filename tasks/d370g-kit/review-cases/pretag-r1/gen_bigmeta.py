import sys, os
# gen_bigmeta.py TEXT_BYTES MAXBYTES OUTDIR: one metaclass expression M["..."] of TEXT_BYTES, then a single-inheritance chain filling MAXBYTES
t, cap, out = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3]
os.makedirs(out, exist_ok=True)
head = f'class A0(metaclass=M["{"a" * t}"]):\n    def m(self): ...\n'
tail = "def t_call():\n    return A0.m(None)\n"
parts = [head]; size = len(head) + len(tail); i = 1
while True:
    s = f"class A{i}(A{i-1}):...\n"
    if size + len(s) > cap: break
    parts.append(s); size += len(s); i += 1
parts.append(tail)
open(os.path.join(out, "big.py"), "w").write("".join(parts))
print(out, i, "classes", size, "bytes")
