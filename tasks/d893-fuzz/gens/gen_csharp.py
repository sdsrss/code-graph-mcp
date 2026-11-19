"""Multi-language states for fuzz_seq.py. The language is the file name's tail:
gen_java.py, gen_cpp.py, ... (copies of this file). Def files hold classes K/J with
methods a/b/c (static and instance), free functions where the language has them,
namespaced/out-of-line definitions (C++), objects (Kotlin), modules (Ruby);
callers hold bare, member, static/qualified and constructor-typed calls. Steps
regenerate, delete, move and swap files, rename a class in a file (K <-> J),
and turn a method into a free function or back."""
import os, re

LANG = os.path.basename(__file__)[4:-3]
NAMES = ["a", "b", "c"]
CLASSES = ["K", "J"]

EXT = {"java": "java", "csharp": "cs", "kotlin": "kt", "cpp": "cpp", "c": "c", "ruby": "rb",
       "php": "php", "swift": "swift", "dart": "dart", "ts": "ts"}[LANG]
DEFS = [f"pa/d1.{EXT}", f"pa/d2.{EXT}", f"pb/d3.{EXT}", f"pc/d4.{EXT}"]
CALLERS = [f"pa/u1.{EXT}", f"pb/u2.{EXT}", f"pd/u3.{EXT}"]
EXTRA = [f"pe/d5.{EXT}", f"pb/d6.{EXT}"]
ALL = DEFS + CALLERS


def pkg(path):
    return os.path.dirname(path)


def num(rng):
    return rng.randint(0, 9)


# ---------------------------------------------------------------- definitions
def cls_body(rng, cls):
    """Method list for one class: (name, static?)"""
    ms = []
    for nm in NAMES:
        r = rng.random()
        if r < 0.45:
            ms.append((nm, False))
        elif r < 0.6:
            ms.append((nm, True))
    return ms


def render_class(cls, ms, rng):
    L = LANG
    if L == "java":
        body = "".join(f"    public {'static ' if s else ''}int {m}() {{ return {num(rng)}; }}\n" for m, s in ms)
        return f"class {cls} {{\n{body}}}\n"
    if L == "csharp":
        body = "".join(f"    public {'static ' if s else ''}int {m}() {{ return {num(rng)}; }}\n" for m, s in ms)
        return f"public class {cls} {{\n{body}}}\n"
    if L == "kotlin":
        inst = "".join(f"    fun {m}(): Int = {num(rng)}\n" for m, s in ms if not s)
        stat = [m for m, s in ms if s]
        comp = ("    companion object {\n" + "".join(f"        fun {m}(): Int = {num(rng)}\n" for m in stat) + "    }\n") if stat else ""
        return f"class {cls} {{\n{inst}{comp}}}\n"
    if L == "cpp":
        inl = [(m, s) for m, s in ms if rng.random() < 0.6]
        ool = [(m, s) for m, s in ms if (m, s) not in inl]
        body = "".join(f"    {'static ' if s else ''}int {m}() {{ return {num(rng)}; }}\n" for m, s in inl)
        decl = "".join(f"    {'static ' if s else ''}int {m}();\n" for m, s in ool)
        out = f"class {cls} {{\npublic:\n{body}{decl}}};\n"
        out += "".join(f"\nint {cls}::{m}() {{ return {num(rng)}; }}\n" for m, s in ool)
        return out
    if L == "c":
        return f"struct {cls} {{\n    int x;\n}};\n"
    if L == "ruby":
        body = "".join(f"  def {'self.' if s else ''}{m}\n    {num(rng)}\n  end\n" for m, s in ms)
        kw = "module" if rng.random() < 0.2 else "class"
        return f"{kw} {cls}\n{body}end\n"
    if L == "php":
        body = "".join(f"    public {'static ' if s else ''}function {m}() {{ return {num(rng)}; }}\n" for m, s in ms)
        return f"class {cls} {{\n{body}}}\n"
    if L == "swift":
        body = "".join(f"    {'static ' if s else ''}func {m}() -> Int {{ return {num(rng)} }}\n" for m, s in ms)
        kw = "struct" if rng.random() < 0.3 else "class"
        return f"{kw} {cls} {{\n{body}}}\n"
    if L == "dart":
        body = "".join(f"  {'static ' if s else ''}int {m}() => {num(rng)};\n" for m, s in ms)
        return f"class {cls} {{\n{body}}}\n"
    if L == "ts":
        body = "".join(f"  {'static ' if s else ''}{m}(): number {{ return {num(rng)}; }}\n" for m, s in ms)
        return f"export class {cls} {{\n{body}}}\n"
    raise ValueError(L)


