APP = "try:\n    from _x import C\nexcept ImportError:\n    pass\n\nclass L:\n    def m(self):\n        pass\n\ndef main():\n    C.m(None)\n"
BEFORE = {"app.py": APP, "_x.py": "class C:\n    def m(self):\n        pass\n"}
STEPS = [{"app.py": APP.replace("    pass\n\nclass L", "    class C:\n        def m(self):\n            pass\n\nclass L")}, {"app.py": APP}, {"_x.py": None}]
