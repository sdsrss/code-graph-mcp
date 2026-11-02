W = "class C:\n    pass\n\n\nclass D:\n    pass\n\n\ndef f(x):\n    C.render(x)\n    D.render(x)\n"
BEFORE = {"w.py": W, "o.py": "def h():\n    pass\n"}
STEPS = [{"x.py": "class X:\n    def render(self):\n        pass\n"}]
