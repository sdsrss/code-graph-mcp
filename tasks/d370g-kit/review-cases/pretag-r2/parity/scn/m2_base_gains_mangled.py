BEFORE = {
 "ext.py": "class Base:\n    def _C__x(self):\n        return 1\n",
 "a.py": "from ext import Base\n\n\nclass C(Base):\n    def __m(self):\n        return 2\n\n\ndef run():\n    return C._C__m(C())\n",
}
STEPS = [{"ext.py": "class Base:\n    def _C__m(self):\n        return 1\n"}]
SHOW = ["a.py:run"]
