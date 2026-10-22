BEFORE = {
 "pkg/__init__.py": "from .core import X\n",
 "pkg/core.py": "def X():\n    pass\n",
 "pkg/core2.py": "def X():\n    pass\n",
 "zzz/decoy.py": "def X():\n    pass\n",
 "yyy/decoy2.py": "def X():\n    pass\n",
}
for i in range(5):
    BEFORE[f"callers/app{i}.py"] = "import pkg\nfrom pkg import X\n\ndef main():\n    pkg.X()\n\ndef other():\n    X()\n"
for i in range(80):
    BEFORE[f"filler/f{i}.py"] = f"def filler{i}():\n    return {i}\n"
step = {f"filler/f{i}.py": f"def filler{i}():\n    return {i} + 1\n" for i in range(80)}
step["pkg/__init__.py"] = "from .core2 import X\n"
STEPS = [step]
