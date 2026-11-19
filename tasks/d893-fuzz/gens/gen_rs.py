"""Rust states for fuzz_seq.py: one crate whose lib.rs declares every module; free fns,
structs with impl methods, traits, `use` imports (plain, renamed, glob, re-export), path,
bare, method and associated calls; steps edit, delete, move and swap module files."""
NAMES = ["a", "b", "c"]
MODS = {"src/m.rs": "m", "src/n.rs": "n", "src/k.rs": "k", "src/sub/imp.rs": "sub::imp", "src/other.rs": "other",
        "src/moved.rs": "moved", "src/sub/imp2.rs": "sub::imp2"}
DEFS = ["src/m.rs", "src/n.rs", "src/k.rs", "src/sub/imp.rs", "src/other.rs"]
CALLERS = ["src/views.rs", "src/usex.rs", "src/sub/user.rs"]
EXTRA = ["src/moved.rs", "src/sub/imp2.rs"]
FIXED = {
    "Cargo.toml": "[package]\nname = \"ex\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    "src/lib.rs": "pub mod m;\npub mod n;\npub mod k;\npub mod sub;\npub mod other;\npub mod moved;\npub mod views;\npub mod usex;\n",
}
SUBMODS = ["src/sub/mod.rs"]
ALL = DEFS + CALLERS + SUBMODS

def def_file(rng, path):
    out = []
    for nm in NAMES:
        r = rng.random()
        if r < 0.4: out.append(f"pub fn {nm}() -> i32 {{\n    {rng.randint(0, 9)}\n}}\n")
        elif r < 0.48: out.append(f"fn {nm}() -> i32 {{\n    1\n}}\n")
    if rng.random() < 0.35:
        out.append("pub struct K;\n\nimpl K {\n    pub fn new() -> K {\n        K\n    }\n    pub fn a(&self) -> i32 {\n        1\n    }\n}\n")
    if rng.random() < 0.15:
        out.append("pub trait T {\n    fn a(&self) -> i32;\n}\n" + ("\nimpl T for K {\n    fn a(&self) -> i32 {\n        2\n    }\n}\n" if "struct K" in "".join(out) else ""))
    if rng.random() < 0.2:
        src = rng.choice([m for p, m in MODS.items() if p != path])
        out.insert(0, rng.choice([f"pub use crate::{src}::a;\n", f"pub use crate::{src}::a as b;\n", f"pub use crate::{src}::*;\n", f"use crate::{src}::a;\n"]))
    rng.shuffle(out)
    return "\n".join(out)

def caller_file(rng, path):
    lines = []
    for _ in range(rng.randint(0, 4)):
        m = rng.choice(list(MODS.values())); x = rng.choice(["a", "K", "b"]); y = rng.choice(["b", "c", "Kk", x])
        lines.append(rng.choice([f"use crate::{m}::{x};", f"use crate::{m}::{x} as {y};", f"use crate::{m}::*;",
                                 f"use crate::{m};" if "::" not in m else f"use crate::{m} as mm;"]))
    body = []
    for nm in NAMES:
        if rng.random() < 0.5: body.append(f"pub fn go_{nm}() -> i32 {{\n    {nm}()\n}}\n")
    if rng.random() < 0.25: body.append(f"pub fn go_p() -> i32 {{\n    crate::{rng.choice(list(MODS.values()))}::a()\n}}\n")
    if rng.random() < 0.2: body.append("pub fn go_m() -> i32 {\n    m::a()\n}\n")
    if rng.random() < 0.2: body.append("pub fn go_k() -> i32 {\n    let k = K::new();\n    k.a()\n}\n")
    if rng.random() < 0.2: body.append("pub fn go_o(o: &K) -> i32 {\n    o.a()\n}\n")
    if rng.random() < 0.15: body.append("pub fn go_x(x: &dyn T) -> i32 {\n    x.a()\n}\n")
    if rng.random() < 0.15: body.append(f"fn {rng.choice(NAMES)}() -> i32 {{\n    5\n}}\n")
    return "\n".join(lines) + "\n\n" + "\n".join(body)

def gen(rng, p):
    if p == "src/sub/mod.rs":
        return rng.choice(["pub mod imp;\npub mod imp2;\npub mod user;\n", "pub mod imp;\npub mod imp2;\npub mod user;\npub use imp::a;\n",
                           "pub mod imp;\npub mod imp2;\npub mod user;\npub use imp::*;\n"])
    return def_file(rng, p) if p in DEFS else caller_file(rng, p)

def initial(rng):
    st = dict(FIXED)
    for p in ALL:
        if p in SUBMODS or rng.random() < 0.85: st[p] = gen(rng, p)
    return st

def step(state, rng):
    st = dict(state)
    for _ in range(rng.randint(1, 3)):
        r = rng.random(); files = sorted(p for p in st if p in ALL or p in EXTRA)
        if r < 0.25 and files:
            src = rng.choice(files); dst = rng.choice([p for p in EXTRA + DEFS + CALLERS if p != src])
            st[dst] = st.pop(src)
        elif r < 0.35 and len(files) >= 2:
            x, y = rng.sample(files, 2); st[x], st[y] = st[y], st[x]
        else:
            p = rng.choice(ALL)
            if p in st and p not in SUBMODS and rng.random() < 0.2: del st[p]
            elif p in st and p in DEFS and rng.random() < 0.4:
                nm = rng.choice(NAMES); body = st[p]
                st[p] = body.replace(f"fn {nm}(", f"fn {nm}2(") if f"fn {nm}(" in body else body + f"\npub fn {nm}() -> i32 {{\n    7\n}}\n"
            else: st[p] = gen(rng, p)
    return st
