from typing import Generic, TypeVar
T = TypeVar("T")

class G(Generic[T]):
    def m(self):
        return "G"

class A(G):
    pass

class B(G[int]):
    def m(self):
        return "B"

class D(A, B):
    pass

def call(x):
    return D.m(x)

print([k.__name__ for k in D.__mro__], call(None))
