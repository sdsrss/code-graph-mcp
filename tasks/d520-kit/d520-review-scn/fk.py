N = 560
def fill(tag):
    return {f"f/{i:03d}.py": f"# {tag}\ndef filler_{i}():\n    return {i}\n" for i in range(N)}
def sc(base, steps):
    out = [dict(base, **fill("s0"))]
    for k, s in enumerate(steps, 1):
        out.append(dict(s, **fill(f"s{k}")))
    return out
S = {}
S["k1_later_batch_source_target_gone"] = sc(
    {"a_mod.py": "def Y():\n    pass\n", "q.py": "def Y():\n    pass\n", "zz_user.py": "from a_mod import Y\nimport a_mod\n\ndef g():\n    Y()\n    a_mod.Y()\n"},
    [{"a_mod.py": "def Z():\n    pass\n", "zz_user.py": "# t\nfrom a_mod import Y\nimport a_mod\n\ndef g():\n    Y()\n    a_mod.Y()\n"},
     {"a_mod.py": "def Y():\n    pass\n"}])
S["k2_later_batch_source_target_renamed_file"] = sc(
    {"a_mod.py": "def Y():\n    pass\n", "zz_user.py": "from a_mod import Y\n\ndef g():\n    Y()\n"},
    [{"a_mod.py": None, "a_mod2.py": "def Y():\n    pass\n", "zz_user.py": "# t\nfrom a_mod import Y\n\ndef g():\n    Y()\n"}])
S["k3_later_batch_source_calls_only"] = sc(
    {"a_mod.py": "class C:\n    def Y(self):\n        pass\n", "zz_user.py": "from a_mod import C\n\ndef g():\n    C().Y()\n"},
    [{"a_mod.py": "class C:\n    def W(self):\n        pass\n", "zz_user.py": "# t\nfrom a_mod import C\n\ndef g():\n    C().Y()\n"}])
SCENARIOS = S
