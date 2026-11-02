class D:
    def m(self):
        return 'D'


class C:
    def m(self):
        return 'C'


match D:
    case C:
        pass


def t_call():
    return C.m(None)
