import os
D = "/var/tmp/d909-review/cost"


def put(name, text):
    d = os.path.join(D, name)
    os.makedirs(d, exist_ok=True)
    assert len(text.encode()) < 1 << 20, (name, len(text))
    open(os.path.join(d, "m.py"), "w").write(text)
    open(os.path.join(d, "other.py"), "w").write("class Row:\n    pass\n\n\ndef index():\n    pass\n")


# K1: 2,900 classes at the bottom of 150 nested `if` blocks (1-space indent)
DEPTH = 150
head = "".join(" " * i + "if x:\n" for i in range(DEPTH))
sp = " " * DEPTH
body = "".join(f"{sp}class C{i}:\n{sp} pass\n" for i in range(2900))
put("k1_classes_deep150", "x = 1\n" + head + body)
# K1d: the same with defs (base already computes a lexical scope per def)
body = "".join(f"{sp}def C{i}():\n{sp} pass\n" for i in range(2900))
put("k1d_defs_deep150", "x = 1\n" + head + body)
# K2: one class per level of a 400-deep `if` chain, the chain repeated
chain = "".join(" " * i + f"if x:\n{' ' * (i + 1)}class L{i}:\n{' ' * (i + 2)}pass\n" for i in range(400))
put("k2_class_per_level400", "x = 1\n" + chain * max(1, (1_000_000 // len(chain))))
# K3: classes nested in classes 300 deep inside one function (walk stops at the first class)
s = "def f():\n"
for i in range(300):
    s += " " * (i + 1) + f"class N{i}:\n"
s += " " * 301 + "pass\n"
put("k3_class_in_class300", s * max(1, (1_000_000 // len(s))))
# K4: functions nested 100 deep, 20 annotated parameters each, each level nests a class Row
s = ""
for i in range(100):
    params = ", ".join(f"p{j}: Row" for j in range(20))
    s += "    " * i + f"def g{i}({params}) -> Row:\n" + "    " * (i + 1) + "class Row:\n" + "    " * (i + 2) + "pass\n"
s += "    " * 100 + "return Row\n"
put("k4_annot_defs100", s * max(1, (1_000_000 // len(s))))
# K5: generic annotations nested 150 deep in headers
ann = "def h(x: " + "Dict[Row, " * 150 + "Row" + "]" * 150 + ") -> Row:\n    class Row:\n        pass\n    return x\n"
put("k5_generic_ann150", "Dict = dict\n\n" + ann * (480_000 // len(ann)))
# K6: references only (no bare calls): a module of value reads of defined names
put("k6_refs_only", "def index():\n    pass\n\n\nclass Row:\n    pass\n\n\n" + "".join(f"v{i} = [index, Row]\n" for i in range(45000)))
# K7: the D#909 shape at scale: 600 functions each nesting def index and passing it
put("k7_nested_index600", "".join(f"def t{i}(app):\n    def index():\n        pass\n    app.add(view_func=index)\n\n\n" for i in range(600)))

# within MAX_AST_DEPTH (64): 30 nested ifs = 60 tree levels
DEPTH = 30
head = "".join(" " * i + "if x:\n" for i in range(DEPTH))
sp = " " * DEPTH
put("k1b_classes_deep30", "x = 1\n" + head + "".join(f"{sp}class C{i}:\n{sp} pass\n" for i in range(12500)))
put("k1bd_defs_deep30", "x = 1\n" + head + "".join(f"{sp}def C{i}():\n{sp} pass\n" for i in range(12500)))
chain = "".join(" " * i + f"if x:\n{' ' * (i + 1)}class L{i}:\n{' ' * (i + 2)}pass\n" for i in range(30))
put("k2b_class_per_level30", "x = 1\n" + chain * (990_000 // len(chain)))
s = ""
for i in range(30):
    params = ", ".join(f"p{j}: Row" for j in range(20))
    s += "    " * i + f"def g{i}({params}) -> Row:\n" + "    " * (i + 1) + "class Row:\n" + "    " * (i + 2) + "pass\n"
s += "    " * 30 + "return Row\n"
put("k4b_annot_defs30", s * (990_000 // len(s)))
line = "def h(p=" + "a(" * 150 + "x" + ")" * 150 + "):\n    pass\n"
put("k8_default_calls150", "def a(*x):\n    pass\n\n\n" + line * (120_000 // len(line)))

# K9: 50,000 top-level classes (one block); K9d: the same with defs
put("k9_flat_classes", "".join(f"class C{i}: pass\n" for i in range(50000)))
put("k9d_flat_defs", "".join(f"def C{i}(): pass\n" for i in range(50000)))

# K10: review-r1 c1_calls150 (calls nested 150 deep, ~120 KB)
line = "def h():\n    return " + "a(" * 150 + "x" + ")" * 150 + "\n"
put("k10_calls150", line * (120_000 // len(line)))
# K11: c14-like: functions nested 100 deep, 20 annotated parameters each (~988 KB)
s = ""
for i in range(100):
    params = ", ".join(f"p{j}: Model" for j in range(20))
    s += "    " * i + f"def g{i}({params}) -> Model:\n"
s += "    " * 100 + "return v\n"
put("k11_annot_defs100", s * max(1, (990_000 // len(s))))
for n in sorted(os.listdir(D)):
    print(n, os.path.getsize(os.path.join(D, n, "m.py")))
