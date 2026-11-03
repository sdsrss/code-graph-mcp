class Drop(type):
    def __new__(mcs, name, bases, ns):
        ns.pop("m", None)
        return super().__new__(mcs, name, bases, ns)
