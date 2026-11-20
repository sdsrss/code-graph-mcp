"""Generator for fuzz_seq-style runs: global/nonlocal lines, header defaults /
annotations / decorators beside nested namesakes, nested classes, cross-file
namesakes. initial(rng) / step(state, rng)."""
FILES = ["a.py", "b.py", "pkg/c.py", "pkg/__init__.py"]
N = ["h", "Row", "make"]


def blk(rng, nm):
    r = rng.randrange(14)
    kw = "class" if nm == "Row" else "def"
    tail = ":\n    pass\n" if kw == "class" else "():\n    pass\n"
    if r == 0:
        return f"{kw} {nm}{tail}"
    if r == 1:  # nested def, maybe global
        g = rng.choice(["", f"    global {nm}\n", f"    global x, {nm}\n"])
        return f"def f_{rng.randrange(3)}():\n{g}    {kw} {nm}{tail.replace(chr(10)+'    ', chr(10)+'        ')}    return {nm}\n"
    if r == 2:  # nonlocal
        return (f"def outer_{rng.randrange(3)}():\n    {kw} {nm}{tail.replace(chr(10)+'    ', chr(10)+'        ')}"
                f"    def inner():\n" + rng.choice([f"        nonlocal {nm}\n", ""]) +
                f"        {kw} {nm}{tail.replace(chr(10)+'    ', chr(10)+'            ')}    inner()\n    return {nm}()\n")
    if r == 3:  # default call with nested namesake
        return f"def d_{rng.randrange(3)}(x={nm}()):\n    {kw} {nm}{tail.replace(chr(10)+'    ', chr(10)+'        ')}    return x\n"
    if r == 4:  # annotation with nested namesake
        return f"def an_{rng.randrange(3)}(x: {nm}) -> {nm}:\n    {kw} {nm}{tail.replace(chr(10)+'    ', chr(10)+'        ')}    return x\n"
    if r == 5:  # decorator
        return f"@{nm}\ndef dec_{rng.randrange(3)}():\n    {kw} {nm}{tail.replace(chr(10)+'    ', chr(10)+'        ')}    return 1\n"
    if r == 6:  # plain user
        return f"def use_{rng.randrange(3)}():\n    y = {nm}()\n    return {nm}\n"
    if r == 7:  # class with method namesake and class-body read
        return f"class K{rng.randrange(3)}:\n    x = staticmethod({nm})\n    def {nm}(self):\n        pass\n"
    if r == 8:
        return f"from {rng.choice(['a', 'b', 'pkg.c'])} import {nm}\n"
    if r == 9:  # global without def
        return f"def k_{rng.randrange(3)}():\n    global {nm}\n    {nm} = None\n"
    if r == 10:  # global then call then def
        return f"def gc_{rng.randrange(3)}():\n    global {nm}\n    {nm}()\n    {kw} {nm}{tail.replace(chr(10)+'    ', chr(10)+'        ')}"
    if r == 11:  # nested def in a method, default
        return f"class M{rng.randrange(3)}:\n    def m(self, x={nm}()):\n        def inner(y: {nm}):\n            pass\n        return inner\n"
    if r == 12:  # lambda in default
        return f"def lam_{rng.randrange(3)}(cb=lambda: {nm}()):\n    {kw} {nm}{tail.replace(chr(10)+'    ', chr(10)+'        ')}    return cb\n"
    return f"global {nm}\n"


def gen_file(rng):
    out = [blk(rng, rng.choice(N)) for _ in range(rng.randint(2, 6))]
    return "\n\n".join(out)


def initial(rng):
    st = {p: gen_file(rng) for p in FILES if p != "pkg/__init__.py" and rng.random() < 0.9}
    st["pkg/__init__.py"] = ""
    return st


def step(state, rng):
    st = dict(state)
    for _ in range(rng.randint(1, 2)):
        p = rng.choice([f for f in FILES if f != "pkg/__init__.py"])
        r = rng.random()
        if p not in st:
            st[p] = gen_file(rng)
            continue
        parts = st[p].split("\n\n\n") if "\n\n\n" in st[p] else st[p].split("\n\n")
        if r < 0.15:
            del st[p]
        elif r < 0.35:  # toggle global/nonlocal lines
            body = st[p]
            if "    global " in body and rng.random() < 0.5:
                lines = [l for l in body.split("\n") if not l.strip().startswith("global ")]
                st[p] = "\n".join(lines)
            elif "nonlocal" in body:
                st[p] = body.replace("        nonlocal", "        pass  # nl", 1) if rng.random() < 0.5 else body.replace("pass  # nl", "nonlocal", 1)
            else:
                nm = rng.choice(N)
                st[p] = body.replace(":\n    ", f":\n    global {nm}\n    ", 1)
        elif r < 0.5:  # reorder blocks (moves start lines)
            rng.shuffle(parts)
            st[p] = "\n\n".join(parts)
        elif r < 0.65:  # rename a nested namesake
            a, b = rng.sample(N + ["h2"], 2)
            st[p] = st[p].replace(f"    def {a}(", f"    def {b}(", 1).replace(f"    class {a}:", f"    class {b}:", 1)
        elif r < 0.8:  # move a block to another file
            if parts:
                i = rng.randrange(len(parts))
                blkx = parts.pop(i)
                st[p] = "\n\n".join(parts)
                q = rng.choice([f for f in FILES if f not in (p, "pkg/__init__.py")])
                st[q] = (st.get(q, "") + "\n\n" + blkx) if st.get(q) else blkx
        else:
            parts.insert(rng.randrange(len(parts) + 1), blk(rng, rng.choice(N)))
            st[p] = "\n\n".join(parts)
    return st
