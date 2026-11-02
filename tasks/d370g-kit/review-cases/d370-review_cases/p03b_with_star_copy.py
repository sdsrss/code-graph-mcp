FILES = {
"w.py": """from base import Base


class C(Base):
    pass


def f(g):
    with g() as (head, *C):
        C.copy()
""",
"base.py": """class Base:
    def copy(self):
        pass
""",
}
