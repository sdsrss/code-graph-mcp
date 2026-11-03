def f(a=str.upper("x")):
    return a


class str:
    def upper(self):
        return "file str.upper"


print(f())
