FILES = {
"test_x.py": """from lib import Base


def test_something():
    class Base:
        pass
    return Base


class D(Base):
    def run(self):
        super().run()
""",
"lib.py": """class Base:
    def run(self):
        pass
""",
}
