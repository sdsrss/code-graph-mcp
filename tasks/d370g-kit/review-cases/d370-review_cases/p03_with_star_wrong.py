FILES = {
"w.py": """from base import Base


class C(Base):
    pass


def f(g):
    with g() as (head, *C):
        C.append(head)
""",
"base.py": """class Base:
    def append(self, x):
        pass
""",
}
