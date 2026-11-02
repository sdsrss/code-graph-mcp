FILES = {
"a.py": """from external_pkg import Base


class C(Base):
    def f(self):
        C.close(self)

    def g(self):
        C.__init__(self)
""",
"b.py": """class Base:
    def __init__(self):
        pass

    def close(self):
        pass
""",
}
