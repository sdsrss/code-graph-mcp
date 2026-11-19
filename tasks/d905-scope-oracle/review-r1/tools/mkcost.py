import os
D = "/var/tmp/d905-review/cost"
LIM = 1_000_000
def put(name, text):
    d = os.path.join(D, name); os.makedirs(d, exist_ok=True)
    assert len(text.encode()) < 1 << 20, (name, len(text))
    open(os.path.join(d, "m.py"), "w").write(text)
    open(os.path.join(d, "other.py"), "w").write("def zz(*a): pass\ndef len(*a): pass\nclass Model: pass\ndef a(*x): pass\ndef v(*x): pass\n")
# C1: calls nested 150 deep, ~120 KB (CHANGELOG case)
line = "def h():\n    return " + "a(" * 150 + "x" + ")" * 150 + "\n"
put("c1_calls150", line * (120_000 // len(line)))
# C2: the unit test's annotation nest (~899 KB)
ann = "def h(x: " + "Dict[A, " * 150 + "A" + "]" * 150 + "):\n    pass\n"
put("c2_ann150", "A = 1\n\n" + ann * (900_000 // len(ann)))
# C3/C4: module-level builtin / non-builtin calls
put("c3_builtin_calls", "len(x)\n" * (LIM // 7))
put("c4_unbound_calls", "zz(x)\n" * (LIM // 6))
# C5: value refs at module level to an unbound non-builtin
put("c5_value_refs", "y = v\n" * (LIM // 6))
# C6: many comprehensions at module level
put("c6_comps", "q = [a for a in b if a]\n" * (LIM // 24))
# C7: big class body
put("c7_class", "class K:\n" + "".join(f"    @property\n    def m{i}(self) -> Model:\n        return len(self)\n    c{i} = staticmethod(zz)\n" for i in range(9000)))
# C8: comprehensions nested 150 deep, many reads inside
inner = "(" + ", ".join(["v"] * 40) + ")"
nest = inner
for i in range(150): nest = f"[{nest} for x{i} in b]"
put("c8_comp150", ("z = " + nest + "\n") * (LIM // (len(nest) + 5)))
# C9: many small functions
put("c9_funcs", "".join(f"def f{i}(p):\n    q = p\n    return v(q, zz, Model)\n" for i in range(18000)))
# C10: lambdas nested 150 deep with reads
lam = "lambda p: " * 150 + "(" + ", ".join(["v"] * 30) + ")"
put("c10_lambda150", ("w = " + lam + "\n") * (LIM // (len(lam) + 5)))
# C11: nested defs 100 deep, each level reading names
s = ""
for i in range(100):
    s += "    " * i + f"def g{i}(p{i}):\n" + "    " * (i + 1) + f"r = [v, zz, len, Model, p{i}]\n"
s += "    " * 100 + "return v\n"
put("c11_defs100", s * max(1, (LIM // len(s))))
for n in sorted(os.listdir(D)): print(n, os.path.getsize(os.path.join(D, n, "m.py")))
