class G:
    def m(self):
        return "G"

Alias = G

class A(G):
    pass

class B(Alias):
    def m(self):
        return "B"

class D(A, B):
    pass

def call(x):
    return D.m(x)

print([k.__name__ for k in D.__mro__], call(None))