def render_free(nm, rng):
    L = LANG
    if L == "kotlin":
        return f"fun {nm}(): Int = {num(rng)}\n"
    if L == "cpp":
        if rng.random() < 0.3:
            return f"namespace ns {{\nint {nm}() {{ return {num(rng)}; }}\n}}\n"
        return f"int {nm}() {{ return {num(rng)}; }}\n"
    if L == "c":
        return f"int {nm}(void) {{ return {num(rng)}; }}\n"
    if L == "ruby":
        return f"def {nm}\n  {num(rng)}\nend\n"
    if L == "php":
        return f"function {nm}() {{ return {num(rng)}; }}\n"
    if L == "swift":
        return f"func {nm}() -> Int {{ return {num(rng)} }}\n"
    if L == "dart":
        return f"int {nm}() => {num(rng)};\n"
    if L == "ts":
        return f"export function {nm}(): number {{ return {num(rng)}; }}\n"
    return None  # java / csharp: no free functions


def header(path, rng, imports=()):
    L = LANG
    p = pkg(path)
    if L == "java":
        return f"package {p};\n\n" + "".join(f"import {i}.*;\n" for i in imports) + "\n"
    if L == "csharp":
        return "".join(f"using {i.upper()};\n" for i in imports) + f"\nnamespace {p.upper()} {{\n"
    if L == "kotlin":
        return f"package {p}\n\n" + "".join(f"import {i}.*\n" for i in imports) + "\n"
    if L == "php":
        return f"<?php\nnamespace {p.upper()};\n\n" + "".join(f"use {i.upper()}\\K;\n" for i in imports) + "\n"
    if L == "cpp" or L == "c":
        return "".join(f"#include \"../{i}/d{'1' if i == 'pa' else '3'}.{EXT}\"\n" for i in imports) + "\n"
    if L == "ruby":
        return "".join(f"require_relative '../{i}/d{'1' if i == 'pa' else '3'}'\n" for i in imports) + "\n"
    if L == "dart":
        return "".join(f"import '../{i}/d{'1' if i == 'pa' else '3'}.dart';\n" for i in imports) + "\n"
    if L == "ts":
        return "".join(f"import {{ K, a }} from '../{i}/d{'1' if i == 'pa' else '3'}';\n" for i in imports) + "\n"
    return ""


def footer():
    return "}\n" if LANG == "csharp" else ""


def def_file(rng, path):
    parts = []
    for cls in CLASSES:
        if rng.random() < 0.55:
            parts.append(render_class(cls, cls_body(rng, cls), rng))
    for nm in NAMES:
        if rng.random() < 0.4:
            f = render_free(nm, rng)
            if f:
                parts.append(f)
    if LANG == "kotlin" and rng.random() < 0.2:
        parts.append("object O {\n    fun a(): Int = 1\n}\n")
    rng.shuffle(parts)
    imps = sorted({rng.choice(["pa", "pb", "pc"]) for _ in range(rng.randint(0, 1))} - {pkg(path)})
    return header(path, rng, imps) + "\n".join(parts) + footer()


# ---------------------------------------------------------------- callers
def calls(rng):
    L = LANG
    out = []
    for nm in NAMES:
        r = rng.random()
        cls = rng.choice(CLASSES)
        if r < 0.25 and L not in ("java", "csharp"):
            out.append(("bare", nm, cls))
        elif r < 0.45:
            out.append(("member", nm, cls))
        elif r < 0.65:
            out.append(("static", nm, cls))
        elif r < 0.85:
            out.append(("ctor", nm, cls))
    if L in ("java", "csharp") and rng.random() < 0.4:
        out.append(("bare", rng.choice(NAMES), "K"))
    return out


