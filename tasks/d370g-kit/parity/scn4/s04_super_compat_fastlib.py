C = ("try:\n    from fastlib import Base\nexcept ImportError:\n    class Base(object):\n        pass\n\n\n"
     "class D(Base):\n    def run(self):\n        super().run()\n")
F = "class Base:\n    def run(self):\n        pass\n"
BEFORE = {"compat.py": C, "other.py": "def h():\n    pass\n"}
STEPS = [{"fastlib.py": F}, {"fastlib.py": None}, {"fastlib.py": F}, {"fastlib.py": "class Base:\n    pass\n"}]
