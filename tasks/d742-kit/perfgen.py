import os, sys
# perfgen.py ROOT : the timing-table shapes with n=6000, plus a large JS tree for the import scan
root = sys.argv[1]; n = 6000
py_classes = "".join(f"class C{i}:\n    def h(self):\n        pass\n\n" for i in range(n))
js_classes = "".join(f"class C{i} {{\n  h() {{ return 1; }}\n}}\n" for i in range(n))
py_calls = "def g():\n" + "    h()\n" * n
js_calls = "function g(o) {\n" + "  h();\n" * n + "}\n"
js_member = "function g(o) {\n" + "  o.h();\n" * n + "}\n"
shapes = {
  "py_same_def": {"m.py": py_classes + py_calls},
  "py_same_class": {"m.py": py_classes + "class K:\n" + "    h()\n" * n},
  "py_other": {"b.py": py_classes, "a.py": py_calls},
  "js_same_bare": {"m.js": js_classes + js_calls},
  "js_other_bare": {"b.js": js_classes, "a.js": js_calls},
  "js_same_member": {"m.js": js_classes + js_member},
}
for name, files in shapes.items():
    for f, b in files.items():
        p = os.path.join(root, name, f); os.makedirs(os.path.dirname(p), exist_ok=True); open(p, "w").write(b)
# large JS tree: 400 libs x 30 exports, 4000 importers x 25 imports each = 100k import edges
d = os.path.join(root, "bigjs")
for k in range(400):
    p = os.path.join(d, "lib", f"m{k}.ts"); os.makedirs(os.path.dirname(p), exist_ok=True)
    open(p, "w").write("".join(f"export function a{k}_{j}() {{ return {j}; }}\n" for j in range(30)))
for i in range(4000):
    k = i % 400
    names = [f"a{k}_{j}" for j in range(25)]
    body = f"import {{ {', '.join(names)} }} from '../lib/m{k}';\n"
    body += f"export function u{i}() {{ return {' + '.join(n + '()' for n in names[:3])} + zzz{i % 7}(); }}\n"
    ext = "tsx" if i % 2 else "ts"
    p = os.path.join(d, "app", f"f{i}.{ext}"); os.makedirs(os.path.dirname(p), exist_ok=True); open(p, "w").write(body)
