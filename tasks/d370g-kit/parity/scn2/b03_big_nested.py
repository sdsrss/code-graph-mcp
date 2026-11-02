F = {f"mid/f{i:04d}.py": f"def filler_{i}():\n    return {i}\n" for i in range(80)}
BEFORE = dict(F)
BEFORE.update({
 "pkg/__init__.py": "def outer():\n    def X():\n        pass\n    return X\n",
 "app/c.py": "import pkg\n\ndef run():\n    pkg.X()\n",
 "app/other.py": "def X():\n    pass\n",
})
S = {p: c + "\n# edit\n" for p, c in F.items()}
S.update({"pkg/__init__.py": "def outer():\n    return 1\n\ndef X():\n    pass\n"})
STEPS = [S]
SHOW = ["run@"]
