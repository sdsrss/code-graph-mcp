FILES = {
"a.py": """from b import Base


class C(Base):
    def f(self):
        C.close(self)

    def g(self):
        C.render(self)
""",
"b.py": "class Base:\n    pass\n",
"c.py": "class Base:\n    def close(self):\n        pass\n\n    def render(self):\n        pass\n",
}
