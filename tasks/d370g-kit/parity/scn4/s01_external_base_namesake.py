A = ("from external_pkg import Base\n\n\nclass C(Base):\n    def f(self):\n        C.close(self)\n\n"
     "    def g(self):\n        C.render(self)\n")
B = "class Base:\n    def close(self):\n        pass\n\n    def render(self):\n        pass\n"
BEFORE = {"a.py": A, "other.py": "def h():\n    pass\n"}
STEPS = [{"b.py": B}, {"b.py": None}, {"b.py": B}, {"b.py": "class Base:\n    pass\n"}]
