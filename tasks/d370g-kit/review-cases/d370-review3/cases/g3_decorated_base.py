from dataclasses import dataclass

class Base:
    def m(self):
        return "Base"

class Mixin(Base):
    pass

@dataclass
class Data(Base):
    def m(self):
        return "Data"

class D(Mixin, Data):
    pass

def call(x):
    return D.m(x)

print([k.__name__ for k in D.__mro__], call(None))
