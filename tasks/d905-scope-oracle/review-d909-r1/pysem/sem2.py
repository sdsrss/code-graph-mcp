class Row:
    pass
class C:
    def m(self, x: Row):
        pass
    def Row(self):
        pass
print("S24 (3.14 lazy):", C.m.__annotations__)

class D:
    def Row(self):
        pass
    def m(self):
        def inner(x: Row):
            pass
        return inner
print("S22:", D().m().__annotations__)

class E:
    y = 1
    def m(self, x: Row):
        pass
    def Row(self):
        pass
print("S24b non-first, method later:", E.m.__annotations__)
