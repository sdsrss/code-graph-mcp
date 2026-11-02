SRC = """from dataclasses import dataclass

class Base:
    def m(self):
        return "Base"

class Mixin(Base):
    pass

{dec}class Data(Base):
    def m(self):
        return "Data"

class D(Mixin, Data):
    pass

def call(x):
    return D.m(x)
"""
BEFORE = {"a.py": SRC.format(dec="")}
STEPS = [{"a.py": SRC.format(dec="@dataclass\n")}, {"b.py": "class Base:\n    def m(self):\n        pass\n"}, {"a.py": SRC.format(dec="")}, {"b.py": None}]
