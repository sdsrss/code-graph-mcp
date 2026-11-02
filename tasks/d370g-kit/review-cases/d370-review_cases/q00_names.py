FILES = {"a.py": """class Outer:
    class Inner:
        def m(self):
            pass

def make():
    class Local:
        def m(self):
            pass
    return Local

class Top:
    def m(self):
        pass
"""}
