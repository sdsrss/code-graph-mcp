APP = "import ext\n\ndef go():\n    pass\n\ndef main():\n    ext.sub.go()\n"
BEFORE = {"app.py": APP, "ext.py": "def go():\n    pass\n"}
STEPS = [{"lib/ext.py": APP}, {"app.py": None}, {"ext.py": None}, {"src/ext/__init__.py": APP}]
