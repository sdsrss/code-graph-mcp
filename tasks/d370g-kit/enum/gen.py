#!/usr/bin/env python3
"""gen.py SEED N OUT: N random one-call Python files for the D#370(g) rule.

Each file defines a few classes (bases of every kind the rule must handle),
optional metaclasses and patches, and `def t_call(): return K.m(None)`. Every
function named `m` (and every metaclass attribute `m`) appends a unique tag to
`TRACE` when it runs, so `truth.py` can name the def CPython runs. The external
package the files import lives in OUT/../extpkg (outside the indexed corpus).
"""
import os
import random
import sys

EXT = '''TRACE = []
class E1:
    def m(self=None, *a):
        TRACE.append("ext:E1.m")
class E2(E1):
    pass
class EMeta(type):
    pass
'''

EXTSTAR = "".join(
    f'class K{i}:\n    def m(self=None, *a):\n        TRACE.append("ext:K{i}.m")\n' for i in range(6)
) + "from extpkg import TRACE\n"


BODY_KINDS = [
    "none", "none", "def", "def", "def", "def", "two_defs", "attr", "walrus_default",
    "lambda_walrus", "slots", "locals", "cond_def", "nested", "static", "classattr_none",
    "def_and_walrus_body",
]
SPECIAL_KINDS = ["deco", "if", "try", "alias", "subscript", "call", "below"]
LEAF_KINDS = ["dict", "Exception", "object", "E1", "E2", "extpkg.E2", "E1", "E2", "dup"]
META_KINDS = ["none"] * 14 + ["meta_prop", "meta_plain", "meta_getattribute", "abc", "kw"]
PATCH_KINDS = ["none"] * 6 + ["assign", "paren", "del", "starred", "setattr", "alias"]


