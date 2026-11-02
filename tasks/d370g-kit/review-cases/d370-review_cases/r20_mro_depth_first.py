FILES = {"mixins.py": """class Base1:
    def m(self):
        pass


class A(Base1):
    pass


class B:
    def m(self):
        pass


class C(A, B):
    def go(self):
        C.m(self)
"""}
