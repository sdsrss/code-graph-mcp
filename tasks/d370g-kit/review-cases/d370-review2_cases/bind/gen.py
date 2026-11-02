import os, json
SN = {
 "assign": "C = D",
 "tuple1": "C, = (D,)",
 "listpat": "[C] = [D]",
 "paren": "(C) = D",
 "for": "for C in [D]:\n    pass",
 "with": "import contextlib\nwith contextlib.nullcontext(D) as C:\n    pass",
 "walrus": "(C := D)",
 "walrus_comp": "[(C := D) for _ in [1]]",
 "match_cap": "match D:\n    case C:\n        pass",
 "match_as": "match D:\n    case type() as C:\n        pass",
 "match_list": "match [D]:\n    case [C]:\n        pass",
 "match_kw": "class Box:\n    __match_args__ = ('v',)\n    def __init__(self, v):\n        self.v = v\nmatch Box(D):\n    case Box(v=C):\n        pass",
 "match_pos": "class Box:\n    __match_args__ = ('v',)\n    def __init__(self, v):\n        self.v = v\nmatch Box(D):\n    case Box(C):\n        pass",
 "global_fn": "def f():\n    global C\n    C = D\nf()",
 "annassign": "C: type = D",
 "globals_item": "globals()['C'] = D",
 "globals_update": "globals().update(C=D)",
 "sysmod_attr": "import sys\nsys.modules[__name__].C = D",
 "setattr_mod": "import sys\nsetattr(sys.modules[__name__], 'C', D)",
 "exec": "exec('C = D')",
 "vars": "vars()['C'] = D",
 "locals": "locals()['C'] = D",
 "dunder_import": "__import__(__name__).C = D",
 "alias_patch": "E = C\nE.m = D.m",
 "paren_patch": "(C).m = D.m",
 "setattr_cls": "setattr(C, 'm', D.m)",
 "dict_splat_kw": "def g(**kw):\n    return kw\ng(**{'C': 1})",
 "starred_for": "for *C, in [[D]]:\n    C = C[0]",
 "async_for": "import asyncio\nasync def h():\n    global C\n    async def it():\n        yield D\n    async for C in it():\n        pass\nasyncio.run(h())",
 "except_star": "try:\n    raise ExceptionGroup('g', [ValueError()])\nexcept* ValueError:\n    C = D",
 "type_alias": "type C = D",
 "chain": "x = C = D",
 "aug": "C |= {}" ,
}
expect = {}
for k, s in SN.items():
    d = f"k_{k}"
    os.makedirs(d, exist_ok=True)
    open(f"{d}/a.py", "w").write(
        "class D:\n    def m(self):\n        return 'D'\n\n\nclass C:\n    def m(self):\n        return 'C'\n\n\n"
        + s + "\n\n\ndef t_call():\n    return C.m(None)\n")
