FILES = {
"servers.py": """from base1 import Base1
from other import B


class A(Base1):
    pass


class C(A, B):
    def go(self):
        C.close(self)
""",
"base1.py": "class Base1:\n    def close(self):\n        pass\n",
"other.py": "class B:\n    def close(self):\n        pass\n",
}
