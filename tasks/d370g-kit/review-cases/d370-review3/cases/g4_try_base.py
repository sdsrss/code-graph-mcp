class Base:
    def m(self):
        return "Base"

class Mixin(Base):
    pass

try:
    class Impl(Base):
        def m(self):
            return "Impl"
except ImportError:
    Impl = None

class D(Mixin, Impl):
    pass

def call(x):
    return D.m(x)

print([k.__name__ for k in D.__mro__], call(None))
