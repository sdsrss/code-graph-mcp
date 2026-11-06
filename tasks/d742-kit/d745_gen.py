import os, sys
root, n = sys.argv[1], int(sys.argv[2])
def w(d, f, s):
    os.makedirs(os.path.join(root, d), exist_ok=True)
    open(os.path.join(root, d, f), "w").write(s)
py_cls = "".join(f"class C{i}:\n    def h(self):\n        pass\n\n" for i in range(n))
js_cls = "".join(f"class C{i} {{\n  h() {{ return 1; }}\n}}\n" for i in range(n))
w("pyfn", "m.py", py_cls + "def g():\n" + "    h()\n" * n)
w("pycb", "m.py", py_cls + "class K:\n" + "    h()\n" * n)
w("pyx", "b.py", py_cls); w("pyx", "a.py", "def g():\n" + "    h()\n" * n)
w("jsfn", "m.js", js_cls + "function g() {\n" + "  h();\n" * n + "}\n")
w("jsx", "b.js", js_cls); w("jsx", "a.js", "function g() {\n" + "  h();\n" * n + "}\n")
w("jsm", "m.js", js_cls + "function g(o) {\n" + "  o.h();\n" * n + "}\n")
