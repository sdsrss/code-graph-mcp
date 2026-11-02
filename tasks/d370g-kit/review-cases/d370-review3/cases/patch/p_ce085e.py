class C:
    def m(self):
        return "C.m"
    def n(self):
        pass

for C.m in [len]: pass

def call(x):
    return C.m(x)
