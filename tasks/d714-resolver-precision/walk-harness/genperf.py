import os, sys
root = sys.argv[1]; kind = sys.argv[2]
os.makedirs(root, exist_ok=True)
def w(p, s):
    open(os.path.join(root, p), "w").write(s)
names = []
if kind == "cyclic":
    N = int(os.environ.get("NN", "60"))
    for i in range(N):
        body = "".join(f"export * from './s{j}.js';\n" for j in range(N) if j != i)
        body += f"export function g{i}() {{ return {i}; }}\n"
        w(f"s{i}.js", body)
    w("index.js", "".join(f"export * from './s{j}.js';\n" for j in range(N)))
    names = [f"g{i}" for i in range(0, N, 3)] + [f"missing{i}" for i in range(20)]
elif kind == "dag":
    L, Wd = 8, 20
    for l in range(L):
        for k in range(Wd):
            body = "".join(f"export * from './l{m}_{q}.js';\n" for m in range(l + 1, L) for q in range(Wd))
            body += f"export function h{l}_{k}() {{ return 1; }}\n"
            w(f"l{l}_{k}.js", body)
    w("index.js", "".join(f"export * from './l0_{q}.js';\n" for q in range(Wd)))
    names = [f"h{l}_0" for l in range(L)] + [f"missing{i}" for i in range(20)]
for a in range(10):
    imps = ", ".join(names)
    calls = "; ".join(f"{n}()" for n in names)
    w(f"app{a}.js", f"import {{ {imps} }} from './index.js';\nexport function main{a}() {{ {calls}; }}\n")
