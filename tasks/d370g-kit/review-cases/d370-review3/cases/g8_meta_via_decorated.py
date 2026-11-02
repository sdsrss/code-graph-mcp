def keep(cls):
    return cls

class Meta(type):
    @property
    def m(cls):
        return lambda *a: "Meta.m"

@keep
class Root(metaclass=Meta):
    pass

class C(Root):
    def m(self):
        return "C.m"

def call(x):
    return C.m(x)

print(call(None))
