import os, sys
# bigjsg.py ROOT: perfgen's bigjs tree; with "g", each importer also calls gfn(), defined only in lib/g.js and never imported
root, variant = sys.argv[1], sys.argv[2]
d = root
for k in range(400):
    p = os.path.join(d, "lib", f"m{k}.ts"); os.makedirs(os.path.dirname(p), exist_ok=True)
    open(p, "w").write("".join(f"export function a{k}_{j}() {{ return {j}; }}\n" for j in range(30)))
if variant == "g":
    open(os.path.join(d, "lib", "g.js"), "w").write("export function gfn() { return 0; }\n")
for i in range(4000):
    k = i % 400
    names = [f"a{k}_{j}" for j in range(25)]
    body = f"import {{ {', '.join(names)} }} from '../lib/m{k}';\n"
    extra = " + gfn()" if variant == "g" else ""
    body += f"export function u{i}() {{ return {' + '.join(n + '()' for n in names[:3])} + zzz{i % 7}(){extra}; }}\n"
    ext = "tsx" if i % 2 else "ts"
    p = os.path.join(d, "app", f"f{i}.{ext}"); os.makedirs(os.path.dirname(p), exist_ok=True); open(p, "w").write(body)
