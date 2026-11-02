FILES = {
"w.py": """import sys
from a import A
from b import B

if sys.version_info >= (3, 12):
    class C(A):
        def m(self):
            pass
else:
    class C(B):
        pass


class D:
    def go(self):
        C.m(self)
        C.n(self)
""",
"a.py": "class A:\n    def m(self):\n        pass\n\n    def n(self):\n        pass\n",
"b.py": "class B:\n    def m(self):\n        pass\n\n    def n(self):\n        pass\n",
}
