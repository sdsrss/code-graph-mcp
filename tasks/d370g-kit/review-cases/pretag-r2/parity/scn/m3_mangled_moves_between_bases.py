BEFORE = {
 "ext.py": "class Base:\n    def _C__m(self):\n        return 1\n",
 "ext2.py": "from ext import Base\n\nclass Mid(Base):\n    pass\n",
 "a.py": "from ext2 import Mid\n\n\nclass C(Mid):\n    def __m(self):\n        return 2\n\n\ndef run():\n    return C._C__m(C())\n",
}
STEPS = [{"ext2.py": "from ext import Base\n\nclass Mid(Base):\n    def _C__m(self):\n        return 3\n"}]
SHOW = ["a.py:run"]
