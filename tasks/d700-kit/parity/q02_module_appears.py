APP = "import ext\nfrom ext import C\n\ndef go():\n    pass\n\nclass L:\n    def m(self):\n        pass\n\ndef main():\n    ext.sub.go()\n    C.m(None)\n"
EXT = "def go():\n    pass\n\nclass C:\n    def m(self):\n        pass\n"
BEFORE = {"app.py": APP}
STEPS = [{"ext.py": EXT}, {"ext.py": None}, {"pkg/ext.py": EXT}, {"ext/__init__.py": EXT}]
