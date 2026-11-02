F = {f"mid/f{i:04d}.py": f"def filler_{i}():\n    return {i}\n" for i in range(80)}
BEFORE = dict(F)
BEFORE.update({
 "pkg/__init__.py": "VERSION = 1\n",
 "app/c.py": "import pkg\n\ndef X():\n    pass\n\ndef run():\n    pkg.X()\n",
})
S = {p: c + "\n# edit\n" for p, c in F.items()}
S.update({"pkg/__init__.py": "VERSION = 1\n\ndef X():\n    pass\n"})
STEPS = [S]
SHOW = ["run@"]
