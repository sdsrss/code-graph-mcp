FILES = {
"compat.py": """try:
    from fastlib import Base
except ImportError:
    class Base(object):
        pass


class D(Base):
    def run(self):
        super().run()
""",
"fastlib.py": """class Base:
    def run(self):
        pass
""",
}
