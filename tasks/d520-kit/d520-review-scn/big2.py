N = 560
def fill(tag):
    return {f"f/{i:03d}.py": f"# {tag}\ndef filler_{i}():\n    return {i}\n" for i in range(N)}
def sc(base, steps):
    out = [dict(base, **fill("s0"))]
    for k, s in enumerate(steps, 1):
        out.append(dict(s, **fill(f"s{k}")))
    return out
S = {}
S["c1_early_from_import_name_removed"] = sc(
    {"zzm.py": "def Y():\n    pass\n", "q.py": "def Y():\n    pass\n", "a_imp.py": "from zzm import Y\n\ndef g():\n    Y()\n"},
    [{"zzm.py": "def Z():\n    pass\n", "a_imp.py": "# t\nfrom zzm import Y\n\ndef g():\n    Y()\n"}, {"zzm.py": "def Y():\n    pass\n"}])
S["c2_early_from_import_rename_module"] = sc(
    {"zzm.py": "def Y():\n    pass\n", "q.py": "def Y():\n    pass\n", "a_imp.py": "from zzm import Y\nimport zzm\n\ndef g():\n    Y()\n    zzm.Y()\n"},
    [{"zzm.py": None, "zzn.py": "def Y():\n    pass\n", "a_imp.py": "# t\nfrom zzm import Y\nimport zzm\n\ndef g():\n    Y()\n    zzm.Y()\n"},
     {"zzn.py": None, "zzm.py": "def Y():\n    pass\n"}])
S["c3_early_importer_chain"] = sc(
    {"zpkg/__init__.py": "from .core import Y\n", "zpkg/core.py": "def Y():\n    pass\n", "q.py": "def Y():\n    pass\n",
     "a_imp.py": "from zpkg import Y\nimport zpkg\n\ndef g():\n    Y()\n    zpkg.Y()\n", "a_imp2.py": "from a_imp import g\n\ndef h():\n    g()\n"},
    [{"zpkg/core.py": "def Z():\n    pass\n", "a_imp.py": "# t\nfrom zpkg import Y\nimport zpkg\n\ndef g():\n    Y()\n    zpkg.Y()\n", "a_imp2.py": "# t\nfrom a_imp import g\n\ndef h():\n    g()\n"},
     {"zpkg/core.py": "def Y():\n    pass\n", "zpkg/__init__.py": "from .core import Y\n# t\n"}])
S["c4_late_from_import_name_removed"] = sc(
    {"am.py": "def Y():\n    pass\n", "q.py": "def Y():\n    pass\n", "zz_imp.py": "from am import Y\n\ndef g():\n    Y()\n"},
    [{"am.py": "def Z():\n    pass\n", "zz_imp.py": "# t\nfrom am import Y\n\ndef g():\n    Y()\n"}, {"am.py": "def Y():\n    pass\n"}])
S["c5_early_method_calls"] = sc(
    {"zzm.py": "class C:\n    def Y(self):\n        pass\n", "a_imp.py": "from zzm import C\n\ndef g(c: C):\n    C().Y()\n"},
    [{"zzm.py": "class D:\n    def Y(self):\n        pass\n", "a_imp.py": "# t\nfrom zzm import C\n\ndef g(c: C):\n    C().Y()\n"},
     {"zzm.py": "class C:\n    def Y(self):\n        pass\n"}])
SCENARIOS = S
