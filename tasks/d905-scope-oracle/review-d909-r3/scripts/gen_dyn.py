"""Round-3 reviewer generator (initial/step): disagreement shapes (header,
lambda/comprehension defaults, decorator calls), class scopes (nested classes,
class bodies calling before/after a method namesake, classes in classes),
global/nonlocal incl. in class bodies, local-value calls (parameter namesake),
imports and star imports of the same names, cross-file namesakes, moves."""
FILES = ["m.py", "lib.py", "pkg/x.py", "use.py"]
N = ["h", "Row", "make"]


def ind(s, n):
    return "".join((" " * n + l if l else l) + "\n" for l in s.rstrip("\n").split("\n"))


def d(rng, nm):
    if nm == "Row" or rng.random() < 0.2:
        return f"class {nm}:\n    def __init__(self, *a, **k):\n        pass\n"
    return f"def {nm}(*a, **k):\n    return 1\n"


def blk(rng, nm):
    r = rng.randrange(22)
    k = rng.randrange(3)
    if r == 0:
        return d(rng, nm)
    if r == 1:  # header default, nested namesake, body call too
        return f"def hd_{k}(x={nm}()):\n" + ind(d(rng, nm), 4) + f"    return {nm}()\n"
    if r == 2:  # lambda default
        return f"def lam_{k}(cb=lambda: {nm}()):\n" + ind(d(rng, nm), 4) + "    return cb()\n"
    if r == 3:  # comprehension default
        return f"def comp_{k}(x=[{nm}() for _ in (1,)]):\n" + ind(d(rng, nm), 4) + "    return x\n"
    if r == 4:  # decorator call on a def whose body defines the name
        return f"@{nm}()\ndef dc_{k}():\n" + ind(d(rng, nm), 4) + f"    return {nm}()\n"
    if r == 5:  # method default, class body defines the name, nested namesake
        return (f"class E{k}:\n    def {nm}(self, *a):\n        return 1\n"
                f"    def m(self, x={nm}()):\n" + ind(d(rng, nm), 8) + f"        return {nm}()\n")
    if r == 6:  # class body call before / after its method namesake
        pre = rng.random() < 0.5
        body = f"    x = {nm}()\n" if pre else ""
        body += f"    def {nm}(self):\n        return 1\n"
        body += "" if pre else f"    y = {nm}()\n"
        return f"class K{k}:\n" + body
    if r == 7:  # function with nested class/def, sibling nested fn and nested class body call
        return (f"def F{k}():\n" + ind(d(rng, nm), 4) +
                f"    def g():\n        return {nm}()\n"
                f"    class In:\n        z = {nm}()\n        def q(self):\n            return {nm}()\n"
                "    return g()\n")
    if r == 8:  # global + nested def, call after
        g = rng.choice([f"    global {nm}\n", f"    global zz, {nm}\n", ""])
        return f"def gl_{k}():\n{g}" + ind(d(rng, nm), 4) + f"    return {nm}()\n"
    if r == 9:  # nonlocal
        return (f"def outer_{k}():\n" + ind(d(rng, nm), 4) + "    def inner():\n" +
                rng.choice([f"        nonlocal {nm}\n", ""]) + ind(d(rng, nm), 8) +
                f"        return {nm}()\n    inner()\n    return {nm}()\n")
    if r == 10:  # global in a class body with a method namesake
        return f"class G{k}:\n    global {nm}\n    def {nm}(self, *a):\n        return 1\n"
    if r == 11:
        return rng.choice([f"from lib import {nm}\n", "from pkg.x import *\n", f"from pkg.x import {nm}\n",
                           f"from lib import {nm} as {nm}2\n"])
    if r == 12:  # local value call: parameter namesake
        return f"def p_{k}({nm}=None):\n    return {nm}()\n"
    if r == 13:  # module-level call and alias
        return rng.choice([f"v_{k} = {nm}()\n", f"{nm}_alias = {nm}\n", f"{nm} = {nm}\n"])
    if r == 14:  # conditional module-level def
        return "if True:\n" + ind(d(rng, nm), 4)
    if r == 15:  # async nested
        return f"async def a_{k}():\n    async def {nm}():\n        return 1\n    return await {nm}()\n"
    if r == 16:  # decorated nested def in a function, called
        return f"def dn_{k}():\n    @staticmethod\n    def {nm}():\n        return 1\n    return {nm}()\n"
    if r == 17:  # class in class
        return f"class O{k}:\n    class {nm}:\n        def __init__(self, *a, **k):\n            pass\n    def m(self):\n        return {nm}()\n"
    if r == 18:  # call in a function that sees no definition of its own
        return f"def use_{k}():\n    return {nm}()\n"
    if r == 19:  # header annotation and default together, nested class namesake
        return (f"def an_{k}(x: {nm} = {nm}()) -> {nm}:\n" + ind(f"class {nm}:\n    def __init__(self, *a, **k):\n        pass\n", 4) +
                f"    return {nm}()\n")
    if r == 20:  # nested def's own header call inside an outer function that defines the name
        return (f"def w_{k}():\n" + ind(d(rng, nm), 4) +
                f"    def inner(x={nm}()):\n" + ind(d(rng, nm), 8) + "        return x\n    return inner\n")
    return f"global {nm}\n"


def gen_file(rng):
    return "\n\n".join(blk(rng, rng.choice(N)) for _ in range(rng.randint(2, 6)))


def initial(rng):
    st = {p: gen_file(rng) for p in FILES if rng.random() < 0.85}
    st["pkg/__init__.py"] = ""
    return st


def step(state, rng):
    st = dict(state)
    for _ in range(rng.randint(1, 2)):
        p = rng.choice(FILES)
        if p not in st:
            st[p] = gen_file(rng)
            continue
        parts = st[p].split("\n\n")
        r = rng.random()
        if r < 0.12:
            del st[p]
        elif r < 0.3:  # toggle global/nonlocal
            body = st[p]
            if "global " in body and rng.random() < 0.5:
                st[p] = "\n".join(l for l in body.split("\n") if not l.strip().startswith("global "))
            elif "nonlocal" in body:
                st[p] = body.replace("nonlocal", "pass  # nl", 1) if rng.random() < 0.5 else body.replace("pass  # nl", "nonlocal", 1)
            else:
                st[p] = body.replace(":\n    ", f":\n    global {rng.choice(N)}\n    ", 1)
        elif r < 0.42:
            rng.shuffle(parts)
            st[p] = "\n\n".join(parts)
        elif r < 0.55:  # rename a nested namesake
            a, b = rng.sample(N + ["h2"], 2)
            st[p] = st[p].replace(f"    def {a}(", f"    def {b}(", 1).replace(f"    class {a}:", f"    class {b}:", 1)
        elif r < 0.7:  # move a block
            if parts:
                bx = parts.pop(rng.randrange(len(parts)))
                st[p] = "\n\n".join(parts)
                q = rng.choice([f for f in FILES if f != p])
                st[q] = (st[q] + "\n\n" + bx) if st.get(q) else bx
        elif r < 0.8:  # drop a block
            if parts:
                parts.pop(rng.randrange(len(parts)))
                st[p] = "\n\n".join(parts)
        else:
            parts.insert(rng.randrange(len(parts) + 1), blk(rng, rng.choice(N)))
            st[p] = "\n\n".join(parts)
    return st
