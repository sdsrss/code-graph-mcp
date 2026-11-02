class C:
    def m(self):
        return "C.m"
    def n(self):
        pass

[(C).m for _ in ()]

def call(x):
    return C.m(x)