def render_call(i, kind, nm, cls):
    L = LANG
    if L in ("java", "csharp"):
        expr = {"bare": f"{nm}()", "member": f"x.{nm}()", "static": f"{cls}.{nm}()", "ctor": f"new {cls}().{nm}()"}[kind]
        ret = "int" if L == "java" else "public int"
        param = "Object x" if L == "java" else "dynamic x"
        return f"    {ret} go{i}({param}) {{ return {expr}; }}\n"
    if L == "kotlin":
        expr = {"bare": f"{nm}()", "member": f"x.{nm}()", "static": f"{cls}.{nm}()", "ctor": f"{cls}().{nm}()"}[kind]
        return f"fun go{i}(x: Any): Int = {expr}\n"
    if L == "cpp":
        expr = {"bare": f"{nm}()", "member": f"x.{nm}()", "static": f"{cls}::{nm}()", "ctor": f"{cls}().{nm}()"}[kind]
        if kind == "bare" and i % 2:
            expr = f"ns::{nm}()"
        return f"int go{i}(auto& x) {{ return {expr}; }}\n"
    if L == "c":
        return f"int go{i}(void) {{ return {nm}(); }}\n"
    if L == "ruby":
        expr = {"bare": f"{nm}()", "member": f"x.{nm}", "static": f"{cls}.{nm}", "ctor": f"{cls}.new.{nm}"}[kind]
        return f"def go{i}(x)\n  {expr}\nend\n"
    if L == "php":
        expr = {"bare": f"{nm}()", "member": f"$x->{nm}()", "static": f"{cls}::{nm}()", "ctor": f"(new {cls}())->{nm}()"}[kind]
        return f"function go{i}($x) {{ return {expr}; }}\n"
    if L == "swift":
        expr = {"bare": f"{nm}()", "member": f"x.{nm}()", "static": f"{cls}.{nm}()", "ctor": f"{cls}().{nm}()"}[kind]
        return f"func go{i}(x: AnyObject) -> Int {{ return {expr} }}\n"
    if L == "dart":
        expr = {"bare": f"{nm}()", "member": f"x.{nm}()", "static": f"{cls}.{nm}()", "ctor": f"{cls}().{nm}()"}[kind]
        return f"int go{i}(dynamic x) => {expr};\n"
    if L == "ts":
        expr = {"bare": f"{nm}()", "member": f"x.{nm}()", "static": f"{cls}.{nm}()", "ctor": f"new {cls}().{nm}()"}[kind]
        return f"export function go{i}(x: any): number {{ return {expr}; }}\n"
    raise ValueError(L)


def caller_file(rng, path):
    imps = sorted({rng.choice(["pa", "pb", "pc"]) for _ in range(rng.randint(0, 2))} - {pkg(path)})
    body = "".join(render_call(i, *c) for i, c in enumerate(calls(rng)))
    if LANG in ("java", "csharp"):
        body = f"class U{sum(map(ord, path)) % 97} {{\n{body}}}\n"
    return header(path, rng, imps) + body + footer()


def gen(rng, p):
    return def_file(rng, p) if p in DEFS or p in EXTRA else caller_file(rng, p)


def initial(rng):
    st = {}
    for p in ALL:
        if rng.random() < 0.85:
            st[p] = gen(rng, p)
    return st


def retarget(body, dst):
    """Package/namespace clause follows the directory, as in a real move."""
    p = pkg(dst)
    if LANG in ("java",):
        return re.sub(r"^package \w+;", f"package {p};", body, count=1, flags=re.M)
    if LANG == "kotlin":
        return re.sub(r"^package \w+$", f"package {p}", body, count=1, flags=re.M)
    if LANG == "csharp":
        return re.sub(r"^namespace \w+ \{", f"namespace {p.upper()} {{", body, count=1, flags=re.M)
    if LANG == "php":
        return re.sub(r"^namespace \w+;", f"namespace {p.upper()};", body, count=1, flags=re.M)
    return body


def step(state, rng):
    st = dict(state)
    for _ in range(rng.randint(1, 3)):
        r = rng.random()
        files = sorted(st)
        if r < 0.15 and files:
            src = rng.choice(files)
            dst = rng.choice([p for p in EXTRA + DEFS + CALLERS if p != src])
            st[dst] = retarget(st.pop(src), dst)
        elif r < 0.22 and len(files) >= 2:
            x, y = rng.sample(files, 2)
            st[x], st[y] = retarget(st[y], x), retarget(st[x], y)
        elif r < 0.5:
            # class rename in a def file: K <-> J (and the out-of-line `K::` prefix)
            defs = [p for p in files if p in DEFS or p in EXTRA]
            if defs:
                p = rng.choice(defs)
                a, b = rng.choice([("K", "J"), ("J", "K"), ("K", "Q"), ("J", "Q")])
                st[p] = re.sub(rf"\b{a}\b", b, st[p])
        elif r < 0.62:
            # a method becomes a free function or the other way (where both exist)
            defs = [p for p in files if p in DEFS or p in EXTRA]
            if defs:
                p = rng.choice(defs)
                nm = rng.choice(NAMES)
                f = render_free(nm, rng)
                if f and rng.random() < 0.5:
                    st[p] = st[p].rstrip("}\n") + "\n}\n" + f if LANG == "csharp" else st[p] + "\n" + f
                else:
                    st[p] = gen(rng, p)
        else:
            p = rng.choice(ALL)
            if p in st and rng.random() < 0.2:
                del st[p]
            else:
                st[p] = gen(rng, p)
    return st
