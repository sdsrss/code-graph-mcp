# multi-batch (>500 files) run: target def renamed in a late batch; callers in early batch; module re-export edited too
F = {f"mid/f{i:04d}.py": f"def filler_{i}():\n    return {i}\n" for i in range(560)}
BEFORE = dict(F)
BEFORE.update({
 "aaa/c.py": "import zzz.pkg as pkg\nfrom zzz.pkg import X, W\n\ndef run():\n    pkg.X()\n    pkg.W()\n    X()\n    W()\n",
 "aaa/decoy.py": "def X():\n    pass\n\ndef W():\n    pass\n\ndef V():\n    pass\n",
 "zzz/__init__.py": "",
 "zzz/pkg/__init__.py": "from .m import X\nfrom .n import W\n",
 "zzz/pkg/m.py": "def X():\n    pass\n",
 "zzz/pkg/n.py": "def W():\n    pass\n",
})
S = {p: c + "\n# edit\n" for p, c in F.items()}
S.update({"zzz/pkg/m.py": "def Xr():\n    pass\n", "zzz/pkg/n.py": None, "zzz/pkg/o.py": "def W():\n    pass\n", "zzz/pkg/__init__.py": "from .m import X\nfrom .o import W\n"})
STEPS = [S]
SHOW = ["aaa/c.py"]
