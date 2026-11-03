APP = "import lib.ext\n\ndef go():\n    pass\n\ndef main():\n    lib.ext.go()\n    lib.ext.sub.go()\n"
BEFORE = {"src/lib/ext.py": "def go():\n    pass\n", "src/app.py": APP}
STEPS = [{"src/__init__.py": ""}, {"src/lib/__init__.py": ""}, {"src/__init__.py": None}, {"src/app.py": APP.replace("import lib.ext", "import lib.ext as lib")}]
