"""Go states for fuzz_seq.py: packages with funcs, types with methods; imports (plain and
aliased), package-qualified, bare same-package, and method calls; steps edit, delete, move
and swap files across packages (package clause follows the directory)."""
import posixpath
NAMES = ["A", "B", "C"]
DEFS = ["pa/a.go", "pa/a2.go", "pb/b.go", "pc/c.go", "pd/d.go"]
CALLERS = ["cmd/main.go", "pb/use.go", "pe/e.go"]
EXTRA = ["pf/f.go", "pa/a3.go", "pc/c2.go"]
ALL = DEFS + CALLERS
PKGS = ["pa", "pb", "pc", "pd", "pf"]

def pkg_of(path):
    d = posixpath.dirname(path)
    return "main" if d == "cmd" else d

def def_body(rng):
    out = []
    for nm in NAMES:
        r = rng.random()
        if r < 0.4: out.append(f"func {nm}() int {{\n\treturn {rng.randint(0, 9)}\n}}\n")
        elif r < 0.48: out.append(f"func {nm.lower()}() int {{\n\treturn 1\n}}\n")
    if rng.random() < 0.35:
        out.append("type K struct{}\n\nfunc (k *K) A() int {\n\treturn 1\n}\n\nfunc (k K) Run() int {\n\treturn k.A()\n}\n")
    if rng.random() < 0.15:
        out.append("type I interface {\n\tA() int\n}\n")
    rng.shuffle(out)
    return out

def caller_body(rng, path):
    imps, body = [], []
    for _ in range(rng.randint(0, 3)):
        p = rng.choice([q for q in PKGS if q != pkg_of(path)])
        imps.append(rng.choice([f"\t\"ex/{p}\"", f"\tx \"ex/{p}\"", f"\t{p}2 \"ex/{p}\""]))
    imps = sorted(set(imps))
    for nm in NAMES:
        if rng.random() < 0.4: body.append(f"func Go{nm}() int {{\n\treturn {nm}()\n}}\n")
    for imp in imps:
        alias = imp.split()[0] if imp.split()[0].startswith(('x', 'p')) and '"' not in imp.split()[0] else imp.strip().strip('"').split('/')[-1]
        if rng.random() < 0.7: body.append(f"func Go{alias}{rng.randint(0,99)}() int {{\n\treturn {alias}.A()\n}}\n")
    if rng.random() < 0.25: body.append("func GoK(k *K) int {\n\treturn k.A()\n}\n")
    if rng.random() < 0.15: body.append("func GoI(i I) int {\n\treturn i.A()\n}\n")
    return imps, body

def render(path, imps, body):
    head = f"package {pkg_of(path)}\n\n"
    if imps: head += "import (\n" + "\n".join(imps) + "\n)\n\n"
    return head + "\n".join(body)

def gen(rng, p):
    if p in DEFS:
        return render(p, [], def_body(rng))
    imps, body = caller_body(rng, p)
    return render(p, imps, body)

def initial(rng):
    st = {"go.mod": "module ex\n\ngo 1.21\n"}
    for p in ALL:
        if rng.random() < 0.85: st[p] = gen(rng, p)
    return st

def repkg(body, dst):
    lines = body.split("\n", 1)
    return f"package {pkg_of(dst)}\n" + (lines[1] if len(lines) > 1 else "")

def step(state, rng):
    st = dict(state)
    for _ in range(rng.randint(1, 3)):
        r = rng.random(); files = sorted(p for p in st if p.endswith(".go"))
        if r < 0.25 and files:
            src = rng.choice(files); dst = rng.choice([p for p in EXTRA + ALL if p != src])
            st[dst] = repkg(st.pop(src), dst)
        elif r < 0.35 and len(files) >= 2:
            x, y = rng.sample(files, 2); st[x], st[y] = repkg(st[y], x), repkg(st[x], y)
        else:
            p = rng.choice(ALL)
            if p in st and rng.random() < 0.2: del st[p]
            elif p in st and p in DEFS and rng.random() < 0.4:
                nm = rng.choice(NAMES); body = st[p]
                st[p] = body.replace(f"func {nm}(", f"func {nm}2(") if f"func {nm}(" in body else body + f"\nfunc {nm}() int {{\n\treturn 7\n}}\n"
            else: st[p] = gen(rng, p)
    return st
