from ext import Base


class C(Base):
    def __m(self):
        return "a.C.__m"


def run():
    return C._C__m(C())


print(run())
