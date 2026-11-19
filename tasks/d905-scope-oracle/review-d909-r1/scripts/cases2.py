CASES = {
    "T01 decorated nested classes in sibling functions": {"m.py":
        "from dataclasses import dataclass\n\n\ndef a():\n    @dataclass\n    class Row:\n        x: int = 0\n    return Row()\n\n\n"
        "def b():\n    @dataclass\n    class Row:\n        x: int = 0\n    return Row()\n"},
    "T02 class in if inside function vs module class": {"m.py":
        "class Row:\n    pass\n\n\ndef a(flag):\n    if flag:\n        class Row:\n            pass\n    return Row()\n\n\ndef b():\n    return Row()\n"},
    "T03 class nested in class nested in function": {"m.py":
        "def a():\n    class Outer:\n        class Row:\n            pass\n    return Row()\n\n\ndef b():\n    class Row:\n        pass\n    return Row()\n"},
    "T04 class same name as the function containing it": {"m.py":
        "def Row():\n    class Row:\n        pass\n    return Row()\n\n\ndef b():\n    return Row()\n"},
    "T05 nested class vs nested def same name same function": {"m.py":
        "def a():\n    def Row():\n        pass\n    class Row:\n        pass\n    return Row()\n"},
    "T06 method calls enclosing class by name (module class + nested elsewhere)": {"m.py":
        "class Node:\n    def clone(self):\n        return Node()\n\n\ndef g():\n    class Node:\n        pass\n    return Node()\n"},
    "T07 method annotation names its own nested class (fn)": {"m.py":
        "class Node:\n    pass\n\n\ndef make():\n    class Node:\n        def merge(self, other: Node) -> Node:\n            return other\n    return Node\n"},
    "T08 property shadows a module class in the first method's header (3.14 lazy)": {"m.py":
        "class time:\n    pass\n\n\nclass Event:\n    def __init__(self, at: time):\n        self._at = at\n\n    @property\n    def time(self) -> time:\n        return self._at\n"},
    "T09 class decorator nested": {"m.py":
        "def register(c):\n    return c\n\n\ndef a():\n    def register(c):\n        return c\n    @register\n    class K:\n        pass\n    return K\n\n\n@register\nclass Top:\n    pass\n"},
    "T10 local value call of a class (class bound to a param)": {"m.py":
        "class Row:\n    pass\n\n\ndef a(Row=Row):\n    return Row()\n\n\ndef b():\n    class Row:\n        pass\n    return Row\n"},
    "T11 class body call of a sibling nested class (calls in class body)": {"m.py":
        "class Row:\n    pass\n\n\nclass C:\n    class Row:\n        pass\n    x = Row()\n\n\ndef f():\n    class Row:\n        pass\n    return Row()\n"},
    "T12 lambda body calls nested class": {"m.py":
        "class Row:\n    pass\n\n\ndef a():\n    class Row:\n        pass\n    return lambda: Row()\n"},
    "T13 comprehension calls nested class": {"m.py":
        "class Row:\n    pass\n\n\ndef a(xs):\n    class Row:\n        pass\n    return [Row() for _ in xs]\n"},
    "T14 global class used in a later module-level call": {"m.py":
        "def setup():\n    global Row\n    class Row:\n        pass\n\n\nsetup()\nr = Row()\n\n\ndef other():\n    class Row:\n        pass\n    return Row\n"},
}
