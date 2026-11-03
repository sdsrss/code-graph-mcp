class B:
    @classmethod
    def __init_subclass__(cls, **kw):
        return "B.__init_subclass__"


class A[T]:
    pass


class C(A, B):
    pass


def t_call():
    return C.__init_subclass__()


print(t_call())
print(C.__mro__)
