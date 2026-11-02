FILES = {
"a.py": """from b import Base


class C(Base):
    def g(self):
        C.render(self)
""",
"b.py": "class Base:\n    pass\n",
"c.py": "class Base:\n    def render(self):\n        pass\n",
"d.py": "class Other:\n    def render(self):\n        pass\n",
}
