"""JS/TS states for fuzz_seq.py: exported / local functions, classes with methods,
re-exports, named / renamed / namespace imports, bare / member / namespace calls, and
steps that edit, delete, move and swap files."""
import posixpath
NAMES = ["a", "b", "c"]
DEFS = ["src/m.ts", "src/n.ts", "lib/k.ts", "src/sub/impl.ts", "other.ts", "src/sub/index.ts"]
CALLERS = ["src/views.ts", "use.ts", "lib/user.ts", "src/rev.js"]
EXTRA = ["src/moved/m.ts", "pkg/n.ts", "lib/k2.ts", "src/sub/impl2.ts", "other2.ts"]
ALL = DEFS + CALLERS

def spec(frm, to):
    rel = posixpath.relpath(posixpath.splitext(to)[0], posixpath.dirname(frm) or ".")
    if rel.endswith("/index"): rel = rel[:-6]
    return rel if rel.startswith(".") else "./" + rel

def def_file(rng, path):
    out = []
    for nm in NAMES:
        r = rng.random()
        if r < 0.35: out.append(f"export function {nm}() {{\n  return {rng.randint(0, 9)};\n}}\n")
        elif r < 0.45: out.append(f"function {nm}() {{\n  return 1;\n}}\n")
        elif r < 0.52: out.append(f"export const {nm} = () => {rng.randint(0, 9)};\n")
    if rng.random() < 0.35:
        out.append("export class A {\n  a() {\n    return 1;\n  }\n  run() {\n    return this.a();\n  }\n}\n")
    if rng.random() < 0.15:
        out.append("export class B extends A {\n  b() {\n    return a();\n  }\n}\n" if "class A" in "".join(out) else "export class B {}\n")
    if rng.random() < 0.25:
        src = rng.choice([d for d in DEFS if d != path])
        out.insert(0, rng.choice([f"export {{ a }} from '{spec(path, src)}';\n", f"export {{ a as b }} from '{spec(path, src)}';\n",
                                  f"export * from '{spec(path, src)}';\n", f"import {{ a }} from '{spec(path, src)}';\n"]))
    rng.shuffle(out)
    return "\n".join(out)

def caller_file(rng, path):
    lines = []
    for _ in range(rng.randint(0, 4)):
        d = rng.choice(DEFS); s = spec(path, d); x = rng.choice(["a", "A", "b"]); y = rng.choice(["b", "c", "B", x])
        lines.append(rng.choice([f"import {{ {x} }} from '{s}';", f"import {{ {x} as {y} }} from '{s}';",
                                 f"import * as m from '{s}';", f"import * as mm from '{s}';"]))
    body = []
    for nm in NAMES:
        if rng.random() < 0.5: body.append(f"export function go_{nm}() {{\n  return {nm}();\n}}\n")
    if rng.random() < 0.25: body.append("export function go_A() {\n  return new A();\n}\n")
    if rng.random() < 0.2: body.append("export function go_o(o) {\n  return o.a();\n}\n")
    if rng.random() < 0.2: body.append("export function go_m() {\n  return m.a();\n}\n")
    if rng.random() < 0.15: body.append("export function go_mm() {\n  return mm.b();\n}\n")
    if rng.random() < 0.15: body.append("export class W extends B {\n  go() {\n    return this.a();\n  }\n}\n")
    if rng.random() < 0.15: body.append("export const REFS = [a, b];\n")
    if rng.random() < 0.15: body.append(f"{rng.choice(NAMES)}();\n")
    if rng.random() < 0.15: body.append(f"function {rng.choice(NAMES)}() {{\n  return 5;\n}}\n")
    return "\n".join(lines) + "\n\n" + "\n".join(body)

def gen(rng, p):
    return def_file(rng, p) if p in DEFS else caller_file(rng, p)

def initial(rng):
    return {p: gen(rng, p) for p in ALL if rng.random() < 0.85}

def step(state, rng):
    st = dict(state)
    for _ in range(rng.randint(1, 3)):
        r = rng.random(); files = sorted(st)
        if r < 0.25 and files:
            src = rng.choice(files); dst = rng.choice([p for p in EXTRA + ALL if p != src])
            st[dst] = st.pop(src)
        elif r < 0.35 and len(files) >= 2:
            x, y = rng.sample(files, 2); st[x], st[y] = st[y], st[x]
        else:
            p = rng.choice(ALL)
            if p in st and rng.random() < 0.2: del st[p]
            elif p in st and p in DEFS and rng.random() < 0.4:
                nm = rng.choice(NAMES); body = st[p]
                st[p] = body.replace(f"function {nm}(", f"function {nm}2(") if f"function {nm}(" in body else body + f"\nexport function {nm}() {{\n  return 7;\n}}\n"
            else: st[p] = gen(rng, p)
    return st
