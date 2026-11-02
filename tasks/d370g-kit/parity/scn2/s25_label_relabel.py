F = {f"mid/f{i:04d}.py": f"def filler_{i}():\n    return {i}\n" for i in range(70)}
BEFORE = dict(F)
BEFORE.update({
 "pkg/__init__.py": "from .m import W\n\ndef X():\n    pass\n",
 "pkg/m.py": "def W():\n    pass\n",
 "app/c.py": "import pkg\n\ndef run():\n    pkg.X()\n    pkg.W()\n    pkg.V()\n",
 "app/v1.py": "def V():\n    pass\n",
})
S1 = {"lib/d1.py": "def X():\n    pass\n\ndef W():\n    pass\n\ndef V():\n    pass\n"}
S2 = {p: c + "\n# e\n" for p, c in F.items()}
S2.update({"lib/d2.py": "def X():\n    pass\n\ndef W():\n    pass\n\ndef V():\n    pass\n"})
S3 = {"lib/d1.py": None}
S4 = dict({p: c + "\n# f\n" for p, c in F.items()}); S4.update({"lib/d2.py": None, "app/v1.py": None})
STEPS = [S1, S2, S3, S4]
SHOW = ["app/c.py"]
