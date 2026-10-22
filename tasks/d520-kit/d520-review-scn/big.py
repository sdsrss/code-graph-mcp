# >500-file runs: fillers rewritten each step so the run spans two batches.
N = 560
def fill(tag):
    return {f"f/{i:03d}.py": f"# {tag}\ndef filler_{i}():\n    return {i}\n" for i in range(N)}
def sc(base, steps):
    out = [dict(base, **fill("s0"))]
    for k, s in enumerate(steps, 1):
        out.append(dict(s, **fill(f"s{k}")))
    return out
S = {}
for cname, mname, imp in (("a_app.py", "zzm.py", "zzm"), ("zz_app.py", "am.py", "am")):
    tag = "early" if cname.startswith("a") else "late"
    caller = f"import {imp}\n\ndef f():\n    return {imp}.X()\n"
    caller2 = f"# touched\nimport {imp}\n\ndef f():\n    return {imp}.X()\n"
    S[f"b1_{tag}_module_loses_X"] = sc({mname: "def X():\n    pass\n", "q.py": "def X():\n    pass\n", cname: caller},
        [{mname: "def Y():\n    pass\n"}, {mname: "def X():\n    pass\n"}])
    S[f"b2_{tag}_module_loses_X_caller_touched"] = sc({mname: "def X():\n    pass\n", "q.py": "def X():\n    pass\n", cname: caller},
        [{mname: "def Y():\n    pass\n", cname: caller2}, {mname: "def X():\n    pass\n", cname: caller}])
    S[f"b3_{tag}_function_to_method"] = sc({mname: "def X():\n    pass\n", "q.py": "def X():\n    pass\n", cname: caller},
        [{mname: "class C:\n    def X(self):\n        pass\n", cname: caller2}, {mname: "def X():\n    pass\n", cname: caller}])
    S[f"b4_{tag}_PX_and_bare"] = sc({mname: "def X():\n    pass\n", "q.py": "def X():\n    pass\n",
        cname: f"import {imp}\nfrom q import X\n\ndef f():\n    {imp}.X()\n    X()\n"},
        [{mname: "def Y():\n    pass\n"}, {mname: "def X():\n    pass\n", "q.py": "def Z():\n    pass\n"}, {"q.py": "def X():\n    pass\n"}])
    S[f"b5_{tag}_module_deleted_readded"] = sc({mname: "def X():\n    pass\n", "q.py": "def X():\n    pass\n", cname: caller},
        [{mname: None}, {mname: "def X():\n    pass\n"}])
    S[f"b6_{tag}_caller_added"] = sc({mname: "def X():\n    pass\n", "q.py": "def X():\n    pass\n"},
        [{cname: caller}, {cname: None}])
S["b7_reexport_pkg_late"] = sc({"zpkg/__init__.py": "from .core import X\n", "zpkg/core.py": "def X():\n    pass\n", "zpkg/other.py": "def X():\n    pass\n",
     "a_app.py": "import zpkg\n\ndef f():\n    return zpkg.X()\n"},
    [{"zpkg/__init__.py": "from .other import X\n"}, {"zpkg/__init__.py": "V = 1\n", "a_app.py": "# t\nimport zpkg\n\ndef f():\n    return zpkg.X()\n"}, {"zpkg/__init__.py": "from .core import X\n"}])
S["b8_reexport_pkg_early"] = sc({"apkg/__init__.py": "from .core import X\n", "apkg/core.py": "def X():\n    pass\n", "apkg/other.py": "def X():\n    pass\n",
     "zz_app.py": "import apkg\n\ndef f():\n    return apkg.X()\n"},
    [{"apkg/__init__.py": "from .other import X\n"}, {"apkg/__init__.py": "V = 1\n", "zz_app.py": "# t\nimport apkg\n\ndef f():\n    return apkg.X()\n"}, {"apkg/__init__.py": "from .core import X\n"}])
SCENARIOS = S
