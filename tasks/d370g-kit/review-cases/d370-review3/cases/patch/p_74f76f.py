class C:
    def m(self):
        return "C.m"
    def n(self):
        pass

def p():
    (C).m = len

def call(x):
    return C.m(x)
