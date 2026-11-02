FILES = {
"w.py": """from base import Base


class C(Base):
    class Inner:
        def __init__(self):
            pass

    @staticmethod
    def helper():
        pass

    def go(self):
        C.Inner()
        C.helper()
        C.create()
        C.__init__(self)


class Other:
    class Inner:
        pass

    def helper(self):
        pass

    def __init__(self):
        pass


C.create()
""",
"base.py": """class Base:
    @classmethod
    def create(cls):
        pass

    def __init__(self):
        pass
""",
}
