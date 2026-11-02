FILES = {"r.py": """class C:
    def m(self, n):
        if n:
            C.m(self, n - 1)


class D:
    def m(self):
        pass
""", "s.py": """class Base:
    def m(self):
        pass


class E(Base):
    def m(self):
        E.m(self)
        super().m()
"""}