class Gen:
    def __init__(self, rnd):
        self.r = rnd
        self.lines = []
        self.tag = 0

    def emit(self, s=""):
        self.lines.append(s)

    def m_def(self, indent, owner, name="m", deco=None, extra=""):
        self.tag += 1
        t = f"{owner}.{name}#{self.tag}"
        if deco:
            self.emit(f"{indent}@{deco}")
        self.emit(f"{indent}def {name}(self=None, *a{extra}):")
        self.emit(f'{indent}    TRACE.append("{t}")')

    def body(self, indent, owner, kind):
        p = indent
        if kind == "none":
            self.emit(f"{p}pass")
        elif kind == "def":
            self.m_def(p, owner)
        elif kind == "two_defs":
            self.m_def(p, owner)
            self.m_def(p, owner)
        elif kind == "attr":
            self.emit(f"{p}m = staticmethod(_free)")
        elif kind == "walrus_default":
            self.m_def(p, owner)
            self.emit(f"{p}def z(self, a=(m := staticmethod(_free))):")
            self.emit(f"{p}    pass")
        elif kind == "lambda_walrus":
            self.m_def(p, owner)
            self.emit(f"{p}f = lambda a=(m := staticmethod(_free)): a")
        elif kind == "slots":
            self.emit(f'{p}__slots__ = ("m",)')
        elif kind == "locals":
            self.m_def(p, owner)
            self.emit(f'{p}locals()["m"] = staticmethod(_free)')
        elif kind == "cond_def":
            self.emit(f"{p}if True:")
            self.m_def(p + "    ", owner)
        elif kind == "nested":
            self.m_def(p, owner)
            self.emit(f"{p}def other(self):")
            self.m_def(p + "    ", owner + ".other")
        elif kind == "static":
            self.m_def(p, owner, deco="staticmethod")
        elif kind == "classattr_none":
            self.emit(f"{p}x = 1")
        elif kind == "def_and_walrus_body":
            self.m_def(p, owner)
            self.emit(f"{p}def z(self):")
            self.emit(f"{p}    (m := 1)")

    def scenario(self):
        r = self.r
        self.emit("TRACE = []")
        self.emit("from extstar import *")
        self.emit("import extpkg")
        self.emit("from extpkg import E1, E2")
        self.emit("from abc import ABCMeta")
        self.emit("from typing import Generic, TypeVar")
        self.emit("T = TypeVar('T')")
        self.emit("def _free(*a):")
        self.emit('    TRACE.append("free")')
        self.emit("def keep(c):")
        self.emit("    return c")
        self.emit("def mk(c):")
        self.emit("    return c")
        metas = set()
        n = r.randint(1, 5)
        names = [f"K{i}" for i in range(n)]
        defined = []
        specials = 0
        for i, name in enumerate(names):
            bases = []
            dup = False
            for _ in range(r.choice([0, 1, 1, 2, 2, 3])):
                roll = r.random()
                if defined and roll < 0.5:
                    bases.append(r.choice(defined))
                elif roll < 0.75:
                    bases.append(self.special(r.choice(SPECIAL_KINDS), defined, names[i + 1:], specials))
                    specials += 1
                else:
                    leaf = r.choice(LEAF_KINDS)
                    if leaf == "dup":
                        b = r.choice(defined) if defined else "E1"
                        bases += [b, b]
                        dup = True
                    else:
                        bases.append(leaf)
            # Duplicates only where `dup` asked for them (Python refuses them).
            if not dup:
                bases = list(dict.fromkeys(bases))
            # File classes subclass-first (Python's own requirement), others
            # where they fell: keeps most files importable.
            slots = [j for j, x in enumerate(bases) if x in defined]
            ordered = sorted((bases[j] for j in slots), key=defined.index, reverse=True)
            for j, x in zip(slots, ordered):
                bases[j] = x
            meta = r.choice(META_KINDS)
            kw = ""
            if meta != "none":
                kw = self.meta(meta, metas)
            args = ", ".join(x for x in [", ".join(bases), kw] if x)
            self.emit(f"class {name}({args}):" if args else f"class {name}:")
            self.body("    ", name, r.choice(BODY_KINDS))
            defined.append(name)
        target = r.choice(names)
        patch = r.choice(PATCH_KINDS)
        victim = r.choice(names)
        if patch == "assign":
            self.emit(f"{victim}.m = staticmethod(_free)")
        elif patch == "paren":
            self.emit(f"({victim}).m = staticmethod(_free)")
        elif patch == "del":
            self.emit("try:")
            self.emit(f"    del {victim}.m")
            self.emit("except AttributeError:")
            self.emit("    pass")
        elif patch == "starred":
            self.emit(f"*{victim}.m, = [staticmethod(_free)]")
        elif patch == "setattr":
            self.emit(f'setattr({victim}, "m", staticmethod(_free))')
        elif patch == "alias":
            self.emit(f"_V = {victim}")
            self.emit("_V.m = staticmethod(_free)")
        self.emit("def t_call():")
        self.emit(f"    return {target}.m(None)")
        return "\n".join(self.lines) + "\n"

    def meta(self, kind, metas):
        if kind == "abc":
            return "metaclass=ABCMeta"
        if kind == "kw":
            return "**{'metaclass': type}"
        name = {"meta_prop": "MP", "meta_plain": "MQ", "meta_getattribute": "MG"}[kind]
        if name not in metas:
            metas.add(name)
            self.emit(f"class {name}(type):")
            if kind == "meta_prop":
                self.tag += 1
                self.emit("    @property")
                self.emit("    def m(cls):")
                self.emit(f'        TRACE.append("{name}.m#{self.tag}")')
                self.emit("        return _free")
            elif kind == "meta_plain":
                self.m_def("    ", name)
            else:
                self.tag += 1
                self.emit("    def __getattribute__(cls, n):")
                self.emit(f'        if n == "m": TRACE.append("{name}.ga#{self.tag}"); return _free')
                self.emit("        return type.__getattribute__(cls, n)")
        return f"metaclass={name}"

    def special(self, kind, defined, later, k):
        """A base the rule must not treat as a plain class of the file."""
        r = self.r
        name = f"S{k}"
        inner = f"({r.choice(defined)})" if defined and r.random() < 0.7 else ""
        body = r.choice(["none", "def", "def"])
        if kind == "deco":
            self.emit("@keep")
            self.emit(f"class {name}{inner}:")
            self.body("    ", name, body)
        elif kind == "if":
            self.emit("if True:")
            self.emit(f"    class {name}{inner}:")
            self.body("        ", name, body)
        elif kind == "try":
            self.emit("try:")
            self.emit(f"    class {name}{inner}:")
            self.body("        ", name, body)
            self.emit("except Exception:")
            self.emit("    pass")
        elif kind == "alias":
            if not defined:
                return "E1"
            self.emit(f"{name} = {r.choice(defined)}")
        elif kind == "subscript":
            base = f"Generic[T], {inner[1:-1]}" if inner else "Generic[T]"
            self.emit(f"class G{k}({base}):")
            self.body("    ", f"G{k}", body)
            return f"G{k}[int]"
        elif kind == "call":
            if not defined:
                return "mk(E1)"
            return f"mk({r.choice(defined)})"
        elif kind == "below":
            # A class of the file defined further down: when this class
            # statement runs, the name is still extstar's class.
            return r.choice(later) if later else "E1"
        return name


def main():
    seed, n, out = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3]
    os.makedirs(out, exist_ok=True)
    ext = os.path.join(os.path.dirname(os.path.abspath(out)), "extpkg")
    os.makedirs(ext, exist_ok=True)
    open(os.path.join(ext, "__init__.py"), "w").write(EXT)
    open(os.path.join(os.path.dirname(ext), "extstar.py"), "w").write(EXTSTAR)
    for i in range(n):
        g = Gen(random.Random(seed * 1_000_003 + i))
        open(os.path.join(out, f"f{i:05d}.py"), "w").write(g.scenario())


if __name__ == "__main__":
    main()
