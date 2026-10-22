BEFORE = {"pkg/__init__.py": "", "pkg/m.py": "def Y():\n    return 1\n",
          "app.py": "import pkg.m as mm\n\ndef via_alias():\n    return mm.Y()\n",
          "app_imp.py": "from pkg.m import Y\n\ndef imp_call():\n    return Y()\n"}
for i in range(520):
    BEFORE[f"f/{i:03d}.py"] = f"def filler_{i}():\n    return {i}\n"
STEP = {f"f/{i:03d}.py": f"# e\ndef filler_{i}():\n    return {i}\n" for i in range(520)}
STEP["app.py"] = "# e\n" + BEFORE["app.py"]
STEP["app_imp.py"] = "# e\n" + BEFORE["app_imp.py"]
STEP["pkg/m.py"] = "# e\n" + BEFORE["pkg/m.py"]
STEPS = [STEP]
SHOW = ["app"]
